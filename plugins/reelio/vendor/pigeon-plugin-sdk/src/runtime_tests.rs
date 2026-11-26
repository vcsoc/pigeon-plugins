//! Runtime behaviour over in-memory pipes: the handshake, the protocol-thread guarantees of
//! section 9, coalescing, service calls and the lifecycle.

use std::io::{self, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::protocol::{Hello, HostInfo, Mods, PointerPhase, ProtocolRange, Theme};
use crate::scene::Node;

/// A `Read` fed from a channel: the test writes host lines into it.
struct ChanReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A `Write` into a shared buffer the test can read.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Harness {
    to_plugin: Sender<Vec<u8>>,
    out: Captured,
}

impl Harness {
    fn send(&self, msg: &Incoming) {
        let mut line = msg.to_value().to_string();
        line.push('\n');
        self.to_plugin.send(line.into_bytes()).unwrap();
    }

    fn send_raw(&self, line: &str) {
        self.to_plugin.send(format!("{line}\n").into_bytes()).unwrap();
    }

    /// Every JSON object the plugin has written so far.
    fn lines(&self) -> Vec<Value> {
        let text = String::from_utf8(self.out.0.lock().unwrap().clone()).unwrap();
        text.lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn wait_for<F: Fn(&Value) -> bool>(&self, what: &str, pred: F) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = self.lines().into_iter().find(|v| pred(v)) {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; output so far: {:?}", self.lines());
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn hello(session_dir: &str) -> Incoming {
    Incoming::Hello(Hello {
        host: HostInfo { name: "Pigeon".into(), version: "0.4.1".into() },
        protocol: ProtocolRange { min: 2, max: 2 },
        theme: Theme { mode: "dark".into(), scale: 2.0, colors: [("accent".to_string(), "#3b82f6".to_string())].into_iter().collect(), ..Theme::default() },
        locale: "en".into(),
        scale: 2.0,
        data_dir: None,
        session_dir: session_dir.into(),
        features: vec!["canvas".into(), "scene.patch".into()],
    })
}

fn spec() -> PluginSpec {
    PluginSpec::new("test-plugin", "1.2.3").contribution(crate::Contribution::workspace_view("main", "Test", "sparkle"))
}

/// Start a plugin over pipes and return the harness plus the plugin (not yet running).
fn start(session_dir: &str) -> (Harness, Plugin) {
    let (tx, rx) = mpsc::channel();
    let out = Captured::default();
    let input = BufReader::new(ChanReader { rx, buf: Vec::new(), pos: 0 });
    let harness = Harness { to_plugin: tx, out: out.clone() };
    harness.send(&hello(session_dir));
    let plugin = Plugin::start(input, out, spec()).expect("start");
    (harness, plugin)
}

fn temp_session(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("pigeon-sdk-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().into_owned()
}

#[test]
fn handshake_sends_ready_with_the_spec_and_the_host_state() {
    let (h, plugin) = start(&temp_session("handshake"));
    assert_eq!(plugin.hello().host.version, "0.4.1");
    let ready = h.wait_for("ready", |v| v["t"] == "ready");
    assert_eq!(ready["v"], 2);
    assert_eq!(ready["plugin"], json!({"id":"test-plugin","version":"1.2.3"}));
    assert_eq!(ready["contributions"][0]["slot"], "workspace.view");
    let ctx = plugin.ctx();
    assert_eq!(ctx.theme().color("accent"), Some("#3b82f6"));
    assert!(ctx.has_feature("canvas"));
    assert!(!ctx.has_feature("source"));
}

#[test]
fn rejects_a_host_that_does_not_speak_protocol_two() {
    let (tx, rx) = mpsc::channel();
    let out = Captured::default();
    let input = BufReader::new(ChanReader { rx, buf: Vec::new(), pos: 0 });
    let bad = Incoming::Hello(Hello {
        protocol: ProtocolRange { min: 3, max: 4 },
        ..match hello("/tmp") {
            Incoming::Hello(h) => h,
            _ => unreachable!(),
        }
    });
    tx.send(format!("{}\n", bad.to_value()).into_bytes()).unwrap();
    let err = Plugin::start(input, out, spec()).err().expect("must refuse");
    assert!(err.to_string().contains("protocol 3-4"), "{err}");
}

#[test]
fn ping_is_answered_while_the_app_handler_is_blocked() {
    // Section 9: the protocol thread answers ping even when the app (or a worker) is stuck.
    let (h, plugin) = start(&temp_session("ping"));
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let ctx = plugin.ctx();
    let runner = thread::spawn(move || {
        plugin.run(move |_ctx, msg| {
            if let Incoming::Focus { .. } = msg {
                gate_rx.recv().ok();
            }
        });
    });
    h.send(&Incoming::Focus { has: true });
    thread::sleep(Duration::from_millis(50));
    h.send(&Incoming::Ping { id: Some("p-9".into()) });
    let pong = h.wait_for("pong while stalled", |v| v["t"] == "pong");
    assert_eq!(pong["id"], "p-9");
    gate_tx.send(()).unwrap();
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
    drop(ctx);
}

#[test]
fn pointer_moves_and_wheel_ticks_are_coalesced_but_presses_are_not() {
    let (h, plugin) = start(&temp_session("coalesce"));
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let seen: Arc<Mutex<Vec<Incoming>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let runner = thread::spawn(move || {
        plugin.run(move |_ctx, msg| {
            if let Incoming::Focus { .. } = msg {
                // Hold the app inside the handler so every event below queues up as one batch.
                started_tx.send(()).ok();
                gate_rx.recv().ok();
                return;
            }
            seen2.lock().unwrap().push(msg);
        });
    });
    h.send(&Incoming::Focus { has: true });
    started_rx.recv_timeout(Duration::from_secs(5)).expect("app entered the handler");
    let pointer = |phase: PointerPhase, x: f32, buttons: u32| Incoming::UiEvent {
        instance: "t/main".into(),
        node: "tl".into(),
        event: NodeEvent::Pointer { phase, x, y: 1.0, button: 0, buttons, mods: Mods::default(), clicks: 0, pointer_id: 1, coalesced: 1 },
    };
    h.send(&pointer(PointerPhase::Down, 0.0, 1));
    for i in 1..=50 {
        h.send(&pointer(PointerPhase::Move, i as f32, 1));
    }
    for _ in 0..10 {
        h.send(&Incoming::UiEvent {
            instance: "t/main".into(),
            node: "tl".into(),
            event: NodeEvent::Wheel { dx: 0.0, dy: -1.0, x: 7.0, y: 8.0, mods: Mods::default(), coalesced: 1 },
        });
    }
    h.send(&pointer(PointerPhase::Up, 50.0, 0));
    // The reader handles lines in order, so the pong proves every event above is queued.
    h.send(&Incoming::Ping { id: Some("sync".into()) });
    h.wait_for("sync pong", |v| v["t"] == "pong" && v["id"] == "sync");
    gate_tx.send(()).unwrap();
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
    let seen = seen.lock().unwrap();
    // down, one merged move (50 merged), one merged wheel (10 merged), up, shutdown.
    let mut moves = Vec::new();
    let mut wheels = Vec::new();
    let mut presses = Vec::new();
    for m in seen.iter() {
        match m {
            Incoming::UiEvent { event: NodeEvent::Pointer { phase: PointerPhase::Move, x, coalesced, .. }, .. } => moves.push((*x, *coalesced)),
            Incoming::UiEvent { event: NodeEvent::Wheel { dy, coalesced, x, .. }, .. } => wheels.push((*dy, *coalesced, *x)),
            Incoming::UiEvent { event: NodeEvent::Pointer { phase, .. }, .. } => presses.push(*phase),
            _ => {}
        }
    }
    assert_eq!(moves, vec![(50.0, 50)], "moves merged, latest position kept, count kept");
    assert_eq!(wheels, vec![(-10.0, 10, 7.0)], "wheel ticks summed");
    assert_eq!(presses, vec![PointerPhase::Down, PointerPhase::Up], "presses never merged or dropped");
}

#[test]
fn blocking_call_receives_the_host_reply_by_id() {
    let (h, plugin) = start(&temp_session("call"));
    let ctx = plugin.ctx();
    let worker = {
        let ctx = ctx.clone();
        thread::spawn(move || ctx.call("library.query", json!({"limit": 2})))
    };
    let req = h.wait_for("req", |v| v["t"] == "req");
    assert_eq!(req["method"], "library.query");
    assert_eq!(req["params"]["limit"], 2);
    h.send(&Incoming::Response { id: req["id"].as_str().unwrap().into(), result: Some(json!({"items": [], "total": 0})), error: None });
    let reply = worker.join().unwrap().expect("reply");
    assert_eq!(reply["total"], 0);
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn host_error_replies_carry_the_code() {
    let (h, plugin) = start(&temp_session("err"));
    let ctx = plugin.ctx();
    let worker = {
        let ctx = ctx.clone();
        thread::spawn(move || ctx.call("secrets.get", json!({"key": "k"})))
    };
    let req = h.wait_for("req", |v| v["t"] == "req");
    h.send_raw(&json!({"v":2,"t":"res","id":req["id"],"error":{"code":"permission_denied","message":"plugin lacks secrets"}}).to_string());
    match worker.join().unwrap() {
        Err(ServiceError::Host { code, .. }) => assert_eq!(code, "permission_denied"),
        other => panic!("expected host error, got {other:?}"),
    }
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn calls_time_out_and_late_replies_are_ignored() {
    let (h, plugin) = start(&temp_session("timeout"));
    let ctx = plugin.ctx();
    let r = ctx.call_timeout("progress.start", json!({"title": "x"}), Duration::from_millis(60));
    assert_eq!(r, Err(ServiceError::Timeout));
    let req = h.wait_for("req", |v| v["t"] == "req");
    // The late answer must not break anything.
    h.send(&Incoming::Response { id: req["id"].as_str().unwrap().into(), result: Some(json!({})), error: None });
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn async_callbacks_run_on_the_app_thread() {
    let (h, plugin) = start(&temp_session("async"));
    let ctx = plugin.ctx();
    let result: Arc<Mutex<Option<(Value, thread::ThreadId)>>> = Arc::new(Mutex::new(None));
    let result2 = Arc::clone(&result);
    ctx.call_async("selection.get", json!({}), move |_ctx, reply| {
        *result2.lock().unwrap() = Some((reply.unwrap(), thread::current().id()));
    });
    let req = h.wait_for("req", |v| v["t"] == "req");
    h.send(&Incoming::Response { id: req["id"].as_str().unwrap().into(), result: Some(json!({"ids": ["a"]})), error: None });
    let app_thread = thread::spawn(move || {
        let id = thread::current().id();
        plugin.run(|_, _| {});
        id
    });
    h.send(&Incoming::Shutdown);
    let id = app_thread.join().unwrap();
    let got = result.lock().unwrap().clone().expect("callback ran");
    assert_eq!(got.0["ids"][0], "a");
    assert_eq!(got.1, id, "callback must run on the app loop thread");
}

#[test]
fn provider_requests_reach_the_app_and_can_be_answered() {
    let (h, plugin) = start(&temp_session("provider"));
    let runner = thread::spawn(move || {
        plugin.run(|ctx, msg| {
            if let Incoming::Request { id, method, params } = msg {
                assert_eq!(method, "source.describe");
                let _ = ctx.answer(&id, json!({"title": "Cloud", "params": params}));
            }
        });
    });
    h.send(&Incoming::Request { id: "host-1".into(), method: "source.describe".into(), params: json!({"source":"cloud"}) });
    let res = h.wait_for("res", |v| v["t"] == "res" && v["id"] == "host-1");
    assert_eq!(res["result"]["title"], "Cloud");
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn scenes_are_validated_before_anything_is_sent() {
    let (h, plugin) = start(&temp_session("validate"));
    let ctx = plugin.ctx();
    let dup = Node::column("root").child(Node::button("a", "A")).child(Node::button("a", "B"));
    let err = ctx.set_scene("test/main", dup).unwrap_err();
    assert!(err.to_string().contains("duplicate node id a"), "{err}");
    ctx.set_scene("test/main", Node::column("root").child(Node::button("ok", "OK"))).unwrap();
    let ui_set = h.wait_for("ui.set", |v| v["t"] == "ui.set");
    assert_eq!(ui_set["scene"]["children"][0]["id"], "ok");
    let count = h.lines().iter().filter(|v| v["t"] == "ui.set").count();
    assert_eq!(count, 1, "the invalid scene must not reach the host");
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn shutdown_and_end_of_input_both_end_the_loop() {
    let (h, plugin) = start(&temp_session("shutdown"));
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();

    let (h2, plugin2) = start(&temp_session("eof"));
    let runner2 = thread::spawn(move || {
        let mut got = Vec::new();
        plugin2.run(|_, m| got.push(format!("{m:?}")));
        got
    });
    drop(h2);
    let got = runner2.join().unwrap();
    assert!(got.last().unwrap().starts_with("Shutdown"), "{got:?}");
}

#[test]
fn theme_changes_update_the_context_and_reach_the_app() {
    let (h, plugin) = start(&temp_session("theme"));
    let ctx = plugin.ctx();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let runner = thread::spawn(move || {
        plugin.run(move |_, m| {
            if let Incoming::ThemeChanged(t) = m {
                seen2.lock().unwrap().push(t.mode);
            }
        })
    });
    h.send(&Incoming::ThemeChanged(Theme { mode: "light".into(), ..Theme::default() }));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
    assert_eq!(ctx.theme().mode, "light");
    assert_eq!(*seen.lock().unwrap(), vec!["light".to_string()]);
}

#[test]
fn an_oversized_host_line_is_skipped_and_the_stream_continues() {
    let (h, plugin) = start(&temp_session("oversize"));
    let runner = thread::spawn(move || {
        let mut kinds = Vec::new();
        plugin.run(|_, m| kinds.push(format!("{m:?}").chars().take(12).collect::<String>()));
        kinds
    });
    let huge = format!(r#"{{"v":2,"t":"ping","pad":"{}"}}"#, "x".repeat(1024 * 1024 + 10));
    h.send_raw(&huge);
    h.send(&Incoming::Ping { id: Some("after".into()) });
    h.wait_for("pong after oversized line", |v| v["t"] == "pong" && v["id"] == "after");
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn resource_ring_waits_for_acks_and_reports_busy() {
    let session = temp_session("ring");
    let (h, plugin) = start(&session);
    let ctx = plugin.ctx();
    let res = ctx.resources().with_ack_wait(Duration::from_millis(80));
    let pixels = vec![7u8; 4 * 4 * 4];
    for expected in 1..=3u64 {
        assert_eq!(res.put_rgba("thumb:1", 4, 4, &pixels, false).unwrap(), expected);
    }
    // Seq 4 reuses slot 1, which the host has not acknowledged yet.
    let busy = res.put_rgba("thumb:1", 4, 4, &pixels, false);
    assert!(matches!(busy, Err(ResourceError::Busy { .. })), "{busy:?}");
    h.send(&Incoming::ResourceAck { rid: "thumb:1".into(), seq: 1 });
    assert_eq!(res.put_rgba("thumb:1", 4, 4, &pixels, false).unwrap(), 4, "a failed put must not consume a sequence number");
    let put = h.wait_for("resource.put seq 4", |v| v["t"] == "resource.put" && v["seq"] == 4);
    assert_eq!(put["rid"], "thumb:1");
    assert_eq!(put["kind"], "rgba");
    assert_eq!(put["stride"], 16);
    let file = put["file"].as_str().unwrap();
    assert!(file.starts_with(&session), "{file}");
    assert_eq!(std::fs::read(file).unwrap(), pixels);
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

#[test]
fn rgba_size_mismatch_is_refused_locally() {
    let (_h, plugin) = start(&temp_session("size"));
    let ctx = plugin.ctx();
    let err = ctx.resources().put_rgba("x", 2, 2, &[0u8; 5], false).unwrap_err();
    assert!(matches!(err, ResourceError::BadSize { expected: 16, actual: 5 }));
}

#[test]
fn rids_with_path_characters_are_sanitized_for_file_names() {
    let session = temp_session("sanitize");
    let (h, plugin) = start(&session);
    let ctx = plugin.ctx();
    ctx.resources().put_encoded("icon:../logo", ResourceKind::Png, b"\x89PNG", 1, 1).unwrap();
    let put = h.wait_for("resource.put", |v| v["t"] == "resource.put");
    let file = put["file"].as_str().unwrap();
    assert!(file.ends_with("icon____logo.1.png") || file.contains("icon"), "{file}");
    assert!(!file.contains(".."), "{file}");
    let runner = thread::spawn(move || plugin.run(|_, _| {}));
    h.send(&Incoming::Shutdown);
    runner.join().unwrap();
}

use crate::protocol::ResourceKind;
use crate::protocol::NodeEvent;
use crate::resources::ResourceError;
