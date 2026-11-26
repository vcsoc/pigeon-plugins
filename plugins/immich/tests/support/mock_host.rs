//! Mock Pigeon host for the plugin tests. Spawns the real `pigeon-immich --plugin` binary, speaks protocol v2
//! over its stdio, answers the plugin's host-service requests (secrets, progress, library, dialogs, windows,
//! notifications) from an in-memory store, and records every scene, toast and progress update.
//!
//! This stands in for the WP2 mock host until its crate is available; the message shapes are the ones in
//! plugin-ui-protocol.md section 2, 3, 8, 10 and 11.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub struct MockHost {
    pub child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Value>,
    next_id: u64,
    pub secrets: Arc<Mutex<HashMap<String, String>>>,
    pub scenes: HashMap<String, Value>,
    pub toasts: Vec<Value>,
    pub progress: Vec<Value>,
    pub modals: Vec<Value>,
    pub library_records: Vec<Value>,
    pub pick_folder: Option<String>,
    pub import_ids: Vec<String>,
    pub progress_seq: u64,
    pub calls: Vec<Value>,
    pub unanswered: Vec<String>,
    pub ready: Value,
    pub stderr_path: std::path::PathBuf,
}

pub fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_pigeon-immich")
}

impl MockHost {
    pub fn start(data_dir: &std::path::Path, session_dir: &std::path::Path) -> MockHost {
        Self::start_mode("--plugin", data_dir, session_dir)
    }

    /// Starts the binary in the given mode. Only `--plugin` gets the hello/ready handshake.
    pub fn start_mode(mode: &str, data_dir: &std::path::Path, session_dir: &std::path::Path) -> MockHost {
        let stderr_path = data_dir.with_extension("stderr.log");
        let stderr = std::fs::File::create(&stderr_path).expect("stderr log");
        let mut child = Command::new(binary())
            .arg(mode)
            .env("PIGEON_PLUGIN_DATA", data_dir)
            .env("PIGEON_SESSION_DIR", session_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("spawn pigeon-immich");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let mut host = MockHost {
            child,
            stdin,
            rx,
            next_id: 1,
            secrets: Arc::new(Mutex::new(HashMap::new())),
            scenes: HashMap::new(),
            toasts: vec![],
            progress: vec![],
            modals: vec![],
            library_records: vec![],
            pick_folder: None,
            import_ids: vec![],
            progress_seq: 0,
            calls: vec![],
            unanswered: vec![],
            ready: Value::Null,
            stderr_path,
        };
        if mode == "--self-test" {
            return host;
        }
        host.send(json!({"v":2,"t":"hello","host":{"name":"Pigeon","version":"test"},"protocol":{"min":2,"max":2},
            "theme":{"mode":"dark"},"locale":"en","scale":1.0,"data_dir":data_dir,"session_dir":session_dir,
            "features":["scene.patch","canvas","list.window","resources.file","source","search"]}));
        host.ready = host.wait_for(|m| m["t"] == "ready", Duration::from_secs(10)).expect("plugin sends ready within 10 s");
        host
    }

    pub fn send(&mut self, message: Value) {
        let line = format!("{message}\n");
        self.stdin.write_all(line.as_bytes()).expect("write to plugin");
        self.stdin.flush().expect("flush");
    }

    /// Reads messages until `predicate` matches. Plugin requests are answered on the way; scenes, toasts and
    /// progress are recorded. Returns `None` on timeout.
    pub fn wait_for(&mut self, predicate: impl Fn(&Value) -> bool, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let message = self.rx.recv_timeout(remaining).ok()?;
            self.observe(&message);
            if predicate(&message) {
                return Some(message);
            }
        }
    }

    fn observe(&mut self, message: &Value) {
        match message["t"].as_str() {
            Some("ui.set") => {
                if let Some(instance) = message["instance"].as_str() {
                    self.scenes.insert(instance.to_owned(), message["scene"].clone());
                }
            }
            Some("req") => self.answer(message),
            Some("log") => {}
            _ => {}
        }
    }

    fn answer(&mut self, request: &Value) {
        let method = request["method"].as_str().unwrap_or("").to_owned();
        let params = request["params"].clone();
        self.calls.push(json!({"method":method,"params":params}));
        let result: Result<Value, String> = match method.as_str() {
            "secrets.set" => {
                if let (Some(k), Some(v)) = (params["key"].as_str(), params["value"].as_str()) {
                    self.secrets.lock().unwrap().insert(k.into(), v.into());
                }
                Ok(json!({}))
            }
            "secrets.get" => match self.secrets.lock().unwrap().get(params["key"].as_str().unwrap_or("")) {
                Some(v) => Ok(json!({"value": v})),
                None => Err("not_found: no secret".into()),
            },
            "secrets.delete" => {
                self.secrets.lock().unwrap().remove(params["key"].as_str().unwrap_or(""));
                Ok(json!({}))
            }
            "progress.start" => {
                self.progress_seq += 1;
                let job = format!("job-{}", self.progress_seq);
                self.progress.push(json!({"method":method,"params":params.clone(),"job":job}));
                Ok(json!({"job": job}))
            }
            "progress.update" | "progress.end" => {
                self.progress.push(json!({"method":method,"params":params}));
                Ok(json!({}))
            }
            "notify.toast" => {
                self.toasts.push(params);
                Ok(json!({}))
            }
            "window.open_modal" => {
                self.modals.push(params);
                Ok(json!({}))
            }
            "dialog.pick_folder" => match &self.pick_folder {
                Some(path) => Ok(json!({"path": path})),
                None => Err("cancelled".into()),
            },
            "library.get" => Ok(json!({"records": self.library_records.clone()})),
            "library.import" => {
                // Reports one id per imported path unless the test pins the ids.
                let ids: Vec<String> = if self.import_ids.is_empty() {
                    params["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(|p| format!("local-{}", p.rsplit('/').next().unwrap_or(p))).collect()
                } else {
                    self.import_ids.clone()
                };
                Ok(json!({"ids": ids}))
            }
            "library.set_tags" | "library.set_meta" => Ok(json!({})),
            "log" => Ok(json!({})),
            _ => Err("unsupported_method: mock host".into()),
        };
        let id = request["id"].clone();
        let response = match result {
            Ok(value) => json!({"v":2,"t":"res","id":id,"result":value}),
            Err(message) => json!({"v":2,"t":"res","id":id,"error":{"code":"failed","message":message}}),
        };
        self.send(response);
    }

    /// Sends a host-to-plugin request and returns the `res` message.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.request_timeout(method, params, Duration::from_secs(20))
            .unwrap_or_else(|| panic!("no response to {method}"))
    }

    pub fn request_timeout(&mut self, method: &str, params: Value, timeout: Duration) -> Option<Value> {
        self.next_id += 1;
        let id = format!("h{}", self.next_id);
        self.send(json!({"v":2,"t":"req","id":id,"method":method,"params":params}));
        self.wait_for(|m| m["t"] == "res" && m["id"] == id.as_str(), timeout)
    }

    /// Sends a `ui.event` for a node of a slot instance.
    pub fn event(&mut self, instance: &str, node: &str, kind: &str, value: Value, rev: u64) {
        self.send(json!({"v":2,"t":"ui.event","instance":instance,"node":node,"type":kind,"value":value,"rev":rev}));
    }

    /// Asks the plugin to render an instance and returns the scene it sends back.
    pub fn open(&mut self, instance: &str) -> Value {
        self.send(json!({"v":2,"t":"visibility","instance":instance,"visible":true}));
        let message = self
            .wait_for(|m| m["t"] == "ui.set" && m["instance"] == instance, Duration::from_secs(10))
            .unwrap_or_else(|| panic!("no scene for {instance}"));
        message["scene"].clone()
    }

    /// Pumps messages (answering plugin requests and recording scenes) until `condition` holds or time runs out.
    pub fn until(&mut self, timeout: Duration, mut condition: impl FnMut(&MockHost) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition(self) {
                return true;
            }
            let _ = self.wait_for(|_| false, Duration::from_millis(25));
        }
        condition(self)
    }

    /// Answers a ping and reports the round-trip time.
    pub fn ping(&mut self, timeout: Duration) -> Option<Duration> {
        let started = Instant::now();
        self.send(json!({"v":2,"t":"ping","id":"ping"}));
        self.wait_for(|m| m["t"] == "pong", timeout)?;
        Some(started.elapsed())
    }

    pub fn shutdown(&mut self) {
        self.send(json!({"v":2,"t":"shutdown"}));
        let _ = self.child.wait();
    }

    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    pub fn toast_text(&self) -> Vec<String> {
        self.toasts.iter().filter_map(|t| t["text"].as_str().map(str::to_owned)).collect()
    }
}

impl Drop for MockHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Finds the first node with `id` in a scene tree.
pub fn find_node<'a>(scene: &'a Value, id: &str) -> Option<&'a Value> {
    if scene["id"] == id {
        return Some(scene);
    }
    for key in ["children"] {
        if let Some(children) = scene[key].as_array() {
            for child in children {
                if let Some(found) = find_node(child, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Text of every text node in a scene, joined by newlines.
pub fn scene_text(scene: &Value) -> String {
    let mut out = Vec::new();
    collect_text(scene, &mut out);
    out.join("\n")
}

fn collect_text(scene: &Value, out: &mut Vec<String>) {
    if let Some(text) = scene["text"].as_str() {
        out.push(text.to_owned());
    }
    if let Some(label) = scene["label"].as_str() {
        out.push(label.to_owned());
    }
    if let Some(children) = scene["children"].as_array() {
        for child in children {
            collect_text(child, out);
        }
    }
}
