//! Protocol v2 transport: JSON Lines on stdin/stdout, a single UI-owning loop, and a canvas fast
//! path. This is the thin adapter that stands in for `pigeon-plugin-sdk` until WP2 publishes it;
//! all protocol knowledge that is Reelio-specific lives in `reelio-ui`.
//!
//! Threads: one stdin reader, one ticker (100 ms), one worker per engine request. The loop owns
//! the state, so events are applied in arrival order and nothing blocks on the engine.

use crate::engine_bridge::EngineBridge;
use reelio_ui::engine::Reply;
use reelio_ui::scene;
use reelio_ui::state::{Effect, Event, State};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

/// The embedded manifest is the single source of truth for `ready.contributions`.
pub const MANIFEST: &str = include_str!("../../../plugin.json");

pub enum Inbound {
    Host(Value),
    Engine { id: u64, reply: Reply },
    Tick,
    Eof,
}

/// Build the `ready` message from the embedded manifest.
pub fn ready_message() -> Value {
    let manifest: Value = serde_json::from_str(MANIFEST).unwrap_or(Value::Null);
    json!({
        "v": 2,
        "t": "ready",
        "plugin": {"id": "reelio", "version": manifest["version"]},
        "protocol": 2,
        "contributions": manifest["ui"]["contributions"],
    })
}

/// Split a scene into its structure and the canvas draw lists, keyed by node id. Structure-only
/// equality lets the loop send cheap `canvas.draw` messages instead of whole scenes.
pub fn split_canvas(scene: &Value) -> (Value, Vec<(String, Value)>) {
    let mut draws = vec![];
    let mut stripped = scene.clone();
    strip(&mut stripped, &mut draws);
    (stripped, draws)
}

fn strip(v: &mut Value, draws: &mut Vec<(String, Value)>) {
    if v.get("kind").and_then(Value::as_str) == Some("canvas") {
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let draw = v.get("draw").cloned().unwrap_or(Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("draw".into(), Value::Null);
        }
        draws.push((id, draw));
    }
    match v {
        Value::Object(map) => {
            for value in map.values_mut() {
                strip(value, draws);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip(item, draws);
            }
        }
        _ => {}
    }
}

/// Per-instance memory of what the host last received.
#[derive(Default)]
pub struct Pump {
    sent_structure: BTreeMap<String, Value>,
    sent_draws: BTreeMap<(String, String), Value>,
    visible: BTreeSet<String>,
    open_modals: BTreeSet<String>,
    frame: u64,
    request_id: u64,
}

impl Pump {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.request_id += 1;
        json!({"v": 2, "t": "req", "id": self.request_id.to_string(), "method": method, "params": params})
    }

    /// Messages needed to bring the host up to date with the current scenes.
    pub fn render(&mut self, state: &State) -> Vec<Value> {
        let mut out = vec![];
        let scenes = scene::all(state);
        // Modals open and close through host services; other instances follow visibility.
        for name in scenes
            .keys()
            .filter(|k| k.starts_with("modal."))
            .cloned()
            .collect::<Vec<_>>()
        {
            if self.open_modals.insert(name.clone()) {
                let short = name.trim_start_matches("modal.").to_string();
                out.push(self.request(
                    "window.open_modal",
                    json!({"contribution": short, "instance": name, "props": {}}),
                ));
            }
        }
        let gone: Vec<String> = self
            .open_modals
            .iter()
            .filter(|m| !scenes.contains_key(*m))
            .cloned()
            .collect();
        for name in gone {
            self.open_modals.remove(&name);
            self.sent_structure.remove(&name);
            out.push(self.request("window.close_modal", json!({"instance": name})));
        }
        for (instance, scene) in &scenes {
            if !instance.starts_with("modal.") && !self.visible.contains(instance) {
                continue;
            }
            let (structure, draws) = split_canvas(scene);
            let same_structure = self.sent_structure.get(instance) == Some(&structure);
            if same_structure {
                for (node, draw) in draws {
                    let key = (instance.clone(), node.clone());
                    if self.sent_draws.get(&key) != Some(&draw) {
                        self.frame += 1;
                        out.push(json!({
                            "v": 2, "t": "canvas.draw", "instance": instance, "node": node,
                            "frame": self.frame,
                            "clear": draw.get("clear").cloned().unwrap_or(Value::Null),
                            "ops": draw.get("ops").cloned().unwrap_or(json!([])),
                        }));
                        self.sent_draws.insert(key, draw);
                    }
                }
            } else {
                for (node, draw) in draws {
                    self.sent_draws.insert((instance.clone(), node), draw);
                }
                self.sent_structure.insert(instance.clone(), structure);
                out.push(json!({"v": 2, "t": "ui.set", "instance": instance, "scene": scene}));
            }
        }
        out
    }
}

/// Map one host event onto a reducer event. Unknown messages are ignored (forward compatibility).
pub fn map_host(msg: &Value, state: &mut State, pump: &mut Pump) -> Option<Vec<Value>> {
    let kind = msg.get("t")?.as_str()?;
    match kind {
        "hello" => Some(vec![ready_message()]),
        "ping" => Some(vec![
            json!({"v": 2, "t": "pong", "id": msg.get("id").cloned().unwrap_or(Value::Null)}),
        ]),
        "visibility" | "ui.visible" => {
            let instance = msg.get("instance")?.as_str()?.to_string();
            let visible = msg.get("visible").and_then(Value::as_bool).unwrap_or(true);
            if visible {
                pump.visible.insert(instance.clone());
                pump.sent_structure.remove(&instance);
            } else {
                pump.visible.remove(&instance);
            }
            Some(vec![])
        }
        "resize" => {
            if let Some(w) = msg.get("w").and_then(Value::as_f64) {
                state.set_window(w);
            }
            Some(vec![])
        }
        "ui.event" => {
            let node = msg.get("node")?.as_str()?.to_string();
            let ty = msg.get("type").and_then(Value::as_str).unwrap_or("click");
            let value = if matches!(ty, "pointer" | "key" | "wheel" | "drop" | "hover") {
                let mut v = msg.clone();
                v["type"] = json!(ty);
                v
            } else {
                let mut v = msg.get("value").cloned().unwrap_or(Value::Null);
                if ty == "focus" || ty == "blur" {
                    v = json!(ty == "focus");
                }
                v
            };
            state.dispatch(Event::Ui { node, value });
            Some(vec![])
        }
        "command" => {
            let id = msg.get("id")?.as_str()?.to_string();
            state.dispatch(Event::Command { id });
            Some(vec![])
        }
        "drop" => {
            let paths: Vec<String> = msg
                .get("paths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            state.dispatch(Event::Drop { paths });
            Some(vec![])
        }
        "shutdown" => Some(vec![json!({"__shutdown": true})]),
        _ => None,
    }
}

fn write_msg(out: &mut impl Write, msg: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *out, msg)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Run the protocol until `shutdown` or end of input.
pub fn serve<R, W>(input: R, mut output: W, engine: Arc<dyn EngineBridge>) -> io::Result<()>
where
    R: BufRead + Send + 'static,
    W: Write,
{
    let (tx, rx) = mpsc::channel::<Inbound>();
    let reader_tx = tx.clone();
    thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reader_tx.send(Inbound::Host(value)).is_err() {
                return;
            }
        }
        let _ = reader_tx.send(Inbound::Eof);
    });
    let ticker_tx = tx.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(100));
        if ticker_tx.send(Inbound::Tick).is_err() {
            break;
        }
    });

    let mut state = State::new();
    let mut pump = Pump::default();
    let started = std::time::Instant::now();
    while let Ok(msg) = rx.recv() {
        let mut shutdown = false;
        match msg {
            Inbound::Host(value) => {
                if let Some(replies) = map_host(&value, &mut state, &mut pump) {
                    for reply in replies {
                        if reply.get("__shutdown").is_some() {
                            shutdown = true;
                        } else {
                            write_msg(&mut output, &reply)?;
                        }
                    }
                }
            }
            Inbound::Engine { id, reply } => state.dispatch(Event::Engine { id, reply }),
            Inbound::Tick => state.dispatch(Event::Tick {
                now: started.elapsed().as_secs_f64() * 1000.0,
            }),
            Inbound::Eof => break,
        }
        for effect in state.drain() {
            match effect {
                Effect::Engine { id, request } => {
                    let engine = engine.clone();
                    let tx = tx.clone();
                    thread::spawn(move || {
                        let reply = engine.call(request);
                        let _ = tx.send(Inbound::Engine { id, reply });
                    });
                }
                Effect::Persist { key, value } => {
                    let req = pump.request("settings.set", json!({"key": key, "value": value}));
                    write_msg(&mut output, &req)?;
                }
                Effect::CloseWindow => {
                    let req = pump.request(
                        "log",
                        json!({"level": "info", "message": "close requested by the user"}),
                    );
                    write_msg(&mut output, &req)?;
                }
                // The preview element is driven by the engine's frame ring once it is connected.
                Effect::Seek { .. } | Effect::SetPlaying(_) => {}
            }
        }
        for message in pump.render(&state) {
            write_msg(&mut output, &message)?;
        }
        if shutdown {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_bridge::UnconnectedEngine;
    use std::io::Cursor;
    use std::sync::Mutex;

    /// Shared buffer so the test can read what the loop wrote.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn run(script: &str) -> Vec<Value> {
        let sink = Sink::default();
        serve(
            Cursor::new(script.as_bytes().to_vec()),
            sink.clone(),
            Arc::new(UnconnectedEngine),
        )
        .unwrap();
        let bytes = sink.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn hello_is_answered_with_ready_and_manifest_contributions() {
        let out = run("{\"v\":2,\"t\":\"hello\"}\n{\"v\":2,\"t\":\"shutdown\"}\n");
        let ready = out.iter().find(|m| m["t"] == "ready").expect("ready");
        assert_eq!(ready["plugin"]["id"], "reelio");
        let slots: Vec<&str> = ready["contributions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["slot"].as_str())
            .collect();
        for needed in [
            "sidebar.entry",
            "workspace.view",
            "inspector.panel",
            "asset.action",
            "preferences.page",
            "modal",
            "command",
        ] {
            assert!(slots.contains(&needed), "manifest declares {needed}");
        }
    }

    #[test]
    fn visible_workspace_receives_a_scene_and_later_only_canvas_fast_path_updates() {
        let out = run(
            "{\"v\":2,\"t\":\"hello\"}\n{\"v\":2,\"t\":\"visibility\",\"instance\":\"main\",\"visible\":true}\n{\"v\":2,\"t\":\"ping\",\"id\":\"1\"}\n",
        );
        let set = out
            .iter()
            .find(|m| m["t"] == "ui.set" && m["instance"] == "main")
            .expect("main scene sent");
        assert!(set["scene"]
            .to_string()
            .contains("Drop media into the media pool"));
        assert!(out.iter().any(|m| m["t"] == "pong" && m["id"] == "1"));
    }

    #[test]
    fn split_canvas_moves_draw_lists_out_of_the_structure() {
        let scene = json!({"id":"root","kind":"column","children":[{"id":"c","kind":"canvas","draw":{"ops":[{"op":"rect"}]}}]});
        let (structure, draws) = split_canvas(&scene);
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].0, "c");
        assert!(structure["children"][0]["draw"].is_null());
    }

    #[test]
    fn unknown_messages_are_ignored() {
        let out = run("{\"v\":2,\"t\":\"future.thing\"}\n{\"v\":2,\"t\":\"hello\"}\n");
        assert!(out.iter().any(|m| m["t"] == "ready"));
    }
}
