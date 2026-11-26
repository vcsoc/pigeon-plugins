//! `ReelioEngine`: the concrete [`Engine`]. Every method delegates to a command module; the
//! behaviour is the reference Tauri command behaviour with Tauri removed.

use crate::api::Engine;
use crate::context::Ctx;
use crate::host::{ConfirmDialog, ConfirmKind, EngineHost, FileDialog};
use crate::types::*;
use crate::{preferences, preview, resources, subtitles, updates, version};
use reelio_application::{dependencies::InstallPlan, Application};
use reelio_domain::{plan, Project};
use reelio_ffmpeg::MediaBackend;
use reelio_subtitles::local_model::Service as SubtitleService;
use reelio_updates::UpdateService;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{atomic::Ordering, mpsc, Arc};

/// Fixed project homepage (former `open_project_homepage` target).
pub const HOMEPAGE_URL: &str = "https://github.com/vcsoc/reelio";

/// Bounded interface zoom (former `interface_zoom_factor`).
pub fn interface_zoom_factor(percent: u16) -> EngineResult<f64> {
    if !(50..=200).contains(&percent) {
        return Err("Interface zoom must be between 50% and 200%".into());
    }
    Ok(f64::from(percent) / 100.0)
}

/// The concrete engine. Cheap to share through `Arc`.
pub struct ReelioEngine {
    ctx: Arc<Ctx>,
}

impl ReelioEngine {
    /// Opens the engine under the host's data directory (`reelio.sqlite3`, `subtitles/`).
    ///
    /// Opening or migrating the database never resets it: a failure is returned as an error.
    pub fn new(host: Arc<dyn EngineHost>) -> Result<(Self, mpsc::Receiver<EngineEvent>), String> {
        let data_dir = host.data_dir();
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let app = Application::new(&data_dir.join("reelio.sqlite3"))?;
        let subtitles = SubtitleService::new(data_dir.join("subtitles"))?;
        let (events, receiver) = mpsc::channel();
        let ctx = Ctx {
            app: Arc::new(app),
            host,
            events,
            preview: Arc::new(preview::PreviewService::default()),
            updates: Arc::new(UpdateService::default()),
            subtitles,
        };
        Ok((Self { ctx: Arc::new(ctx) }, receiver))
    }

    fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    fn dependency_repair_message(plan: &InstallPlan) -> String {
        format!(
            "Reelio needs these system components:\n\n{}\n\nInstall or repair them using {}? System authentication may be required. Do not close Reelio while the package manager is running. Source media will not be changed.",
            plan.packages.join(", "),
            plan.executable
        )
    }

    /// Any active export, repair or update download blocks closing (former `CloseRequested` guard).
    fn close_blocked(&self) -> bool {
        self.ctx.app.active_job()
            || self.ctx.app.dependencies.is_repairing()
            || self.ctx.updates.downloading()
    }
}

impl Engine for ReelioEngine {
    fn app_version(&self) -> String {
        version::app_version()
    }

    fn open_project_homepage(&self) -> EngineResult<()> {
        self.ctx().host.open_url(HOMEPAGE_URL)
    }

    fn interface_zoom(&self, percent: u16) -> EngineResult<()> {
        let factor = interface_zoom_factor(percent)?;
        self.ctx().host.set_interface_zoom(factor)?;
        tracing::info!(percent, "Interface zoom applied");
        Ok(())
    }

    fn window_action(&self, action: WindowAction) -> EngineResult<bool> {
        match action {
            WindowAction::ConfirmClose => {
                let ctx = self.ctx();
                if ctx.app.active_job()
                    || ctx.app.dependencies.is_repairing()
                    || ctx.updates.downloading()
                    || ctx.subtitles.is_active()
                {
                    return Err(
                        "Finish or cancel the active operation before quitting Reelio.".into(),
                    );
                }
                if ctx.subtitles.protected() && ctx.subtitles.draft()?.is_none() {
                    return Err("Save the subtitle recovery draft before quitting Reelio.".into());
                }
                ctx.host.window_control(action)?;
                Ok(false)
            }
            // Native and custom close requests share the frontend confirmation flow.
            WindowAction::Close => {
                self.ctx().host.window_control(action)?;
                Ok(false)
            }
            other => self.ctx().host.window_control(other),
        }
    }

    fn window_platform(&self) -> WindowPlatform {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        WindowPlatform {
            os: std::env::consts::OS.into(),
            tiling: cfg!(target_os = "linux")
                && (desktop.contains("hyprland") || std::env::var_os("OMARCHY_PATH").is_some()),
        }
    }

    fn close_requested(&self) -> CloseDecision {
        if self.close_blocked() {
            self.ctx().emit(EngineEvent::CloseBlocked);
            CloseDecision::Blocked
        } else {
            self.ctx().emit(EngineEvent::CloseRequested);
            CloseDecision::Allowed
        }
    }

    fn media_dropped(
        &self,
        paths: Vec<PathBuf>,
        position: Point,
        viewport: Option<Size>,
    ) -> Vec<String> {
        let ctx = self.ctx();
        let mut granted = vec![];
        for path in paths.iter().take(256) {
            if path.is_file() {
                if let Ok(p) = ctx.grant(path) {
                    granted.push(p.to_string_lossy().into_owned());
                }
            }
        }
        ctx.emit(EngineEvent::MediaDropped {
            paths: granted.clone(),
            position,
            viewport,
        });
        granted
    }

    fn backend_status(&self) -> EngineResult<reelio_ffmpeg::Capabilities> {
        self.ctx().app.backend.capabilities()
    }

    fn dependencies_status(&self) -> EngineResult<DependencyReport> {
        let ctx = self.ctx();
        let report = ctx.app.dependencies.status(&ctx.app.backend);
        tracing::info!(
            preview_available = report.preview_available,
            backend_available = report.backend_available,
            network_available = report.network_available,
            missing_count = report.missing_components.len(),
            "Runtime component check completed"
        );
        Ok(report)
    }

    fn dependencies_repair(&self) -> EngineResult<DependencyReport> {
        let ctx = self.ctx();
        ctx.emit(EngineEvent::DependencyRepair {
            phase: RepairPhase::Started,
            message: "Checking the repair plan".into(),
        });
        let result = ctx.app.dependencies.install(|plan| {
            ctx.host.confirm(&ConfirmDialog {
                title: "Install required Reelio components".into(),
                message: Self::dependency_repair_message(plan),
                kind: ConfirmKind::Warning,
            })
        });
        match result {
            Ok(()) => ctx.emit(EngineEvent::DependencyRepair {
                phase: RepairPhase::Finished,
                message: "Repair finished".into(),
            }),
            Err(ref error) => ctx.emit(EngineEvent::DependencyRepair {
                phase: RepairPhase::Failed,
                message: error.clone(),
            }),
        }
        ctx.emit(EngineEvent::DependenciesChanged);
        result?;
        Ok(ctx.app.dependencies.status(&ctx.app.backend))
    }

    fn choose_media(&self, folder: bool) -> EngineResult<Vec<String>> {
        let ctx = self.ctx();
        let start = ctx.default_folder()?;
        let mut paths = vec![];
        if folder {
            let Some(dir) = ctx.host.pick_folder(&FileDialog {
                title: None,
                directory: Some(start),
                file_name: None,
                filters: vec![],
                multiple: false,
            }) else {
                return Ok(paths);
            };
            // Deliberately non-recursive; directory imports have a hard entry limit.
            for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.take(256) {
                let p = entry.map_err(|e| e.to_string())?.path();
                if p.is_file() {
                    paths.push(ctx.grant(&p)?.to_string_lossy().into_owned());
                }
            }
        } else {
            let Some(files) = ctx.host.pick_files(&FileDialog {
                title: Some("Open media or add tracks".into()),
                directory: Some(start),
                file_name: None,
                filters: vec![],
                multiple: true,
            }) else {
                return Ok(paths);
            };
            for f in files {
                paths.push(ctx.grant(&f)?.to_string_lossy().into_owned());
            }
        }
        Ok(paths)
    }

    fn media_probe(&self, path: PathBuf) -> EngineResult<Asset> {
        self.ctx().app.import(&path)
    }

    fn preview_source(&self, path: PathBuf) -> EngineResult<String> {
        preview::preview_source(self.ctx(), path)
    }

    fn project_create(&self) -> Project {
        Project::default()
    }

    fn project_validate(&self, project: Project) -> reelio_domain::ExportPlan {
        plan(&project)
    }

    fn project_open(&self) -> EngineResult<Option<Project>> {
        let ctx = self.ctx();
        let Some(files) = ctx.host.pick_files(&FileDialog {
            title: None,
            directory: Some(ctx.default_folder()?),
            file_name: None,
            filters: vec![("Reelio project".into(), vec!["reelio".into()])],
            multiple: false,
        }) else {
            return Ok(None);
        };
        let Some(path) = files.into_iter().next() else {
            return Ok(None);
        };
        let project = ctx.app.open_project_with_sources(&path)?;
        // Explicitly opening a validated project grants its individual referenced files, not their directories.
        for a in &project.assets {
            if a.path.is_file() {
                ctx.grant(&a.path)?;
            }
        }
        ctx.app
            .storage
            .lock()
            .map_err(|_| "Storage lock failed")?
            .recent(&path.to_string_lossy(), "project")?;
        Ok(Some(project))
    }

    fn project_save(&self, project: Project) -> EngineResult<Option<String>> {
        let ctx = self.ctx();
        let Some(path) = ctx.host.save_file(&FileDialog {
            title: None,
            directory: Some(ctx.default_folder()?),
            file_name: Some("Untitled.reelio".into()),
            filters: vec![("Reelio project".into(), vec!["reelio".into()])],
            multiple: false,
        }) else {
            return Ok(None);
        };
        ctx.app.save_project(&project, &path)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }

    fn export_plan(&self, project: Project) -> EngineResult<reelio_domain::ExportPlan> {
        self.ctx().app.validate_sources(&project)?;
        Ok(plan(&project))
    }

    fn export_start(&self, request: ExportStartRequest) -> EngineResult<Option<String>> {
        let ExportStartRequest {
            project,
            approve_conversion,
        } = request;
        let ctx = self.ctx();
        ctx.app.validate_sources(&project)?;
        let export_plan = plan(&project);
        if !export_plan.errors.is_empty() {
            return Err(export_plan.errors.join("; "));
        }
        if export_plan
            .tracks
            .iter()
            .any(|t| t.compatibility.target_codec.is_some())
            && !approve_conversion
        {
            return Err("Approve subtitle conversion before exporting".into());
        }
        let filename = format!(
            "{}.{}",
            project
                .name
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                })
                .collect::<String>(),
            project.container.extension()
        );
        let Some(destination) = ctx.host.save_file(&FileDialog {
            title: Some("Export to a NEW file (never overwrites)".into()),
            directory: Some(ctx.default_folder()?),
            file_name: Some(filename),
            filters: vec![],
            multiple: false,
        }) else {
            return Ok(None);
        };
        if destination.exists() {
            return Err("Destination exists. Choose a new filename.".into());
        }
        let (id, cancel) = ctx.app.reserve_job(&project, &destination)?;
        let job_id = id.clone();
        let worker = ctx.app.clone();
        let events = ctx.events.clone();
        std::thread::spawn(move || {
            let emit = |id: &str| {
                let _ = events.send(EngineEvent::ExportUpdated { id: id.to_string() });
            };
            let mut completed_progress = None;
            let result = (|| {
                worker.update_job(&job_id, "processing", None, None)?;
                worker.backend.export(
                    &project,
                    &destination,
                    approve_conversion,
                    &cancel,
                    |progress| {
                        let phase = progress.phase.clone();
                        let complete = progress.percent == Some(100.0);
                        let mut value = json!(progress);
                        if complete {
                            // Commit the final crop report atomically with the completed status.
                            value["phase"] = json!("completed");
                            completed_progress = Some(value);
                            return;
                        }
                        if let Err(error) = worker.update_job(&job_id, &phase, Some(&value), None) {
                            tracing::error!(%error, "Could not persist export progress");
                        }
                        emit(&job_id);
                    },
                )
            })();
            let (status, error) = match result {
                Ok(_) => ("completed", None),
                Err(e) => {
                    if cancel.load(Ordering::SeqCst) {
                        ("cancelled", Some(e))
                    } else {
                        ("failed", Some(e))
                    }
                }
            };
            if let Err(e) = worker.update_job(
                &job_id,
                status,
                if status == "completed" {
                    completed_progress.as_ref()
                } else {
                    None
                },
                error.as_deref(),
            ) {
                tracing::error!(error = %e, "Could not persist export result");
            }
            let _ = worker.finish(&job_id);
            emit(&job_id);
        });
        Ok(Some(id))
    }

    fn export_cancel(&self, id: String) -> EngineResult<()> {
        self.ctx().app.cancel(&id)
    }

    fn jobs_list(&self) -> EngineResult<Vec<reelio_storage::Job>> {
        self.ctx().app.jobs()
    }

    fn job_project(&self, id: String) -> EngineResult<Project> {
        let ctx = self.ctx();
        let value = ctx
            .app
            .storage
            .lock()
            .map_err(|_| "Storage lock failed")?
            .job_project(&id)?;
        let mut project: Project = serde_json::from_value(value).map_err(|e| e.to_string())?;
        project.upgrade_legacy()?;
        let errors = project.validate();
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        if let Some(artwork) = &project.artwork {
            reelio_application::artwork::jpeg_bytes(artwork)?;
        }
        let paths = project
            .assets
            .iter()
            .map(|a| a.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        if !project.assets.is_empty()
            && !ctx.host.confirm(&ConfirmDialog {
                title: "Restore job settings".into(),
                message: format!("Restore this job and authorize access to these sources?\n\n{paths}\n\nMissing sources can be relinked. Export still requires a new destination."),
                kind: ConfirmKind::Info,
            })
        {
            return Err("Job source access was not authorized".into());
        }
        for asset in &project.assets {
            if asset.path.is_file() {
                ctx.grant(&asset.path)?;
            }
        }
        Ok(project)
    }

    fn settings_get(&self) -> EngineResult<SettingsView> {
        let ctx = self.ctx();
        // Resolve the folder before locking storage: the resolver takes the same lock.
        let folder = ctx.default_folder()?;
        let db = ctx.app.storage.lock().map_err(|_| "Storage lock failed")?;
        Ok(SettingsView {
            theme: db.setting("theme")?.unwrap_or(json!("dark")),
            recents: db.recents()?,
            backend: db.setting("backend")?,
            default_folder: folder.to_string_lossy().into_owned(),
            image_format: db.setting("image_format")?.unwrap_or(json!("png")),
        })
    }

    fn settings_choose_folder(&self) -> EngineResult<Option<String>> {
        preferences::choose_folder(self.ctx())
    }

    fn settings_update(&self, request: SettingsUpdateRequest) -> EngineResult<()> {
        let SettingsUpdateRequest { key, value } = request;
        match key.as_str() {
            "image_format" => {
                if !["png", "jpeg", "webp"].contains(&value.as_str().unwrap_or("")) {
                    return Err("Choose PNG, JPEG or WebP".into());
                }
            }
            "default_folder" => preferences::validate_folder(&value)?,
            "theme" => {
                if !["light", "dark", "system"].contains(&value.as_str().unwrap_or("")) {
                    return Err("Invalid theme".into());
                }
            }
            "backend" => {
                for k in ["ffmpeg", "ffprobe"] {
                    let p = value[k].as_str().ok_or("Missing backend path")?;
                    if !std::path::Path::new(p).is_absolute() || !std::path::Path::new(p).is_file()
                    {
                        return Err("Backend executables must be existing absolute paths".into());
                    }
                }
            }
            _ => return Err("Unknown setting".into()),
        }
        self.ctx()
            .app
            .storage
            .lock()
            .map_err(|_| "Storage lock failed")?
            .set_setting(&key, &value)
    }

    fn choose_artwork(&self) -> EngineResult<Option<reelio_domain::Artwork>> {
        resources::choose_artwork(self.ctx())
    }

    fn artwork_import(&self, path: PathBuf) -> EngineResult<ImportedCover> {
        resources::artwork_import(self.ctx(), path)
    }

    fn artwork_edit(&self, request: ArtworkEditRequest) -> EngineResult<reelio_domain::Artwork> {
        resources::artwork_edit(request.artwork, request.edits)
    }

    fn artwork_save_copy(&self, artwork: reelio_domain::Artwork) -> EngineResult<Option<String>> {
        resources::artwork_save_copy(self.ctx(), artwork)
    }

    fn poster_search(&self, request: PosterSearchRequest) -> EngineResult<PosterBatch> {
        resources::poster_search(request.query, request.proportion, request.offset)
    }

    fn poster_image(&self, request: PosterImageRequest) -> EngineResult<reelio_domain::Artwork> {
        resources::poster_image(request.url, request.proportion)
    }

    fn choose_subtitles(&self) -> EngineResult<Vec<String>> {
        resources::choose_subtitles(self.ctx())
    }

    fn subtitle_preview(&self, request: SubtitlePreviewRequest) -> EngineResult<String> {
        resources::subtitle_preview(self.ctx(), request)
    }

    fn resource_queue(&self, request: ResourceQueueRequest) -> EngineResult<PendingInfo> {
        resources::resource_queue(self.ctx(), request)
    }

    fn resource_cancel(&self, id: String) -> EngineResult<()> {
        resources::resource_cancel(self.ctx(), id)
    }

    fn resource_download(&self, id: String) -> EngineResult<ImportResult> {
        resources::resource_download(self.ctx(), id)
    }

    fn search_browser_open(&self, request: SearchBrowserRequest) -> EngineResult<()> {
        resources::search_browser_open(self.ctx(), request)
    }

    fn search_browser_allows(&self, url: String) -> bool {
        resources::search_browser_allows(&url)
    }

    fn search_browser_download(
        &self,
        request: SearchBrowserDownloadRequest,
    ) -> EngineResult<PendingInfo> {
        resources::search_browser_download(self.ctx(), request)
    }

    fn update_check(&self) -> EngineResult<Report> {
        updates::update_check(self.ctx())
    }

    fn update_download(&self, offer_id: String) -> EngineResult<Option<String>> {
        updates::update_download(self.ctx(), offer_id)
    }

    fn update_cancel(&self) {
        updates::update_cancel(self.ctx())
    }

    fn subtitle_editor_open(
        &self,
        path: Option<PathBuf>,
    ) -> EngineResult<Option<SubtitleEditorOpened>> {
        subtitles::subtitle_editor_open(self.ctx(), path)
    }

    fn subtitle_editor_save(&self, document: Document) -> EngineResult<Option<String>> {
        subtitles::subtitle_editor_save(self.ctx(), document)
    }

    fn subtitle_editor_dirty(&self, dirty: bool) {
        subtitles::subtitle_editor_dirty(self.ctx(), dirty)
    }

    fn subtitle_editor_draft(
        &self,
        request: SubtitleEditorDraftRequest,
    ) -> EngineResult<Option<Document>> {
        subtitles::subtitle_editor_draft(self.ctx(), request)
    }

    fn subtitle_models(&self) -> EngineResult<Value> {
        subtitles::subtitle_models(self.ctx())
    }

    fn subtitle_model_start(&self, request: SubtitleModelStartRequest) -> EngineResult<JobInfo> {
        subtitles::subtitle_model_start(self.ctx(), request)
    }

    fn subtitle_model_status(&self) -> EngineResult<JobInfo> {
        subtitles::subtitle_model_status(self.ctx())
    }

    fn subtitle_model_cancel(&self) {
        subtitles::subtitle_model_cancel(self.ctx())
    }

    fn subtitle_model_results(&self, id: String) -> EngineResult<Value> {
        subtitles::subtitle_model_results(self.ctx(), id)
    }
}

#[cfg(test)]
mod tests {
    use super::interface_zoom_factor;

    #[test]
    fn accepts_bounded_interface_scales() {
        assert_eq!(interface_zoom_factor(50).unwrap(), 0.5);
        assert_eq!(interface_zoom_factor(100).unwrap(), 1.0);
        assert_eq!(interface_zoom_factor(200).unwrap(), 2.0);
    }

    #[test]
    fn rejects_out_of_range_interface_scales() {
        for percent in [0, 49, 201, u16::MAX] {
            assert!(interface_zoom_factor(percent).is_err());
        }
    }
}
