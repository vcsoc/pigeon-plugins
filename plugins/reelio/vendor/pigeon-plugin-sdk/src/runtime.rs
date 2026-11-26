//! Plugin lifecycle, protocol threads, the event loop and the [`Ctx`] handle.
//!
//! Threads (section 9 of the protocol):
//! * **reader** (`pigeon-reader`): reads stdin, answers `ping` itself, resolves `res` replies,
//!   records `resource.ack` and theme/locale changes, and queues everything else for the app. It
//!   never waits on the app or on any worker, so `pong` does not depend on plugin worker threads.
//! * **writer** (`pigeon-writer`): drains the outbox, control lines (pong, responses) first.
//! * **app** (the thread that calls [`Plugin::run`]): receives batches of events, coalesces
//!   pointer moves and wheel ticks, and calls the handler.
//! * **workers**: any thread you spawn with [`Ctx::spawn`]; they call the `Ctx` methods freely.
//!
//! ```no_run
//! use pigeon_plugin_sdk::{runtime::{Plugin, PluginSpec}, scene::Node, Contribution, Incoming, NodeEvent};
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let spec = PluginSpec::new("hello-ui", "1.0.0")
//!         .contribution(Contribution::workspace_view("main", "Hello", "sparkle"));
//!     let plugin = Plugin::stdio(spec)?;
//!     let ctx = plugin.ctx();
//!     let mut clicks = 0;
//!     plugin.run(move |ctx, msg| match msg {
//!         Incoming::Visibility { instance, visible: true } => {
//!             let _ = ctx.set_scene(instance, Node::column("root").child(Node::button("go", "Click")));
//!         }
//!         Incoming::UiEvent { instance, event: NodeEvent::Click, .. } => {
//!             clicks += 1;
//!             let _ = ctx.set_scene(instance, Node::text("root", &format!("Clicked {clicks}")));
//!         }
//!         _ => {}
//!     });
//!     drop(ctx);
//!     Ok(())
//! }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use crate::canvas::DrawList;
use crate::contribution::Contribution;
use crate::protocol::{
    ErrorBody, Hello, Incoming, NodeEvent, Outgoing, PatchOp, PluginId, PointerPhase, Theme,
    HOST_QUEUE_CAPACITY, MAX_LINE_BYTES, PROTOCOL_VERSION,
};
use crate::scene::Node;
use crate::transport::{lock, read_line_bounded, write_loop, LineError, Outbox};

/// Default timeout for host service calls (section 10).
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Soft cap on queued app events; pointer, wheel and hover events beyond it are dropped and counted.
pub const MAX_QUEUED_EVENTS: usize = 4096;
/// Maximum events the app loop takes in one batch.
const BATCH_MAX: usize = 512;

/// Identity and static contributions of the plugin, sent in `ready`.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginSpec {
    pub id: String,
    pub version: String,
    pub contributions: Vec<Contribution>,
}

impl PluginSpec {
    pub fn new(id: &str, version: &str) -> PluginSpec {
        PluginSpec { id: id.into(), version: version.into(), contributions: Vec::new() }
    }

    /// Add one contribution.
    pub fn contribution(mut self, c: Contribution) -> PluginSpec {
        self.contributions.push(c);
        self
    }
}

/// Why the plugin could not start.
#[derive(Debug)]
pub enum StartError {
    Io(io::Error),
    /// The host sent something other than a usable `hello`.
    Protocol(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Io(e) => write!(f, "I/O error during startup: {e}"),
            StartError::Protocol(m) => write!(f, "startup failed: {m}"),
        }
    }
}

impl std::error::Error for StartError {}

impl From<io::Error> for StartError {
    fn from(e: io::Error) -> StartError {
        StartError::Io(e)
    }
}

/// Failure of a host service call.
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceError {
    /// The host answered with an error (for example `permission_denied`).
    Host { code: String, message: String },
    /// No answer within the timeout. The request is forgotten; a late answer is ignored.
    Timeout,
    /// The connection closed before an answer arrived.
    Closed,
    /// The request could not be sent (oversized or malformed).
    Protocol(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Host { code, message } => write!(f, "{code}: {message}"),
            ServiceError::Timeout => write!(f, "host did not answer in time"),
            ServiceError::Closed => write!(f, "connection to the host closed"),
            ServiceError::Protocol(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// The answer to a host service call.
pub type Reply = Result<Value, ServiceError>;

type Callback = Box<dyn FnOnce(&Ctx, Reply) + Send>;

enum Pending {
    Waiter(SyncSender<Reply>),
    Callback(Callback),
}

enum Item {
    Event(Incoming),
    Reply(Callback, Reply),
}

/// State the reader thread keeps current for the app (theme and locale change over time).
#[derive(Debug, Clone, Default)]
pub struct HostState {
    pub host_name: String,
    pub host_version: String,
    pub theme: Theme,
    pub locale: String,
    pub scale: f32,
    pub data_dir: Option<PathBuf>,
    pub session_dir: PathBuf,
    pub features: Vec<String>,
}

pub(crate) struct Shared {
    pub(crate) plugin: PluginId,
    pub(crate) outbox: Outbox,
    pending: Mutex<HashMap<String, Pending>>,
    next_id: AtomicU64,
    inbound: Sender<Item>,
    queued: AtomicUsize,
    dropped: AtomicU64,
    pub(crate) state: Mutex<HostState>,
    pub(crate) acks: Mutex<HashMap<String, u64>>,
    pub(crate) acks_cv: Condvar,
    /// Last published sequence number per resource id.
    pub(crate) rings: Mutex<HashMap<String, u64>>,
    /// Serializes resource publication (see `resources.rs`).
    pub(crate) publish_lock: Mutex<()>,
    closed: AtomicBool,
}

impl Shared {
    pub(crate) fn session_dir(&self) -> PathBuf {
        lock(&self.state).session_dir.clone()
    }
}

/// A cloneable handle to the connection. Send it to worker threads freely.
#[derive(Clone)]
pub struct Ctx {
    pub(crate) shared: Arc<Shared>,
}

/// The running plugin. Create it with [`Plugin::stdio`] or [`Plugin::start`], then call [`Plugin::run`].
pub struct Plugin {
    shared: Arc<Shared>,
    inbound: Receiver<Item>,
    hello: Hello,
    _reader: JoinHandle<()>,
    _writer: JoinHandle<()>,
}

impl Plugin {
    /// Start over the process's stdin and stdout (the normal case).
    pub fn stdio(spec: PluginSpec) -> Result<Plugin, StartError> {
        Plugin::start(BufReader::new(io::stdin()), io::stdout(), spec)
    }

    /// Start over any reader and writer (tests, or a custom transport).
    ///
    /// Reads the host's `hello` (blocking), checks the protocol range, sends `ready` and starts
    /// the reader and writer threads.
    pub fn start<R, W>(mut input: R, output: W, spec: PluginSpec) -> Result<Plugin, StartError>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
    {
        let first = read_line_bounded(&mut input, MAX_LINE_BYTES).map_err(|e| match e {
            LineError::Eof => StartError::Protocol("host closed stdin before hello".into()),
            LineError::TooLong(n) => StartError::Protocol(format!("hello is too large ({n} bytes)")),
            LineError::Io(e) => StartError::Io(e),
        })?;
        let hello = match Incoming::parse(&first).map_err(|e| StartError::Protocol(e.to_string()))? {
            Incoming::Hello(h) => h,
            other => return Err(StartError::Protocol(format!("expected hello, got {other:?}"))),
        };
        if hello.protocol.min > PROTOCOL_VERSION || hello.protocol.max < PROTOCOL_VERSION {
            return Err(StartError::Protocol(format!(
                "host speaks protocol {}-{}, this plugin speaks {PROTOCOL_VERSION}",
                hello.protocol.min, hello.protocol.max
            )));
        }
        let (tx, rx) = mpsc::channel();
        let state = HostState {
            host_name: hello.host.name.clone(),
            host_version: hello.host.version.clone(),
            theme: hello.theme.clone(),
            locale: hello.locale.clone(),
            scale: hello.scale,
            data_dir: hello.data_dir.as_ref().map(PathBuf::from),
            session_dir: PathBuf::from(&hello.session_dir),
            features: hello.features.clone(),
        };
        let shared = Arc::new(Shared {
            plugin: PluginId { id: spec.id.clone(), version: spec.version.clone() },
            outbox: Outbox::new(HOST_QUEUE_CAPACITY),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            inbound: tx,
            queued: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            state: Mutex::new(state),
            acks: Mutex::new(HashMap::new()),
            acks_cv: Condvar::new(),
            rings: Mutex::new(HashMap::new()),
            publish_lock: Mutex::new(()),
            closed: AtomicBool::new(false),
        });
        let w_shared = Arc::clone(&shared);
        let writer = thread::Builder::new().name("pigeon-writer".into()).spawn(move || {
            write_loop(&w_shared.outbox, output);
        })?;
        let ready = Outgoing::Ready { plugin: shared.plugin.clone(), protocol: PROTOCOL_VERSION, contributions: spec.contributions };
        let line = ready.to_line().map_err(|e| StartError::Protocol(e.to_string()))?;
        shared.outbox.send(line);
        let r_shared = Arc::clone(&shared);
        let reader = thread::Builder::new().name("pigeon-reader".into()).spawn(move || {
            reader_loop(&r_shared, input);
        })?;
        Ok(Plugin { shared, inbound: rx, hello, _reader: reader, _writer: writer })
    }

    /// The host's `hello` as received at startup.
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// A handle for worker threads and for sending replies.
    pub fn ctx(&self) -> Ctx {
        Ctx { shared: Arc::clone(&self.shared) }
    }

    /// Run the app loop until `shutdown` or end of input. The handler is called on this thread,
    /// one message at a time, after pointer and wheel coalescing. Service replies are routed to
    /// their callbacks on this thread too.
    pub fn run<F>(self, mut handler: F)
    where
        F: FnMut(&Ctx, Incoming),
    {
        let ctx = self.ctx();
        'outer: while let Ok(first) = self.inbound.recv() {
            let mut batch = vec![first];
            while batch.len() < BATCH_MAX {
                match self.inbound.try_recv() {
                    Ok(item) => batch.push(item),
                    Err(_) => break,
                }
            }
            let events = batch.iter().filter(|i| matches!(i, Item::Event(_))).count();
            self.shared.queued.fetch_sub(events, Ordering::SeqCst);
            for item in coalesce(batch) {
                match item {
                    Item::Event(Incoming::Shutdown) => {
                        handler(&ctx, Incoming::Shutdown);
                        break 'outer;
                    }
                    Item::Event(msg) => handler(&ctx, msg),
                    Item::Reply(cb, reply) => cb(&ctx, reply),
                }
            }
        }
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.outbox.close();
    }
}

/// Merge adjacent pointer moves and adjacent wheel ticks for the same node. Other events are never
/// merged or reordered; `down`, `up` and `key` always pass through.
fn coalesce(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for item in items {
        if let Some(last) = out.last_mut() {
            if merge_into(last, &item) {
                continue;
            }
        }
        out.push(item);
    }
    out
}

fn merge_into(last: &mut Item, next: &Item) -> bool {
    let (Item::Event(Incoming::UiEvent { instance: li, node: ln, event: le }),
         Item::Event(Incoming::UiEvent { instance: ni, node: nn, event: ne })) = (last, next)
    else {
        return false;
    };
    if li != ni || ln != nn {
        return false;
    }
    match (le, ne) {
        (
            NodeEvent::Pointer { phase: PointerPhase::Move, x, y, buttons, coalesced, .. },
            NodeEvent::Pointer { phase: PointerPhase::Move, x: nx, y: ny, buttons: nb, coalesced: nc, .. },
        ) => {
            *x = *nx;
            *y = *ny;
            *buttons = *nb;
            *coalesced += *nc;
            true
        }
        (
            NodeEvent::Wheel { dx, dy, x, y, mods, coalesced },
            NodeEvent::Wheel { dx: ndx, dy: ndy, x: nx, y: ny, mods: nm, coalesced: nc },
        ) => {
            *dx += *ndx;
            *dy += *ndy;
            *x = *nx;
            *y = *ny;
            *mods = *nm;
            *coalesced += *nc;
            true
        }
        _ => false,
    }
}

fn reader_loop<R: BufRead>(shared: &Arc<Shared>, mut input: R) {
    loop {
        match read_line_bounded(&mut input, MAX_LINE_BYTES) {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                match Incoming::parse(&line) {
                    Ok(msg) => dispatch(shared, msg),
                    Err(e) => eprintln!("pigeon-plugin-sdk: ignoring host line: {e}"),
                }
            }
            Err(LineError::TooLong(n)) => eprintln!("pigeon-plugin-sdk: ignoring oversized host line ({n} bytes)"),
            Err(LineError::Eof) | Err(LineError::Io(_)) => break,
        }
    }
    shared.closed.store(true, Ordering::SeqCst);
    shared.outbox.close();
    let waiting: Vec<(String, Pending)> = lock(&shared.pending).drain().collect();
    for (_, p) in waiting {
        match p {
            Pending::Waiter(tx) => {
                let _ = tx.send(Err(ServiceError::Closed));
            }
            Pending::Callback(cb) => {
                let _ = shared.inbound.send(Item::Reply(cb, Err(ServiceError::Closed)));
            }
        }
    }
    enqueue(shared, Incoming::Shutdown, false);
}

fn dispatch(shared: &Arc<Shared>, msg: Incoming) {
    match msg {
        Incoming::Ping { id } => {
            if let Ok(line) = (Outgoing::Pong { id }).to_line() {
                shared.outbox.send_control(line);
            }
        }
        Incoming::Response { id, result, error } => {
            let reply = match error {
                Some(ErrorBody { code, message }) => Err(ServiceError::Host { code, message }),
                None => Ok(result.unwrap_or(Value::Null)),
            };
            let pending = lock(&shared.pending).remove(&id);
            match pending {
                Some(Pending::Waiter(tx)) => {
                    let _ = tx.send(reply);
                }
                Some(Pending::Callback(cb)) => {
                    let _ = shared.inbound.send(Item::Reply(cb, reply));
                }
                None => {}
            }
        }
        Incoming::ResourceAck { rid, seq } => {
            let mut acks = lock(&shared.acks);
            let entry = acks.entry(rid).or_insert(0);
            if seq > *entry {
                *entry = seq;
            }
            shared.acks_cv.notify_all();
        }
        Incoming::ThemeChanged(theme) => {
            lock(&shared.state).theme = theme.clone();
            enqueue(shared, Incoming::ThemeChanged(theme), false);
        }
        Incoming::LocaleChanged { locale } => {
            lock(&shared.state).locale = locale.clone();
            enqueue(shared, Incoming::LocaleChanged { locale }, false);
        }
        // Provider requests always reach the app; an unanswered one times out on the host.
        Incoming::UiEvent { ref event, .. } if is_coalescible(event) => enqueue(shared, msg, true),
        Incoming::Hello(_) => {}
        other => enqueue(shared, other, false),
    }
}

/// Events that may be dropped when the app is far behind: moves, wheel ticks, hover, scroll and
/// list windows. Presses, releases, keys, edits and clicks are never dropped.
fn is_coalescible(event: &NodeEvent) -> bool {
    match event {
        NodeEvent::Pointer { phase, .. } => matches!(phase, PointerPhase::Move | PointerPhase::Enter | PointerPhase::Leave),
        NodeEvent::Wheel { .. } | NodeEvent::Hover { .. } | NodeEvent::ListWindow { .. } | NodeEvent::Scroll { .. } => true,
        NodeEvent::Key { .. } => false,
        _ => false,
    }
}

fn enqueue(shared: &Arc<Shared>, msg: Incoming, droppable: bool) {
    if droppable && shared.queued.load(Ordering::SeqCst) >= MAX_QUEUED_EVENTS {
        shared.dropped.fetch_add(1, Ordering::SeqCst);
        return;
    }
    shared.queued.fetch_add(1, Ordering::SeqCst);
    let _ = shared.inbound.send(Item::Event(msg));
}

impl Ctx {
    /// The plugin's identity.
    pub fn plugin(&self) -> PluginId {
        self.shared.plugin.clone()
    }

    /// Snapshot of host state (theme, locale, scale, paths). Updated by the reader thread.
    pub fn host(&self) -> HostState {
        lock(&self.shared.state).clone()
    }

    /// The current theme tokens.
    pub fn theme(&self) -> Theme {
        lock(&self.shared.state).theme.clone()
    }

    /// Private folder for this session; files written here are referenced by resources.
    pub fn session_dir(&self) -> PathBuf {
        self.shared.session_dir()
    }

    /// Whether the host advertised a feature in `hello` (for example `scene.patch`).
    pub fn has_feature(&self, name: &str) -> bool {
        lock(&self.shared.state).features.iter().any(|f| f == name)
    }

    /// Whether the connection is still open.
    pub fn is_open(&self) -> bool {
        !self.shared.closed.load(Ordering::SeqCst)
    }

    /// Events dropped because the app fell far behind (coalescible events only).
    pub fn dropped_events(&self) -> u64 {
        self.shared.dropped.load(Ordering::SeqCst)
    }

    /// Send any message. Blocks only if the normal outbound queue is full.
    pub fn send(&self, msg: &Outgoing) -> Result<(), ServiceError> {
        let line = msg.to_line().map_err(|e| ServiceError::Protocol(e.to_string()))?;
        if self.shared.outbox.send(line) {
            Ok(())
        } else {
            Err(ServiceError::Closed)
        }
    }

    /// Run `f` on a new worker thread with its own handle. Heavy work belongs here.
    pub fn spawn<F>(&self, f: F) -> JoinHandle<()>
    where
        F: FnOnce(Ctx) + Send + 'static,
    {
        let ctx = self.clone();
        thread::spawn(move || f(ctx))
    }

    // ---- scene and canvas ----

    /// Replace the scene of one slot instance. Validates the scene first (unique ids, limits).
    pub fn set_scene(&self, instance: impl Into<String>, scene: Node) -> Result<(), ServiceError> {
        scene.validate().map_err(|e| ServiceError::Protocol(e.to_string()))?;
        self.send(&Outgoing::UiSet { instance: instance.into(), scene })
    }

    /// Apply patch operations to the current scene (requires feature `scene.patch`).
    pub fn patch(&self, instance: impl Into<String>, ops: Vec<PatchOp>) -> Result<(), ServiceError> {
        self.send(&Outgoing::UiPatch { instance: instance.into(), ops })
    }

    /// Fast path: replace one canvas' draw list without resending the scene.
    pub fn canvas_draw(&self, instance: impl Into<String>, node: impl Into<String>, frame: u64, list: DrawList) -> Result<(), ServiceError> {
        list.validate().map_err(|e| ServiceError::Protocol(e.to_string()))?;
        self.send(&Outgoing::CanvasDraw(crate::protocol::CanvasDraw {
            instance: instance.into(),
            node: node.into(),
            frame,
            ops: list.ops,
            region: None,
        }))
    }

    /// Answer a `list.window` event with the rows `start..start+rows.len()`.
    pub fn list_rows(&self, instance: impl Into<String>, node: impl Into<String>, start: u64, rows: Vec<Node>) -> Result<(), ServiceError> {
        for row in &rows {
            row.validate().map_err(|e| ServiceError::Protocol(e.to_string()))?;
        }
        self.send(&Outgoing::ListRows { instance: instance.into(), node: node.into(), start, rows })
    }

    pub fn focus(&self, instance: impl Into<String>, node: impl Into<String>) -> Result<(), ServiceError> {
        self.send(&Outgoing::UiFocus { instance: instance.into(), node: node.into() })
    }

    pub fn scroll_to(&self, instance: impl Into<String>, node: impl Into<String>, align: &str) -> Result<(), ServiceError> {
        self.send(&Outgoing::UiScrollTo { instance: instance.into(), node: node.into(), align: align.into() })
    }

    pub fn beep(&self) -> Result<(), ServiceError> {
        self.send(&Outgoing::UiBeep {})
    }

    /// Add contributions at run time (dynamic slots).
    pub fn contribute(&self, contributions: Vec<Contribution>) -> Result<(), ServiceError> {
        self.send(&Outgoing::Contribute { contributions })
    }

    /// Remove dynamic contributions by id.
    pub fn withdraw(&self, ids: Vec<String>) -> Result<(), ServiceError> {
        self.send(&Outgoing::Withdraw { ids })
    }

    /// Tell the host a library source changed (`invalidate`, `insert` or `remove`).
    pub fn source_changed(&self, source: &str, kind: &str, node: Option<&str>, ids: Option<Vec<String>>) -> Result<(), ServiceError> {
        self.send(&Outgoing::SourceChanged {
            source: source.into(),
            kind: kind.into(),
            node: node.map(String::from),
            ids,
        })
    }

    /// Progress of a `search.provider` (indexing and similar).
    pub fn search_status(&self, mode_id: &str, status: Value) -> Result<(), ServiceError> {
        self.send(&Outgoing::SearchStatus { mode_id: mode_id.into(), status })
    }

    // ---- replies to host requests ----

    /// Answer a provider request from the host (`req`) with a result.
    pub fn answer(&self, id: &str, result: Value) -> Result<(), ServiceError> {
        self.send(&Outgoing::Response { id: id.into(), result: Some(result), error: None })
    }

    /// Answer a provider request from the host with an error.
    pub fn answer_error(&self, id: &str, code: &str, message: &str) -> Result<(), ServiceError> {
        self.send(&Outgoing::Response {
            id: id.into(),
            result: None,
            error: Some(ErrorBody { code: code.into(), message: message.into() }),
        })
    }

    // ---- host services (section 10) ----

    /// Call a host service and wait up to [`DEFAULT_CALL_TIMEOUT`]. Never call this from the
    /// handler for long periods; use a worker thread or [`Ctx::call_async`].
    pub fn call(&self, method: &str, params: Value) -> Reply {
        self.call_timeout(method, params, DEFAULT_CALL_TIMEOUT)
    }

    /// Call a host service and wait up to `timeout`.
    pub fn call_timeout(&self, method: &str, params: Value, timeout: Duration) -> Reply {
        let (tx, rx) = mpsc::sync_channel(1);
        let id = self.register(Pending::Waiter(tx));
        if let Err(e) = self.send_request(&id, method, params) {
            lock(&self.shared.pending).remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.shared.pending).remove(&id);
                Err(ServiceError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(ServiceError::Closed),
        }
    }

    /// Call a host service and run `callback(ctx, reply)` on the app thread when it answers.
    pub fn call_async<F>(&self, method: &str, params: Value, callback: F)
    where
        F: FnOnce(&Ctx, Reply) + Send + 'static,
    {
        let id = self.register(Pending::Callback(Box::new(callback)));
        if let Err(e) = self.send_request(&id, method, params) {
            if let Some(Pending::Callback(cb)) = lock(&self.shared.pending).remove(&id) {
                let _ = self.shared.inbound.send(Item::Reply(cb, Err(e)));
            }
        }
    }

    fn register(&self, pending: Pending) -> String {
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst).to_string();
        lock(&self.shared.pending).insert(id.clone(), pending);
        id
    }

    fn send_request(&self, id: &str, method: &str, params: Value) -> Result<(), ServiceError> {
        let msg = Outgoing::Request { id: id.into(), method: method.into(), params };
        self.send(&msg)
    }

    /// Fire-and-forget log line for the host's diagnostics.
    pub fn log(&self, level: &str, message: &str) {
        eprintln!("[{}] {message}", level);
        let _ = self.call_async("log", serde_json::json!({ "level": level, "message": message }), |_, _| {});
    }

    /// `settings.get`: the value stored for `key` (plugin-private, persisted by the host).
    pub fn settings_get(&self, key: &str) -> Result<Option<Value>, ServiceError> {
        let r = self.call("settings.get", serde_json::json!({ "key": key }))?;
        Ok(r.get("value").cloned().filter(|v| !v.is_null()))
    }

    /// `settings.set`.
    pub fn settings_set(&self, key: &str, value: Value) -> Result<(), ServiceError> {
        self.call("settings.set", serde_json::json!({ "key": key, "value": value })).map(|_| ())
    }

    /// `notify.toast`.
    pub fn toast(&self, level: &str, text: &str) -> Result<(), ServiceError> {
        self.call("notify.toast", serde_json::json!({ "level": level, "text": text })).map(|_| ())
    }

    /// `progress.start`: returns the job id shown in the footer.
    pub fn progress_start(&self, title: &str, cancellable: bool) -> Result<String, ServiceError> {
        let r = self.call("progress.start", serde_json::json!({ "title": title, "cancellable": cancellable }))?;
        Ok(r.get("job").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// `progress.update`; `value` is 0..=1, or None for indeterminate.
    pub fn progress_update(&self, job: &str, value: Option<f64>, message: Option<&str>) -> Result<(), ServiceError> {
        self.call("progress.update", serde_json::json!({ "job": job, "value": value, "message": message })).map(|_| ())
    }

    /// `progress.end`.
    pub fn progress_end(&self, job: &str, ok: bool, message: Option<&str>) -> Result<(), ServiceError> {
        self.call("progress.end", serde_json::json!({ "job": job, "ok": ok, "message": message })).map(|_| ())
    }

    /// `library.query` with a filter object (see methods.schema.json).
    pub fn library_query(&self, filter: Value, limit: u32) -> Reply {
        self.call("library.query", serde_json::json!({ "filter": filter, "limit": limit }))
    }

    /// `selection.get`.
    pub fn selection(&self) -> Reply {
        self.call("selection.get", serde_json::json!({}))
    }

    /// `shell.open_url` (https, http or mailto only).
    pub fn open_url(&self, url: &str) -> Result<(), ServiceError> {
        self.call("shell.open_url", serde_json::json!({ "url": url })).map(|_| ())
    }

    /// `dialog.open_file`: the chosen paths, empty when cancelled.
    pub fn open_file_dialog(&self, title: &str, multiple: bool) -> Result<Vec<String>, ServiceError> {
        let r = self.call("dialog.open_file", serde_json::json!({ "title": title, "multiple": multiple }))?;
        Ok(r.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default())
    }

    /// `window.open_modal`: open one of this plugin's `modal` contributions.
    pub fn open_modal(&self, contribution: &str, props: Value) -> Reply {
        self.call("window.open_modal", serde_json::json!({ "contribution": contribution, "props": props }))
    }

    /// Resource writer (images, video frames). See [`crate::resources`].
    pub fn resources(&self) -> crate::resources::Resources {
        crate::resources::Resources::new(self.clone())
    }

    /// Current theme color token or None.
    pub fn theme_color(&self, token: &str) -> Option<String> {
        lock(&self.shared.state).theme.color(token).map(String::from)
    }

    /// Directory of the plugin's private data (`PIGEON_PLUGIN_DATA`), if the host set one.
    pub fn data_dir(&self) -> Option<PathBuf> {
        lock(&self.shared.state).data_dir.clone()
    }
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ctx({}@{})", self.shared.plugin.id, self.shared.plugin.version)
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
