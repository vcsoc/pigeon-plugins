//! The Immich plugin: request handlers, background sync, the verified transfer queue, pickers and scenes.
//!
//! Threading (nothing here runs on a protocol thread):
//! * `general` pool (4 threads): `library.source` list/children/describe, scene rendering, small commands.
//! * `previews` pool (4 threads): thumbnail requests. Fetching and decoding are bounded by the thumbnail cache.
//! * One dedicated thread per long job: connect, sync, a transfer batch, a tag write-back.
//!   Only one connect/sync runs at a time (`busy`), so long jobs never pile up.
//! * Locks: `state` is held for copies and field updates only, never across network, disk or SQLite work.
//!   The SQLite lock is taken after state is released; `still_current` checks take state inside the SQLite
//!   transaction, which is the only nesting and never runs the other way round.

use crate::api::{self, ImmichClient, Result};
use crate::config::{self, Config};
use crate::db::{self, Db, Node};
use crate::pool::Pool;
use pigeon_plugin_sdk::{runtime::Ctx, scene::Node as SceneNode, Incoming, NodeEvent};
use crate::thumbs::Thumbs;
use crate::transfer;
use crate::ui;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SETTINGS: &str = "immich/settings";
pub const STATUS: &str = "immich/status";
pub const TRANSFER_PREFIX: &str = "immich/transfer/";
pub const TAGS_PREFIX: &str = "immich/tags/";
/// Largest number of files in one transfer batch (reported explicitly, never truncated silently).
pub const BATCH_LIMIT: usize = 200;
/// Largest number of images one tag assignment may touch.
pub const TAG_LIMIT: usize = 1000;
/// Ring of files handed to the host for thumbnails; a slot is reused only after this many newer requests.
pub const THUMB_RING: u64 = 256;
/// Dialogs wait for a human, so they get a longer host timeout than services.
pub const DIALOG_TIMEOUT: Duration = Duration::from_secs(600);

pub struct App {
    pub data_dir: PathBuf,
    pub session_dir: PathBuf,
    pub ctx: Ctx,
    pub db: Arc<Db>,
    pub thumbs: Arc<Thumbs>,
    general: Pool,
    previews: Pool,
    pub state: Mutex<State>,
    ring: AtomicU64,
}

#[derive(Clone)]
pub struct Session {
    pub client: ImmichClient,
    pub id: u64,
}

/// Preferences form values (never persisted; the password is only ever held here, in memory).
#[derive(Clone, Default)]
pub struct Draft {
    pub enabled: bool,
    pub server: String,
    pub email: String,
    pub password: String,
    pub remember: bool,
    pub rev: u64,
}

#[derive(Clone)]
pub enum ItemSource {
    Upload { path: PathBuf, record: Value },
    Download { asset_id: String },
}

#[derive(Clone)]
pub struct BatchItem {
    pub name: String,
    pub source: ItemSource,
    pub state: String,
    pub done: bool,
    pub attempted: bool,
}

pub struct Batch {
    pub id: String,
    pub upload: bool,
    pub destination: Option<PathBuf>,
    pub items: Vec<BatchItem>,
    pub running: bool,
    pub stop: Arc<AtomicBool>,
    pub job: String,
    pub status: String,
}

pub struct TagPicker {
    pub id: String,
    pub ids: Vec<String>,
    pub selected: Option<String>,
    pub status: String,
}

pub struct State {
    pub config: Config,
    pub session: Option<Session>,
    /// Bumped on every connect, disconnect and disable. Work started under an older value is discarded.
    pub session_seq: u64,
    pub draft: Draft,
    pub status: String,
    pub busy: bool,
    pub batch: Option<Batch>,
    pub picker: Option<TagPicker>,
    pub cancels: HashMap<String, Arc<AtomicBool>>,
    /// Cancels that arrived before their job registered (the host answers `progress.start` first).
    pub early_cancels: std::collections::HashSet<String>,
    pub next_batch: u64,
}

impl State {
    fn new(config: Config) -> State {
        let draft = Draft {
            enabled: config.enabled,
            server: config.server_url.clone(),
            email: config.email.clone(),
            password: String::new(),
            remember: config.secret_id.is_some(),
            rev: 0,
        };
        State {
            config,
            session: None,
            session_seq: 0,
            draft,
            status: String::new(),
            busy: false,
            batch: None,
            picker: None,
            cancels: HashMap::new(),
            early_cancels: std::collections::HashSet::new(),
            next_batch: 1,
        }
    }
}

/// Registers a job's cancel flag. A cancel that arrived earlier for this job takes effect immediately.
fn register_cancel(state: &mut State, job: &str, flag: Arc<AtomicBool>) {
    if state.early_cancels.remove(job) {
        flag.store(true, Ordering::SeqCst);
    }
    state.cancels.insert(job.to_owned(), flag);
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn string_list(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect()
}

/// Ids named by a host command or request (`context.ids` or `context.selection`).
fn context_ids(value: &Value) -> Vec<String> {
    let direct = string_list(&value["ids"]);
    if direct.is_empty() {
        string_list(&value["selection"])
    } else {
        direct
    }
}

/// Merges names from a tag list (`["a", ...]`), trimming and dropping duplicates and blanks.
pub fn clean_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names.into_iter().map(str::trim).filter(|n| !n.is_empty()) {
        if !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

impl App {
    pub fn new(data_dir: PathBuf, session_dir: PathBuf, ctx: Ctx) -> Result<Arc<App>> {
        std::fs::create_dir_all(&data_dir).map_err(|_| "Could not create the Immich data folder".to_owned())?;
        let db = Db::open(&data_dir.join("immich.sqlite3"))?;
        let config = config::load(&data_dir);
        Ok(Arc::new(App {
            thumbs: Arc::new(Thumbs::new(data_dir.join("thumbs"))),
            data_dir,
            session_dir,
            ctx,
            db: Arc::new(db),
            general: Pool::new("immich-work", 4, 256),
            previews: Pool::new("immich-preview", 4, 256),
            state: Mutex::new(State::new(config)),
            ring: AtomicU64::new(0),
        }))
    }

    /// Host service call (SDK timeout). Errors are plain text for the UI.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.ctx.call(method, params).map_err(|e| e.to_string())
    }

    pub fn call_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.ctx.call_timeout(method, params, timeout).map_err(|e| e.to_string())
    }

    /// Answers a host request (`res`), or fails it with the message.
    fn answer(&self, id: &str, result: Result<Value>) {
        let _ = match result {
            Ok(value) => self.ctx.answer(id, value),
            Err(message) => self.ctx.answer_error(id, "failed", &message),
        };
    }

    /// Renders a scene built as JSON. The SDK's typed `Node` accepts every prop, so nothing is dropped.
    fn set_scene(&self, instance: &str, scene: Value) {
        match serde_json::from_value::<SceneNode>(scene) {
            Ok(node) => {
                let _ = self.ctx.set_scene(instance, node);
            }
            Err(error) => self.ctx.log("warn", &format!("invalid scene for {instance}: {error}")),
        }
    }

    /// Runs `job` on the general pool; a saturated pool answers `busy` (the caller decides what that means).
    fn spawn_general(&self, job: impl FnOnce() + Send + 'static) -> bool {
        self.general.submit(Box::new(job)).is_ok()
    }

    /// Starts a named thread for one long job.
    fn spawn_job(self: &Arc<Self>, name: &str, job: impl FnOnce(Arc<App>) + Send + 'static) {
        let app = self.clone();
        let _ = std::thread::Builder::new().name(name.to_owned()).spawn(move || job(app));
    }

    /// Starts the saved connection when the user had remembered the password (startup auto-sync).
    pub fn start(self: &Arc<Self>) {
        let remembered = {
            let state = self.state.lock().ok();
            state.is_some_and(|s| s.config.enabled && s.config.secret_id.is_some())
        };
        if remembered {
            self.begin_busy("Connecting to Immich...");
            self.spawn_job("immich-connect", |app| {
                let result = app.connect_saved().and_then(|()| app.sync_full());
                app.finish_busy(result.map(|_| "Immich connected; thumbnail references synchronized".to_owned()));
            });
        }
    }

    fn begin_busy(&self, status: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.busy = true;
            state.status = status.to_owned();
        }
        self.render_settings();
        self.render_status();
    }

    fn finish_busy(&self, result: Result<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.busy = false;
            state.status = result.unwrap_or_else(|message| message);
        }
        self.render_settings();
        self.render_status();
    }

    /// Entry point for every message from the host. The SDK calls this on its app thread: it only
    /// parses and forwards, so nothing here waits on the network, the disk or the database.
    pub fn handle(self: &Arc<Self>, message: Incoming) {
        match message {
            Incoming::Request { id, method, params } => self.request(id, method, params),
            Incoming::Visibility { instance, visible } => {
                if visible {
                    let app = self.clone();
                    let _ = self.spawn_general(move || app.render(&instance));
                }
            }
            Incoming::UiEvent { instance, node, event } => self.event(&instance, &node, event),
            Incoming::Command { command, context, .. } => self.command(command, context),
            Incoming::Cancel { job } => {
                if let Ok(mut state) = self.state.lock() {
                    match state.cancels.get(&job) {
                        Some(flag) => flag.store(true, Ordering::SeqCst),
                        None if state.early_cancels.len() < 64 => {
                            state.early_cancels.insert(job);
                        }
                        None => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn request(self: &Arc<Self>, id: String, method: String, params: Value) {
        let thumbnail = method == "source.thumbnail" || method == "source.open";
        let app = self.clone();
        let job_id = id.clone();
        let job = move || {
            let result = app.dispatch(&method, params);
            app.answer(&job_id, result);
        };
        let accepted = if thumbnail {
            self.previews.submit(Box::new(job)).is_ok()
        } else {
            self.general.submit(Box::new(job)).is_ok()
        };
        if !accepted {
            let _ = self.ctx.answer_error(&id, "busy", "Immich is busy; try again");
        }
    }

    fn dispatch(self: &Arc<Self>, method: &str, params: Value) -> Result<Value> {
        if let Some(source) = params["source"].as_str() {
            if source != "immich" {
                return Err("This plugin does not provide that source".into());
            }
        }
        match method {
            "source.describe" => Ok(self.describe()),
            "source.children" => self.children(&params),
            "source.list" => self.list(&params),
            "source.thumbnail" => self.thumbnail(&params),
            "source.open" => self.open(&params),
            "source.upload" => self.upload_files(&params),
            "source.download" => self.download_ids(&params),
            "source.write_meta" => self.write_meta(&params),
            _ => Err("unknown method".into()),
        }
    }

    // ----- library.source ------------------------------------------------------------------------------

    /// Account used for catalogue reads: the live session, or the configured account while offline.
    fn account(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        if let Some(session) = &state.session {
            return Some(session.client.account_key.clone());
        }
        let config = &state.config;
        if config.server_url.is_empty() || config.email.is_empty() {
            return None;
        }
        let base = api::normalize_server_url(&config.server_url).ok()?;
        Some(api::account_key(&base, &config.email))
    }

    fn status_line(&self) -> String {
        match self.state.lock() {
            Ok(state) if state.session.is_some() => "Connected".into(),
            Ok(state) if state.config.enabled => "Not connected: open Preferences > Immich".into(),
            _ => "Immich is disabled: enable it in Preferences > Immich".into(),
        }
    }

    fn describe(&self) -> Value {
        json!({
            "title": "Immich",
            "icon": "cloud",
            "root": {"id":"root","title":"Immich","children":true},
            "capabilities": ["browse","upload","download","metadata_write","tags"],
            "status": self.status_line(),
        })
    }

    fn children(&self, params: &Value) -> Result<Value> {
        let node = Node::parse(params["node"].as_str().unwrap_or("root")).ok_or("Unknown Immich collection")?;
        let Some(account) = self.account() else {
            return Ok(json!({"nodes":[],"status":self.status_line()}));
        };
        let counts = self.db.tree_counts(&account)?;
        let nodes: Vec<Value> = match node {
            Node::Root => vec![json!({"id":"annual","title":"Annual","icon":"calendar","children":true,"count":counts.total})],
            Node::Annual => counts
                .years
                .iter()
                .map(|(year, count)| json!({"id":Node::Year(*year).id(),"title":year.to_string(),"icon":"calendar","children":true,"count":count}))
                .collect(),
            Node::Year(year) => (1..=12usize)
                .map(|m| {
                    let count = counts.months.get(&(year, m)).copied().unwrap_or(0);
                    json!({"id":Node::Month(year, m).id(),"title":Node::Month(year, m).title(),"icon":"calendar-blank","children":false,"count":count})
                })
                .collect(),
            Node::Month(..) => vec![],
        };
        Ok(json!({"nodes": nodes}))
    }

    fn list(&self, params: &Value) -> Result<Value> {
        let node = Node::parse(params["node"].as_str().unwrap_or("root")).ok_or("Unknown Immich collection")?;
        let offset: u64 = match &params["cursor"] {
            Value::Number(n) => n.as_u64().unwrap_or(0),
            Value::String(s) => s.parse().unwrap_or(0),
            _ => 0,
        };
        let limit = params["limit"].as_u64().unwrap_or(120).clamp(1, 500);
        let Some(account) = self.account() else {
            return Ok(json!({"items":[],"total":0}));
        };
        let (rows, total) = self.db.list(&account, node, offset, limit)?;
        let next = offset + rows.len() as u64;
        let items: Vec<Value> = rows.iter().map(db::item).collect();
        let mut out = json!({"items": items, "total": total});
        if next < total {
            out["next"] = json!(next.to_string());
        }
        Ok(out)
    }

    /// Fetches the preview through the cache and copies it into a ring slot the host can read.
    fn preview_file(&self, asset_id: &str) -> Result<PathBuf> {
        let (client, session_id) = self.session_client()?;
        let account = client.account_key.clone();
        let fetch = || self.with_auth(&client, Some(session_id), |c| c.thumbnail(asset_id));
        let current = || self.session_is(session_id);
        let cached = self.thumbs.ensure(&account, asset_id, &fetch, &current)?;
        let slot = self.ring.fetch_add(1, Ordering::Relaxed) % THUMB_RING;
        std::fs::create_dir_all(&self.session_dir).map_err(|_| "Session folder unavailable".to_owned())?;
        let target = self.session_dir.join(format!("immich-thumb-{slot}.jpg"));
        std::fs::copy(&cached, &target).map_err(|_| "Could not hand the thumbnail to Pigeon".to_owned())?;
        Ok(target)
    }

    fn thumbnail(&self, params: &Value) -> Result<Value> {
        let id = api::valid_id(params["id"].as_str().unwrap_or(""))?.to_owned();
        match self.preview_file(&id) {
            Ok(file) => Ok(json!({"file": file})),
            Err(error) => Ok(json!({"error": error})),
        }
    }

    fn open(&self, params: &Value) -> Result<Value> {
        let id = api::valid_id(params["id"].as_str().unwrap_or(""))?.to_owned();
        match params["purpose"].as_str().unwrap_or("view") {
            "view" => Ok(json!({"file": self.preview_file(&id)?})),
            _ => Err("Immich items are thumbnail-only: download the original to a folder to edit or export it".into()),
        }
    }

    /// Files dropped on a managed Immich collection are uploaded as originals.
    fn upload_files(self: &Arc<Self>, params: &Value) -> Result<Value> {
        Node::parse(params["node"].as_str().unwrap_or("root")).ok_or("Choose an Immich collection")?;
        let files = string_list(&params["files"]);
        if files.is_empty() {
            return Err("Choose one or more files to upload".into());
        }
        let items = files
            .iter()
            .map(|file| {
                let path = std::path::Path::new(file);
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                (path.to_path_buf(), name.clone(), json!({"filename": name}))
            })
            .map(|(path, name, record)| BatchItem {
                name,
                source: ItemSource::Upload { path, record },
                state: "Queued".into(),
                done: false,
                attempted: false,
            })
            .collect();
        self.start_batch(true, items, None)
    }

    fn download_ids(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let ids = string_list(&params["ids"]);
        let destination = PathBuf::from(params["dest"].as_str().ok_or("Choose a local destination folder")?);
        self.download_batch(ids, destination)
    }

    fn download_batch(self: &Arc<Self>, ids: Vec<String>, destination: PathBuf) -> Result<Value> {
        if ids.is_empty() {
            return Err("Select Immich references to download".into());
        }
        if !destination.is_dir() {
            return Err("Choose an existing local folder".into());
        }
        let account = self.account().ok_or("Connect to Immich first")?;
        let items = ids
            .iter()
            .map(|id| {
                let valid = api::valid_id(id).is_ok();
                let known = valid && self.db.get(&account, id).ok().flatten().is_some();
                let (state, attempted) = if known { ("Queued".to_owned(), false) } else { ("The reference does not belong to the connected Immich account".to_owned(), true) };
                BatchItem {
                    name: self.name_of(&account, id),
                    source: ItemSource::Download { asset_id: id.clone() },
                    state,
                    done: false,
                    attempted,
                }
            })
            .collect();
        self.start_batch(false, items, Some(destination))
    }

    fn name_of(&self, account: &str, id: &str) -> String {
        self.db.get(account, id).ok().flatten().map(|r| r.filename).unwrap_or_else(|| id.to_owned())
    }

    // ----- transfers -----------------------------------------------------------------------------------

    /// Opens a verified batch and its transfer dialog, then runs it on a dedicated thread.
    fn start_batch(self: &Arc<Self>, upload: bool, items: Vec<BatchItem>, destination: Option<PathBuf>) -> Result<Value> {
        if items.is_empty() {
            return Err("Choose at least one file".into());
        }
        if items.len() > BATCH_LIMIT {
            return Err(format!("Choose at most {BATCH_LIMIT} files per transfer batch; split the selection"));
        }
        self.session_client()?;
        let (id, stop) = {
            let mut state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
            if state.batch.as_ref().is_some_and(|b| b.running) {
                return Err("Finish the current transfer batch first".into());
            }
            let id = format!("b{}", state.next_batch);
            state.next_batch += 1;
            let stop = Arc::new(AtomicBool::new(false));
            state.batch = Some(Batch {
                id: id.clone(),
                upload,
                destination,
                items,
                running: true,
                stop: stop.clone(),
                job: String::new(),
                status: String::new(),
            });
            (id, stop)
        };
        // The footer job id is allocated by the host; cancel (`cancel {job}`) then maps to the batch's stop flag.
        let job = self.progress_start("Transferring originals");
        if let Ok(mut state) = self.state.lock() {
            register_cancel(&mut state, &job, stop);
            if let Some(batch) = state.batch.as_mut().filter(|b| b.id == id) {
                batch.job = job.clone();
            }
        }
        let title = if upload { "Copy originals to Immich" } else { "Download originals to local folder" };
        let _ = self.call("window.open_modal", json!({"contribution":"transfer","instance":format!("{TRANSFER_PREFIX}{id}"),"props":{"title":title}}));
        self.spawn_transfer_thread(id.clone());
        Ok(json!({"job": job, "batch": id}))
    }

    fn spawn_transfer_thread(self: &Arc<Self>, batch_id: String) {
        self.spawn_job("immich-transfer", move |app| app.run_batch(&batch_id));
    }

    /// Processes pending items one at a time. The current file always finishes; stop only prevents the next.
    fn run_batch(self: &Arc<Self>, batch_id: &str) {
        loop {
            let next = {
                let Ok(mut state) = self.state.lock() else { return };
                let Some(batch) = state.batch.as_mut().filter(|b| b.id == batch_id) else { return };
                if batch.stop.load(Ordering::SeqCst) {
                    None
                } else {
                    batch.items.iter().position(|i| !i.done && !i.attempted).map(|index| {
                        batch.items[index].attempted = true;
                        batch.items[index].state = "Verifying original and metadata...".into();
                        (index, batch.items[index].source.clone(), batch.upload, batch.destination.clone(), batch.job.clone())
                    })
                }
            };
            let Some((index, source, upload, destination, job)) = next else { break };
            self.render_transfer(batch_id);
            let progress = self.batch_counts(batch_id);
            self.progress_update(&job, progress.0, progress.1, "Verifying original files");
            let outcome = self.run_item(&source, upload, destination.as_deref());
            self.apply_item_outcome(batch_id, index, outcome);
            self.render_transfer(batch_id);
        }
        self.finish_batch(batch_id);
    }

    fn batch_counts(&self, batch_id: &str) -> (usize, usize) {
        let Ok(state) = self.state.lock() else { return (0, 0) };
        match state.batch.as_ref().filter(|b| b.id == batch_id) {
            Some(batch) => (batch.items.iter().filter(|i| i.done).count(), batch.items.len()),
            None => (0, 0),
        }
    }

    fn run_item(&self, source: &ItemSource, upload: bool, destination: Option<&Path>) -> Result<(bool, String)> {
        let (client, session_id) = self.session_client()?;
        let account = client.account_key.clone();
        match source {
            ItemSource::Upload { path, record } => {
                if !upload {
                    return Err("This item is not an upload".into());
                }
                let canonical = std::fs::canonicalize(path).map_err(|_| "The source file is unavailable".to_owned())?;
                if !canonical.is_file() {
                    return Err("Choose a regular file".into());
                }
                let result = self.with_auth(&client, Some(session_id), |c| transfer::upload(c, &canonical, record))?;
                let verified = result["verified"] == true && result["metadataVerified"] == true;
                if verified {
                    let _ = self.db.record_receipt(&account, result["id"].as_str().unwrap_or(""), &canonical.to_string_lossy(), "upload", result["checksum"].as_str().unwrap_or(""), None, now_ms());
                }
                Ok((verified, if verified { "Original and metadata verified".into() } else { "Incomplete verification; retry required".into() }))
            }
            ItemSource::Download { asset_id } => {
                let destination = destination.ok_or("Choose a local destination folder")?;
                let result = self.with_auth(&client, Some(session_id), |c| transfer::download(c, asset_id, destination, &account))?;
                if result["verified"] != true || result["metadataVerified"] != true {
                    return Ok((false, "Incomplete verification; retry required".into()));
                }
                match self.index_download(&account, &result) {
                    Ok(()) => Ok((true, "Original and metadata verified; indexed in Pigeon".into())),
                    Err(error) => Ok((false, format!("Verified, but Pigeon could not index it yet: {error}. Retry to index it."))),
                }
            }
        }
    }

    /// Asks the host to index a verified download and restores the complete tags, note, rating, favourite
    /// and capture date, as the built-in client did. Unsupported relationships stay in the sidecar.
    fn index_download(&self, account: &str, result: &Value) -> Result<()> {
        let pigeon = &result["pigeon"];
        let remote = &result["asset"];
        let path = result["path"].as_str().unwrap_or("").to_owned();
        let imported = self.call("library.import", json!({"paths":[path],"mode":"reference"}))?;
        let ids: Vec<String> = imported["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
        if ids.is_empty() {
            return Err("the host did not report the indexed file".into());
        }
        let mut tags: Vec<String> = Vec::new();
        for name in pigeon["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(remote["tags"].as_array().into_iter().flatten().filter_map(|t| t["value"].as_str()))
        {
            let name = name.trim();
            if !name.is_empty() && !tags.iter().any(|t| t == name) {
                tags.push(name.to_owned());
            }
        }
        let note = pigeon["note"]
            .as_str()
            .filter(|n| !n.is_empty())
            .or_else(|| remote["exifInfo"]["description"].as_str())
            .unwrap_or("")
            .to_owned();
        let rating = pigeon["rating"]
            .as_u64()
            .or_else(|| remote["exifInfo"]["rating"].as_u64())
            .unwrap_or(0)
            .min(5);
        let favorite = pigeon["favorite"] == true || remote["isFavorite"] == true;
        let created = chrono::DateTime::parse_from_rfc3339(remote["fileCreatedAt"].as_str().unwrap_or(""))
            .map(|d| d.timestamp_millis())
            .ok();
        for id in &ids {
            self.call("library.set_tags", json!({"id":id,"tags":tags,"mode":"add"}))?;
            let mut patch = json!({"note":note,"rating":rating,"favorite":favorite});
            if let Some(created) = created {
                patch["created"] = json!(created);
            }
            self.call("library.set_meta", json!({"id":id,"patch":patch}))?;
            self.db.record_receipt(
                account,
                result["id"].as_str().unwrap_or(""),
                &path,
                "download",
                result["checksum"].as_str().unwrap_or(""),
                result["sidecar"].as_str(),
                now_ms(),
            )?;
        }
        Ok(())
    }

    fn apply_item_outcome(&self, batch_id: &str, index: usize, outcome: Result<(bool, String)>) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(item) = state.batch.as_mut().filter(|b| b.id == batch_id).and_then(|b| b.items.get_mut(index)) {
                match outcome {
                    Ok((done, text)) => {
                        item.done = done;
                        item.state = text;
                    }
                    Err(text) => {
                        item.done = false;
                        item.state = text;
                    }
                }
            }
        }
    }

    fn finish_batch(&self, batch_id: &str) {
        let (job, complete, total) = {
            let Ok(mut state) = self.state.lock() else { return };
            let Some(batch) = state.batch.as_mut().filter(|b| b.id == batch_id) else { return };
            batch.running = false;
            for item in batch.items.iter_mut().filter(|i| !i.attempted && !i.done) {
                item.state = "Not started; the batch was stopped".into();
            }
            let (job, complete, total) = (batch.job.clone(), batch.items.iter().filter(|i| i.done).count(), batch.items.len());
            batch.status = format!("{complete} of {total} verified");
            (job, complete, total)
        };
        if let Ok(mut state) = self.state.lock() {
            state.cancels.remove(&job);
        }
        self.progress_end(&job);
        self.render_transfer(batch_id);
        let level = if complete == total { "success" } else { "warning" };
        self.toast(level, &format!("Immich transfer: {complete} of {total} verified"));
    }

    fn retry_batch(self: &Arc<Self>, batch_id: &str) {
        let restart = {
            let Ok(mut state) = self.state.lock() else { return };
            let Some(batch) = state.batch.as_mut().filter(|b| b.id == batch_id) else { return };
            if batch.running {
                return;
            }
            for item in batch.items.iter_mut().filter(|i| !i.done) {
                item.attempted = false;
                item.state = "Queued for retry".into();
            }
            batch.running = true;
            batch.stop.store(false, Ordering::SeqCst);
            batch.stop.clone()
        };
        let job = self.progress_start("Transferring originals");
        if let Ok(mut state) = self.state.lock() {
            register_cancel(&mut state, &job, restart);
            if let Some(batch) = state.batch.as_mut().filter(|b| b.id == batch_id) {
                batch.job = job;
            }
        }
        self.spawn_transfer_thread(batch_id.to_owned());
    }

    fn stop_batch(&self, batch_id: &str) {
        if let Ok(state) = self.state.lock() {
            if let Some(batch) = state.batch.as_ref().filter(|b| b.id == batch_id) {
                batch.stop.store(true, Ordering::SeqCst);
            }
        }
        self.render_transfer(batch_id);
    }

    // ----- tags ----------------------------------------------------------------------------------------

    /// Opens the tag picker for remote references. The ids must belong to the connected account.
    fn open_tag_picker(self: &Arc<Self>, ids: Vec<String>) -> Result<Value> {
        let account = self.session_client()?.0.account_key;
        if ids.is_empty() || ids.len() > TAG_LIMIT {
            return Err(format!("Choose between one and {TAG_LIMIT} Immich images."));
        }
        let mut checked = Vec::new();
        for id in &ids {
            api::valid_id(id)?;
            if self.db.get(&account, id)?.is_none() {
                return Err("An image is not part of the current Immich session.".into());
            }
            if !checked.contains(id) {
                checked.push(id.clone());
            }
        }
        let id = {
            let mut state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
            let id = format!("t{}", state.next_batch);
            state.next_batch += 1;
            state.picker = Some(TagPicker { id: id.clone(), ids: checked, selected: None, status: String::new() });
            id
        };
        let _ = self.call("window.open_modal", json!({"contribution":"tag-picker","instance":format!("{TAGS_PREFIX}{id}"),"props":{"title":"Add Immich tag"}}));
        self.render_tags(&id);
        Ok(json!({"picker": id}))
    }

    fn apply_picked_tag(self: &Arc<Self>, picker_id: &str) {
        let (tag, ids) = {
            let Ok(state) = self.state.lock() else { return };
            let Some(picker) = state.picker.as_ref().filter(|p| p.id == picker_id) else { return };
            (picker.selected.clone(), picker.ids.clone())
        };
        let Some(tag) = tag else { return };
        let picker_id = picker_id.to_owned();
        self.spawn_job("immich-tag", move |app_arc| {
            let result = app_arc.assign_tag(&tag, &ids);
            let status = match &result {
                Ok(0) => "Every selected image already has that tag".to_owned(),
                Ok(count) => format!("Added the tag to {count} images"),
                Err(message) => message.clone(),
            };
            if let Ok(mut state) = app_arc.state.lock() {
                if let Some(picker) = state.picker.as_mut().filter(|p| p.id == picker_id) {
                    picker.status = status.clone();
                }
            }
            app_arc.toast(if result.is_ok() { "success" } else { "error" }, &status);
            app_arc.render_tags(&picker_id);
            app_arc.render_settings();
        });
    }

    /// Assigns an existing tag on Immich and then records it locally. Partial failures are errors.
    fn assign_tag(&self, tag_id: &str, ids: &[String]) -> Result<usize> {
        api::valid_id(tag_id)?;
        let (client, session_id) = self.session_client()?;
        let account = client.account_key.clone();
        let tags = self.db.tags(&account)?;
        let name = tags.iter().find(|(id, _)| id == tag_id).map(|(_, n)| n.clone()).ok_or("Choose an existing Immich tag.")?;
        self.with_auth(&client, Some(session_id), |c| c.add_existing_tag(tag_id, ids))?;
        self.db.add_asset_tag(&account, ids, &name)
    }

    /// Tag write-back from Pigeon: upserts the tag names on Immich and assigns them to each reference.
    /// Every failure is reported (toast + status); nothing is silently treated as success.
    fn write_meta(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let ids = string_list(&params["ids"]);
        let names = clean_names(params["tags"].as_array().into_iter().flatten().filter_map(Value::as_str));
        let (client, session_id) = self.session_client()?;
        let account = client.account_key.clone();
        let remote: Vec<String> = ids.into_iter().filter(|id| api::valid_id(id).is_ok()).collect();
        if remote.is_empty() || names.is_empty() {
            return Ok(json!({"queued": 0}));
        }
        let queued = remote.len();
        self.spawn_job("immich-tags", move |app| {
            for id in &remote {
                if let Err(message) = app.push_tags(&client, session_id, &account, id, &names) {
                    app.toast("error", &format!("Immich tag sync failed: {message}"));
                }
            }
        });
        Ok(json!({"queued": queued}))
    }

    fn push_tags(&self, client: &ImmichClient, session_id: u64, account: &str, asset_id: &str, names: &[String]) -> Result<()> {
        let tags = self.with_auth(client, Some(session_id), |c| c.json(reqwest::Method::PUT, "/tags", Some(&json!({"tags":names}))))?;
        let tags = tags.as_array().ok_or("Invalid Immich tag response")?;
        if names.iter().any(|name| !tags.iter().any(|tag| tag["value"].as_str() == Some(name.as_str()))) {
            return Err("Immich did not create every requested tag".into());
        }
        for tag in tags {
            let tag_id = tag["id"].as_str().unwrap_or("");
            let asset = asset_id.to_owned();
            self.with_auth(client, Some(session_id), |c| c.add_existing_tag(tag_id, std::slice::from_ref(&asset)))?;
        }
        self.db.set_synced_tags(account, asset_id, names)
    }

    // ----- connect, sync ------------------------------------------------------------------------------

    fn session_client(&self) -> Result<(ImmichClient, u64)> {
        let state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
        state
            .session
            .as_ref()
            .map(|s| (s.client.clone(), s.id))
            .ok_or_else(|| "Connect to Immich first (Preferences > Immich).".to_owned())
    }

    fn session_is(&self, id: u64) -> bool {
        self.state.lock().map(|s| s.session.as_ref().is_some_and(|x| x.id == id)).unwrap_or(false)
    }

    /// Runs `operation`; on HTTP 401 re-authenticates with the remembered password (never a session-only one)
    /// and retries once. Account changes during the retry are refused.
    fn with_auth<T>(&self, client: &ImmichClient, session_id: Option<u64>, operation: impl Fn(&ImmichClient) -> Result<T>) -> Result<T> {
        match operation(client) {
            Err(error) if error.contains("HTTP 401") => {
                let fresh = self.reauthenticate(session_id)?;
                if fresh.account_key != client.account_key {
                    return Err("Immich account changed during the transfer".into());
                }
                operation(&fresh)
            }
            result => result,
        }
    }

    fn reauthenticate(&self, session_id: Option<u64>) -> Result<ImmichClient> {
        let config = {
            let state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
            if let Some(expected) = session_id {
                if state.session.as_ref().map(|s| s.id) != Some(expected) {
                    return Err("Immich account or session changed; reconnect before retrying".into());
                }
            }
            state.config.clone()
        };
        if config.session_only {
            return Err("Immich session expired (HTTP 401). Enter your password in Preferences to reconnect; session-only passwords are not stored.".into());
        }
        let key = config.secret_id.clone().ok_or("Immich session expired (HTTP 401); reconnect in Preferences")?;
        let password = config::read_password(&self.ctx, &key)?;
        let client = ImmichClient::login(&config.server_url, &config.email, &password)?;
        if let Ok(mut state) = self.state.lock() {
            if let Some(session) = state.session.as_mut().filter(|s| session_id.is_none_or(|id| s.id == id)) {
                session.client = client.clone();
            }
        }
        Ok(client)
    }

    fn connect_saved(self: &Arc<Self>) -> Result<()> {
        let config = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config.clone();
        let key = config.secret_id.clone().ok_or("Enter the Immich password to reconnect this account")?;
        let password = config::read_password(&self.ctx, &key)?;
        let client = ImmichClient::login(&config.server_url, &config.email, &password)?;
        self.install_session(client, true)
    }

    /// Fetches the account's catalogue and makes it current. `new_session` bumps the session id (connect) so
    /// previews of an older session are refused; a plain sync keeps the id.
    fn install_session(self: &Arc<Self>, client: ImmichClient, new_session: bool) -> Result<()> {
        self.refresh(client, new_session, false).map(|_| ())
    }

    /// Full sync of the connected account (the Preferences "Sync now" button).
    pub fn sync_full(self: &Arc<Self>) -> Result<String> {
        let (client, _) = self.session_client()?;
        let outcome = self.refresh(client, false, false)?;
        Ok(format!("Immich synchronized: {} images; originals stay on the server", outcome["count"]))
    }

    /// Re-reads the catalogue and evicts this account's cached thumbnails (the root "Sync Immich" action).
    fn sync_collection(self: &Arc<Self>) -> Result<String> {
        let (client, _) = self.session_client()?;
        let outcome = self.refresh(client, false, true)?;
        Ok(format!("Immich synced: {} images; thumbnails refresh on demand", outcome["count"]))
    }

    /// The one place that talks to the server for catalogue data. Network first, then one short transaction.
    fn refresh(self: &Arc<Self>, client: ImmichClient, new_session: bool, refresh_thumbs: bool) -> Result<Value> {
        let account = client.account_key.clone();
        let generation = self.db.generation(&account)?;
        let (seq, cancel) = {
            let state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
            (state.session_seq, Arc::new(AtomicBool::new(false)))
        };
        let job = self.progress_start("Syncing Immich metadata");
        if let Ok(mut state) = self.state.lock() {
            register_cancel(&mut state, &job, cancel.clone());
        }
        let fetched = (|| -> Result<(Vec<Value>, Vec<Value>)> {
            let progress = |n: usize| self.progress_update(&job, n as usize, 0, "Fetching Immich metadata");
            let cancelled = || cancel.load(Ordering::SeqCst);
            let assets = self.with_auth(&client, None, |c| c.list_images(&cancelled, &progress))?;
            let tags = self.with_auth(&client, None, |c| c.list_tags())?;
            Ok((assets, tags))
        })();
        let outcome = fetched.and_then(|(assets, tags)| {
            let applied = self.db.apply_snapshot(
                &account,
                generation,
                &assets,
                &tags,
                &|| {
                    let Ok(state) = self.state.lock() else { return false };
                    state.session_seq == seq && !cancel.load(Ordering::SeqCst)
                },
                now_ms(),
            )?;
            let applied = applied.ok_or("Immich sync was cancelled or superseded; nothing was changed")?;
            if refresh_thumbs {
                let ids = self.db.ids(&account)?;
                self.thumbs.invalidate(&account, &ids)?;
            }
            Ok(json!({"count": applied.count, "missing": applied.missing}))
        });
        if let Ok(mut state) = self.state.lock() {
            state.cancels.remove(&job);
        }
        self.progress_end(&job);
        let result = outcome?;
        if let Ok(mut state) = self.state.lock() {
            if new_session {
                state.session_seq += 1;
                let id = state.session_seq;
                state.session = Some(Session { client, id });
            } else if let Some(session) = state.session.as_mut().filter(|s| s.client.account_key == account) {
                session.client = client;
            }
        }
        self.render_status();
        self.render_settings();
        Ok(result)
    }

    /// Preferences "Connect / save": validates, optionally remembers the password, then connects and syncs.
    fn configure(self: &Arc<Self>, input: Draft) -> Result<String> {
        if !input.enabled {
            let mut config = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config.clone();
            config.enabled = false;
            config::save(&self.data_dir, &config)?;
            let mut state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
            state.config = config;
            state.session = None;
            state.session_seq += 1;
            return Ok("Immich disabled".into());
        }
        let url = api::normalize_server_url(&input.server)?;
        let email = input.email.trim().to_owned();
        if email.is_empty() {
            return Err("An Immich email is required".into());
        }
        let key = config::secret_key(&url, &email)?;
        let supplied = input.password.clone();
        let password = if supplied.is_empty() {
            let config = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config.clone();
            if config.secret_id.as_deref() != Some(key.as_str()) {
                return Err("Enter a password for this Immich account".into());
            }
            config::read_password(&self.ctx, &key)?
        } else {
            supplied.clone()
        };
        let client = ImmichClient::login(&url, &email, &password)?;
        if !input.remember {
            // Disable a previously remembered connection without deleting its keychain entry: otherwise a
            // restart silently reconnects with an older password.
            let previous = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config.clone();
            let history = previous.secret_history.clone();
            if previous.enabled && !previous.session_only {
                let mut disabled = previous;
                disabled.enabled = false;
                config::save(&self.data_dir, &disabled)?;
            }
            let session_only = Config { enabled: true, server_url: url, email, secret_id: None, session_only: true, secret_history: history };
            let mut state = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?;
            state.config = session_only;
        } else {
            let mut history = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config.secret_history.clone();
            if !supplied.is_empty() {
                config::store_password(&self.ctx, &key, &password)?;
                if !history.contains(&key) {
                    history.push(key.clone());
                }
            }
            let config = Config { enabled: true, server_url: url, email, secret_id: Some(key), session_only: false, secret_history: history };
            config::save(&self.data_dir, &config)?;
            self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config = config;
        }
        self.install_session(client, true)?;
        Ok("Immich connected; thumbnail references synchronized".into())
    }

    /// "Test connection": logs in and reads the tag list. Nothing is saved and the connection is not changed.
    fn test_connection(&self, input: &Draft) -> Result<String> {
        let url = api::normalize_server_url(&input.server)?;
        let email = input.email.trim().to_owned();
        let password = if input.password.is_empty() {
            let config = self.state.lock().map_err(|_| "Immich state unavailable".to_owned())?.config.clone();
            let key = config::secret_key(&url, &email)?;
            if config.secret_id.as_deref() != Some(key.as_str()) {
                return Err("Enter a password for this Immich account".into());
            }
            config::read_password(&self.ctx, &key)?
        } else {
            input.password.clone()
        };
        let client = ImmichClient::login(&url, &email, &password)?;
        let tags = client.list_tags()?;
        Ok(format!("Connection works: {} tags on the server. Nothing was saved.", tags.len()))
    }

    /// Forgets the session. Keychain entries are kept (use Preferences to forget a password).
    fn disconnect(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.session = None;
            state.session_seq += 1;
            state.draft.password.clear();
            state.status = "Immich disconnected; previews revoked".into();
        }
        self.render_settings();
        self.render_status();
    }

    // ----- events and commands -------------------------------------------------------------------------

    fn event(self: &Arc<Self>, instance: &str, node: &str, event: NodeEvent) {
        let (kind, value, rev) = match event {
            NodeEvent::Click => ("click", Value::Null, 0),
            NodeEvent::Change { value, rev } => ("change", value, rev),
            NodeEvent::Submit { value, rev } => ("submit", value, rev),
            NodeEvent::Blur { value, rev } => ("blur", value, rev),
            _ => return,
        };
        let instance = instance.to_owned();
        let node = node.to_owned();
        let kind = kind.to_owned();
        if instance == SETTINGS {
            self.settings_event(&node, &kind, value, rev);
        } else if let Some(batch) = instance.strip_prefix(TRANSFER_PREFIX) {
            match (node.as_str(), kind.as_str()) {
                ("stop", "click") => self.stop_batch(batch),
                ("retry", "click") => {
                    let app = self.clone();
                    let batch = batch.to_owned();
                    let _ = self.spawn_general(move || app.retry_batch(&batch));
                }
                _ => {}
            }
        } else if let Some(picker) = instance.strip_prefix(TAGS_PREFIX) {
            match (node.as_str(), kind.as_str()) {
                ("tag", "change") => {
                    if let Ok(mut state) = self.state.lock() {
                        if let Some(p) = state.picker.as_mut().filter(|p| p.id == picker) {
                            p.selected = value.as_str().map(str::to_owned);
                        }
                    }
                    self.render_tags(picker);
                }
                ("apply", "click") => self.apply_picked_tag(picker),
                _ => {}
            }
        }
    }

    fn settings_event(self: &Arc<Self>, node: &str, kind: &str, value: Value, rev: u64) {
        if kind == "change" {
            if let Ok(mut state) = self.state.lock() {
                // Section 5.1: adopt the host's edit only when it is not older than ours.
                if rev >= state.draft.rev {
                    match node {
                        "server" => state.draft.server = value.as_str().unwrap_or("").to_owned(),
                        "email" => state.draft.email = value.as_str().unwrap_or("").to_owned(),
                        "password" => state.draft.password = value.as_str().unwrap_or("").to_owned(),
                        "enabled" => state.draft.enabled = value.as_bool().unwrap_or(false),
                        "remember" => state.draft.remember = value.as_bool().unwrap_or(false),
                        _ => return,
                    }
                    state.draft.rev = rev;
                }
            }
            return;
        }
        if kind != "click" {
            return;
        }
        let busy = self.state.lock().map(|s| s.busy).unwrap_or(true);
        match node {
            "disconnect" => self.disconnect(),
            "connect" | "test" | "sync" if busy => self.toast("warning", "Immich is busy; wait for the current operation"),
            "connect" => {
                let input = self.state.lock().map(|s| s.draft.clone()).unwrap_or_default();
                self.begin_busy("Connecting to Immich...");
                self.spawn_job("immich-configure", move |app| {
                    let result = app.configure(input.clone());
                    if let Ok(mut state) = app.state.lock() {
                        state.draft.password.clear();
                    }
                    app.finish_busy(result);
                });
            }
            "test" => {
                let input = self.state.lock().map(|s| s.draft.clone()).unwrap_or_default();
                self.begin_busy("Testing the connection...");
                self.spawn_job("immich-test", move |app| {
                    let result = app.test_connection(&input);
                    app.finish_busy(result);
                });
            }
            "sync" => {
                self.begin_busy("Syncing Immich metadata...");
                self.spawn_job("immich-sync", |app| {
                    let result = app.sync_full();
                    app.finish_busy(result);
                });
            }
            _ => {}
        }
    }

    fn command(self: &Arc<Self>, id: String, context: Value) {
        let ids = context_ids(&context);
        let app = self.clone();
        match id.as_str() {
            "send" => {
                let _ = self.spawn_general(move || app.send_local(ids));
            }
            "download" => {
                self.spawn_job("immich-download-pick", move |app| {
                    let picked = app.call_timeout("dialog.pick_folder", json!({"title":"Download originals to folder"}), DIALOG_TIMEOUT);
                    match picked.ok().and_then(|v| v["path"].as_str().map(PathBuf::from)) {
                        Some(folder) => {
                            if let Err(message) = app.download_batch(ids, folder) {
                                app.toast("error", &message);
                            }
                        }
                        None => {}
                    }
                });
            }
            "tag" => {
                let _ = self.spawn_general(move || {
                    if let Err(message) = app.open_tag_picker(ids) {
                        app.toast("error", &message);
                    }
                });
            }
            "sync" => {
                self.begin_busy("Syncing Immich metadata...");
                self.spawn_job("immich-sync", |app| {
                    let result = app.sync_full();
                    app.finish_busy(result);
                });
            }
            "sync-collection" => {
                self.begin_busy("Syncing Immich metadata...");
                self.spawn_job("immich-sync-collection", |app| {
                    let result = app.sync_collection();
                    app.finish_busy(result);
                });
            }
            _ => {}
        }
    }

    /// "Send to Immich" on local items: reads their public records from the host, then uploads them.
    fn send_local(self: &Arc<Self>, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let records = match self.call("library.get", json!({"ids": ids})) {
            Ok(value) => value["records"].as_array().or_else(|| value["items"].as_array()).or_else(|| value.as_array()).cloned().unwrap_or_default(),
            Err(message) => {
                self.toast("error", &message);
                return;
            }
        };
        let items: Vec<BatchItem> = ids
            .iter()
            .map(|id| {
                let record = records.iter().find(|r| r["id"].as_str() == Some(id.as_str())).cloned();
                match record {
                    None => BatchItem { name: id.clone(), source: ItemSource::Upload { path: PathBuf::new(), record: json!({}) }, state: "Not found in the library".into(), done: false, attempted: true },
                    Some(record) if api::valid_id(id).is_ok() => BatchItem { name: record["name"].as_str().unwrap_or(id).to_owned(), source: ItemSource::Upload { path: PathBuf::new(), record }, state: "This reference already lives on Immich; it was not copied or downloaded".into(), done: false, attempted: true },
                    Some(record) => {
                        let path = PathBuf::from(record["path"].as_str().unwrap_or(""));
                        BatchItem {
                            name: record["name"].as_str().unwrap_or(id).to_owned(),
                            source: ItemSource::Upload { path, record },
                            state: "Queued".into(),
                            done: false,
                            attempted: false,
                        }
                    }
                }
            })
            .collect();
        if let Err(message) = self.start_batch(true, items, None) {
            self.toast("error", &message);
        }
    }

    // ----- rendering -----------------------------------------------------------------------------------

    fn render(&self, instance: &str) {
        if instance == SETTINGS {
            self.render_settings();
        } else if instance == STATUS {
            self.render_status();
        } else if let Some(id) = instance.strip_prefix(TRANSFER_PREFIX) {
            self.render_transfer(id);
        } else if let Some(id) = instance.strip_prefix(TAGS_PREFIX) {
            self.render_tags(id);
        }
    }

    fn connected_counts(&self) -> (u64, u64) {
        let Some(account) = self.account() else { return (0, 0) };
        let images = self.db.tree_counts(&account).map(|c| c.total).unwrap_or(0);
        let tags = self.db.tags(&account).map(|t| t.len() as u64).unwrap_or(0);
        (images, tags)
    }

    pub fn render_settings(&self) {
        let (images, tags) = self.connected_counts();
        let view = {
            let Ok(state) = self.state.lock() else { return };
            ui::PrefsView {
                enabled: state.draft.enabled,
                server: state.draft.server.clone(),
                email: state.draft.email.clone(),
                remember: state.draft.remember,
                rev: state.draft.rev,
                connected: state.session.is_some(),
                images,
                tags,
                status: state.status.clone(),
                busy: state.busy,
            }
        };
        self.set_scene(SETTINGS, ui::prefs(&view));
    }

    pub fn render_status(&self) {
        let (images, _) = self.connected_counts();
        let connected = self.state.lock().map(|s| s.session.is_some()).unwrap_or(false);
        self.set_scene(STATUS, ui::status_item(connected, images));
    }

    fn render_transfer(&self, batch_id: &str) {
        let scene = {
            let Ok(state) = self.state.lock() else { return };
            let Some(batch) = state.batch.as_ref().filter(|b| b.id == batch_id) else { return };
            let heading = if batch.upload { "Copy originals to Immich" } else { "Download originals to local folder" };
            ui::transfer(&ui::TransferView {
                heading: heading.into(),
                upload: batch.upload,
                running: batch.running,
                items: batch
                    .items
                    .iter()
                    .map(|i| ui::ItemView { name: i.name.clone(), state: i.state.clone(), done: i.done })
                    .collect(),
                status: batch.status.clone(),
            })
        };
        self.set_scene(&format!("{TRANSFER_PREFIX}{batch_id}"), scene);
    }

    fn render_tags(&self, picker_id: &str) {
        let account = self.account();
        let tags = account.as_deref().and_then(|a| self.db.tags(a).ok()).unwrap_or_default();
        let scene = {
            let Ok(state) = self.state.lock() else { return };
            let Some(picker) = state.picker.as_ref().filter(|p| p.id == picker_id) else { return };
            ui::tag_picker(&ui::TagView {
                count: picker.ids.len(),
                tags,
                selected: picker.selected.clone(),
                status: picker.status.clone(),
            })
        };
        self.set_scene(&format!("{TAGS_PREFIX}{picker_id}"), scene);
    }

    fn toast(&self, level: &str, text: &str) {
        let _ = self.ctx.toast(level, text);
    }

    // ----- progress jobs -------------------------------------------------------------------------------

    /// Starts a footer job and returns the host's job id (cancel requests name it).
    fn progress_start(&self, title: &str) -> String {
        self.ctx.progress_start(title, true).unwrap_or_default()
    }

    fn progress_update(&self, job: &str, done: usize, total: usize, message: &str) {
        let value = if total > 0 { Some(done as f64 / total as f64) } else { None };
        let _ = self.ctx.progress_update(job, value, Some(message));
    }

    fn progress_end(&self, job: &str) {
        let _ = self.ctx.progress_end(job, true, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_deduplicated() {
        assert_eq!(clean_names([" travel ", "", "travel", "family"]), vec!["travel".to_owned(), "family".to_owned()]);
    }

    #[test]
    fn context_accepts_either_ids_or_selection() {
        assert_eq!(context_ids(&json!({"ids":["a"]})), vec!["a".to_owned()]);
        assert_eq!(context_ids(&json!({"selection":["b"]})), vec!["b".to_owned()]);
        assert!(context_ids(&json!({})).is_empty());
    }

    #[test]
    fn draft_is_loaded_from_config_without_a_password() {
        let state = State::new(Config { enabled: true, server_url: "https://x.test".into(), email: "a@x.test".into(), secret_id: Some("k".into()), session_only: false, secret_history: vec![] });
        assert!(state.draft.password.is_empty());
        assert!(state.draft.remember);
        assert_eq!(state.draft.server, "https://x.test");
    }
}
