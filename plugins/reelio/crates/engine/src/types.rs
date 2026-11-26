//! Plain serde request/response/event types for the Reelio engine.
//!
//! Shapes mirror the arguments and return values of the former Tauri commands
//! (`generate_handler!` in the reference `apps/desktop/src-tauri/src/lib.rs`).
//! Field names are the Rust argument names (snake_case).

pub use reelio_application::dependencies::DependencyReport;
pub use reelio_application::resources::{
    PendingInfo, PosterBatch, PosterProportion, SearchKind, SubtitleFile,
};
pub use reelio_artwork::ImageEdits;
pub use reelio_domain::{Artwork, Asset, ExportPlan, Project};
pub use reelio_ffmpeg::Capabilities;
pub use reelio_storage::Job;
pub use reelio_subtitles::{local_model::JobInfo, Document};
pub use reelio_updates::{Report, Target};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Every engine call returns this. Errors are user-facing strings, as in the reference.
pub type EngineResult<T> = Result<T, String>;

/// Window actions accepted from the UI (bounded enum, same wire names as the reference).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    State,
    Drag,
    Minimize,
    ToggleMaximize,
    Close,
    ConfirmClose,
}

/// Output of `window_platform`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPlatform {
    pub os: String,
    pub tiling: bool,
}

/// Output of `settings_get`. Same JSON shape as the reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    pub theme: Value,
    pub recents: Vec<Value>,
    pub backend: Option<Value>,
    pub default_folder: String,
    pub image_format: Value,
}

/// Output of `subtitle_editor_open` (reference: `{"document", "path"}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleEditorOpened {
    pub document: Document,
    pub path: PathBuf,
}

/// Output of `artwork_import` (reference `ImportedCover`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedCover {
    pub asset: Asset,
    pub artwork: Artwork,
}

/// Output of `resource_download` (reference `ImportResult`, tagged by `kind`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportResult {
    Artwork { artwork: Artwork },
    Subtitles { files: Vec<SubtitleFile> },
}

/// Argument bundle for `export_start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportStartRequest {
    pub project: Project,
    pub approve_conversion: bool,
}

/// Argument bundle for `settings_update`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsUpdateRequest {
    pub key: String,
    pub value: Value,
}

/// Argument bundle for `artwork_edit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtworkEditRequest {
    pub artwork: Artwork,
    pub edits: ImageEdits,
}

/// Argument bundle for `poster_search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosterSearchRequest {
    pub query: String,
    pub proportion: PosterProportion,
    pub offset: u32,
}

/// Argument bundle for `poster_image`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosterImageRequest {
    pub url: String,
    pub proportion: Option<PosterProportion>,
}

/// Argument bundle for `subtitle_preview`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitlePreviewRequest {
    pub project: Project,
    pub track_id: String,
}

/// Argument bundle for `resource_queue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceQueueRequest {
    pub kind: SearchKind,
    pub url: String,
}

/// Argument bundle for `search_browser_open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchBrowserRequest {
    pub kind: SearchKind,
    pub query: String,
}

/// Argument bundle for `subtitle_editor_draft` (`clear` wins; then `document`; else read).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleEditorDraftRequest {
    pub document: Option<Document>,
    pub clear: bool,
}

/// Argument bundle for `subtitle_model_start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleModelStartRequest {
    pub model: String,
    pub document: Option<Document>,
    pub language: String,
}

/// Output of `close_requested`. The host blocks native close when `Blocked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseDecision {
    Blocked,
    Allowed,
}

/// A point in host logical coordinates (drop position).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A size in host pixels (drop viewport).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// Severity of a user notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Phase of an in-app dependency repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairPhase {
    Started,
    Finished,
    Failed,
}

/// Events delivered over the engine channel. Events are notifications only:
/// authoritative state is always re-read through the commands (jobs live in SQLite).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum EngineEvent {
    /// Job progress/result changed; re-read `jobs_list` / `job_project`. Was `export:updated`.
    #[serde(rename = "export:updated")]
    ExportUpdated { id: String },
    /// Files dropped on the window, already authorized. Was `media:dropped`.
    #[serde(rename = "media:dropped")]
    MediaDropped {
        paths: Vec<String>,
        position: Point,
        viewport: Option<Size>,
    },
    /// Close was blocked by active work. Was `app:close-blocked`.
    #[serde(rename = "app:close-blocked")]
    CloseBlocked,
    /// Close may proceed to the confirmation flow. Was `app:close-requested`.
    #[serde(rename = "app:close-requested")]
    CloseRequested,
    /// Runtime component state changed. Was `dependencies:changed`.
    #[serde(rename = "dependencies:changed")]
    DependenciesChanged,
    /// A resource offer is ready for local confirmation. Was `resource:download-requested`.
    #[serde(rename = "resource:download-requested")]
    ResourceDownloadRequested { info: PendingInfo },
    /// A resource download failed. Was `resource:error`.
    #[serde(rename = "resource:error")]
    ResourceError { error: String },
    /// A preview stream was registered and can be played from `url` (new in the engine API).
    #[serde(rename = "preview:ready")]
    PreviewReady { path: PathBuf, url: String },
    /// Dependency repair progress (new in the engine API).
    #[serde(rename = "dependencies:repair")]
    DependencyRepair { phase: RepairPhase, message: String },
    /// A user-facing notification (new in the engine API).
    #[serde(rename = "notification")]
    Notification {
        level: NotificationLevel,
        text: String,
    },
}
