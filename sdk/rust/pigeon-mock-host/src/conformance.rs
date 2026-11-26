//! Conformance cases: data-driven behaviour checks for any plugin (protocol v2 section 13).
//!
//! A case is a JSON file in `conformance/cases/` (format: `conformance/README.md`). Each case
//! launches a fresh plugin process, runs its steps in order, and reports pass, fail or skipped
//! (for checks only a real host can make, such as the frame budget). The same case files are the
//! input of `pigeon --conformance` in the Pigeon repository.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pigeon_plugin_sdk::protocol::NodeEvent;
use serde_json::{json, Map, Value};

use crate::render::{self, ImageRef, Theme};
use crate::session::{find_node, LaunchOptions, Session};
use crate::Schemas;

/// One step outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Pass,
    Fail(String),
    /// Not checkable by the mock (it is a host-side property).
    Skipped(String),
}

/// The result of one case.
#[derive(Debug, Clone)]
pub struct CaseResult {
    pub id: String,
    pub title: String,
    pub passed: bool,
    pub skipped: usize,
    pub steps: Vec<(usize, String, Outcome)>,
    pub elapsed: Duration,
}

/// Runner options.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Directory where golden PNGs live (`conformance/goldens`).
    pub goldens: PathBuf,
    /// Default timeout for waits.
    pub timeout: Duration,
}

/// Load every `*.json` case in a directory, sorted by file name.
pub fn load_cases(dir: &Path) -> Result<Vec<(PathBuf, Value)>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        out.push((p, v));
    }
    Ok(out)
}

/// Run one case against the plugin in `plugin_dir`.
pub fn run_case(case: &Value, plugin_dir: &Path, schemas: Arc<Schemas>, opts: &RunOptions) -> CaseResult {
    let started = Instant::now();
    let id = case["id"].as_str().unwrap_or("unnamed").to_string();
    let title = case["title"].as_str().unwrap_or("").to_string();
    let mut results = Vec::new();
    let launch = Session::launch(plugin_dir, schemas, LaunchOptions::default());
    let session = match launch {
        Ok(s) => s,
        Err(e) => {
            results.push((0, "launch".into(), Outcome::Fail(e.to_string())));
            return finish(id, title, results, started);
        }
    };
    let steps = case["steps"].as_array().cloned().unwrap_or_default();
    for (index, step) in steps.iter().enumerate() {
        let name = step_name(step);
        let outcome = match run_step(&session, step, opts) {
            Ok(o) => o,
            Err(e) => Outcome::Fail(e),
        };
        let stop = matches!(outcome, Outcome::Fail(_));
        results.push((index + 1, name, outcome));
        if stop {
            break;
        }
    }
    session.shutdown(Duration::from_secs(5));
    finish(id, title, results, started)
}

fn finish(id: String, title: String, steps: Vec<(usize, String, Outcome)>, started: Instant) -> CaseResult {
    let passed = steps.iter().all(|(_, _, o)| !matches!(o, Outcome::Fail(_)));
    let skipped = steps.iter().filter(|(_, _, o)| matches!(o, Outcome::Skipped(_))).count();
    CaseResult { id, title, passed, skipped, steps, elapsed: started.elapsed() }
}

fn step_name(step: &Value) -> String {
    step.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_else(|| "invalid step".into())
}

fn timeout_of(v: &Value, default: Duration) -> Duration {
    v["timeout_ms"].as_u64().map(Duration::from_millis).unwrap_or(default)
}

/// Execute one step. `Err` is a failure with a message.
fn run_step(s: &Session, step: &Value, opts: &RunOptions) -> Result<Outcome, String> {
    let obj: &Map<String, Value> = step.as_object().ok_or("step must be an object")?;
    let (kind, arg) = obj.iter().next().ok_or("empty step")?;
    let timeout = timeout_of(arg, opts.timeout);
    match kind.as_str() {
        "ready" => {
            let ready = s.wait_ready(Duration::from_secs(10))?;
            let min = arg["min_contributions"].as_u64().unwrap_or(1) as usize;
            let n = ready["contributions"].as_array().map(|a| a.len()).unwrap_or(0);
            if n < min {
                return Err(format!("ready has {n} contributions, expected at least {min}"));
            }
            Ok(Outcome::Pass)
        }
        "slots" => {
            let ready = s.wait_ready(Duration::from_secs(10))?;
            let contributions = ready["contributions"].as_array().cloned().unwrap_or_default();
            for slot in arg.as_array().ok_or("slots takes an array of slot ids")? {
                let slot = slot.as_str().unwrap_or("");
                if !contributions.iter().any(|c| c["slot"] == slot) {
                    return Err(format!("no contribution for slot {slot}"));
                }
            }
            Ok(Outcome::Pass)
        }
        "show" => {
            s.show(arg.as_str().ok_or("show takes an instance")?)?;
            Ok(Outcome::Pass)
        }
        "wait_node" => {
            let instance = str_arg(arg, "instance")?;
            let node = str_arg(arg, "node")?;
            s.wait_for_node(&instance, &node, timeout).map(|_| Outcome::Pass).ok_or_else(|| format!("no node {node} in {instance} within {timeout:?}"))
        }
        "expect_node" => {
            // Waits (up to the timeout) until the node exists and has the expected props.
            let instance = str_arg(arg, "instance")?;
            let node = str_arg(arg, "node")?;
            let expect = arg["props"].clone();
            let deadline = Instant::now() + timeout;
            loop {
                let scene = s.scene(&instance);
                let verdict = match scene.as_ref().and_then(|sc| find_node(sc, &node)) {
                    Some(found) => check_props(found, &expect),
                    None => Err(format!("no node {node} in {instance}")),
                };
                match verdict {
                    Ok(()) => return Ok(Outcome::Pass),
                    Err(e) if Instant::now() >= deadline => return Err(e),
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        "send" => {
            // Any host message, written as protocol JSON (commands, drops, selection changes).
            let msg = pigeon_plugin_sdk::protocol::Incoming::from_value(arg.clone()).map_err(|e| e.to_string())?;
            s.send(&msg)?;
            Ok(Outcome::Pass)
        }
        "event" => {
            let instance = str_arg(arg, "instance")?;
            let node = str_arg(arg, "node")?;
            let event = event_from(arg)?;
            s.send_event(&instance, &node, event)?;
            Ok(Outcome::Pass)
        }
        "ping" => {
            let within = Duration::from_millis(arg["within_ms"].as_u64().unwrap_or(1000));
            match s.ping(within) {
                Some(_) => Ok(Outcome::Pass),
                None => Err(format!("no pong within {within:?}")),
            }
        }
        "raw" => {
            s.send_raw(arg.as_str().ok_or("raw takes a string line")?)?;
            Ok(Outcome::Pass)
        }
        "stall" => {
            // A hung plugin: pause the process, ask for a ping (no pong may arrive during the
            // stall), resume, and require the pong afterwards.
            let ms = arg["ms"].as_u64().unwrap_or(2000);
            s.pause()?;
            let during = s.ping(Duration::from_millis(300));
            std::thread::sleep(Duration::from_millis(ms));
            s.resume()?;
            if during.is_some() {
                return Err("a stopped process answered ping".into());
            }
            match s.ping(Duration::from_secs(5)) {
                Some(_) => Ok(Outcome::Pass),
                None => Err("no pong after resume".into()),
            }
        }
        "crash" => {
            let status = s.crash();
            if status.is_some() {
                Ok(Outcome::Pass)
            } else {
                Err("process did not report an exit status".into())
            }
        }
        "expect_toast" => {
            let needle = arg["text_contains"].as_str().unwrap_or("");
            let deadline = Instant::now() + timeout;
            loop {
                let found = s.services(|sv| sv.toasts.iter().any(|t| t.text.contains(needle)));
                if found {
                    return Ok(Outcome::Pass);
                }
                if Instant::now() >= deadline {
                    return Err(format!("no toast containing {needle:?}"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "expect_progress_end" => {
            let deadline = Instant::now() + timeout;
            loop {
                let done = s.services(|sv| sv.jobs.iter().any(|j| j.ended.is_some()));
                if done {
                    return Ok(Outcome::Pass);
                }
                if Instant::now() >= deadline {
                    return Err("no progress job ended".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "expect_canvas" => {
            let instance = str_arg(arg, "instance")?;
            let node = str_arg(arg, "node")?;
            let min_ops = arg["min_ops"].as_u64().unwrap_or(1) as usize;
            let key = (instance.clone(), node.clone());
            let want = arg.get("contains").cloned();
            let deadline = Instant::now() + timeout;
            loop {
                // Canvases are drawn either inside the scene or through canvas.draw updates.
                let draw = s.with_state(|st| st.canvases.get(&key).cloned()).or_else(|| {
                    s.scene(&instance).as_ref().and_then(|sc| find_node(sc, &node)).and_then(|n| n.get("draw").cloned())
                });
                let verdict = match &draw {
                    None => Err(format!("no canvas {node} in {instance}")),
                    Some(d) => {
                        let ops = d["ops"].as_array().cloned().unwrap_or_default();
                        if ops.len() < min_ops {
                            Err(format!("canvas has {} ops, expected at least {min_ops}", ops.len()))
                        } else {
                            match &want {
                                Some(w) if !ops.iter().any(|op| subset(op, w)) => Err(format!("no draw op matching {w}")),
                                _ => Ok(()),
                            }
                        }
                    }
                };
                match verdict {
                    Ok(()) => return Ok(Outcome::Pass),
                    Err(e) if Instant::now() >= deadline => return Err(e),
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        "expect_list_rows" => {
            let instance = str_arg(arg, "instance")?;
            let node = str_arg(arg, "node")?;
            let min = arg["min"].as_u64().unwrap_or(1) as usize;
            let key = (instance, node);
            let rows = s.wait_until(timeout, |st| st.list_rows.get(&key).cloned().filter(|r| r.len() >= min), "rows");
            rows.map(|_| Outcome::Pass).ok_or_else(|| format!("fewer than {min} list rows were sent"))
        }
        "expect_resource" => {
            let rid = str_arg(arg, "rid")?;
            let kind = arg["kind"].as_str().unwrap_or("");
            let put = s.wait_until(timeout, |st| st.resources.get(&rid).cloned(), "resource").ok_or_else(|| format!("no resource.put for {rid}"))?;
            if !kind.is_empty() && put["kind"] != kind {
                return Err(format!("resource {rid} has kind {}, expected {kind}", put["kind"]));
            }
            Ok(Outcome::Pass)
        }
        "provider" => {
            let method = str_arg(arg, "method")?;
            let params = arg.get("params").cloned().unwrap_or(json!({}));
            let envelope = s.request(&method, params, timeout)?;
            if let Some(err) = envelope.get("error") {
                return Err(format!("{method} failed: {err}"));
            }
            let answer = envelope.get("result").cloned().unwrap_or(Value::Null);
            if let Some(expect) = arg.get("expect") {
                check_props(&answer, expect)?;
            }
            Ok(Outcome::Pass)
        }
        "expect_no_violations" => {
            let v = s.violations();
            if v.is_empty() {
                Ok(Outcome::Pass)
            } else {
                Err(format!("{} schema violation(s), first: {} in {}", v.len(), v[0].0, v[0].1))
            }
        }
        "golden" => {
            let instance = str_arg(arg, "instance")?;
            let file = str_arg(arg, "file")?;
            let size = arg["size"].as_array().and_then(|a| Some((a.first()?.as_u64()? as u32, a.get(1)?.as_u64()? as u32))).unwrap_or((640, 480));
            let max_fraction = arg["max_fraction"].as_f64().unwrap_or(0.0);
            let scene = s
                .wait_until(timeout, |st| st.scenes.get(&instance).cloned(), "scene")
                .ok_or_else(|| format!("no scene for {instance}"))?;
            let images = session_images(s);
            let surface = render::render_scene(&scene, size, &Theme::dark(), &images, 1.0);
            let path = opts.goldens.join(&file);
            let cmp = render::compare_png(&surface, &path)?;
            if cmp.written {
                return Ok(Outcome::Skipped(format!("golden {file} written (first run or UPDATE_GOLDENS)")));
            }
            if !cmp.passes(max_fraction) {
                return Err(format!(
                    "golden {file}: {:.4}% of pixels differ (max channel delta {}), limit {:.4}%",
                    cmp.differing_fraction * 100.0,
                    cmp.max_channel_delta,
                    max_fraction * 100.0
                ));
            }
            Ok(Outcome::Pass)
        }
        "frame_budget" => Ok(Outcome::Skipped(
            "frame budget (>= 20 fps while a plugin is stalled) is a property of the host's render loop; \
             the mock host has no render loop. Run it against pigeon --conformance."
                .into(),
        )),
        other => Err(format!("unknown step {other}")),
    }
}

/// Decoded resource images from the session (only raw RGBA and PNG files the host would read).
fn session_images(s: &Session) -> std::collections::HashMap<String, ImageRef> {
    let puts = s.with_state(|st| st.resources.clone());
    let mut out = std::collections::HashMap::new();
    for (rid, put) in puts {
        let Some(file) = put["file"].as_str() else { continue };
        let Ok(bytes) = std::fs::read(file) else { continue };
        let image = match put["kind"].as_str() {
            Some("rgba") => {
                let (w, h) = (put["w"].as_u64().unwrap_or(0) as u32, put["h"].as_u64().unwrap_or(0) as u32);
                if bytes.len() == (w * h * 4) as usize {
                    Some(ImageRef { w, h, rgba: bytes })
                } else {
                    None
                }
            }
            Some("png") => render::png::decode(&bytes).ok(),
            _ => None,
        };
        if let Some(img) = image {
            out.insert(rid, img);
        }
    }
    out
}

fn str_arg(arg: &Value, key: &str) -> Result<String, String> {
    arg[key].as_str().map(String::from).ok_or_else(|| format!("missing string field {key}"))
}

/// True when every key of `want` is present in `got` with an equal value (numbers compare as f64).
fn subset(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Object(g), Value::Object(w)) => w.iter().all(|(k, wv)| g.get(k).map(|gv| subset(gv, wv)).unwrap_or(false)),
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => got == want,
    }
}

fn check_props(node: &Value, expect: &Value) -> Result<(), String> {
    let Some(map) = expect.as_object() else { return Ok(()) };
    for (k, want) in map {
        let got = node.get(k);
        if !got.map(|g| subset(g, want)).unwrap_or(false) {
            return Err(format!("prop {k} is {:?}, expected {want}", got));
        }
    }
    Ok(())
}

fn event_from(arg: &Value) -> Result<NodeEvent, String> {
    let kind = arg["type"].as_str().ok_or("event needs a type")?;
    let mut obj = arg.as_object().cloned().unwrap_or_default();
    obj.insert("type".into(), Value::from(kind));
    Ok(NodeEvent::from_fields(&obj))
}
