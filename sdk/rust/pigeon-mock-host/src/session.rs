//! A launched plugin, driven the way Pigeon drives it (protocol v2 sections 2, 3, 9 and 10).
//!
//! Every line the plugin writes is validated against `schema/ui-v2` and recorded. Every line the
//! host sends is validated first, so a test cannot accidentally send an invalid host message
//! (use [`Session::send_raw`] to send deliberately broken lines).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::{fs::DirBuilderExt, process::CommandExt}};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pigeon_plugin_sdk::protocol::{HostInfo, Hello, Incoming, NodeEvent, ProtocolRange, Theme, MAX_LINE_BYTES, MAX_SCENE_NODES};
use serde_json::{json, Value};

use crate::behavior::{find_matching, node_children};
use crate::render::{resource, ImageRef};
use crate::schemas::{method_permission, Schemas, Violation};
use crate::services::{Fixtures, Services};

/// Bytes of stderr kept per session (protocol section 2).
pub const STDERR_KEEP: usize = 200 * 1024;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The `ui` block of a plugin.json, plus the identity fields the mock needs.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub ui: Value,
    pub permissions: Vec<String>,
    pub dir: PathBuf,
}

/// Read and validate `<dir>/plugin.json`.
pub fn load_manifest(dir: &Path, schemas: &Schemas) -> Result<Manifest, String> {
    let text = fs::read_to_string(dir.join("plugin.json")).map_err(|e| format!("cannot read plugin.json: {e}"))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("plugin.json is not JSON: {e}"))?;
    let id = doc["id"].as_str().ok_or("plugin.json has no id")?.to_string();
    let version = doc["version"].as_str().ok_or("plugin.json has no version")?.to_string();
    let ui = doc.get("ui").cloned().unwrap_or(Value::Null);
    if !ui.is_null() {
        let problems = schemas.check_manifest_ui(&ui);
        if !problems.is_empty() {
            return Err(format!("plugin.json ui block is invalid: {}", problems[0]));
        }
    }
    let permissions = ui["permissions"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(Manifest { id, version, ui, permissions, dir: dir.to_path_buf() })
}

/// Options for one launch.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub theme: Theme,
    pub scale: f32,
    pub locale: String,
    pub fixtures: Fixtures,
    /// Run `run --setup` first (Pigeon always does; tests may skip it for speed after a first run).
    pub run_setup: bool,
}

impl Default for LaunchOptions {
    fn default() -> LaunchOptions {
        LaunchOptions { theme: crate::render::dark_theme_protocol(), scale: 2.0, locale: "en".into(), fixtures: Fixtures::default(), run_setup: true }
    }
}

/// Everything the mock has observed from the plugin, guarded by one mutex.
#[derive(Default)]
pub struct State {
    /// The full `ready` message, once received.
    pub ready: Option<Value>,
    /// Latest scene per instance, including fast canvas and list updates. Patches are only recorded.
    pub scenes: BTreeMap<String, Value>,
    pub(crate) scene_revisions: BTreeMap<String, u64>,
    pub patches: Vec<Value>,
    /// Latest draw value `{frame, clear, ops}` per (instance, node).
    pub canvases: BTreeMap<(String, String), Value>,
    pub canvas_updates: u64,
    pub list_rows: BTreeMap<(String, String), Vec<Value>>,
    /// Latest `resource.put` per rid.
    pub resources: BTreeMap<String, Value>,
    pub resource_puts: u64,
    pub(crate) images: HashMap<String, ImageRef>,
    pub focus: Vec<Value>,
    pub scroll_to: Vec<Value>,
    pub beeps: u64,
    pub contributions: Vec<Value>,
    pub source_changed: Vec<Value>,
    pub search_status: Vec<Value>,
    pub pongs: Vec<Value>,
    /// Service requests the plugin made, with the reply the mock gave.
    pub requests: Vec<Value>,
    pub service_replies: BTreeMap<String, Value>,
    /// Answers the plugin gave to provider requests (`res` lines), keyed by request id.
    pub provider_answers: BTreeMap<String, Value>,
    /// Every line the plugin wrote, as JSON (or a string for unparseable lines).
    pub outgoing: Vec<Value>,
    /// Schema violations and protocol problems, each with the offending line.
    pub violations: Vec<(Violation, String)>,
    pub unknown_types: Vec<String>,
    pub exited: Option<ExitStatus>,
    pub stderr_tail: String,
    pub closed: bool,
}

pub(crate) struct Shared {
    pub state: Mutex<State>,
    pub cond: Condvar,
    pub schemas: Arc<Schemas>,
    pub services: Mutex<Services>,
    pub permissions: Vec<String>,
    pub session_dir: PathBuf,
    /// Plugin stdin, shared so the reader can answer service requests.
    pub stdin: Arc<Mutex<Option<ChildStdin>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// A running plugin session.
pub struct Session {
    pub manifest: Manifest,
    pub session_dir: PathBuf,
    pub data_dir: PathBuf,
    shared: Arc<Shared>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Mutex<Child>,
    pid: u32,
    readers: Mutex<Vec<JoinHandle<()>>>,
    host_req_counter: AtomicU64,
    _temp_root: TempRoot,
}

/// Why a launch failed.
#[derive(Debug)]
pub enum LaunchError {
    Setup(String),
    Spawn(std::io::Error),
    Manifest(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::Setup(m) => write!(f, "run --setup failed: {m}"),
            LaunchError::Spawn(e) => write!(f, "cannot start the plugin: {e}"),
            LaunchError::Manifest(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for LaunchError {}

impl Session {
    /// Launch the plugin in `dir`: `run --setup` (unless disabled), then `run --plugin`, then
    /// send `hello`.
    pub fn launch(dir: &Path, schemas: Arc<Schemas>, options: LaunchOptions) -> Result<Session, LaunchError> {
        // The launcher runs with cwd = plugin folder, so the path must be absolute.
        let dir = &dir.canonicalize().map_err(LaunchError::Spawn)?;
        let manifest = load_manifest(dir, &schemas).map_err(LaunchError::Manifest)?;
        let n = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("pigeon-mock-{}-{n}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&base).map_err(LaunchError::Spawn)?;
        let temp_root = TempRoot(base);
        let session_dir = temp_root.0.join("session");
        let data_dir = temp_root.0.join("data");
        fs::create_dir(&session_dir).map_err(LaunchError::Spawn)?;
        fs::create_dir(&data_dir).map_err(LaunchError::Spawn)?;

        if options.run_setup {
            let log = temp_root.0.join("setup.log");
            let output = fs::File::create(&log).map_err(LaunchError::Spawn)?;
            let mut setup = launcher(dir, "--setup", &data_dir, &session_dir)
                .stdin(Stdio::null())
                .stdout(output.try_clone().map_err(LaunchError::Spawn)?)
                .stderr(output)
                .spawn().map_err(LaunchError::Spawn)?;
            let status = wait_child(&mut setup, Duration::from_secs(120)).map_err(LaunchError::Spawn)?;
            let Some(status) = status else { return Err(LaunchError::Setup("timed out after 120 s".into())); };
            if !status.success() {
                let mut tail = Vec::new();
                let _ = fs::File::open(log).and_then(|f| f.take(2048).read_to_end(&mut tail));
                return Err(LaunchError::Setup(format!("{status}: {}", String::from_utf8_lossy(&tail).trim())));
            }
        }

        let mut child = launcher(dir, "--plugin", &data_dir, &session_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(LaunchError::Spawn)?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        #[cfg(unix)]
        if let Err(error) = make_nonblocking(&stdin) {
            terminate(&mut child);
            let _ = child.wait();
            return Err(LaunchError::Spawn(error));
        }

        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            schemas: Arc::clone(&schemas),
            services: Mutex::new(Services::new(options.fixtures.clone(), manifest.permissions.clone())),
            permissions: manifest.permissions.clone(),
            session_dir: session_dir.clone(),
            stdin: Arc::clone(&stdin),
        });

        let mut readers = Vec::new();
        let out_shared = Arc::clone(&shared);
        readers.push(thread::Builder::new().name("mock-stdout".into()).spawn(move || {
            read_stdout(&out_shared, stdout);
        }).map_err(LaunchError::Spawn)?);
        let err_shared = Arc::clone(&shared);
        readers.push(thread::Builder::new().name("mock-stderr".into()).spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut stderr = stderr;
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > STDERR_KEEP {
                            let cut = buf.len() - STDERR_KEEP;
                            buf.drain(..cut);
                        }
                    }
                }
            }
            let mut st = lock(&err_shared.state);
            st.stderr_tail = String::from_utf8_lossy(&buf).into_owned();
            err_shared.cond.notify_all();
        }).map_err(LaunchError::Spawn)?);

        let session = Session {
            manifest,
            session_dir: session_dir.clone(),
            data_dir,
            shared,
            stdin,
            child: Mutex::new(child),
            pid,
            readers: Mutex::new(readers),
            host_req_counter: AtomicU64::new(1),
            _temp_root: temp_root,
        };
        let hello = Incoming::Hello(Hello {
            host: HostInfo { name: "Pigeon".into(), version: "mock".into() },
            protocol: ProtocolRange { min: 2, max: 2 },
            theme: options.theme.clone(),
            locale: options.locale.clone(),
            scale: options.scale,
            data_dir: Some(session.data_dir.to_string_lossy().into_owned()),
            session_dir: session_dir.to_string_lossy().into_owned(),
            features: vec!["canvas".into(), "resources.file".into(), "list.window".into(), "source".into(), "search".into()],
        });
        session.write_host_line(&hello.to_value())?;
        Ok(session)
    }

    /// Wait until `ready` arrives (protocol: within 10 s).
    pub fn wait_ready(&self, timeout: Duration) -> Result<Value, String> {
        self.wait_until(timeout, |st| st.ready.clone(), "ready").ok_or_else(|| {
            let st = lock(&self.shared.state);
            format!("no ready within {timeout:?}; stderr tail: {}", st.stderr_tail.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>())
        })
    }

    /// Run `f` on the state under the lock, returning its result.
    pub fn with_state<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&lock(&self.shared.state))
    }

    /// Wait until `pred` returns Some, or the timeout passes. Returns the value.
    pub fn wait_until<T>(&self, timeout: Duration, mut pred: impl FnMut(&State) -> Option<T>, _what: &str) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut st = lock(&self.shared.state);
        loop {
            if let Some(v) = pred(&st) {
                return Some(v);
            }
            let now = Instant::now();
            if st.closed || now >= deadline {
                return None;
            }
            let (guard, _) = match self.shared.cond.wait_timeout(st, deadline - now) {
                Ok(p) => p,
                Err(p) => p.into_inner(),
            };
            st = guard;
        }
    }

    /// The latest scene of an instance, if the plugin sent one.
    pub fn scene(&self, instance: &str) -> Option<Value> {
        self.with_state(|st| st.scenes.get(instance).cloned())
    }

    /// Capture the current scene and its decoded resource images under the same lock.
    pub fn render_snapshot(&self, instance: &str) -> Option<(Value, HashMap<String, ImageRef>)> {
        self.with_state(|st| Some((st.scenes.get(instance)?.clone(), st.images.clone())))
    }

    /// Wait for a scene to contain a node with this id.
    pub fn wait_for_node(&self, instance: &str, node: &str, timeout: Duration) -> Option<Value> {
        let inst = instance.to_string();
        let node = node.to_string();
        self.wait_until(timeout, move |st| st.scenes.get(&inst).and_then(|s| find_node(s, &node).cloned()), "node")
    }

    /// Send a validated host-to-plugin message. Refuses messages that break the schema.
    pub fn send(&self, msg: &Incoming) -> Result<(), String> {
        let value = msg.to_value();
        let problems = self.shared.schemas.check_to_plugin(&value);
        if let Some(p) = problems.first() {
            return Err(format!("mock refuses to send an invalid host message: {p}"));
        }
        self.write_host_line(&value).map_err(|e| e.to_string())
    }

    /// Send a raw line exactly as given (for robustness tests: garbage, unknown types, oversize).
    pub fn send_raw(&self, line: &str) -> Result<(), String> {
        let mut guard = lock(&self.stdin);
        let stdin = guard.as_mut().ok_or("stdin closed")?;
        write_with_deadline(stdin, line.as_bytes()).and_then(|_| write_with_deadline(stdin, b"\n")).map_err(|e| e.to_string())
    }

    fn write_host_line(&self, value: &Value) -> Result<(), LaunchError> {
        write_to_plugin(&self.stdin, value).map_err(LaunchError::Setup)
    }

    /// Deliver a `ui.event` for a node.
    pub fn send_event(&self, instance: &str, node: &str, event: NodeEvent) -> Result<(), String> {
        self.send(&Incoming::UiEvent { instance: instance.into(), node: node.into(), event })
    }

    /// Tell the plugin an instance became visible (what Pigeon does when a slot opens).
    pub fn show(&self, instance: &str) -> Result<(), String> {
        self.send(&Incoming::Visibility { instance: instance.into(), visible: true })
    }

    /// Ask the host-side question "are you alive": send ping and wait for the matching pong.
    pub fn ping(&self, timeout: Duration) -> Option<Duration> {
        let id = format!("ping-{}", self.host_req_counter.fetch_add(1, Ordering::SeqCst));
        let start = Instant::now();
        self.send(&Incoming::Ping { id: Some(id.clone()) }).ok()?;
        self.wait_until(timeout, |st| st.pongs.iter().any(|p| p["id"] == id).then_some(()), "pong")?;
        Some(start.elapsed())
    }

    /// Send a provider request (`req`) and wait for the plugin's `res`.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = format!("h{}", self.host_req_counter.fetch_add(1, Ordering::SeqCst));
        let problems = self.shared.schemas.check_to_plugin(&json!({"v":2,"t":"req","id":id,"method":method,"params":params}));
        if let Some(p) = problems.first() {
            return Err(format!("invalid provider request: {p}"));
        }
        // Check the params against the method's schema too.
        let key = format!("{method}.params");
        let pv = self.shared.schemas.check_method(&key, &params);
        if let Some(p) = pv.first() {
            return Err(format!("params for {method} violate the schema: {p}"));
        }
        self.write_host_line(&json!({"v":2,"t":"req","id":id,"method":method,"params":params})).map_err(|e| e.to_string())?;
        let rid = id.clone();
        self.wait_until(timeout, move |st| st.provider_answers.get(&rid).cloned(), "res")
            .ok_or_else(|| format!("no answer to {method} within {timeout:?}"))
    }

    /// The last service request of a method the plugin made (its params, and whether it succeeded).
    pub fn last_request(&self, method: &str) -> Option<Value> {
        self.with_state(|st| st.requests.iter().rev().find(|r| r["method"] == method).cloned())
    }

    /// Schema violations seen so far (plugin to host).
    pub fn violations(&self) -> Vec<(Violation, String)> {
        self.with_state(|st| st.violations.clone())
    }

    /// Read the service fakes (toasts, progress, modals, settings, ...).
    pub fn services<T>(&self, f: impl FnOnce(&Services) -> T) -> T {
        f(&lock(&self.shared.services))
    }

    /// Mutate the services fakes (for example to change the selection or dialog answers).
    pub fn services_mut<T>(&self, f: impl FnOnce(&mut Services) -> T) -> T {
        f(&mut lock(&self.shared.services))
    }

    /// Process id (for stall simulation tests).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Pause the plugin process (SIGSTOP) for `dur`, then resume it (SIGCONT). Simulates a hung worker.
    pub fn stall(&self, dur: Duration) -> Result<(), String> {
        signal(self.pid, "STOP")?;
        thread::sleep(dur);
        signal(self.pid, "CONT")
    }

    /// Start a stall without waiting; pair with [`Session::resume`].
    pub fn pause(&self) -> Result<(), String> {
        signal(self.pid, "STOP")
    }

    /// Resume after [`Session::pause`].
    pub fn resume(&self) -> Result<(), String> {
        signal(self.pid, "CONT")
    }

    /// Kill the plugin process (simulates a crash). Returns once it has exited.
    pub fn crash(&self) -> Option<ExitStatus> {
        let mut child = lock(&self.child);
        terminate(&mut child);
        let status = child.wait().ok();
        drop(child);
        self.join_readers();
        self.mark_exited(status);
        status
    }

    fn mark_exited(&self, status: Option<ExitStatus>) {
        let mut st = lock(&self.shared.state);
        st.exited = status;
        st.closed = true;
        self.shared.cond.notify_all();
    }

    /// Whether the process is still running.
    pub fn is_running(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Ask the plugin to shut down (`shutdown`), close stdin, and wait for the exit. Kills it if it
    /// does not exit in time. Returns true when it exited successfully on its own.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let _ = self.send(&Incoming::Shutdown);
        lock(&self.stdin).take();
        let status = wait_child(&mut lock(&self.child), timeout).ok().flatten();
        self.join_readers();
        self.mark_exited(status);
        status.is_some_and(|code| code.success())
    }

    fn join_readers(&self) {
        // Ordinary subprocesses share the plugin group and are killed at shutdown. Do not join
        // indefinitely if a deliberately detached child still has stdout or stderr open.
        let deadline = Instant::now() + Duration::from_millis(500);
        for reader in lock(&self.readers).drain(..) {
            while !reader.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if reader.is_finished() { let _ = reader.join(); }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.is_running() { self.crash(); }
        lock(&self.stdin).take();
        self.join_readers();
    }
}

struct TempRoot(PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn launcher(dir: &Path, mode: &str, data: &Path, session: &Path) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg(dir.join("run")).arg(mode).current_dir(dir)
        .env("PIGEON_PLUGIN_DATA", data).env("PIGEON_SESSION_DIR", session);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

fn wait_child(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => { terminate(child); child.wait()?; return Ok(None); }
            result => {
                // The group may still contain subprocesses that inherited the transport pipes.
                #[cfg(unix)]
                let _ = signal(child.id(), "KILL");
                return result;
            }
        }
    }
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    let _ = signal(child.id(), "KILL");
    let _ = child.kill();
}

#[cfg(unix)]
fn signal(pid: u32, sig: &str) -> Result<(), String> {
    let sig = match sig { "STOP" => libc::SIGSTOP, "CONT" => libc::SIGCONT, "KILL" => libc::SIGKILL, _ => return Err("unknown signal".into()) };
    let pid = i32::try_from(pid).map_err(|e| e.to_string())?;
    // SAFETY: kill receives only a scalar signal and the negative id of our isolated process group.
    if unsafe { libc::kill(-pid, sig) } == 0 { Ok(()) } else { Err(io::Error::last_os_error().to_string()) }
}

#[cfg(not(unix))]
fn signal(_: u32, _: &str) -> Result<(), String> { Err("signals require Unix".into()) }

#[cfg(unix)]
fn make_nonblocking(pipe: &ChildStdin) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fd belongs to the live pipe. These two fcntl calls take only scalar arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(io::Error::last_os_error())
    } else { Ok(()) }
}

fn write_with_deadline(pipe: &mut ChildStdin, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !bytes.is_empty() {
        match pipe.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(io::Error::new(io::ErrorKind::TimedOut, "plugin is not reading stdin")),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Find a node by id anywhere in a scene value.
pub fn find_node<'a>(scene: &'a Value, id: &str) -> Option<&'a Value> {
    find_matching(scene, &|node| node["id"].as_str() == Some(id))
}

fn find_node_mut<'a>(scene: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if scene["id"].as_str() == Some(id) { return Some(scene); }
    let object = scene.as_object_mut()?;
    if object.contains_key("children") {
        object.get_mut("children")?.as_array_mut()?.iter_mut().find_map(|child| find_node_mut(child, id))
    } else {
        object.get_mut("window")?.get_mut("rows")?.as_array_mut()?.iter_mut().find_map(|row| find_node_mut(row, id))
    }
}

fn read_stdout(shared: &Arc<Shared>, stdout: impl Read) {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    loop {
        let mut buf = Vec::new();
        let Ok(n) = (&mut reader).take((MAX_LINE_BYTES + 2) as u64).read_until(b'\n', &mut buf) else { break };
        if n == 0 { break; }
        let payload = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let payload = payload.strip_suffix(b"\r").unwrap_or(payload);
        if payload.len() > MAX_LINE_BYTES || (n == MAX_LINE_BYTES + 2 && !buf.ends_with(b"\n")) {
            record_violation(shared, Violation { path: String::new(), message: "line exceeds the 1 MiB limit".into() }, "");
            if !buf.ends_with(b"\n") && reader.skip_until(b'\n').is_err() { break; }
            continue;
        }
        match std::str::from_utf8(payload) {
            Ok(line) if !line.is_empty() => handle_line(shared, line),
            Ok(_) => {},
            Err(_) => record_violation(shared, Violation { path: String::new(), message: "line is not UTF-8".into() }, ""),
        }
    }
    let mut st = lock(&shared.state);
    st.closed = true;
    shared.cond.notify_all();
}

fn record_violation(shared: &Arc<Shared>, v: Violation, line: &str) {
    let mut st = lock(&shared.state);
    st.violations.push((v, line.chars().take(300).collect()));
    shared.cond.notify_all();
}

fn handle_line(shared: &Arc<Shared>, line: &str) {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            record_violation(shared, Violation { path: String::new(), message: format!("not JSON: {e}") }, line);
            return;
        }
    };
    let t = value["t"].as_str().unwrap_or("").to_string();
    let known = ["ready", "contribute", "withdraw", "ui.set", "ui.patch", "ui.focus", "ui.scroll_to", "ui.beep",
        "canvas.draw", "list.rows", "resource.put", "resource.drop", "req", "res", "pong", "source.changed", "search.status"];
    if !known.contains(&t.as_str()) {
        if t.is_empty() || value["v"].as_u64() != Some(2) {
            record_violation(shared, Violation { path: "/".into(), message: "missing or invalid v2 envelope".into() }, line);
            return;
        }
        let mut st = lock(&shared.state);
        st.unknown_types.push(t.clone());
        st.outgoing.push(value);
        shared.cond.notify_all();
        return;
    }
    let problems = shared.schemas.check_to_host(&value);
    if let Some(p) = problems.into_iter().next() {
        record_violation(shared, p, line);
        lock(&shared.state).outgoing.push(value);
        return;
    }
    lock(&shared.state).outgoing.push(value.clone());
    dispatch(shared, &t, value);
}

fn dispatch(shared: &Arc<Shared>, t: &str, v: Value) {
    match t {
        "ready" => {
            let mut st = lock(&shared.state);
            st.contributions = v["contributions"].as_array().cloned().unwrap_or_default();
            st.ready = Some(v);
            shared.cond.notify_all();
        }
        "contribute" => {
            let mut st = lock(&shared.state);
            if let Some(list) = v["contributions"].as_array() {
                st.contributions.extend(list.iter().cloned());
            }
            shared.cond.notify_all();
        }
        "withdraw" => {
            let mut st = lock(&shared.state);
            let ids: Vec<String> = v["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            st.contributions.retain(|c| !ids.iter().any(|i| c["id"] == i.as_str()));
            shared.cond.notify_all();
        }
        "ui.set" => {
            let instance = v["instance"].as_str().unwrap_or("").to_string();
            let scene = v["scene"].clone();
            let mut st = lock(&shared.state);
            if let Err(message) = validate_scene(&scene, &mut HashSet::new()) {
                st.violations.push((Violation { path: "/scene".into(), message }, instance));
            } else {
                *st.scene_revisions.entry(instance.clone()).or_default() += 1;
                st.scenes.insert(instance, scene);
            }
            shared.cond.notify_all();
        }
        "ui.patch" => {
            let mut st = lock(&shared.state);
            st.patches.push(v);
            shared.cond.notify_all();
        }
        "ui.focus" => push(shared, |st| st.focus.push(v)),
        "ui.scroll_to" => push(shared, |st| st.scroll_to.push(v)),
        "ui.beep" => push(shared, |st| st.beeps += 1),
        "canvas.draw" => {
            let key = (v["instance"].as_str().unwrap_or("").to_string(), v["node"].as_str().unwrap_or("").to_string());
            let mut draw = json!({"frame": v["frame"], "ops": v["ops"]});
            let mut st = lock(&shared.state);
            if let Some(node) = st.scenes.get_mut(&key.0).and_then(|sc| find_node_mut(sc, &key.1)) {
                if node["kind"] == "canvas" && !node["draw"]["frame"].as_u64().is_some_and(|f| f > v["frame"].as_u64().unwrap_or_default()) {
                    if let Some(clear) = node["draw"].get("clear") { draw["clear"] = clear.clone(); }
                    node["draw"] = draw.clone();
                    *st.scene_revisions.entry(key.0.clone()).or_default() += 1;
                }
            }
            st.canvases.insert(key, draw);
            st.canvas_updates += 1;
            shared.cond.notify_all();
        }
        "list.rows" => {
            let key = (v["instance"].as_str().unwrap_or("").to_string(), v["node"].as_str().unwrap_or("").to_string());
            push(shared, |st| {
                st.list_rows.insert(key.clone(), v["rows"].as_array().cloned().unwrap_or_default());
                if let Some(node) = st.scenes.get_mut(&key.0).and_then(|sc| find_node_mut(sc, &key.1)) {
                    if node["kind"] == "list" {
                        node["window"] = json!({"start": v["start"], "rows": v["rows"]});
                        *st.scene_revisions.entry(key.0).or_default() += 1;
                    }
                }
            });
        }
        "resource.put" => {
            let rid = v["rid"].as_str().unwrap_or("").to_string();
            let seq = v["seq"].as_u64().unwrap_or(0);
            let image = match resource::load(&shared.session_dir, &v) {
                Ok(image) => image,
                Err(message) => {
                    record_violation(shared, Violation { path: "/file".into(), message }, &rid);
                    return;
                }
            };
            {
                let mut st = lock(&shared.state);
                let usage: usize = st.images.iter().filter(|(key, _)| *key != &rid).map(|(_, img)| img.rgba.len()).sum();
                if usage + image.rgba.len() > resource::CACHE_BYTES {
                    st.violations.push((Violation { path: "/file".into(), message: "decoded resource cache exceeds its limit".into() }, rid.clone()));
                } else if st.resources.get(&rid).and_then(|old| old["seq"].as_u64()).is_none_or(|old| seq > old) {
                    st.images.insert(rid.clone(), image);
                    st.resources.insert(rid.clone(), v);
                    st.resource_puts += 1;
                }
                shared.cond.notify_all();
            }
            // Acknowledge only after reading the file; the plugin can now recycle it.
            let ack = json!({"v": 2, "t": "resource.ack", "rid": rid, "seq": seq});
            if let Err(e) = write_to_plugin(&shared.stdin, &ack) {
                record_violation(shared, Violation { path: "/mock".into(), message: format!("cannot ack {rid}: {e}") }, "");
            }
        }
        "resource.drop" => {
            let rid = v["rid"].as_str().unwrap_or("").to_string();
            push(shared, |st| {
                st.resources.remove(&rid);
                st.images.remove(&rid);
            });
        }
        "source.changed" => push(shared, |st| st.source_changed.push(v)),
        "search.status" => push(shared, |st| st.search_status.push(v)),
        "pong" => push(shared, |st| st.pongs.push(v)),
        "res" => {
            let id = v["id"].as_str().unwrap_or("").to_string();
            push(shared, |st| {
                st.provider_answers.insert(id, v);
            });
        }
        "req" => handle_service_request(shared, v),
        _ => {}
    }
}

fn push(shared: &Arc<Shared>, f: impl FnOnce(&mut State)) {
    let mut st = lock(&shared.state);
    f(&mut st);
    shared.cond.notify_all();
}

fn validate_scene<'a>(scene: &'a Value, ids: &mut HashSet<&'a str>) -> Result<(), String> {
    let id = scene["id"].as_str().ok_or("missing node id")?;
    if !ids.insert(id) { return Err(format!("duplicate node id {id:?}")); }
    if ids.len() > MAX_SCENE_NODES { return Err(format!("scene exceeds {MAX_SCENE_NODES} nodes")); }
    for child in node_children(scene) { validate_scene(child, ids)?; }
    Ok(())
}

/// Answer one host-service request (section 10): permission check, parameter and result schema
/// checks, then the fake.
fn handle_service_request(shared: &Arc<Shared>, req: Value) {
    let id = req["id"].as_str().unwrap_or("").to_string();
    let method = req["method"].as_str().unwrap_or("").to_string();
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
    let reply = answer(shared, &method, &params);
    let line = match &reply {
        Ok(result) => json!({"v":2,"t":"res","id":id,"result":result}),
        Err((code, message)) => json!({"v":2,"t":"res","id":id,"error":{"code":code,"message":message}}),
    };
    {
        let mut st = lock(&shared.state);
        st.requests.push(json!({"id": id, "method": method, "params": params, "ok": reply.is_ok()}));
        st.service_replies.insert(id.clone(), line.clone());
        shared.cond.notify_all();
    }
    if let Err(e) = write_to_plugin(&shared.stdin, &line) {
        record_violation(shared, Violation { path: "/mock".into(), message: format!("cannot answer {method}: {e}") }, "");
    }
}

/// Write one host line to the plugin (used by the reader to answer service requests).
pub(crate) fn write_to_plugin(stdin: &Arc<Mutex<Option<ChildStdin>>>, value: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(value).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut guard = lock(stdin);
    let pipe = guard.as_mut().ok_or("stdin closed")?;
    if line.len() > MAX_LINE_BYTES + 1 { return Err("host message exceeds the 1 MiB limit".into()); }
    write_with_deadline(pipe, line.as_bytes()).map_err(|e| e.to_string())
}

fn answer(shared: &Arc<Shared>, method: &str, params: &Value) -> Result<Value, (String, String)> {
    if let Some(perm) = method_permission(method) {
        if !shared.permissions.iter().any(|p| p == perm) {
            return Err(("permission_denied".into(), format!("plugin does not declare the {perm} permission")));
        }
    }
    let problems = shared.schemas.check_method(&format!("{method}.params"), params);
    if let Some(p) = problems.first() {
        if p.message.starts_with("no schema for") {
            return Err(("unknown_method".into(), format!("the host has no method {method}")));
        }
        return Err(("invalid_params".into(), p.to_string()));
    }
    let result = lock(&shared.services).call(method, params)?;
    let problems = shared.schemas.check_method(&format!("{method}.result"), &result);
    if let Some(p) = problems.first() {
        // A bug in the mock's own fake, not in the plugin: recorded so the mock's tests catch it.
        record_violation(shared, Violation { path: "/mock".into(), message: format!("fake for {method} returned an invalid result: {p}") }, "");
    }
    Ok(result)
}
