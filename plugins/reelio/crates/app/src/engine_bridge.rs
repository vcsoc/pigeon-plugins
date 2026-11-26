//! Engine bridge: maps every UI [`Request`] onto a `reelio_engine::Engine` method.
//!
//! The engine is blocking; the transport runs each request on its own worker thread, so the
//! plugin's protocol loop never waits on media work. Native dialogs and URL opening belong to the
//! Pigeon host (`dialogs.*`, `shell.*` services); until those services are wired through the SDK
//! the [`PluginHost`] refuses them with an explicit message instead of guessing.

use pigeon_plugin_sdk::runtime::Ctx;
use reelio_engine::{
    ConfirmDialog, Engine, EngineHost, ExportStartRequest, FileDialog, Project, ReelioEngine,
    SettingsUpdateRequest, SubtitlePreviewRequest, WindowAction,
};
use reelio_ui::engine::{Reply, Request};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// The engine seam used by the transport. Implementations must be thread-safe.
pub trait EngineBridge: Send + Sync + 'static {
    fn call(&self, request: Request) -> Reply;
}

/// Host services the engine needs, served by the Pigeon host through the SDK (`dialogs` and `shell`
/// permissions in `plugin.json`). Engine calls run on worker threads, so the blocking `Ctx::call`
/// waits for the user without stalling the plugin's protocol thread.
pub struct PluginHost {
    data_dir: PathBuf,
    videos_dir: Option<PathBuf>,
    ctx: Option<Ctx>,
    consent: Option<std::sync::mpsc::SyncSender<crate::consent::Ask>>,
}

impl PluginHost {
    pub fn new(data_dir: PathBuf, ctx: Option<Ctx>) -> Self {
        let videos_dir = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Movies"));
        Self {
            data_dir,
            videos_dir,
            ctx,
            consent: None,
        }
    }

    #[cfg(test)]
    pub fn new_for_tests(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            videos_dir: None,
            ctx: None,
            consent: None,
        }
    }

    pub fn from_env(ctx: Ctx, consent: std::sync::mpsc::SyncSender<crate::consent::Ask>) -> Self {
        let data_dir = std::env::var_os("PIGEON_PLUGIN_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".data"));
        let mut host = Self::new(data_dir, Some(ctx));
        host.consent = Some(consent);
        host
    }

    fn dialog(&self, method: &str, request: &FileDialog) -> Result<Value, String> {
        let ctx = self
            .ctx
            .as_ref()
            .ok_or("The Pigeon host dialog service is not available in this context.")?;
        let filters: Vec<Value> = request
            .filters
            .iter()
            .map(|(name, exts)| json!({"name": name, "extensions": exts}))
            .collect();
        let params = json!({
            "filters": filters,
            "multiple": request.multiple,
        });
        let mut params = params;
        if let Some(title) = &request.title {
            params["title"] = json!(title);
        }
        if let Some(path) = &request.directory {
            params["directory"] = json!(path);
        }
        if let Some(name) = &request.file_name {
            params["file_name"] = json!(name);
        }
        ctx.call(method, params).map_err(|e| e.to_string())
    }

    fn paths(value: &Value) -> Vec<PathBuf> {
        value
            .get("paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_else(|| {
                value["path"]
                    .as_str()
                    .map(PathBuf::from)
                    .into_iter()
                    .collect()
            })
    }
}

impl EngineHost for PluginHost {
    fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }
    fn videos_dir(&self) -> Option<PathBuf> {
        self.videos_dir.clone()
    }
    fn pick_files(&self, request: &FileDialog) -> Option<Vec<PathBuf>> {
        let paths = Self::paths(&self.dialog("dialog.open_file", request).ok()?);
        (!paths.is_empty()).then_some(paths)
    }
    fn pick_folder(&self, request: &FileDialog) -> Option<PathBuf> {
        let value = self.dialog("dialog.pick_folder", request).ok()?;
        Self::paths(&value).into_iter().next()
    }
    fn save_file(&self, request: &FileDialog) -> Option<PathBuf> {
        let value = self.dialog("dialog.save_file", request).ok()?;
        Self::paths(&value).into_iter().next()
    }
    fn confirm(&self, request: &ConfirmDialog) -> bool {
        self.consent
            .as_ref()
            .is_some_and(|tx| crate::consent::ask(tx, request.clone()))
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        let ctx = self
            .ctx
            .as_ref()
            .ok_or("The Pigeon shell service is not available in this context.")?;
        ctx.open_url(url).map_err(|e| e.to_string())
    }
    fn open_search_browser(&self, url: &str) -> Result<(), String> {
        // Protocol v2 does not expose a webview or browser cookies. Open only the
        // engine-generated search URL; users can paste a public URL or import a local file.
        self.open_url(url)
    }
    fn window_control(&self, _action: WindowAction) -> Result<bool, String> {
        Ok(false)
    }
    fn set_interface_zoom(&self, _factor: f64) -> Result<(), String> {
        Ok(())
    }
}

/// The real engine (`reelio-engine`).
pub struct Real {
    engine: ReelioEngine,
}

impl Real {
    /// Start the engine. The receiver carries `EngineEvent`s (previews, repairs, notifications).
    pub fn start(
        host: PluginHost,
    ) -> Result<(Self, std::sync::mpsc::Receiver<reelio_engine::EngineEvent>), String> {
        let (engine, events) = ReelioEngine::new(Arc::new(host))?;
        Ok((Self { engine }, events))
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("Invalid project data: {e}"))
}

fn encode<T: Serialize>(result: Result<T, String>) -> Reply {
    result.and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))
}

impl EngineBridge for Real {
    fn call(&self, request: Request) -> Reply {
        let e = &self.engine;
        match request {
            Request::BackendStatus => encode(e.backend_status()),
            Request::DependenciesStatus => encode(e.dependencies_status()),
            Request::DependenciesRepair => encode(e.dependencies_repair()),
            Request::SettingsGet => encode(e.settings_get()),
            Request::SettingsUpdate { key, value } => encode(
                e.settings_update(SettingsUpdateRequest { key, value })
                    .map(|()| Value::Null),
            ),
            Request::SettingsChooseFolder => encode(e.settings_choose_folder()),
            Request::ChooseMedia { folder } => encode(e.choose_media(folder)),
            Request::SubtitleEditorOpen { path } => {
                encode(e.subtitle_editor_open(path.map(PathBuf::from)))
            }
            Request::SubtitleEditorSave { document } => decode::<reelio_engine::Document>(document)
                .and_then(|d| encode(e.subtitle_editor_save(d))),
            Request::PosterSearch {
                query,
                proportion,
                offset,
            } => {
                decode::<reelio_engine::PosterProportion>(Value::String(proportion)).and_then(|p| {
                    encode(e.poster_search(reelio_engine::PosterSearchRequest {
                        query,
                        proportion: p,
                        offset,
                    }))
                })
            }
            Request::PosterImage { url, proportion } => {
                let proportion = match proportion {
                    Some(p) => {
                        decode::<reelio_engine::PosterProportion>(Value::String(p)).map(Some)
                    }
                    None => Ok(None),
                };
                proportion.and_then(|proportion| {
                    encode(e.poster_image(reelio_engine::PosterImageRequest { url, proportion }))
                })
            }
            Request::ArtworkEdit { artwork, edits } => decode::<reelio_engine::Artwork>(artwork)
                .and_then(|a| decode::<reelio_engine::ImageEdits>(edits).map(|edits| (a, edits)))
                .and_then(|(artwork, edits)| {
                    encode(e.artwork_edit(reelio_engine::ArtworkEditRequest { artwork, edits }))
                }),
            Request::SearchBrowserOpen { kind, query } => {
                decode::<reelio_engine::SearchKind>(Value::String(kind)).and_then(|kind| {
                    encode(
                        e.search_browser_open(reelio_engine::SearchBrowserRequest { kind, query })
                            .map(|()| Value::Null),
                    )
                })
            }
            Request::SearchBrowserAllows { url } => encode(Ok(e.search_browser_allows(url))),
            Request::SearchBrowserDownload { kind, url } => {
                if !e.search_browser_allows(url.clone()) {
                    return Err("Enter a valid public HTTPS download URL".into());
                }
                decode::<reelio_engine::SearchKind>(Value::String(kind)).and_then(|kind| {
                    encode(
                        e.search_browser_download(reelio_engine::SearchBrowserDownloadRequest {
                            kind,
                            url,
                            cookie: String::new(),
                        }),
                    )
                })
            }
            Request::SubtitleModels => encode(e.subtitle_models()),
            Request::SubtitleModelStart {
                model,
                document,
                language,
            } => document
                .map(decode::<reelio_engine::Document>)
                .transpose()
                .and_then(|document| {
                    encode(
                        e.subtitle_model_start(reelio_engine::SubtitleModelStartRequest {
                            model,
                            document,
                            language,
                        }),
                    )
                }),
            Request::SubtitleEditorDraft { document, clear } => document
                .map(decode::<reelio_engine::Document>)
                .transpose()
                .and_then(|document| {
                    encode(
                        e.subtitle_editor_draft(reelio_engine::SubtitleEditorDraftRequest {
                            document,
                            clear,
                        }),
                    )
                }),
            Request::SubtitleModelStatus => encode(e.subtitle_model_status()),
            Request::SubtitleModelCancel => {
                e.subtitle_model_cancel();
                Ok(Value::Null)
            }
            Request::SubtitleModelResults { id } => encode(e.subtitle_model_results(id)),
            Request::UpdateCheck => encode(e.update_check()),
            Request::UpdateDownload { offer_id } => encode(e.update_download(offer_id)),
            Request::SubtitleEditorDirty { dirty } => {
                e.subtitle_editor_dirty(dirty);
                Ok(Value::Null)
            }
            Request::ChooseSubtitles => encode(e.choose_subtitles().map(|paths| {
                paths
                    .into_iter()
                    .map(|path| match bounded_subtitle(&path) {
                        Ok(()) => json!({"name": reelio_ui::model::basename(&path), "path": path}),
                        Err(error) => {
                            json!({"error": error, "name": reelio_ui::model::basename(&path)})
                        }
                    })
                    .collect::<Vec<_>>()
            })),
            Request::MediaDropped { paths } => {
                let paths = paths.into_iter().map(PathBuf::from).collect();
                let point = reelio_engine::Point { x: 0.0, y: 0.0 };
                encode(Ok(e.media_dropped(paths, point, None)))
            }
            Request::MediaProbe { path } => encode(e.media_probe(PathBuf::from(path))),
            Request::ProjectValidate { project } => {
                decode::<Project>(project).and_then(|p| encode(Ok(e.project_validate(p))))
            }
            Request::ProjectOpen => encode(e.project_open()),
            Request::ProjectSave { project } => {
                decode::<Project>(project).and_then(|p| encode(e.project_save(p)))
            }
            Request::ExportPlan { project } => {
                decode::<Project>(project).and_then(|p| encode(e.export_plan(p)))
            }
            Request::ExportStart {
                project,
                approve_conversion,
            } => decode::<Project>(project).and_then(|p| {
                encode(e.export_start(ExportStartRequest {
                    project: p,
                    approve_conversion,
                }))
            }),
            Request::ExportCancel { id } => encode(e.export_cancel(id).map(|()| Value::Null)),
            Request::JobsList => encode(e.jobs_list()),
            Request::JobProject { id } => encode(e.job_project(id)),
            Request::ArtworkImport { path } => encode(e.artwork_import(PathBuf::from(path))),
            Request::ChooseArtwork => encode(e.choose_artwork()),
            Request::ArtworkSaveCopy { artwork } => decode::<reelio_engine::Artwork>(artwork)
                .and_then(|art| encode(e.artwork_save_copy(art))),
            Request::SubtitlePreview { project, track_id } => {
                decode::<Project>(project).and_then(|p| {
                    encode(e.subtitle_preview(SubtitlePreviewRequest {
                        project: p,
                        track_id,
                    }))
                })
            }
            Request::PreviewSource { path } => encode(e.preview_source(PathBuf::from(path))),
            Request::ResourceDownload { id } => encode(e.resource_download(id)),
            Request::ResourceCancel { id } => encode(e.resource_cancel(id).map(|()| Value::Null)),
            Request::WindowAction { action } => decode::<WindowAction>(Value::String(action))
                .and_then(|a| encode(e.window_action(a))),
        }
    }
}

fn bounded_subtitle(path: &str) -> Result<(), String> {
    use std::io::Read;
    const LIMIT: u64 = 4 * 1024 * 1024;
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !["srt", "ass", "ssa", "vtt"].contains(&ext.as_str()) {
        return Err("Choose an SRT, ASS, SSA or WebVTT subtitle file.".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err("Subtitles must be regular files of at most 4 MiB.".into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT || bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Err("Subtitles must be UTF-8 text of at most 4 MiB.".into());
    }
    Ok(())
}

/// Used when the engine cannot start (for example an unwritable data folder): every command
/// reports the start-up error, so the UI shows it instead of pretending the engine works.
pub struct Unavailable(pub String);

impl EngineBridge for Unavailable {
    fn call(&self, _request: Request) -> Reply {
        Err(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelio_ui::scene;
    use reelio_ui::state::{Effect, Event, State};
    use std::process::Command;

    /// Synchronous mock host: applies each engine effect with the real engine, feeds replies back.
    fn pump(state: &mut State, bridge: &dyn EngineBridge) {
        loop {
            let effects = state.drain();
            if effects.is_empty() {
                break;
            }
            for effect in effects {
                if let Effect::Engine { id, request } = effect {
                    let reply = bridge.call(request);
                    state.dispatch(Event::Engine { id, reply });
                }
            }
        }
    }

    #[test]
    fn dropped_media_is_probed_by_the_real_engine_and_becomes_a_lane() {
        let Ok(ffmpeg) = Command::new("ffmpeg").arg("-version").output() else {
            eprintln!("ffmpeg not installed; skipping generated-media flow");
            return;
        };
        if !ffmpeg.status.success() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("reelio-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("clip.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x240:rate=25",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-shortest",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
            ])
            .arg(&media)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg generated the fixture");

        let data = dir.join("data");
        let bridge = match Real::start(PluginHost::new_for_tests(data.clone())) {
            Ok((b, _events)) => b,
            Err(e) => panic!("engine start: {e}"),
        };
        let mut state = State::new();
        pump(&mut state, &bridge);
        state.drain();
        state.dispatch(Event::Drop {
            paths: vec![media.to_string_lossy().into_owned()],
        });
        pump(&mut state, &bridge);

        assert_eq!(
            state.project().assets.len(),
            1,
            "probe result added as a source"
        );
        assert!(
            state.project().tracks.iter().any(|t| t.enabled),
            "video and audio lanes enabled"
        );
        let scene = scene::main_view(&state).to_string();
        assert!(
            scene.contains("lane.canvas:"),
            "timeline lane rendered from real probe data"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_preview_url_decodes_to_exact_rgba_frames() {
        if Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("reelio-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("frames.mp4");
        let ok = Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x180:rate=25",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&media)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let (bridge, _events) =
            Real::start(PluginHost::new_for_tests(dir.join("data"))).expect("engine");
        let path = media.to_string_lossy().into_owned();
        bridge
            .call(Request::MediaDropped {
                paths: vec![path.clone()],
            })
            .expect("authorized");
        bridge
            .call(Request::MediaProbe { path: path.clone() })
            .expect("probe");
        let url = bridge
            .call(Request::PreviewSource { path })
            .expect("preview url");
        let url = url.as_str().expect("url string").to_string();
        let (w, h) = (160u32, 90u32);
        let args = crate::player::decoder_args(&url, 0.0, w, h, false);
        let out = Command::new("ffmpeg").args(&args).output().unwrap();
        let frame = w as usize * h as usize * 4;
        assert!(
            out.stdout.len() >= frame * 10,
            "decoded frames: {} bytes",
            out.stdout.len()
        );
        assert_eq!(out.stdout.len() % frame, 0, "whole frames only");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
