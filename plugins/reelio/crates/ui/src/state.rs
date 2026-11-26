//! Application state and reducer. Every user action and engine reply enters through
//! [`State::dispatch`]; effects (engine requests, persistence) leave through [`State::drain`].
//! The reducer is synchronous and never waits on I/O, so the host UI thread stays free.

use crate::chapters::{chapter_ends, parse_chapter_time};
use crate::engine::{self, Backend, Job, Plan, Reply, Request, RuntimeDependencies, Settings};
use crate::geometry::{
    self, clip_bounds, clip_to_video, offset_after_drag, reference_video_end, source_duration,
    timeline_duration, timeline_viewport_duration, trim_after_drag, video_timeline_end, Edge, View,
    MAX_OFFSET,
};
use crate::layout::{self, WorkspaceLayout};
use crate::model::{
    self, add_asset, asset_tracks, basename, empty_project, is_image_asset, Artwork, Asset,
    AudioRole, Chapter, Container, Kind, Project, Track, TrackPatch,
};
use crate::playback::{frame_target, preview_frame_ms, SeekScheduler, PLAYBACK_SPEEDS};
use crate::store::ProjectStore;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};

/// Notices hide after 5 s; errors after 8 s. Hover or focus pauses the countdown.
pub const NOTICE_MS: f64 = 5000.0;
pub const ERROR_MS: f64 = 8000.0;

pub const LANGUAGES: [&str; 14] = [
    "und", "eng", "spa", "fra", "deu", "ita", "por", "jpn", "kor", "zho", "ara", "hin", "rus",
    "nld",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Timeline,
    Media,
    Tracks,
    Subtitles,
    Audio,
    Metadata,
    Artwork,
    Chapters,
    Export,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::Timeline,
        Section::Media,
        Section::Tracks,
        Section::Subtitles,
        Section::Audio,
        Section::Metadata,
        Section::Artwork,
        Section::Chapters,
        Section::Export,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Section::Timeline => "timeline",
            Section::Media => "media",
            Section::Tracks => "tracks",
            Section::Subtitles => "subtitles",
            Section::Audio => "audio",
            Section::Metadata => "metadata",
            Section::Artwork => "artwork",
            Section::Chapters => "chapters",
            Section::Export => "export",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Timeline => "Timeline",
            Section::Media => "Media information",
            Section::Tracks => "All tracks",
            Section::Subtitles => "Subtitle Studio",
            Section::Audio => "Audio",
            Section::Metadata => "Metadata",
            Section::Artwork => "Poster & cover",
            Section::Chapters => "Chapters",
            Section::Export => "Export & history",
        }
    }

    pub fn from_id(id: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// Workspace event from the host. `node` is the scene node id; `value` the control value.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Ui {
        node: String,
        value: Value,
    },
    Engine {
        id: u64,
        reply: Reply,
    },
    /// Notification from the engine event channel (`{"event": "...", ...}`).
    EngineEvent(Value),
    /// Playback clock reported by the frame pump, in source milliseconds.
    PlaybackClock(f64),
    /// Host clock tick (ms, monotonic). Drives toasts, polling, debounced validation, seeks.
    Tick {
        now: f64,
    },
    Command {
        id: String,
    },
    Drop {
        paths: Vec<String>,
    },
    CoverDrop {
        paths: Vec<String>,
    },
    Quit,
}

/// Work for the host to perform. Engine requests run on a worker, never on the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Engine {
        id: u64,
        request: Request,
    },
    Persist {
        key: String,
        value: String,
    },
    /// Mirror a notification to the Pigeon host toast service (`notify.toast`).
    HostToast {
        level: String,
        text: String,
    },
    /// Ask the host to close the plugin window (after the confirmation flow).
    CloseWindow,
    ConsentDecision(bool),
    /// Set the preview element source/timeline position (host applies it to the media element).
    Seek {
        ms: f64,
        fast: bool,
    },
    /// Start (`playing`) or stop the preview clock and its audio at `from_ms` with `speed`/`volume`.
    Transport {
        playing: bool,
        from_ms: f64,
        speed: f64,
        volume: f64,
    },
    /// A new preview source is ready: decode frames of `url` at this pixel size.
    LoadPreview {
        url: String,
        width: u32,
        height: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub remaining: f64,
    pub paused: bool,
    pub hovering: bool,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    ChooseMedia,
    Dropped,
    Probe {
        path: String,
    },
    Save,
    Validate {
        revision: u64,
    },
    ExportStart,
    Jobs,
    PreviewSource {
        path: String,
    },
    SubtitleTest {
        track: String,
        revision: u64,
    },
    Dependencies,
    Repair,
    ExportValidate {
        revision: u64,
    },
    RestoreJob,
    Settings,
    Backend,
    ArtworkImport {
        apply: bool,
    },
    CoverAuthorize,
    SubtitleOpen,
    SubtitleSave(crate::subtitles::SrtDocument),
    Studio(crate::studio_workflow::StudioRequest),
    TestSubtitleProbe {
        path: String,
    },
    Download {
        project_id: String,
    },
    QuitRecovery,
    UpdateCheck,
    SettingsFolder,
    PosterSearch {
        version: u64,
    },
    PosterImage,
    ArtworkEdit {
        generation: u64,
        revision: u64,
    },
    CoverChoose,
    CoverOffer,
    CoverSave,
    /// Fire-and-forget request (its reply is ignored).
    Silent,
    OpenProject,
    Other,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PreviewState {
    pub asset: Option<String>,
    pub url: Option<String>,
    pub error: Option<String>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleTest {
    pub project_id: String,
    pub track_id: String,
    pub source_path: String,
    pub cues: Vec<crate::subtitles::SubtitleCue>,
}

/// Modal states that the host shows as dialogs (`modal` slot instances).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dialogs {
    pub settings: bool,
    pub dependencies: bool,
    pub about: bool,
    pub quit_confirm: bool,
    pub export_review: Option<Vec<String>>,
    pub export_revision: u64,
    pub engine_consent: Option<(u64, String, String)>,
    pub restore_confirm: Option<String>,
}

pub struct State {
    pub store: ProjectStore,
    /// In-progress pointer gesture on a timeline lane (see `timeline_input`).
    pub gesture: Option<crate::timeline_input::Gesture>,
    /// Open context or project menu (see `menus`).
    pub menu: Option<crate::menus::Menu>,
    /// Subtitle Studio document and history (see `studio`).
    pub studio: Option<crate::studio::Studio>,
    /// Poster & cover studio (see `poster`).
    pub poster: Option<crate::poster::PosterState>,
    /// Live feedback for the gesture in progress; never part of the undo history.
    pub drafts: crate::timeline_input::Drafts,
    pub project_rev: u64,
    pub section: Section,
    pub selected: Option<String>,
    pub playhead: f64,
    pub media_time: f64,
    pub playing: bool,
    pub speed_index: usize,
    pub volume: f64,
    pub plan: Option<Plan>,
    pub plan_due: Option<f64>,
    pub backend: Option<Backend>,
    pub deps: Option<RuntimeDependencies>,
    pub deps_checking: bool,
    pub repair_message: Option<String>,
    pub settings: Settings,
    pub jobs: Vec<Job>,
    pub notifying_export: Option<String>,
    pub busy: Option<String>,
    pub notice: Option<Toast>,
    pub error: Option<Toast>,
    pub dialogs: Dialogs,
    pub layout: WorkspaceLayout,
    pub pool_width: f64,
    pub inspector_width: f64,
    pub viewer_height: Option<f64>,
    pub timeline_view: View,
    pub reveal: Option<(f64, u64)>,
    pub preview: PreviewState,
    pub subtitle_test: Option<SubtitleTest>,
    pub cover_asset: Option<String>,
    pub incoming_artwork: Option<Artwork>,
    pub resources: VecDeque<engine::PendingResource>,
    pub seek: SeekScheduler,
    pub import_queue: VecDeque<String>,
    pub window_width: f64,
    pub now: f64,
    pub last_jobs_poll: f64,
    next_request: u64,
    pending: BTreeMap<u64, Pending>,
    outbox: Vec<Effect>,
}

fn is_video_stream(s: &model::Stream) -> bool {
    s.kind == Kind::Video && !s.attached_pic()
}

impl State {
    pub fn new() -> Self {
        let mut s = State {
            store: ProjectStore::new(empty_project()),
            gesture: None,
            menu: None,
            studio: None,
            poster: None,
            drafts: Default::default(),
            project_rev: 0,
            section: Section::Timeline,
            selected: None,
            playhead: 0.0,
            media_time: 0.0,
            playing: false,
            speed_index: 4,
            volume: 0.05,
            plan: None,
            plan_due: None,
            backend: None,
            deps: None,
            deps_checking: false,
            repair_message: None,
            settings: Settings::default(),
            jobs: vec![],
            notifying_export: None,
            busy: None,
            notice: None,
            error: None,
            dialogs: Dialogs::default(),
            layout: WorkspaceLayout::default(),
            pool_width: 280.0,
            inspector_width: 440.0,
            viewer_height: None,
            timeline_view: View::default(),
            reveal: None,
            preview: PreviewState::default(),
            subtitle_test: None,
            cover_asset: None,
            incoming_artwork: None,
            resources: VecDeque::new(),
            seek: SeekScheduler::new(),
            import_queue: VecDeque::new(),
            window_width: 1440.0,
            now: 0.0,
            last_jobs_poll: f64::NEG_INFINITY,
            next_request: 1,
            pending: BTreeMap::new(),
            outbox: vec![],
        };
        s.start();
        s
    }

    /// Startup requests: backend, settings, dependency check.
    fn start(&mut self) {
        self.request(Request::BackendStatus, Pending::Backend);
        self.request(Request::SettingsGet, Pending::Settings);
        self.check_dependencies();
    }

    pub fn project(&self) -> &Project {
        &self.store.project
    }

    /// Drain the effects produced since the last call.
    pub fn drain(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.outbox)
    }

    /// Queue an engine request with its pending meaning (used by the studio and menus).
    pub fn engine_request_with(&mut self, request: Request, pending: Pending) -> u64 {
        self.request(request, pending)
    }

    fn request(&mut self, request: Request, pending: Pending) -> u64 {
        let id = self.next_request;
        self.next_request += 1;
        self.pending.insert(id, pending);
        self.outbox.push(Effect::Engine { id, request });
        id
    }

    fn persist(&mut self, key: &str, value: String) {
        self.outbox.push(Effect::Persist {
            key: key.into(),
            value,
        });
    }

    /// Central mutation path: edits are recorded, and a validation pass is scheduled (180 ms).
    pub fn edit(&mut self, f: impl FnOnce(&mut Project)) -> bool {
        let changed = self.store.edit(f);
        if changed {
            self.project_rev += 1;
            self.plan_due = Some(self.now + 180.0);
        }
        changed
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            self.error = Some(Toast {
                text,
                error: true,
                remaining: ERROR_MS,
                paused: false,
                hovering: false,
                focused: false,
            });
        } else {
            self.notice = Some(Toast {
                text,
                error: false,
                remaining: NOTICE_MS,
                paused: false,
                hovering: false,
                focused: false,
            });
        }
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.toast(text, true);
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<String>) {
        self.toast(text, false);
    }

    pub fn source_asset(&self) -> Option<&Asset> {
        let p = self.project();
        let previews = self.preview_sources();
        self.preview
            .asset
            .as_ref()
            .and_then(|id| previews.iter().find(|a| &a.id == id).copied())
            .or_else(|| {
                previews
                    .iter()
                    .find(|a| a.streams.iter().any(is_video_stream))
                    .copied()
            })
            .or_else(|| previews.first().copied())
            .or_else(|| p.assets.iter().find(|a| !is_image_asset(Some(a))))
    }

    /// Assets that can be played in the viewer: non-image with real video or audio.
    pub fn preview_sources(&self) -> Vec<&Asset> {
        self.project()
            .assets
            .iter()
            .filter(|a| {
                !is_image_asset(Some(a))
                    && a.streams
                        .iter()
                        .any(|s| is_video_stream(s) || s.kind == Kind::Audio)
            })
            .collect()
    }

    pub fn dependencies_ready(&self) -> bool {
        self.deps
            .as_ref()
            .map(|d| d.preview_available)
            .unwrap_or(false)
    }

    fn check_dependencies(&mut self) {
        if self.deps_checking {
            return;
        }
        self.deps_checking = true;
        self.request(Request::DependenciesStatus, Pending::Dependencies);
    }

    fn video_offset(&self) -> i64 {
        let p = self.project();
        let Some(asset) = self.source_asset() else {
            return 0;
        };
        p.tracks
            .iter()
            .find(|t| {
                t.asset_id == asset.id
                    && p.track_stream(t)
                        .map(|s| s.kind == Kind::Video)
                        .unwrap_or(false)
            })
            .map(|t| t.offset_ms)
            .unwrap_or(0)
    }

    /// Timeline duration of the currently previewed source.
    pub fn source_duration_ms(&self) -> f64 {
        self.source_asset()
            .and_then(|a| a.duration_ms)
            .map(|d| d as f64)
            .unwrap_or(0.0)
    }

    /// Dispatch one host event.
    pub fn dispatch(&mut self, event: Event) {
        match event {
            Event::Tick { now } => self.tick(now),
            Event::Engine { id, reply } => self.engine_reply(id, reply),
            Event::EngineEvent(value) => self.engine_event(&value),
            Event::PlaybackClock(ms) => self.playback_clock(ms),
            Event::Command { id } => self.command(&id),
            Event::Drop { paths } => {
                // The engine authorizes dropped files before any probe; imports follow the reply.
                self.request(Request::MediaDropped { paths }, Pending::Dropped);
            }
            Event::CoverDrop { paths } => {
                if self.busy.is_some() {
                    return;
                }
                if paths.len() != 1 {
                    self.set_error("Drop a single still image onto the cover preview.");
                    return;
                }
                self.busy = Some("Validating cover image".into());
                self.request(Request::MediaDropped { paths }, Pending::CoverAuthorize);
            }
            Event::Quit => self.quit(),
            Event::Ui { node, value } => self.ui(&node, value),
        }
    }

    fn tick(&mut self, now: f64) {
        let elapsed = (now - self.now).max(0.0);
        self.now = now;
        for toast in [self.notice.as_mut(), self.error.as_mut()]
            .into_iter()
            .flatten()
        {
            if !toast.paused {
                toast.remaining -= elapsed;
            }
        }
        if self
            .notice
            .as_ref()
            .map(|t| t.remaining <= 0.0)
            .unwrap_or(false)
        {
            self.notice = None;
        }
        if self
            .error
            .as_ref()
            .map(|t| t.remaining <= 0.0)
            .unwrap_or(false)
        {
            self.error = None;
        }
        if let Some(due) = self.plan_due {
            if now >= due {
                self.plan_due = None;
                self.validate_silently();
            }
        }
        for write in self.seek.tick(now) {
            self.outbox.push(Effect::Seek {
                ms: write.time,
                fast: write.fast,
            });
        }
        self.studio_tick();
        self.cover_tick();
        if now - self.last_jobs_poll >= 1000.0 {
            self.last_jobs_poll = now;
            self.request(Request::JobsList, Pending::Jobs);
        }
    }

    fn validate_silently(&mut self) {
        let revision = self.project_rev;
        self.plan = None;
        let project = serde_json::to_value(self.project()).unwrap_or(Value::Null);
        self.request(
            Request::ProjectValidate { project },
            Pending::Validate { revision },
        );
    }

    fn engine_reply(&mut self, id: u64, reply: Reply) {
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        match pending {
            Pending::Backend => match reply.and_then(engine::decode::<Backend>) {
                Ok(b) => self.backend = Some(b),
                Err(e) => self.set_error(e),
            },
            Pending::Settings => match reply.and_then(engine::decode::<Settings>) {
                Ok(s) => {
                    self.settings = s;
                }
                Err(e) => self.set_error(e),
            },
            Pending::Dependencies => {
                self.deps_checking = false;
                self.deps =
                    Some(
                        reply
                            .and_then(engine::decode)
                            .unwrap_or_else(|e| RuntimeDependencies {
                                missing_components: vec![format!("Dependency check failed: {e}")],
                                repair_hint:
                                    "Preview stays disabled until a successful component check."
                                        .into(),
                                ..Default::default()
                            }),
                    );
                self.refresh_preview();
            }
            Pending::Validate { revision } => {
                if revision == self.project_rev {
                    match reply.and_then(engine::decode::<Plan>) {
                        Ok(plan) => self.plan = Some(plan),
                        Err(e) => self.set_error(e),
                    }
                }
            }
            Pending::Repair => {
                self.busy = None;
                match reply.and_then(engine::decode::<RuntimeDependencies>) {
                    Ok(report) => {
                        self.deps = Some(report);
                        self.refresh_preview();
                    }
                    Err(e) => {
                        self.repair_message = Some(e.clone());
                        self.set_error(e);
                    }
                }
            }
            Pending::ExportValidate { revision } => {
                self.busy = None;
                if revision == self.project_rev {
                    match reply.and_then(engine::decode::<Plan>) {
                        Ok(plan) => {
                            self.plan = Some(plan);
                            self.set_notice("Source validation finished.");
                        }
                        Err(e) => self.set_error(e),
                    }
                }
            }
            Pending::RestoreJob => {
                self.busy = None;
                match reply.and_then(engine::decode::<Project>) {
                    Ok(p) => {
                        self.activate_project(p);
                        self.refresh_preview();
                        self.set_notice("Job settings restored. Relink any missing sources and choose a new output destination.");
                    }
                    Err(e) => self.set_error(e),
                }
            }
            Pending::Jobs => {
                if let Ok(jobs) = reply.and_then(engine::decode::<Vec<Job>>) {
                    self.merge_jobs(jobs);
                }
            }
            Pending::ChooseMedia => {
                self.busy = None;
                match reply.and_then(engine::decode::<Vec<String>>) {
                    Ok(paths) if !paths.is_empty() => self.import_paths(paths),
                    Ok(_) => {}
                    Err(e) => self.set_error(e),
                }
            }
            Pending::Dropped => match reply.and_then(engine::decode::<Vec<String>>) {
                Ok(paths) if !paths.is_empty() => self.import_paths(paths),
                Ok(_) => {}
                Err(e) => self.set_error(e),
            },
            Pending::Probe { path } => {
                let result = reply.and_then(engine::decode::<Asset>);
                self.finish_probe(path, result);
            }
            Pending::CoverAuthorize => match reply.and_then(engine::decode::<Vec<String>>) {
                Ok(paths) if paths.len() == 1 => self.outbox_cover_import(paths[0].clone(), true),
                Ok(_) => {
                    self.busy = None;
                    self.set_error("The dropped cover could not be opened.");
                }
                Err(e) => {
                    self.busy = None;
                    self.set_error(e);
                }
            },
            Pending::ArtworkImport { apply } => {
                self.busy = None;
                match reply.and_then(|v| {
                    let art: Artwork =
                        engine::decode(v.get("artwork").cloned().unwrap_or(Value::Null))?;
                    let asset: Asset =
                        engine::decode(v.get("asset").cloned().unwrap_or(Value::Null))?;
                    Ok((asset, art))
                }) {
                    Ok((asset, art)) if apply => self.apply_cover(asset, art),
                    Ok((_, art)) => self.load_cover_editor(art),
                    Err(e) => self.set_error(e),
                }
            }
            Pending::Save => {
                self.busy = None;
                match reply {
                    Ok(Value::Null) => {}
                    Ok(v) => {
                        let name = v.as_str().map(basename).unwrap_or_default();
                        let snapshot = self.project().clone();
                        self.store.mark_saved(snapshot);
                        self.set_notice(format!("Project saved: {name}"));
                    }
                    Err(e) => self.set_error(e),
                }
            }
            Pending::OpenProject => {
                self.busy = None;
                match reply.and_then(|v| {
                    if v.is_null() {
                        Ok(None)
                    } else {
                        engine::decode::<Project>(v).map(Some)
                    }
                }) {
                    Ok(Some(project)) => {
                        self.activate_project(project);
                        self.set_notice("Project opened. Sources are checked again before export.");
                    }
                    Ok(None) => {}
                    Err(e) => self.set_error(e),
                }
            }
            Pending::ExportStart => {
                self.busy = None;
                match reply {
                    Ok(Value::String(job)) => {
                        self.notifying_export = Some(job);
                        self.last_jobs_poll = f64::NEG_INFINITY;
                        self.set_notice("Export started. Sources are unchanged; the destination appears only after verification.");
                    }
                    Ok(_) => {}
                    Err(e) => self.set_error(e),
                }
            }
            Pending::PreviewSource { path } => {
                if self.preview.asset.is_some()
                    && self.source_asset().map(|a| a.path == path).unwrap_or(false)
                {
                    match reply.and_then(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "bad url".to_string())
                    }) {
                        Ok(url) => {
                            self.preview.url = Some(url.clone());
                            self.preview.error = None;
                            let (width, height) = self.frame_size();
                            self.outbox.push(Effect::LoadPreview { url, width, height });
                        }
                        Err(e) => {
                            self.preview.error =
                                Some(format!("Could not prepare source playback: {e}"));
                        }
                    }
                }
            }
            Pending::SubtitleTest { track, revision } => {
                self.busy = None;
                if revision != self.project_rev {
                    return;
                }
                match reply.and_then(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "bad subtitle preview".to_string())
                }) {
                    Ok(text) => match crate::subtitles::inspect_vtt(&text) {
                        Ok(cues) => {
                            let source_path = self
                                .source_asset()
                                .map(|a| a.path.clone())
                                .unwrap_or_default();
                            self.subtitle_test = Some(SubtitleTest {
                                project_id: self.project().id.clone(),
                                track_id: track,
                                source_path,
                                cues,
                            });
                            self.set_notice("Subtitle preview loaded. Play the source video and review synchronization; no source file was changed.");
                        }
                        Err(e) => self.set_error(e),
                    },
                    Err(e) => self.set_error(e),
                }
            }
            Pending::SubtitleOpen | Pending::SubtitleSave(_) => {
                self.studio_reply(&pending, &reply);
            }
            Pending::Studio(workflow) => self.studio_workflow_reply(workflow, reply),
            Pending::TestSubtitleProbe { path } => {
                self.busy = None;
                match reply.and_then(engine::decode::<Asset>) {
                    Ok(asset)
                        if asset
                            .streams
                            .iter()
                            .any(|stream| stream.kind == Kind::Subtitle) =>
                    {
                        let id = asset.id.clone();
                        self.finish_probe(path, Ok(asset));
                        let track = self
                            .project()
                            .tracks
                            .iter()
                            .find(|t| {
                                t.asset_id == id
                                    && self.project().track_kind(t) == Some(Kind::Subtitle)
                            })
                            .map(|t| t.id.clone());
                        if let Some(track) = track {
                            self.select_track(&track);
                        }
                    }
                    Ok(_) => self.set_error("No text subtitle stream was found in that file."),
                    Err(e) => self.set_error(e),
                }
            }
            Pending::Download { project_id } => {
                self.busy = None;
                if self.project().id != project_id {
                    return;
                }
                match reply {
                    Ok(v) if v["kind"] == "subtitles" => {
                        self.studio_add_candidates(&v["files"]);
                        self.set_notice(
                            "Subtitle download validated. Choose Add & test subtitle to use it.",
                        );
                    }
                    Ok(v) if v["kind"] == "artwork" => match engine::decode(v["artwork"].clone()) {
                        Ok(artwork) => self.load_cover_editor(artwork),
                        Err(e) => self.set_error(e),
                    },
                    Ok(_) => self.set_error("Unrecognized download result."),
                    Err(e) => self.set_error(e),
                }
            }
            Pending::QuitRecovery => {
                self.busy = None;
                match reply {
                    Ok(_) => self.outbox.push(Effect::CloseWindow),
                    Err(e) => self.set_error(format!("Could not save subtitle recovery: {e}")),
                }
            }
            Pending::PosterSearch { .. }
            | Pending::PosterImage
            | Pending::ArtworkEdit { .. }
            | Pending::CoverChoose
            | Pending::CoverOffer
            | Pending::CoverSave => {
                self.poster_reply(&pending, &reply);
            }
            Pending::UpdateCheck => {
                self.busy = None;
                match reply {
                    Ok(v) => {
                        let text = v
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string());
                        self.set_notice(text);
                    }
                    Err(e) => self.set_error(e.clone()),
                }
            }
            Pending::SettingsFolder => match reply {
                Ok(v) if v.is_string() => {
                    self.settings.default_folder = v.as_str().map(str::to_string);
                    let folder = self.settings.default_folder.clone().unwrap_or_default();
                    self.engine_request_with(
                        Request::SettingsUpdate {
                            key: "default_folder".into(),
                            value: serde_json::json!(folder),
                        },
                        Pending::Silent,
                    );
                }
                Ok(_) => {}
                Err(e) => self.set_error(e.clone()),
            },
            Pending::Silent => {}
            Pending::Other => {
                self.busy = None;
                if let Err(e) = reply {
                    self.set_error(e);
                }
            }
        }
        self.after_jobs_change();
    }

    /// Engine notifications: preview readiness, dependency repair, toasts, resource prompts, close.
    fn engine_event(&mut self, value: &Value) {
        match value.get("event").and_then(Value::as_str).unwrap_or("") {
            "preview:ready" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("");
                let url = value.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                if self.source_asset().map(|a| a.path == path).unwrap_or(false) && !url.is_empty() {
                    self.preview.url = Some(url.clone());
                    self.preview.error = None;
                    let (width, height) = self.frame_size();
                    self.outbox.push(Effect::LoadPreview { url, width, height });
                }
            }
            "dependencies:repair" => {
                let message = value.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                self.repair_message = Some(message.clone());
                match value.get("phase").and_then(Value::as_str) {
                    Some("started") => self.busy = Some(message),
                    Some("finished") => {
                        self.set_notice(if message.is_empty() {
                            "Required components are installed and verified. Preview is ready.".into()
                        } else {
                            message
                        });
                        self.check_dependencies();
                    }
                    _ => self.set_error(message),
                }
            }
            "dependencies:changed" => self.check_dependencies(),
            "notification" => {
                let text = value.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                let level = value.get("level").and_then(Value::as_str).unwrap_or("info").to_string();
                self.outbox.push(Effect::HostToast { level: level.clone(), text: text.clone() });
                if level == "error" || level == "warning" {
                    self.set_error(text);
                } else {
                    self.set_notice(text);
                }
            }
            "resource:download-requested" => {
                if let Some(info) = value.get("info") {
                    if let Ok(r) = serde_json::from_value::<engine::PendingResource>(info.clone()) {
                        self.queue_resource(r);
                    }
                }
            }
            "resource:error" => {
                let text = value.get("error").and_then(Value::as_str).unwrap_or("Resource error").to_string();
                self.set_error(text);
            }
            "app:close-requested" => self.quit(),
            "app:close-blocked" => self.set_error(
                "An export, component installation or update download is running. Wait for completion or cancel the operation before closing Reelio.",
            ),
            "export:updated" => self.last_jobs_poll = f64::NEG_INFINITY,
            _ => {}
        }
    }

    /// Frame pixels for the preview decoder: the source size, at most 960 px wide, even dimensions.
    pub fn frame_size(&self) -> (u32, u32) {
        let stream = self
            .source_asset()
            .and_then(|a| a.streams.iter().find(|s| is_video_stream(s)));
        let sw: f64 = stream
            .and_then(|s| s.prop("width").parse().ok())
            .unwrap_or(640.0);
        let sh: f64 = stream
            .and_then(|s| s.prop("height").parse().ok())
            .unwrap_or(360.0);
        let scale = 960f64.min(sw.max(2.0)) / sw.max(2.0);
        let even = |v: f64| (((v.round() as u32) / 2) * 2).max(2);
        (even(sw * scale), even(sh * scale))
    }

    fn merge_jobs(&mut self, jobs: Vec<Job>) {
        self.jobs = jobs
            .into_iter()
            .map(|j| {
                // Keep a newer local copy when the poll is older (reference rule).
                match self.jobs.iter().find(|x| x.id == j.id) {
                    Some(prev) if prev.sequence > j.sequence => prev.clone(),
                    _ => j,
                }
            })
            .collect();
    }

    fn after_jobs_change(&mut self) {
        let Some(id) = self.notifying_export.clone() else {
            return;
        };
        let Some(job) = self.jobs.iter().find(|j| j.id == id).cloned() else {
            return;
        };
        if !engine::job_is_finished(&job.status) {
            return;
        }
        self.notifying_export = None;
        if job.status == "completed" {
            let count = job.progress.as_ref().map(|p| p.notices.len()).unwrap_or(0);
            if count > 0 {
                self.set_notice(format!(
                    "Export completed with {count} timeline adjustment{} to fit the video. See Export history for every cropped or omitted item. Sources are unchanged.",
                    if count == 1 { "" } else { "s" }
                ));
            } else {
                self.set_notice("Export completed. Sources are unchanged.");
            }
        } else {
            self.notice = None;
            let text = job
                .error
                .clone()
                .unwrap_or_else(|| format!("Export {}. Sources are unchanged.", job.status));
            self.set_error(text);
        }
    }

    fn finish_probe(&mut self, _path: String, result: Result<Asset, String>) {
        match result.and_then(|asset| {
            add_asset(self.project(), &asset)
                .map(|p| (asset, p))
                .map_err(|e| e.message().to_owned())
        }) {
            Ok((asset, next)) => {
                self.edit(|p| *p = next);
                if self.preview.asset.is_none() && !is_image_asset(Some(&asset)) {
                    self.preview.asset = Some(asset.id);
                    self.refresh_preview();
                }
            }
            Err(e) => self.set_error(e),
        }
        self.continue_imports();
    }

    fn outbox_cover_import(&mut self, path: String, apply: bool) {
        self.request(
            Request::ArtworkImport { path },
            Pending::ArtworkImport { apply },
        );
    }

    /// Imports run one file at a time; the next probe starts when the previous one completes.
    pub(crate) fn import_and_test_subtitle(&mut self, path: String) {
        if let Some(track) = self
            .project()
            .tracks
            .iter()
            .find(|t| {
                self.project()
                    .track_asset(t)
                    .is_some_and(|a| a.path == path)
                    && self.project().track_kind(t) == Some(Kind::Subtitle)
            })
            .map(|t| t.id.clone())
        {
            self.select_track(&track);
        } else if self.busy.is_none() {
            self.busy = Some(format!("Inspecting {}", basename(&path)));
            self.request(
                Request::MediaProbe { path: path.clone() },
                Pending::TestSubtitleProbe { path },
            );
        }
    }

    fn continue_imports(&mut self) {
        if let Some(next) = self.import_queue.pop_front() {
            self.busy = Some(format!("Inspecting {}", basename(&next)));
            self.request(
                Request::MediaProbe { path: next.clone() },
                Pending::Probe { path: next },
            );
        } else {
            self.busy = None;
        }
    }

    pub fn import_paths(&mut self, paths: Vec<String>) {
        if self.busy.is_some() {
            self.set_error(
                "An import is already running. Wait for it to finish before adding more files.",
            );
            return;
        }
        self.import_queue = paths.into();
        self.continue_imports();
    }

    fn apply_cover(&mut self, asset: Asset, artwork: Artwork) {
        let mut id = asset.id.clone();
        let asset_clone = asset.clone();
        self.edit(|draft| {
            if let Some(existing) = draft.assets.iter().find(|a| a.path == asset_clone.path) {
                id = existing.id.clone();
            } else if let Ok(next) = add_asset(draft, &asset_clone) {
                *draft = next;
            }
            if draft
                .main_video_track_id
                .as_ref()
                .map(|m| draft.tracks.iter().any(|t| &t.id == m && t.asset_id == id))
                .unwrap_or(false)
            {
                draft.main_video_track_id = None;
            }
            draft.tracks.retain(|t| t.asset_id != id);
            draft.artwork = Some(artwork.clone());
        });
        self.cover_asset = Some(id);
        self.incoming_artwork = None;
        self.set_notice("Cover applied. Its C lane is container artwork, not a video stream; the original image is unchanged.");
    }

    fn activate_project(&mut self, project: Project) {
        self.store.replace(project);
        self.project_rev += 1;
        self.selected = None;
        self.preview = PreviewState {
            revision: self.preview.revision + 1,
            ..Default::default()
        };
        self.plan = None;
        self.cover_asset = None;
        self.poster = None;
        self.incoming_artwork = None;
        self.subtitle_test = None;
        self.resources.clear();
        self.playhead = 0.0;
        self.section = Section::Timeline;
        self.dialogs.settings = false;
        self.error = None;
        self.plan_due = Some(self.now);
    }

    fn quit(&mut self) {
        if self.busy.is_some()
            || self.studio_model_running()
            || self.jobs.iter().any(|j| engine::job_is_active(&j.status))
        {
            self.set_error("Finish the active operation before quitting Reelio.");
            return;
        }
        if self.store.dirty() || self.studio.as_ref().map(|s| s.dirty()).unwrap_or(false) {
            self.dialogs.quit_confirm = true;
            return;
        }
        self.outbox.push(Effect::CloseWindow);
    }

    fn command(&mut self, id: &str) {
        match id {
            "project.new" => self.new_project(),
            "project.open" => self.open_project(),
            "project.open_media" => self.open_media(false),
            "project.save" if self.section == Section::Subtitles => {
                self.studio_action("save", Value::Null)
            }
            "project.save" => self.save(),
            "export.validate" => self.validate_export(),
            "edit.undo" if self.section == Section::Subtitles => {
                self.studio_action("undo", Value::Null)
            }
            "edit.redo" if self.section == Section::Subtitles => {
                self.studio_action("redo", Value::Null)
            }
            "edit.undo" => self.undo(),
            "edit.redo" => self.redo(),
            "view.toggle_play" => self.toggle_play(),
            "view.chapter_next" => self.seek_chapter(1),
            "view.chapter_prev" => self.seek_chapter(-1),
            "project.quit" => self.quit(),
            _ => {}
        }
    }

    /// Perform a control action directly (menu items and tests use the same path as clicks).
    pub fn ui_action(&mut self, node: &str, value: Value) {
        self.ui(node, value);
    }

    fn ui(&mut self, node: &str, value: Value) {
        if let Some(control) = node.strip_prefix("viewer.") {
            if control == "name" {
                self.ui("name", value);
            } else {
                self.viewer(control, value);
            }
            return;
        }
        let (verb, arg) = node
            .split_once(':')
            .map(|(a, b)| (a, b.to_string()))
            .unwrap_or((node, String::new()));
        let is_context = value.get("type").and_then(Value::as_str) == Some("context");
        // Any control outside an open menu dismisses it (the opening event itself is excluded).
        if self.menu.is_some()
            && !node.starts_with("menu")
            && !(verb == "lane.canvas" && is_context)
        {
            let keyboard = value.get("type").and_then(Value::as_str) == Some("key");
            if !keyboard {
                self.menu = None;
            }
        }
        match verb {
            "menu" => {
                if value.get("type").and_then(Value::as_str) == Some("key") {
                    let key = value
                        .get("key")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.menu_key(&key);
                } else {
                    self.menu_activate(&arg);
                }
            }
            "nav" => {
                if let Some(s) = Section::from_id(&arg) {
                    if self.section == Section::Subtitles && s != Section::Subtitles {
                        if self.studio_model_running() {
                            return;
                        }
                        if self.playing {
                            self.toggle_play();
                        }
                    }
                    if s == Section::Subtitles {
                        self.studio_enter();
                    } else if s == Section::Artwork
                        && self
                            .poster
                            .as_ref()
                            .and_then(|p| p.editor.as_ref())
                            .is_none()
                    {
                        self.open_applied_cover();
                    } else {
                        self.section = s;
                    }
                    if !matches!(s, Section::Timeline | Section::Artwork | Section::Subtitles) {
                        self.layout.right_collapsed = false;
                        self.persist_layout();
                    }
                }
            }
            "sub" => self.studio_action(&arg, value),
            "sub.open" => self.studio_open_track(&arg),
            "art" => self.poster_action(&arg, value),
            "undo" => self.undo(),
            "redo" => self.redo(),
            "project" => match arg.as_str() {
                "menu" => self.open_project_menu(8.0, 40.0),
                "new" => self.new_project(),
                "open" => self.open_project(),
                "open_media" => self.open_media(false),
                "save" => self.save(),
                "quit" => self.quit(),
                _ => {}
            },
            "layout" => {
                match arg.as_str() {
                    "left" => self.layout.left_collapsed = !self.layout.left_collapsed,
                    "right" => self.layout.right_collapsed = !self.layout.right_collapsed,
                    "full_height" => self.layout.full_height_left = !self.layout.full_height_left,
                    _ => {}
                }
                self.persist_layout();
            }
            "pool" => match arg.as_str() {
                "add" => self.open_media(false),
                "folder" => self.open_media(true),
                _ => {
                    if let Some(id) = arg.strip_prefix("add:") {
                        self.include_asset(id);
                    } else if let Some(id) = arg.strip_prefix("open:") {
                        self.preview_asset(id);
                    }
                }
            },
            "viewer" => self.viewer(&arg, value),
            "timeline" => match arg.as_str() {
                "zoom_in" => self.zoom(-100.0, None),
                "zoom_out" => self.zoom(100.0, None),
                "zoom_reset" => self.timeline_view = View::default(),
                "add_tracks" => self.open_media(false),
                _ => {}
            },
            "lane" => self.lane(&arg, value),
            "lane.canvas" => {
                if is_context {
                    let x = value.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                    let y = value.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                    self.lane_context(&arg, x, y);
                } else {
                    self.canvas_event(&arg, value);
                }
            }
            "track" => self.track(&arg, value),
            "chapter" => self.chapter(&arg, value),
            "cover" => match arg.as_str() {
                "edit" => self.open_applied_cover(),
                "preview" if is_context => self.open_cover_menu(12., 44.),
                "preview" if value.is_null() => self.open_applied_cover(),
                part if (part == "preview" || part.starts_with("lane"))
                    && value["type"] == "double_click" =>
                {
                    self.open_applied_cover()
                }
                "remove" => {
                    self.edit(|d| d.artwork = None);
                    self.set_notice("Project cover removed.");
                }
                _ => {}
            },
            "container" => {
                let c = match arg.as_str() {
                    "mp4" => Some(Container::Mp4),
                    "m4v" => Some(Container::M4v),
                    "matroska" => Some(Container::Matroska),
                    _ => None,
                };
                if let Some(c) = c {
                    self.edit(|d| d.container = c);
                }
            }
            "meta" => {
                let text = value.as_str().unwrap_or("").to_string();
                self.edit(|d| {
                    if text.is_empty() {
                        d.metadata.remove(&arg);
                    } else {
                        d.metadata.insert(arg.clone(), text);
                    }
                });
            }
            "name" => {
                let text = value.as_str().unwrap_or("").to_string();
                self.edit(|d| d.name = text);
            }
            "export" => match arg.as_str() {
                "validate" => self.validate_export(),
                "start" => self.start_export(),
                "confirm" => self.confirm_export(),
                "review_cancel" => self.dialogs.export_review = None,
                "restore_cancel" => self.dialogs.restore_confirm = None,
                "restore_confirm" => {
                    if let Some(id) = self.dialogs.restore_confirm.take() {
                        self.restore_export_job(id);
                    }
                }
                other => {
                    if let Some(id) = other.strip_prefix("restore:") {
                        if self.store.dirty() && self.busy.is_none() {
                            self.dialogs.restore_confirm = Some(id.into());
                        } else {
                            self.restore_export_job(id.into());
                        }
                    }
                    if let Some(id) = other.strip_prefix("cancel:") {
                        self.request(Request::ExportCancel { id: id.into() }, Pending::Other);
                        self.busy = Some("Cancelling export".into());
                    }
                }
            },
            "consent" => {
                if let Some((id, _, _)) = &self.dialogs.engine_consent {
                    if let Some((action, nonce)) = arg.split_once(':') {
                        if nonce.parse::<u64>().ok() == Some(*id)
                            && matches!(action, "approve" | "cancel")
                        {
                            self.dialogs.engine_consent = None;
                            self.outbox
                                .push(Effect::ConsentDecision(action == "approve"));
                        }
                    }
                }
            }
            "resource" => {
                if let Some((action, id)) = arg.split_once(':') {
                    if self.resources.front().is_some_and(|r| r.id == id) {
                        if action == "dismiss" {
                            self.resources.pop_front();
                            self.request(
                                Request::ResourceCancel { id: id.into() },
                                Pending::Silent,
                            );
                        } else if action == "import" && self.busy.is_none() {
                            self.resources.pop_front();
                            self.busy = Some("Downloading and validating resource".into());
                            self.request(
                                Request::ResourceDownload { id: id.into() },
                                Pending::Download {
                                    project_id: self.project().id.clone(),
                                },
                            );
                        }
                    }
                }
            }
            "settings" => match arg.as_str() {
                "open" => self.dialogs.settings = true,
                "close" => self.dialogs.settings = false,
                "theme" => {
                    let v = value.as_str().unwrap_or("dark").to_string();
                    self.settings.theme = v.clone();
                    self.persist("theme", v.clone());
                    self.request(
                        Request::SettingsUpdate {
                            key: "theme".into(),
                            value: json!(v),
                        },
                        Pending::Settings,
                    );
                }
                "check_deps" => self.check_dependencies(),
                "ffmpeg" | "ffprobe" => self.ui(&format!("backend:{arg}"), value),
                "image_format" => self.ui("image_format", value),
                _ => {}
            },
            "about" => match arg.as_str() {
                "open" => self.dialogs.about = true,
                "close" => self.dialogs.about = false,
                "check_updates" => {
                    self.busy = Some("Checking GitHub releases".into());
                    self.engine_request_with(Request::UpdateCheck, Pending::UpdateCheck);
                }
                _ => {}
            },
            "backend" => {
                let text = value.as_str().unwrap_or("").to_string();
                match arg.as_str() {
                    "ffmpeg" => {
                        self.settings
                            .backend
                            .get_or_insert_with(Default::default)
                            .ffmpeg = Some(text)
                    }
                    "ffprobe" => {
                        self.settings
                            .backend
                            .get_or_insert_with(Default::default)
                            .ffprobe = Some(text)
                    }
                    "save" => {
                        let b = self.settings.backend.clone().unwrap_or_default();
                        self.engine_request_with(
                            Request::SettingsUpdate { key: "backend".into(), value: serde_json::json!({"ffmpeg": b.ffmpeg, "ffprobe": b.ffprobe}) },
                            Pending::Silent,
                        );
                        self.set_notice("Backend paths saved. Restart Reelio to apply them.");
                    }
                    _ => {}
                }
            }
            "folder" => match arg.as_str() {
                "choose" => {
                    self.engine_request_with(
                        Request::SettingsChooseFolder,
                        Pending::SettingsFolder,
                    );
                }
                "reset" => {
                    self.settings.default_folder = None;
                    self.engine_request_with(
                        Request::SettingsUpdate {
                            key: "default_folder".into(),
                            value: Value::Null,
                        },
                        Pending::Silent,
                    );
                }
                _ => {}
            },
            "image_format" => {
                let format = value.as_str().unwrap_or("png").to_string();
                self.settings.image_format = Some(format.clone());
                self.engine_request_with(
                    Request::SettingsUpdate {
                        key: "image_format".into(),
                        value: serde_json::json!(format),
                    },
                    Pending::Silent,
                );
            }
            "quit" => match arg.as_str() {
                "confirm" => {
                    if !self.dialogs.quit_confirm
                        || self.studio_model_running()
                        || self.busy.is_some()
                    {
                        return;
                    }
                    if self
                        .studio
                        .as_ref()
                        .is_some_and(|st| st.workflow.draft_writing)
                    {
                        self.set_notice(
                            "Wait for the subtitle recovery write to finish, then retry.",
                        );
                        return;
                    }
                    self.dialogs.quit_confirm = false;
                    if let Some(st) = self.studio.as_mut().filter(|st| st.dirty()) {
                        st.workflow.draft_due = None;
                        let document = serde_json::to_value(&st.doc).ok();
                        self.busy = Some("Saving subtitle recovery".into());
                        self.request(
                            Request::SubtitleEditorDraft {
                                document,
                                clear: false,
                            },
                            Pending::QuitRecovery,
                        );
                    } else {
                        self.outbox.push(Effect::CloseWindow);
                    }
                }
                "cancel" => self.dialogs.quit_confirm = false,
                _ => {}
            },
            "dep" => match arg.as_str() {
                "open" => self.dialogs.dependencies = true,
                "close" => self.dialogs.dependencies = false,
                "repair"
                    if self.busy.is_none()
                        && self.deps.as_ref().is_some_and(|d| d.repair_supported) =>
                {
                    self.busy = Some("Preparing component repair. Follow the system authentication prompt after approval.".into());
                    self.repair_message = self.busy.clone();
                    self.request(Request::DependenciesRepair, Pending::Repair);
                }
                "recheck" => self.check_dependencies(),
                _ => {}
            },
            "toast" => {
                let (kind, dismiss) = arg.split_once(':').unwrap_or((arg.as_str(), ""));
                let slot = if kind == "error" {
                    &mut self.error
                } else {
                    &mut self.notice
                };
                if dismiss == "dismiss" && value.is_null() {
                    *slot = None;
                } else if let Some(t) = slot {
                    match value["type"].as_str() {
                        Some("enter") => t.hovering = true,
                        Some("leave") => t.hovering = false,
                        Some("focus" | "key") => t.focused = true,
                        Some("blur") => t.focused = false,
                        _ => {
                            if let Some(hover) = value.as_bool() {
                                t.hovering = hover;
                            }
                        }
                    }
                    t.paused = t.hovering || t.focused;
                }
            }
            _ => {}
        }
    }

    /// Canvas events from a lane (`lane.canvas:<track>`): pointer, key, wheel.
    fn canvas_event(&mut self, track: &str, value: Value) {
        match value.get("type").and_then(Value::as_str) {
            Some("pointer") => {
                if let Some(p) = crate::timeline_input::parse_pointer(&value) {
                    crate::timeline_input::pointer(self, track, p);
                }
            }
            Some("key") if value.get("phase").and_then(Value::as_str) == Some("down") => {
                let key = value.get("key").and_then(Value::as_str).unwrap_or("");
                if key == "F10"
                    && value
                        .get("mods")
                        .and_then(|m| m.get("shift"))
                        .and_then(Value::as_bool)
                        == Some(true)
                {
                    self.lane_context(track, 24.0, 30.0);
                    return;
                }
                let mods = value.get("mods");
                let shift = mods
                    .and_then(|m| m.get("shift"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let alt = mods
                    .and_then(|m| m.get("alt"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                crate::timeline_input::key(self, track, key, shift, alt);
            }
            Some("wheel") => {
                let dx = value.get("dx").and_then(Value::as_f64).unwrap_or(0.0);
                let dy = value.get("dy").and_then(Value::as_f64).unwrap_or(0.0);
                let x = value.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let alt = value
                    .get("mods")
                    .and_then(|m| m.get("alt"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                crate::timeline_input::wheel(self, dx, dy, x, alt);
            }
            _ => {}
        }
    }

    fn persist_layout(&mut self) {
        let json = layout::workspace_layout_json(self.layout);
        self.persist(layout::WORKSPACE_LAYOUT_KEY, json);
    }

    pub fn undo(&mut self) {
        if self.store.undo() {
            self.project_rev += 1;
            self.plan_due = Some(self.now + 180.0);
        }
    }

    pub fn redo(&mut self) {
        if self.store.redo() {
            self.project_rev += 1;
            self.plan_due = Some(self.now + 180.0);
        }
    }

    fn new_project(&mut self) {
        if self.busy.is_some() {
            return;
        }
        if self.store.dirty() {
            self.dialogs.quit_confirm = false;
        }
        self.activate_project(empty_project());
        self.set_notice("New project ready. Open media or drop files to get started; existing files are unchanged.");
    }

    fn open_project(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Opening project".into());
        self.request(Request::ProjectOpen, Pending::OpenProject);
    }

    fn open_media(&mut self, folder: bool) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Choose media".into());
        self.request(Request::ChooseMedia { folder }, Pending::ChooseMedia);
    }

    fn save(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Saving project".into());
        let project = serde_json::to_value(self.project()).unwrap_or(Value::Null);
        self.request(Request::ProjectSave { project }, Pending::Save);
    }

    pub fn validate_export(&mut self) {
        self.section = Section::Export;
        self.busy = Some("Validating sources and output plan".into());
        let project = serde_json::to_value(self.project()).unwrap_or(Value::Null);
        self.request(
            Request::ExportPlan { project },
            Pending::ExportValidate {
                revision: self.project_rev,
            },
        );
    }

    fn restore_export_job(&mut self, id: String) {
        if self.busy.is_some() || self.jobs.iter().any(|j| engine::job_is_active(&j.status)) {
            return;
        }
        if self.jobs.iter().any(|j| {
            j.id == id
                && j.project_id.as_deref() == Some(self.project().id.as_str())
                && matches!(j.status.as_str(), "failed" | "interrupted" | "cancelled")
        }) {
            self.busy = Some("Restoring export settings".into());
            self.request(Request::JobProject { id }, Pending::RestoreJob);
        }
    }

    fn start_export(&mut self) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        if !self.export_enabled() || self.plan_due.is_some() {
            return;
        }
        self.dialogs.export_revision = self.project_rev;
        let conversions: Vec<&engine::PlanTrack> = plan
            .tracks
            .iter()
            .filter(|t| {
                ["convert_subtitle", "render_audio", "render_video"]
                    .contains(&t.compatibility.action.as_str())
            })
            .collect();
        let mut review: Vec<String> = plan.warnings.clone();
        for t in &conversions {
            review.push(format!(
                "Output track #{}: {} - {}",
                t.output_index + 1,
                action_label(&t.compatibility.action),
                t.compatibility.reason
            ));
        }
        if !conversions.is_empty() {
            review.push("Continuing authorizes the listed subtitle conversions (including possible styling loss) and timeline audio/video rendering.".into());
        }
        let clean = review.is_empty();
        self.dialogs.export_review = Some(review);
        if clean {
            self.confirm_export();
        }
    }

    fn confirm_export(&mut self) {
        if self.dialogs.export_review.take().is_none() {
            return;
        }
        if self.dialogs.export_revision != self.project_rev
            || !self.export_enabled()
            || self.plan_due.is_some()
        {
            self.set_error(
                "The project changed. Review the current export plan before continuing.",
            );
            return;
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let approve = plan.tracks.iter().any(|t| {
            ["convert_subtitle", "render_audio", "render_video"]
                .contains(&t.compatibility.action.as_str())
        });
        self.dialogs.export_review = None;
        self.busy = Some("Starting export".into());
        let project = serde_json::to_value(self.project()).unwrap_or(Value::Null);
        self.request(
            Request::ExportStart {
                project,
                approve_conversion: approve,
            },
            Pending::ExportStart,
        );
    }

    fn refresh_preview(&mut self) {
        let Some(asset) = self.source_asset().cloned() else {
            return;
        };
        if !self.dependencies_ready() {
            return;
        }
        self.preview.url = None;
        self.preview.error = None;
        self.request(
            Request::PreviewSource {
                path: asset.path.clone(),
            },
            Pending::PreviewSource { path: asset.path },
        );
    }

    fn preview_asset(&mut self, id: &str) {
        let Some(asset) = self.project().asset(id).cloned() else {
            return;
        };
        if is_image_asset(Some(&asset)) {
            if self.busy.is_some() {
                return;
            }
            self.busy = Some("Opening image in Poster & cover".into());
            self.outbox_cover_import(asset.path, false);
            return;
        }
        let video = asset.streams.iter().any(is_video_stream);
        let has_any_video = self
            .preview_sources()
            .iter()
            .any(|a| a.streams.iter().any(|s| s.kind == Kind::Video));
        if video || !has_any_video {
            self.preview.asset = Some(id.into());
            self.preview.url = None;
            self.refresh_preview();
        }
        if let Some(track) = self
            .project()
            .tracks
            .iter()
            .find(|t| t.asset_id == id)
            .map(|t| t.id.clone())
        {
            self.select_track(&track);
        }
    }

    /// Select a lane by track id (used by pointer gestures on its canvas).
    pub fn select_track_by_id(&mut self, id: &str) {
        self.select_track(id);
    }

    /// Track as it should be drawn: in-progress offset and trim feedback applied.
    pub fn effective_track(&self, t: &Track) -> Track {
        let mut out = t.clone();
        if let Some((id, offset)) = &self.drafts.offset {
            if *id == t.id {
                out.offset_ms = *offset;
            }
        }
        if let Some((id, patch)) = &self.drafts.trim {
            if *id == t.id {
                patch.apply(&mut out);
            }
        }
        out
    }

    pub fn edit_track_offset(&mut self, id: &str, offset: i64) {
        self.patch_track(
            id,
            TrackPatch {
                offset_ms: Some(offset),
                ..Default::default()
            },
        );
    }

    pub fn edit_track_patch(&mut self, id: &str, patch: TrackPatch) {
        self.patch_track(id, patch);
    }

    fn select_track(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        let Some(track) = self.project().track(id).cloned() else {
            return;
        };
        let kind = self.project().track_kind(&track);
        if kind == Some(Kind::Video) {
            self.preview.asset = Some(track.asset_id.clone());
            self.refresh_preview();
        }
        if kind == Some(Kind::Subtitle) && self.dependencies_ready() {
            let already = self
                .subtitle_test
                .as_ref()
                .map(|t| t.track_id == id)
                .unwrap_or(false);
            if !already {
                self.test_subtitle(id);
            }
        }
    }

    fn test_subtitle(&mut self, track_id: &str) {
        self.busy = Some("Converting a temporary subtitle preview".into());
        let revision = self.project_rev;
        let project = serde_json::to_value(self.project()).unwrap_or(Value::Null);
        self.request(
            Request::SubtitlePreview {
                project,
                track_id: track_id.into(),
            },
            Pending::SubtitleTest {
                track: track_id.into(),
                revision,
            },
        );
    }

    fn include_asset(&mut self, id: &str) {
        let Some(asset) = self.project().asset(id).cloned() else {
            return;
        };
        if is_image_asset(Some(&asset)) {
            self.import_paths(vec![asset.path.clone()]);
            return;
        }
        let has_video = asset.streams.iter().any(is_video_stream);
        self.edit(|d| {
            let fresh: Vec<Track> = asset_tracks(d, &asset)
                .into_iter()
                .filter(|t| {
                    !d.tracks
                        .iter()
                        .any(|p| p.asset_id == t.asset_id && p.stream_index == t.stream_index)
                })
                .collect();
            d.tracks.extend(fresh);
            if has_video {
                // Including a new video replaces the previously included videos (and their mix audio).
                let prior_video: Vec<String> = d
                    .tracks
                    .iter()
                    .filter(|t| {
                        t.enabled && t.asset_id != id && d.track_kind(t) == Some(Kind::Video)
                    })
                    .map(|t| t.asset_id.clone())
                    .collect();
                let kinds: Vec<Option<Kind>> = d
                    .tracks
                    .iter()
                    .map(|t| d.track_kind_of(&t.asset_id, t.stream_index))
                    .collect();
                for (t, kind) in d.tracks.iter_mut().zip(kinds) {
                    if prior_video.contains(&t.asset_id)
                        && (kind == Some(Kind::Video) || t.audio_role == Some(AudioRole::Mix))
                    {
                        t.enabled = false;
                    }
                }
            }
            let attached: Vec<bool> = d
                .tracks
                .iter()
                .map(|t| {
                    d.asset(&t.asset_id)
                        .and_then(|a| a.streams.iter().find(|s| s.index == t.stream_index))
                        .map(|s| s.attached_pic())
                        .unwrap_or(false)
                })
                .collect();
            let kinds: Vec<Option<Kind>> = d
                .tracks
                .iter()
                .map(|t| d.track_kind_of(&t.asset_id, t.stream_index))
                .collect();
            for ((t, kind), attached) in d.tracks.iter_mut().zip(kinds).zip(attached) {
                if t.asset_id == id
                    && matches!(kind, Some(Kind::Video | Kind::Audio | Kind::Subtitle))
                    && !attached
                {
                    t.enabled = true;
                }
            }
        });
        self.preview.asset = Some(id.into());
        self.refresh_preview();
    }

    fn lane(&mut self, arg: &str, value: Value) {
        let (action, id) = arg
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((arg.to_string(), String::new()));
        match action.as_str() {
            "select" => self.select_track(&id),
            "seek" => self.seek_playhead(value.as_f64().unwrap_or(0.0)),
            "offset" => {
                if let Some(v) = value.as_f64().filter(|v| v.is_finite()) {
                    self.patch_track(
                        &id,
                        TrackPatch {
                            offset_ms: Some(
                                v.round().clamp(-(MAX_OFFSET as f64), MAX_OFFSET as f64) as i64,
                            ),
                            ..Default::default()
                        },
                    );
                }
            }
            "move" => {
                let v = value.as_f64().unwrap_or(0.0);
                let snapped = offset_after_drag(0, v, 1.0);
                self.patch_track(
                    &id,
                    TrackPatch {
                        offset_ms: Some(snapped),
                        ..Default::default()
                    },
                );
            }
            "mute" => {
                let muted = self.project().track(&id).map(|t| !t.muted).unwrap_or(false);
                self.patch_track(
                    &id,
                    TrackPatch {
                        muted: Some(muted),
                        ..Default::default()
                    },
                );
            }
            "role" => {
                let role = match value.as_str() {
                    Some("mix") => AudioRole::Mix,
                    _ => AudioRole::Separate,
                };
                self.patch_track(
                    &id,
                    TrackPatch {
                        audio_role: Some(role),
                        ..Default::default()
                    },
                );
            }
            "lang" => {
                let lang = value.as_str().unwrap_or("").to_lowercase();
                self.patch_track(
                    &id,
                    TrackPatch {
                        language: Some(lang),
                        ..Default::default()
                    },
                );
            }
            "ref" => {
                let next = if self.project().main_video_track_id.as_deref() == Some(id.as_str()) {
                    None
                } else {
                    Some(id.clone())
                };
                self.edit(|d| match &next {
                    Some(v) => d.main_video_track_id = Some(v.clone()),
                    None => d.main_video_track_id = None,
                });
                if let Some(t) = self.project().track(&id).cloned() {
                    if next.is_some() {
                        self.preview.asset = Some(t.asset_id.clone());
                        self.refresh_preview();
                    }
                }
            }
            "include" => {
                let enabled = self
                    .project()
                    .track(&id)
                    .map(|t| !t.enabled)
                    .unwrap_or(false);
                self.patch_track(
                    &id,
                    TrackPatch {
                        enabled: Some(enabled),
                        ..Default::default()
                    },
                );
            }
            "remove" => {
                self.edit(|d| {
                    d.tracks.retain(|t| t.id != id);
                    if d.main_video_track_id.as_deref() == Some(id.as_str()) {
                        d.main_video_track_id = None;
                    }
                });
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
            }
            "clip_to_video" => {
                if let Some(t) = self.project().track(&id).cloned() {
                    let action = clip_to_video(self.project(), &t);
                    match action.patch {
                        Some(patch) => self.patch_track(&id, patch),
                        None => self.set_error(action.reason),
                    }
                }
            }
            "trim" => {
                // Canvas drag commits and keyboard steps: {edge:"start"|"end", ms, snap}.
                let edge = if value.get("edge").and_then(Value::as_str) == Some("start") {
                    Edge::Start
                } else {
                    Edge::End
                };
                let ms = value.get("ms").and_then(Value::as_f64).unwrap_or(0.0);
                let snap = value.get("snap").and_then(Value::as_bool).unwrap_or(false);
                self.trim_track(&id, edge, ms, snap);
            }
            "trim_reset" => self.patch_track(
                &id,
                TrackPatch {
                    trim_start_ms: Some(0),
                    trim_end_ms: Some(None),
                    ..Default::default()
                },
            ),
            _ => {}
        }
    }

    /// Apply a trim drag/keyboard step to a track edge. Exposed for the host canvas handler.
    pub fn trim_track(&mut self, id: &str, edge: Edge, delta_ms: f64, snap: bool) {
        let Some(t) = self.project().track(id).cloned() else {
            return;
        };
        let (start, end) = clip_bounds(self.project(), &t);
        let duration = source_duration(self.project(), &t).unwrap_or(0.0);
        let snap_info = if snap {
            geometry::video_trim_snap(self.project(), &t, self.timeline_scale())
        } else {
            None
        };
        let patch = trim_after_drag(edge, start, end, duration, t.offset_ms, delta_ms, snap_info);
        self.patch_track(id, patch);
    }

    /// Pixels per millisecond of the timeline at the current zoom and a 700 px viewport.
    pub fn timeline_scale(&self) -> f64 {
        let end = timeline_viewport_duration(self.project());
        (700.0 / end) * self.timeline_view.zoom
    }

    fn patch_track(&mut self, id: &str, patch: TrackPatch) {
        let trim_changed = patch.trim_start_ms.is_some() || patch.trim_end_ms.is_some();
        self.edit(|d| {
            if patch.default == Some(true) {
                let kind = d.tracks.iter().find(|t| t.id == id).and_then(|t| {
                    d.asset(&t.asset_id)
                        .and_then(|a| a.streams.iter().find(|s| s.index == t.stream_index))
                        .map(|s| s.kind)
                });
                let ids: Vec<String> = d
                    .tracks
                    .iter()
                    .filter(|o| o.id != id && kind.is_some() && d.track_kind(o) == kind)
                    .map(|o| o.id.clone())
                    .collect();
                for t in d.tracks.iter_mut() {
                    if ids.contains(&t.id) {
                        t.default = false;
                    }
                }
            }
            if let Some(t) = d.tracks.iter_mut().find(|t| t.id == id) {
                patch.apply(t);
            }
            if trim_changed {
                let duration = timeline_duration(d);
                chapter_ends(&mut d.chapters, Some(duration));
            }
        });
    }

    fn track(&mut self, arg: &str, value: Value) {
        let (action, id) = arg
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((arg.to_string(), String::new()));
        let flag = value.as_bool();
        let patch = match action.as_str() {
            "title" => TrackPatch {
                title: Some(value.as_str().unwrap_or("").to_string()),
                ..Default::default()
            },
            "language" => TrackPatch {
                language: Some(value.as_str().unwrap_or("").to_lowercase()),
                ..Default::default()
            },
            "default" => TrackPatch {
                default: flag,
                ..Default::default()
            },
            "forced" => TrackPatch {
                forced: flag,
                ..Default::default()
            },
            "commentary" => TrackPatch {
                commentary: flag,
                ..Default::default()
            },
            "descriptive" => TrackPatch {
                descriptive: flag,
                ..Default::default()
            },
            "enabled" => TrackPatch {
                enabled: flag,
                ..Default::default()
            },
            "offset" => TrackPatch {
                offset_ms: value.as_f64().map(|v| v.round() as i64),
                ..Default::default()
            },
            "move" => {
                let dir = value.as_i64().unwrap_or(0);
                self.edit(|d| {
                    let i = d.tracks.iter().position(|t| t.id == id);
                    if let Some(i) = i {
                        let target = i as i64 + dir;
                        if target >= 0 && (target as usize) < d.tracks.len() {
                            d.tracks.swap(i, target as usize);
                        }
                    }
                });
                return;
            }
            "remove" => {
                self.lane(&format!("remove:{id}"), Value::Null);
                return;
            }
            _ => return,
        };
        self.patch_track(&id, patch);
    }

    fn chapter(&mut self, arg: &str, value: Value) {
        let (action, rest) = arg
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((arg.to_string(), String::new()));
        let index: usize = rest.parse().unwrap_or(usize::MAX);
        match action.as_str() {
            "add" => {
                let ms = value.as_f64().unwrap_or(self.playhead);
                self.add_chapter(ms);
            }
            "add_playhead" => self.add_chapter(self.media_time),
            "move" => {
                let ms = value.as_f64().unwrap_or(0.0);
                self.move_chapter(index, ms);
            }
            "time" => match value.as_str().and_then(parse_chapter_time) {
                Some(ms) => {
                    let ms = ms as f64;
                    self.edit(|d| {
                        if let Some(c) = d.chapters.get_mut(index) {
                            c.start_ms = ms as i64;
                        }
                        let dur = d
                            .assets
                            .first()
                            .and_then(|a| a.duration_ms)
                            .map(|x| x as f64);
                        chapter_ends(&mut d.chapters, dur);
                    });
                }
                None => self.set_error("Use HH:MM:SS.mmm. Previous start time restored."),
            },
            "title" => {
                let text = value.as_str().unwrap_or("").to_string();
                self.edit(|d| {
                    if let Some(c) = d.chapters.get_mut(index) {
                        c.title = text;
                    }
                });
            }
            "delete" => {
                self.edit(|d| {
                    if index < d.chapters.len() {
                        d.chapters.remove(index);
                    }
                    let dur = video_timeline_end(d).or_else(|| {
                        d.assets
                            .first()
                            .and_then(|a| a.duration_ms)
                            .map(|x| x as f64)
                    });
                    chapter_ends(&mut d.chapters, dur);
                });
            }
            "sort" => {
                self.edit(|d| {
                    d.chapters.sort_by_key(|c| c.start_ms);
                    let dur = d
                        .assets
                        .first()
                        .and_then(|a| a.duration_ms)
                        .map(|x| x as f64);
                    chapter_ends(&mut d.chapters, dur);
                });
            }
            _ => {}
        }
    }

    pub fn add_chapter(&mut self, ms: f64) {
        let duration = timeline_duration(self.project());
        self.edit(|d| {
            let value = ((ms + 0.5).floor() as i64).clamp(0, (duration as i64 - 1).max(0));
            if d.chapters.iter().any(|c| c.start_ms == value) {
                return;
            }
            let title = format!("Chapter {}", d.chapters.len() + 1);
            d.chapters.push(Chapter {
                start_ms: value,
                end_ms: value + 1,
                title,
            });
            d.chapters.sort_by_key(|c| c.start_ms);
            chapter_ends(&mut d.chapters, Some(duration));
        });
    }

    pub fn move_chapter(&mut self, index: usize, ms: f64) {
        let duration = timeline_duration(self.project());
        self.edit(|d| {
            if index >= d.chapters.len() {
                return;
            }
            let value = ((ms + 0.5).floor() as i64).clamp(0, (duration as i64 - 1).max(0));
            if d.chapters
                .iter()
                .enumerate()
                .any(|(i, o)| i != index && o.start_ms == value)
            {
                return;
            }
            d.chapters[index].start_ms = value;
            d.chapters.sort_by_key(|c| c.start_ms);
            chapter_ends(&mut d.chapters, Some(duration));
        });
    }

    fn viewer(&mut self, arg: &str, value: Value) {
        match arg {
            "play" | "toggle" => self.toggle_play(),
            "stop" => {
                self.playing = false;
                self.transport(false);
                self.media_time = 0.0;
                self.outbox.push(Effect::Seek {
                    ms: 0.0,
                    fast: false,
                });
            }
            "start" => self.set_media_time(0.0),
            "end" => {
                let end = self.source_duration_ms();
                self.set_media_time(end);
            }
            "prev_frame" | "next_frame" => {
                let (rate, fallback_end) = self.frame_rate();
                let frame = preview_frame_ms(rate.as_deref());
                let direction = if arg == "next_frame" { 1.0 } else { -1.0 };
                self.playing = false;
                self.transport(false);
                let target = frame_target(self.media_time, direction, frame, fallback_end);
                self.set_media_time(target);
            }
            "chapter_next" => self.seek_chapter(1),
            "chapter_prev" => self.seek_chapter(-1),
            "seek" => {
                let ms = value.as_f64().unwrap_or(0.0);
                self.seek_media(
                    ms,
                    value.get("final").and_then(Value::as_bool).unwrap_or(false),
                );
            }
            "volume" => {
                self.volume = value.as_f64().unwrap_or(self.volume).clamp(0.0, 1.0);
            }
            "speed" => {
                if let Some(i) = value
                    .as_u64()
                    .map(|v| v as usize)
                    .filter(|i| *i < PLAYBACK_SPEEDS.len())
                {
                    self.speed_index = i;
                }
            }
            "retry" => {
                self.preview.url = None;
                self.preview.error = None;
                self.preview.revision += 1;
                self.refresh_preview();
            }
            "source" => {
                let id = value.as_str().unwrap_or("").to_string();
                if self.project().asset(&id).is_some() {
                    self.preview.asset = Some(id);
                    self.refresh_preview();
                }
            }
            "name" => {
                let text = value.as_str().unwrap_or("").to_string();
                self.edit(|d| d.name = text);
            }
            "add_chapter" => self.add_chapter(self.media_time),
            _ => {}
        }
    }

    fn frame_rate(&self) -> (Option<String>, f64) {
        let end = self.source_duration_ms().max(0.0);
        let rate = self
            .source_asset()
            .and_then(|a| a.streams.iter().find(|s| is_video_stream(s)))
            .and_then(|s| s.properties.get("r_frame_rate").cloned());
        (rate, end)
    }

    /// Playback speed as shown in the transport (`0.10x` ... `10x`).
    pub fn speed(&self) -> f64 {
        PLAYBACK_SPEEDS[self.speed_index]
    }

    /// Emit a transport command from the current media position, speed and volume.
    pub fn transport(&mut self, playing: bool) {
        self.playing = playing;
        let speed = self.speed();
        let volume = self.volume;
        let from_ms = self.media_time;
        self.outbox.push(Effect::Transport {
            playing,
            from_ms,
            speed,
            volume,
        });
    }

    /// Clock reported by the frame pump while playing. Not an edit: no undo entry, no dirtiness.
    pub fn playback_clock(&mut self, ms: f64) {
        self.media_time = ms.clamp(0.0, self.source_duration_ms().max(0.0));
        self.playhead = self.media_time + self.video_offset() as f64;
        if self.playing {
            self.studio_follow();
        }
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        let playing = self.playing;
        self.transport(playing);
    }

    pub(crate) fn set_media_time(&mut self, ms: f64) {
        let end = self.source_duration_ms();
        self.media_time = ms.clamp(0.0, end.max(0.0));
        self.playhead = self.media_time + self.video_offset() as f64;
        self.reveal = Some((self.playhead, self.reveal.map(|r| r.1 + 1).unwrap_or(1)));
        self.outbox.push(Effect::Seek {
            ms: self.media_time,
            fast: false,
        });
    }

    pub fn subtitle_overlay(&self) -> Vec<&str> {
        let ms = self.media_time as i64;
        if self.section == Section::Subtitles {
            if let Some(st) = self.studio.as_ref().filter(|st| !st.doc.cues.is_empty()) {
                return st
                    .doc
                    .cues
                    .iter()
                    .filter(|cue| cue.start_ms <= ms && ms < cue.end_ms)
                    .take(4)
                    .map(|cue| cue.text.as_str())
                    .collect();
            }
        }
        self.subtitle_test
            .as_ref()
            .filter(|test| {
                test.project_id == self.project().id
                    && self.selected.as_ref() == Some(&test.track_id)
            })
            .into_iter()
            .flat_map(|test| &test.cues)
            .filter(|cue| cue.start <= ms && ms < cue.end)
            .take(4)
            .map(|cue| cue.text.as_str())
            .collect()
    }

    /// Seek requested by the viewer seek bar (coalesced by the scheduler).
    fn seek_media(&mut self, ms: f64, final_: bool) {
        let end = self.source_duration_ms();
        let target = ms.clamp(0.0, end.max(0.0));
        self.media_time = target;
        self.playhead = target + self.video_offset() as f64;
        self.reveal = Some((self.playhead, self.reveal.map(|r| r.1 + 1).unwrap_or(1)));
        for write in self.seek.request(target, final_, self.now) {
            self.outbox.push(Effect::Seek {
                ms: write.time,
                fast: write.fast,
            });
        }
    }

    /// Seek the output playhead from a timeline ruler click (accounts for the video offset).
    pub fn seek_playhead(&mut self, ms: f64) {
        self.playhead = ms.max(0.0);
        let offset = self.video_offset() as f64;
        self.seek_media((ms - offset).max(0.0), true);
    }

    fn seek_chapter(&mut self, direction: i32) {
        let pos = self.media_time;
        let starts: Vec<f64> = self
            .project()
            .chapters
            .iter()
            .map(|c| c.start_ms as f64)
            .collect();
        let target = if direction > 0 {
            starts.iter().copied().find(|t| *t > pos + 500.0)
        } else {
            starts.iter().rev().copied().find(|t| *t < pos - 500.0)
        };
        if let Some(t) = target {
            self.set_media_time(t);
        }
    }

    fn zoom(&mut self, delta: f64, pointer: Option<f64>) {
        let duration = timeline_viewport_duration(self.project());
        self.timeline_view = geometry::zoom_timeline(
            self.timeline_view,
            delta,
            700.0,
            duration,
            self.playhead,
            pointer,
        );
    }

    fn validation_ok(&self) -> bool {
        self.plan
            .as_ref()
            .map(|p| p.errors.is_empty())
            .unwrap_or(false)
    }

    pub fn export_enabled(&self) -> bool {
        self.busy.is_none()
            && self.validation_ok()
            && !self.jobs.iter().any(|j| engine::job_is_active(&j.status))
    }

    pub fn reference_end(&self) -> Option<f64> {
        reference_video_end(self.project())
    }

    /// Resource prompts queue (max nine), as in the reference DownloadPrompt.
    pub fn queue_resource(&mut self, r: engine::PendingResource) {
        if self.resources.iter().any(|x| x.id == r.id) {
            return;
        }
        self.resources.push_back(r);
        while self.resources.len() > 9 {
            self.resources.pop_front();
        }
    }

    /// Current window size used for responsive clamps (pool/inspector/viewer).
    pub fn set_window(&mut self, width: f64) {
        self.window_width = width;
        self.pool_width = layout::clamp_pool_width(Some(self.pool_width), Some(width));
    }

    /// Cover asset (for the C lane) when one exists.
    pub fn cover(&self) -> Option<&Artwork> {
        self.project().artwork.as_ref()
    }

    pub fn selected_track(&self) -> Option<&Track> {
        self.selected
            .as_ref()
            .and_then(|id| self.project().track(id))
    }

    pub fn track_title(&self, t: &Track) -> String {
        if !t.title.is_empty() {
            return t.title.clone();
        }
        self.project()
            .track_asset(t)
            .map(|a| basename(&a.path))
            .unwrap_or_else(|| "Media".into())
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

pub fn action_label(action: &str) -> &'static str {
    match action {
        "copy" => "Stream copy",
        "bitstream_filter" => "Bitstream filter",
        "convert_subtitle" => "Convert subtitle",
        "render_audio" => "Render audio",
        "render_video" => "Render video",
        "transcode" => "Needs transcoding",
        _ => "Unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_auto_hide_and_pause_while_hovered() {
        let mut s = State::new();
        s.dispatch(Event::Tick { now: 0.0 });
        s.set_notice("saved");
        s.dispatch(Event::Ui {
            node: "toast:notice:hover".into(),
            value: json!(true),
        });
        s.dispatch(Event::Tick { now: 10_000.0 });
        assert!(s.notice.is_some(), "paused while hovered");
        s.dispatch(Event::Ui {
            node: "toast:notice:hover".into(),
            value: json!(false),
        });
        s.dispatch(Event::Tick { now: 15_000.0 });
        assert!(s.notice.is_none());
        s.set_error("boom");
        s.dispatch(Event::Tick { now: 22_000.0 });
        assert!(s.error.is_some(), "errors last eight seconds");
        s.dispatch(Event::Tick { now: 23_100.0 });
        assert!(s.error.is_none());
    }

    #[test]
    fn layout_preferences_are_persisted_without_dirtying_the_project() {
        let mut s = State::new();
        s.drain();
        s.dispatch(Event::Ui {
            node: "layout:left".into(),
            value: Value::Null,
        });
        let effects = s.drain();
        assert!(effects.iter().any(
            |e| matches!(e, Effect::Persist { key, .. } if key == layout::WORKSPACE_LAYOUT_KEY)
        ));
        assert!(!s.store.dirty());
    }

    #[test]
    fn undo_redo_round_trip_through_reducer() {
        let mut s = State::new();
        s.edit(|p| p.name = "Edited".into());
        s.dispatch(Event::Ui {
            node: "undo".into(),
            value: Value::Null,
        });
        assert_eq!(s.project().name, "Untitled project");
        s.dispatch(Event::Ui {
            node: "redo".into(),
            value: Value::Null,
        });
        assert_eq!(s.project().name, "Edited");
    }

    #[test]
    fn quitting_a_dirty_project_asks_first_and_busy_work_blocks_quit() {
        let mut s = State::new();
        s.edit(|p| p.name = "x".into());
        s.dispatch(Event::Quit);
        assert!(s.dialogs.quit_confirm);
        assert!(!s.drain().contains(&Effect::CloseWindow));
        s.dispatch(Event::Ui {
            node: "quit:confirm".into(),
            value: Value::Null,
        });
        assert!(s.drain().contains(&Effect::CloseWindow));
        s.busy = Some("export".into());
        s.dispatch(Event::Quit);
        assert_eq!(
            s.error.as_ref().map(|t| t.text.as_str()),
            Some("Finish the active operation before quitting Reelio.")
        );
    }

    #[test]
    fn chapters_are_sorted_and_duplicate_starts_are_ignored() {
        let mut s = State::new();
        s.edit(|p| {
            p.assets.push(Asset {
                id: "a".into(),
                path: "/m/a.mp4".into(),
                format: "mp4".into(),
                size: 1,
                modified_ms: 0,
                duration_ms: Some(20_000),
                streams: vec![],
                metadata: BTreeMap::new(),
                chapters: vec![],
            })
        });
        let track = Track {
            id: "t".into(),
            asset_id: "a".into(),
            stream_index: 0,
            enabled: true,
            title: String::new(),
            language: "und".into(),
            default: false,
            forced: false,
            commentary: false,
            descriptive: false,
            offset_ms: 0,
            trim_start_ms: 0,
            trim_end_ms: None,
            muted: false,
            audio_role: None,
        };
        s.edit(|p| p.tracks.push(track));
        s.add_chapter(5000.0);
        s.add_chapter(2000.0);
        s.add_chapter(5000.0);
        let starts: Vec<i64> = s.project().chapters.iter().map(|c| c.start_ms).collect();
        assert_eq!(starts, vec![2000, 5000]);
        assert_eq!(s.project().chapters[0].end_ms, 5000);
        s.dispatch(Event::Ui {
            node: "chapter:delete:0".into(),
            value: Value::Null,
        });
        assert_eq!(s.project().chapters.len(), 1);
        assert!(s.project().chapters.iter().all(|c| c.end_ms > c.start_ms));
    }

    #[test]
    fn container_and_format_choices_are_saved_in_the_project() {
        let mut s = State::new();
        s.dispatch(Event::Ui {
            node: "container:matroska".into(),
            value: Value::Null,
        });
        assert_eq!(s.project().container, Container::Matroska);
        assert!(s.store.dirty());
    }

    #[test]
    fn export_review_requires_approval_for_conversions() {
        let mut s = State::new();
        s.plan = Some(Plan {
            tracks: vec![engine::PlanTrack {
                track_id: "t".into(),
                output_index: 0,
                source_index: 0,
                asset_id: "a".into(),
                compatibility: engine::Compatibility {
                    action: "render_video".into(),
                    reason: "needs trim".into(),
                    warning: None,
                    target_codec: None,
                },
                default: false,
            }],
            ..Default::default()
        });
        s.dispatch(Event::Ui {
            node: "export:start".into(),
            value: Value::Null,
        });
        let review = s.dialogs.export_review.clone().expect("review shown");
        assert!(review.iter().any(|l| l.contains("Render video")));
        assert!(review
            .iter()
            .any(|l| l.starts_with("Continuing authorizes")));
    }
}
