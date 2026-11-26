//! Fakes for the host services of protocol section 10. The mock host answers `req` lines from the
//! plugin with these, after the permission and parameter checks done by the session.
//!
//! Everything is in memory and recorded, so tests can assert on what the plugin asked for
//! (toasts, progress jobs, modals, opened URLs, settings) without a real host.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// Data the fakes answer with. Tests set this before launching or through `Session::services_mut`.
#[derive(Debug, Clone, Default)]
pub struct Fixtures {
    /// Assets returned by `library.query` / `library.get` (each a protocol Item-like object).
    pub assets: Vec<Value>,
    /// Current selection (ids). Kinds and paths are derived from `assets` when present.
    pub selection: Vec<String>,
    /// Answers for `dialog.open_file`.
    pub dialog_paths: Vec<String>,
    /// Initial settings (plugin-private key/value).
    pub settings: BTreeMap<String, Value>,
    /// Initial secrets.
    pub secrets: BTreeMap<String, String>,
}

/// One toast the plugin showed.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub level: String,
    pub text: String,
}

/// One progress job.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub value: Option<f64>,
    pub message: Option<String>,
    pub ended: Option<bool>,
}

/// The services fakes and their recorded side effects.
#[derive(Debug, Default)]
pub struct Services {
    pub fixtures: Fixtures,
    pub permissions: Vec<String>,
    pub settings: BTreeMap<String, Value>,
    pub secrets: BTreeMap<String, String>,
    pub toasts: Vec<Toast>,
    pub jobs: Vec<Job>,
    pub modals: Vec<Value>,
    pub opened_urls: Vec<String>,
    pub logs: Vec<(String, String)>,
    pub clipboard_text: Option<String>,
    pub title: Option<String>,
    pub selection_subscribed: bool,
    job_counter: u64,
}

impl Services {
    pub fn new(fixtures: Fixtures, permissions: Vec<String>) -> Services {
        Services {
            settings: fixtures.settings.clone(),
            secrets: fixtures.secrets.clone(),
            fixtures,
            permissions,
            ..Services::default()
        }
    }

    /// Answer one method. Errors are `(code, message)` pairs. Permission and schema checks are
    /// done by the caller.
    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let str_param = |k: &str| params[k].as_str().unwrap_or("").to_string();
        Ok(match method {
            "selection.get" => {
                let (ids, kinds, paths) = self.selection();
                json!({"ids": ids, "kinds": kinds, "paths": paths})
            }
            "selection.subscribe" => {
                self.selection_subscribed = true;
                json!({"subscribed": true})
            }
            "library.query" => {
                let limit = params["limit"].as_u64().unwrap_or(100) as usize;
                let items: Vec<Value> = self.fixtures.assets.iter().take(limit).cloned().collect();
                let total = self.fixtures.assets.len();
                json!({"items": items, "total": total})
            }
            "library.get" => {
                let wanted: Vec<String> = params["ids"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                let items: Vec<Value> = self.fixtures.assets.iter().filter(|a| wanted.iter().any(|w| a["id"] == w.as_str())).cloned().collect();
                json!({"items": items})
            }
            "library.import" => {
                let count = params["paths"].as_array().map(|a| a.len()).unwrap_or(0);
                let ids: Vec<String> = (0..count).map(|i| format!("imported-{i}")).collect();
                json!({"ids": ids})
            }
            "library.set_tags" | "library.set_meta" | "library.reveal" => json!({}),
            "settings.get" => {
                let key = str_param("key");
                json!({"value": self.settings.get(&key).cloned().unwrap_or(Value::Null)})
            }
            "settings.set" => {
                self.settings.insert(str_param("key"), params["value"].clone());
                json!({})
            }
            "settings.delete" => {
                self.settings.remove(&str_param("key"));
                json!({})
            }
            "secrets.get" => {
                let v = self.secrets.get(&str_param("key")).cloned();
                json!({"value": v})
            }
            "secrets.set" => {
                self.secrets.insert(str_param("key"), str_param("value"));
                json!({})
            }
            "secrets.delete" => {
                self.secrets.remove(&str_param("key"));
                json!({})
            }
            "dialog.open_file" => json!({"paths": self.fixtures.dialog_paths.clone()}),
            "dialog.save_file" => json!({"path": self.fixtures.dialog_paths.first().cloned()}),
            "dialog.pick_folder" => json!({"path": self.fixtures.dialog_paths.first().cloned()}),
            "clipboard.read" => json!({"text": self.clipboard_text.clone()}),
            "clipboard.write" => {
                self.clipboard_text = params["text"].as_str().map(String::from);
                json!({})
            }
            "shell.open_url" => {
                let url = str_param("url");
                if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:")) {
                    return Err(("invalid_url".into(), "only https, http and mailto may be opened".into()));
                }
                self.opened_urls.push(url);
                json!({})
            }
            "shell.open_path" | "shell.reveal" => json!({}),
            "notify.toast" => {
                self.toasts.push(Toast { level: str_param("level"), text: str_param("text") });
                json!({})
            }
            "progress.start" => {
                self.job_counter += 1;
                let id = format!("job-{}", self.job_counter);
                self.jobs.push(Job { id: id.clone(), title: str_param("title"), value: None, message: None, ended: None });
                json!({"job": id})
            }
            "progress.update" => {
                let job = str_param("job");
                match self.jobs.iter_mut().find(|j| j.id == job) {
                    Some(j) => {
                        j.value = params["value"].as_f64();
                        j.message = params["message"].as_str().map(String::from);
                        json!({})
                    }
                    None => return Err(("unknown_job".into(), format!("no job {job}"))),
                }
            }
            "progress.end" => {
                let job = str_param("job");
                match self.jobs.iter_mut().find(|j| j.id == job) {
                    Some(j) => {
                        j.ended = Some(params["ok"].as_bool().unwrap_or(true));
                        j.message = params["message"].as_str().map(String::from);
                        json!({})
                    }
                    None => return Err(("unknown_job".into(), format!("no job {job}"))),
                }
            }
            "window.open_modal" => {
                let instance = format!("{}#{}", str_param("contribution"), self.modals.len() + 1);
                self.modals.push(json!({"contribution": str_param("contribution"), "props": params["props"], "instance": instance}));
                json!({"instance": instance})
            }
            "window.close_modal" => json!({}),
            "window.set_title" => {
                self.title = Some(str_param("title"));
                json!({})
            }
            "drag.begin" => json!({}),
            "theme.get" => json!({"theme": {"mode": "dark", "scale": 2.0, "colors": {}}}),
            "locale.get" => json!({"locale": "en"}),
            "log" => {
                self.logs.push((str_param("level"), str_param("message")));
                json!({})
            }
            other => return Err(("unknown_method".into(), format!("the host has no method {other}"))),
        })
    }

    fn selection(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut kinds: Vec<String> = Vec::new();
        let mut paths: Vec<String> = Vec::new();
        for id in &self.fixtures.selection {
            if let Some(a) = self.fixtures.assets.iter().find(|a| a["id"] == id.as_str()) {
                if let Some(k) = a["kind"].as_str() {
                    if !kinds.iter().any(|x| x == k) {
                        kinds.push(k.to_string());
                    }
                }
                if let Some(p) = a["path"].as_str() {
                    paths.push(p.to_string());
                }
            }
        }
        (self.fixtures.selection.clone(), kinds, paths)
    }
}
