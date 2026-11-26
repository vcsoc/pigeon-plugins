//! A launched plugin, driven the way Pigeon drives it (protocol v2 sections 2, 3, 9 and 10).
//!
//! Every line the plugin writes is validated against `schema/ui-v2` and recorded. Every line the
//! host sends is validated first, so a test cannot accidentally send an invalid host message
//! (use [`Session::send_raw`] to send deliberately broken lines).

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pigeon_plugin_sdk::protocol::{HostInfo, Hello, Incoming, NodeEvent, ProtocolRange, Theme, MAX_LINE_BYTES};
use serde_json::{json, Value};

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
    /// Latest scene per instance (`ui.set`, also after `ui.patch` is not applied: patches are recorded in `patches`).
    pub scenes: BTreeMap<String, Value>,
    pub patches: Vec<Value>,
    /// Latest draw value `{frame, clear, ops}` per (instance, node).
    pub canvases: BTreeMap<(String, String), Value>,
    pub canvas_updates: u64,
    pub list_rows: BTreeMap<(String, String), Vec<Value>>,
    /// Latest `resource.put` per rid.
    pub resources: BTreeMap<String, Value>,
    pub resource_puts: u64,
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
    readers: Vec<JoinHandle<()>>,
    host_req_counter: AtomicU64,
    temp_root: PathBuf,
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
        let temp_root = std::env::temp_dir().join(format!("pigeon-mock-{}-{n}", std::process::id()));
        let session_dir = temp_root.join("session");
        let data_dir = temp_root.join("data");
        fs::create_dir_all(&session_dir).map_err(LaunchError::Spawn)?;
        fs::create_dir_all(&data_dir).map_err(LaunchError::Spawn)?;

        if options.run_setup {
            let out = Command::new("sh")
                .arg(dir.join("run"))
                .arg("--setup")
                .current_dir(dir)
                .env("PIGEON_PLUGIN_DATA", &data_dir)
                .env("PIGEON_SESSION_DIR", &session_dir)
                .output()
                .map_err(LaunchError::Spawn)?;
            if !out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
                return Err(LaunchError::Setup(text.trim().chars().take(400).collect()));
            }
        }

        let mut child = Command::new("sh")
            .arg(dir.join("run"))
            .arg("--plugin")
            .current_dir(dir)
            .env("PIGEON_PLUGIN_DATA", &data_dir)
            .env("PIGEON_SESSION_DIR", &session_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(LaunchError::Spawn)?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            schemas: Arc::clone(&schemas),
            services: Mutex::new(Services::new(options.fixtures.clone(), manifest.permissions.clone())),
            permissions: manifest.permissions.clone(),
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
            readers,
            host_req_counter: AtomicU64::new(1),
            temp_root,
        };
        let hello = Incoming::Hello(Hello {
            host: HostInfo { name: "Pigeon".into(), version: "mock".into() },
            protocol: ProtocolRange { min: 2, max: 2 },
            theme: options.theme.clone(),
            locale: options.locale.clone(),
            scale: options.scale,
            data_dir: Some(session.data_dir.to_string_lossy().into_owned()),
            session_dir: session_dir.to_string_lossy().into_owned(),
            features: vec!["scene.patch".into(), "canvas".into(), "resources.file".into(), "list.window".into(), "source".into(), "search".into()],
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
            if now >= deadline {
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
        stdin.write_all(line.as_bytes()).and_then(|_| stdin.write_all(b"\n")).and_then(|_| stdin.flush()).map_err(|e| e.to_string())
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
        let _ = child.kill();
        let status = child.wait().ok();
        drop(child);
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
    /// does not exit in time. Returns true when it exited on its own.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let _ = self.send(&Incoming::Shutdown);
        lock(&self.stdin).take();
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = lock(&self.child).try_wait() {
                self.mark_exited(Some(status));
                return true;
            }
            if Instant::now() >= deadline {
                self.crash();
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = lock(&self.child).kill();
            let _ = lock(&self.child).wait();
        }
        lock(&self.stdin).take();
        for h in self.readers.drain(..) {
            let _ = h.join();
        }
        let _ = fs::remove_dir_all(&self.temp_root);
    }
}

fn signal(pid: u32, sig: &str) -> Result<(), String> {
    let status = Command::new("kill").arg(format!("-{sig}")).arg(pid.to_string()).status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -{sig} {pid} failed"))
    }
}

/// Find a node by id anywhere in a scene value.
pub fn find_node<'a>(scene: &'a Value, id: &str) -> Option<&'a Value> {
    if scene["id"] == id {
        return Some(scene);
    }
    if let Some(children) = scene["children"].as_array() {
        for c in children {
            if let Some(found) = find_node(c, id) {
                return Some(found);
            }
        }
    }
    None
}

fn read_stdout(shared: &Arc<Shared>, stdout: impl Read) {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    loop {
        let mut buf = Vec::new();
        let read = match reader.read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            record_violation(shared, Violation { path: String::new(), message: format!("line of {} bytes exceeds the 1 MiB limit", line.len()) }, &line);
            continue;
        }
        handle_line(shared, &line);
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
        let mut st = lock(&shared.state);
        st.unknown_types.push(t.clone());
        st.outgoing.push(value);
        shared.cond.notify_all();
        return;
    }
    let problems = shared.schemas.check_to_host(&value);
    if let Some(p) = problems.into_iter().next() {
        record_violation(shared, p, line);
        // Keep going: the message is still recorded below where possible.
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
            let count = count_nodes(&scene);
            if count > 50_000 {
                let msg = format!("scene has {count} nodes (limit 50,000)");
                st.violations.push((Violation { path: "/scene".into(), message: msg }, instance.clone()));
            }
            st.scenes.insert(instance, scene);
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
            let draw = json!({"frame": v["frame"], "ops": v["ops"], "region": v.get("region").cloned().unwrap_or(Value::Null)});
            let mut st = lock(&shared.state);
            st.canvases.insert(key, draw);
            st.canvas_updates += 1;
            shared.cond.notify_all();
        }
        "list.rows" => {
            let key = (v["instance"].as_str().unwrap_or("").to_string(), v["node"].as_str().unwrap_or("").to_string());
            push(shared, |st| {
                st.list_rows.insert(key, v["rows"].as_array().cloned().unwrap_or_default());
            });
        }
        "resource.put" => {
            let rid = v["rid"].as_str().unwrap_or("").to_string();
            let seq = v["seq"].as_u64().unwrap_or(0);
            {
                let mut st = lock(&shared.state);
                st.resources.insert(rid.clone(), v);
                st.resource_puts += 1;
                shared.cond.notify_all();
            }
            // The host copies the file and acknowledges it (section 6); the mock acks at once.
            let ack = json!({"v": 2, "t": "resource.ack", "rid": rid, "seq": seq});
            if let Err(e) = write_to_plugin(&shared.stdin, &ack) {
                record_violation(shared, Violation { path: "/mock".into(), message: format!("cannot ack {rid}: {e}") }, "");
            }
        }
        "resource.drop" => {
            let rid = v["rid"].as_str().unwrap_or("").to_string();
            push(shared, |st| {
                st.resources.remove(&rid);
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

fn count_nodes(scene: &Value) -> usize {
    1 + scene["children"].as_array().map(|c| c.iter().map(count_nodes).sum::<usize>()).unwrap_or(0)
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
    pipe.write_all(line.as_bytes()).and_then(|_| pipe.flush()).map_err(|e| e.to_string())
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
