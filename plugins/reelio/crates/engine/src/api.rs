//! The Tauri-free Reelio API. One method per former Tauri command (see docs/command-parity.md).
//!
//! Threading: every method is synchronous and may block (disk, FFmpeg probes, SQLite, dialogs
//! through the host). Callers must invoke them from worker threads, never the UI thread.
//! Long operations (export, model download, resource download, preview registration) return
//! immediately with a job/offer id and report through [`crate::EngineEvent`]s plus re-readable state.

use crate::types::*;
use serde_json::Value;
use std::path::PathBuf;

/// The complete engine surface. Object-safe so a host or a test can substitute a fake.
pub trait Engine: Send + Sync {
    // ---- application and window (window actions are delegated to the host) ----

    /// Former `app_version`: the compiled `<cargo version>-<package revision>` string.
    fn app_version(&self) -> String;
    /// Former `open_project_homepage`: opens the fixed GitHub project URL.
    fn open_project_homepage(&self) -> EngineResult<()>;
    /// Former `interface_zoom`: percent must be 50..=200; applied through the host.
    fn interface_zoom(&self, percent: u16) -> EngineResult<()>;
    /// Former `window_action`: bounded action; close/confirm-close check active work first.
    fn window_action(&self, action: WindowAction) -> EngineResult<bool>;
    /// Former `window_platform`.
    fn window_platform(&self) -> WindowPlatform;
    /// Former window close-requested handler. Emits `CloseBlocked` or `CloseRequested`.
    fn close_requested(&self) -> CloseDecision;
    /// Former drag-and-drop handler: authorizes up to 256 dropped regular files, emits
    /// `MediaDropped`, and returns the authorized canonical paths.
    fn media_dropped(
        &self,
        paths: Vec<PathBuf>,
        position: Point,
        viewport: Option<Size>,
    ) -> Vec<String>;

    // ---- backend and runtime components ----

    /// Former `backend_status`: discovered FFmpeg/FFprobe capabilities.
    fn backend_status(&self) -> EngineResult<Capabilities>;
    /// Former `dependencies_status`.
    fn dependencies_status(&self) -> EngineResult<DependencyReport>;
    /// Former `dependencies_repair`: asks the host for confirmation, then runs the fixed plan.
    fn dependencies_repair(&self) -> EngineResult<DependencyReport>;

    // ---- media ----

    /// Former `choose_media(folder)`: host picker; folders are non-recursive, max 256 entries.
    fn choose_media(&self, folder: bool) -> EngineResult<Vec<String>>;
    /// Former `media_probe`: authorizes and probes one file.
    fn media_probe(&self, path: PathBuf) -> EngineResult<Asset>;
    /// Former `preview_source`: registers an authorized source for loopback range playback and
    /// returns its URL. Also emits `PreviewReady`.
    fn preview_source(&self, path: PathBuf) -> EngineResult<String>;

    // ---- projects ----

    /// Former `project_create`.
    fn project_create(&self) -> Project;
    /// Former `project_validate`: pure planning, no I/O.
    fn project_validate(&self, project: Project) -> ExportPlan;
    /// Former `project_open`: host picker, validated load, source authorization.
    fn project_open(&self) -> EngineResult<Option<Project>>;
    /// Former `project_save`: host save dialog, atomic publication; returns the saved path.
    fn project_save(&self, project: Project) -> EngineResult<Option<String>>;

    // ---- export and jobs ----

    /// Former `export_plan`: validates sources, then plans.
    fn export_plan(&self, project: Project) -> EngineResult<ExportPlan>;
    /// Former `export_start`: returns the job id, or `None` if the host dialog was cancelled.
    fn export_start(&self, request: ExportStartRequest) -> EngineResult<Option<String>>;
    /// Former `export_cancel`.
    fn export_cancel(&self, id: String) -> EngineResult<()>;
    /// Former `jobs_list`: metadata-only durable job summaries.
    fn jobs_list(&self) -> EngineResult<Vec<Job>>;
    /// Former `job_project`: restores an authorized, validated job snapshot (asks first).
    fn job_project(&self, id: String) -> EngineResult<Project>;

    // ---- settings ----

    /// Former `settings_get`.
    fn settings_get(&self) -> EngineResult<SettingsView>;
    /// Former `settings_choose_folder`.
    fn settings_choose_folder(&self) -> EngineResult<Option<String>>;
    /// Former `settings_update`: whitelisted keys only.
    fn settings_update(&self, request: SettingsUpdateRequest) -> EngineResult<()>;

    // ---- artwork and posters ----

    /// Former `choose_artwork`.
    fn choose_artwork(&self) -> EngineResult<Option<Artwork>>;
    /// Former `artwork_import`.
    fn artwork_import(&self, path: PathBuf) -> EngineResult<ImportedCover>;
    /// Former `artwork_edit`.
    fn artwork_edit(&self, request: ArtworkEditRequest) -> EngineResult<Artwork>;
    /// Former `artwork_save_copy`: new PNG/JPEG/WebP copy, never overwrites.
    fn artwork_save_copy(&self, artwork: Artwork) -> EngineResult<Option<String>>;
    /// Former `poster_search`: inert metadata search, bounded.
    fn poster_search(&self, request: PosterSearchRequest) -> EngineResult<PosterBatch>;
    /// Former `poster_image`: downloads and normalizes one result image.
    fn poster_image(&self, request: PosterImageRequest) -> EngineResult<Artwork>;

    // ---- subtitles attached to media ----

    /// Former `choose_subtitles`.
    fn choose_subtitles(&self) -> EngineResult<Vec<String>>;
    /// Former `subtitle_preview`: WebVTT preview of a text subtitle track.
    fn subtitle_preview(&self, request: SubtitlePreviewRequest) -> EngineResult<String>;

    // ---- resources (confirmed browser-found downloads) ----

    /// Former `resource_queue`: registers an expiring, opaque offer.
    fn resource_queue(&self, request: ResourceQueueRequest) -> EngineResult<PendingInfo>;
    /// Former `resource_cancel`.
    fn resource_cancel(&self, id: String) -> EngineResult<()>;
    /// Former `resource_download`: downloads and validates a previously queued offer.
    fn resource_download(&self, id: String) -> EngineResult<ImportResult>;
    /// Former `search_browser_open`: opens the isolated browser via the host.
    fn search_browser_open(&self, request: SearchBrowserRequest) -> EngineResult<()>;
    /// Navigation/popup policy for the isolated search browser (former `resources::allowed_url`).
    fn search_browser_allows(&self, url: String) -> bool;
    /// Former browser download hook: queues a download offer for local confirmation. Emits
    /// `ResourceDownloadRequested` or `ResourceError`. Web content never downloads directly.
    fn search_browser_download(
        &self,
        request: SearchBrowserDownloadRequest,
    ) -> EngineResult<PendingInfo>;

    // ---- updates ----

    /// Former `update_check`.
    fn update_check(&self) -> EngineResult<Report>;
    /// Former `update_download`: host save dialog, verified no-clobber download.
    fn update_download(&self, offer_id: String) -> EngineResult<Option<String>>;
    /// Former `update_cancel`.
    fn update_cancel(&self);

    // ---- subtitle editor and local models ----

    /// Former `subtitle_editor_open`.
    fn subtitle_editor_open(
        &self,
        path: Option<PathBuf>,
    ) -> EngineResult<Option<SubtitleEditorOpened>>;
    /// Former `subtitle_editor_save`: new SRT copy via host save dialog.
    fn subtitle_editor_save(&self, document: Document) -> EngineResult<Option<String>>;
    /// Former `subtitle_editor_dirty`: marks unsaved subtitle work for close protection.
    fn subtitle_editor_dirty(&self, dirty: bool);
    /// Former `subtitle_editor_draft`.
    fn subtitle_editor_draft(
        &self,
        request: SubtitleEditorDraftRequest,
    ) -> EngineResult<Option<Document>>;
    /// Former `subtitle_models`.
    fn subtitle_models(&self) -> EngineResult<Value>;
    /// Former `subtitle_model_start`.
    fn subtitle_model_start(&self, request: SubtitleModelStartRequest) -> EngineResult<JobInfo>;
    /// Former `subtitle_model_status`.
    fn subtitle_model_status(&self) -> EngineResult<JobInfo>;
    /// Former `subtitle_model_cancel`.
    fn subtitle_model_cancel(&self);
    /// Former `subtitle_model_results`.
    fn subtitle_model_results(&self, id: String) -> EngineResult<Value>;
}
