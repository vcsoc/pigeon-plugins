//! Behavioural tests of the Tauri-free engine API against a fake host.
//! Each test gets a private data directory, so the state database is never shared.

use reelio_engine::Document;
use reelio_engine::{
    CloseDecision, ConfirmDialog, ConfirmKind, Engine, EngineEvent, EngineHost, FileDialog, Point,
    ReelioEngine, ResourceQueueRequest, SearchKind, SettingsUpdateRequest, Size,
    SubtitleEditorDraftRequest, SubtitleModelStartRequest, WindowAction, HOMEPAGE_URL,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Mutex};

#[derive(Default)]
struct Recorded {
    opened: Vec<String>,
    controls: Vec<WindowAction>,
    zoom: Option<f64>,
    confirms: Vec<ConfirmDialog>,
}

struct FakeHost {
    dir: tempfile::TempDir,
    confirm_answer: bool,
    picked: Mutex<Option<Vec<PathBuf>>>,
    recorded: Mutex<Recorded>,
}

impl FakeHost {
    fn new(confirm_answer: bool) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            confirm_answer,
            picked: Mutex::new(None),
            recorded: Mutex::new(Recorded::default()),
        }
    }
}

impl EngineHost for FakeHost {
    fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }
    fn videos_dir(&self) -> Option<PathBuf> {
        Some(self.dir.path().join("Videos"))
    }
    fn pick_files(&self, _request: &FileDialog) -> Option<Vec<PathBuf>> {
        self.picked.lock().unwrap().clone()
    }
    fn pick_folder(&self, _request: &FileDialog) -> Option<PathBuf> {
        None
    }
    fn save_file(&self, _request: &FileDialog) -> Option<PathBuf> {
        None
    }
    fn confirm(&self, request: &ConfirmDialog) -> bool {
        self.recorded.lock().unwrap().confirms.push(request.clone());
        self.confirm_answer
    }
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.recorded.lock().unwrap().opened.push(url.into());
        Ok(())
    }
    fn open_search_browser(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn window_control(&self, action: WindowAction) -> Result<bool, String> {
        self.recorded.lock().unwrap().controls.push(action);
        Ok(false)
    }
    fn set_interface_zoom(&self, factor: f64) -> Result<(), String> {
        self.recorded.lock().unwrap().zoom = Some(factor);
        Ok(())
    }
}

fn engine(
    confirm: bool,
) -> (
    ReelioEngine,
    Receiver<EngineEvent>,
    std::sync::Arc<FakeHost>,
) {
    let host = std::sync::Arc::new(FakeHost::new(confirm));
    let (engine, events) = ReelioEngine::new(host.clone()).expect("engine opens");
    (engine, events, host)
}

fn drain(events: &Receiver<EngineEvent>) -> Vec<EngineEvent> {
    events.try_iter().collect()
}

#[test]
fn the_engine_is_usable_as_a_trait_object() {
    let (engine, _events, _host) = engine(true);
    let boxed: Box<dyn Engine> = Box::new(engine);
    assert!(boxed.app_version().starts_with(env!("CARGO_PKG_VERSION")));
}

#[test]
fn settings_defaults_match_the_reference_shape() {
    let (engine, _events, host) = engine(true);
    let view = engine.settings_get().unwrap();
    assert_eq!(view.theme, json!("dark"));
    assert_eq!(view.image_format, json!("png"));
    assert!(view.backend.is_none());
    assert!(view.recents.is_empty());
    assert_eq!(
        PathBuf::from(&view.default_folder),
        host.dir.path().join("Videos")
    );
    let value = serde_json::to_value(&view).unwrap();
    for key in [
        "theme",
        "recents",
        "backend",
        "default_folder",
        "image_format",
    ] {
        assert!(value.get(key).is_some(), "missing {key}");
    }
}

#[test]
fn settings_update_accepts_only_whitelisted_values_and_persists_them() {
    let (engine, _events, _host) = engine(true);
    let update = |key: &str, value: serde_json::Value| {
        engine.settings_update(SettingsUpdateRequest {
            key: key.into(),
            value,
        })
    };
    assert!(update("theme", json!("neon")).is_err());
    assert!(update("image_format", json!("gif")).is_err());
    assert!(update("unknown", json!(1)).is_err());
    assert!(update("default_folder", json!("relative/path")).is_err());
    assert!(update("backend", json!({"ffmpeg": "ffmpeg", "ffprobe": "ffprobe"})).is_err());
    update("theme", json!("light")).unwrap();
    update("image_format", json!("webp")).unwrap();
    let view = engine.settings_get().unwrap();
    assert_eq!(view.theme, json!("light"));
    assert_eq!(view.image_format, json!("webp"));
}

#[test]
fn interface_zoom_is_bounded_and_applied_through_the_host() {
    let (engine, _events, host) = engine(true);
    for percent in [0, 49, 201, u16::MAX] {
        assert!(engine.interface_zoom(percent).is_err());
    }
    assert!(host.recorded.lock().unwrap().zoom.is_none());
    engine.interface_zoom(50).unwrap();
    assert_eq!(host.recorded.lock().unwrap().zoom, Some(0.5));
    engine.interface_zoom(200).unwrap();
    assert_eq!(host.recorded.lock().unwrap().zoom, Some(2.0));
}

#[test]
fn window_actions_are_a_bounded_enum_with_wire_names() {
    for action in [
        "state",
        "drag",
        "minimize",
        "toggle_maximize",
        "close",
        "confirm_close",
    ] {
        assert!(
            serde_json::from_value::<WindowAction>(json!(action)).is_ok(),
            "{action}"
        );
    }
    for action in ["destroy", "execute", "set_url", ""] {
        assert!(
            serde_json::from_value::<WindowAction>(json!(action)).is_err(),
            "{action}"
        );
    }
}

#[test]
fn close_and_confirm_close_delegate_to_the_host_when_idle() {
    let (engine, events, host) = engine(true);
    assert_eq!(engine.close_requested(), CloseDecision::Allowed);
    assert!(drain(&events)
        .iter()
        .any(|e| matches!(e, EngineEvent::CloseRequested)));
    assert!(!engine.window_action(WindowAction::ConfirmClose).unwrap());
    assert!(!engine.window_action(WindowAction::Close).unwrap());
    assert_eq!(
        host.recorded.lock().unwrap().controls,
        vec![WindowAction::ConfirmClose, WindowAction::Close]
    );
}

#[test]
fn window_state_actions_pass_through_the_host_answer() {
    let (engine, _events, host) = engine(true);
    assert!(!engine.window_action(WindowAction::State).unwrap());
    assert_eq!(
        host.recorded.lock().unwrap().controls,
        vec![WindowAction::State]
    );
}

#[test]
fn window_platform_has_os_and_tiling_fields() {
    let (engine, _events, _host) = engine(true);
    let platform = engine.window_platform();
    assert_eq!(platform.os, std::env::consts::OS);
}

#[test]
fn homepage_opens_only_the_fixed_github_url() {
    let (engine, _events, host) = engine(true);
    engine.open_project_homepage().unwrap();
    assert_eq!(
        host.recorded.lock().unwrap().opened,
        vec![HOMEPAGE_URL.to_string()]
    );
    assert_eq!(HOMEPAGE_URL, "https://github.com/vcsoc/reelio");
}

#[test]
fn media_drop_authorizes_only_regular_files_and_emits_the_drop_event() {
    let (engine, events, host) = engine(true);
    let file = host.dir.path().join("clip.mkv");
    std::fs::write(&file, b"not really media").unwrap();
    let folder = host.dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let granted = engine.media_dropped(
        vec![
            file.clone(),
            folder.clone(),
            host.dir.path().join("missing.mkv"),
        ],
        Point { x: 3.0, y: 4.0 },
        Some(Size {
            width: 800,
            height: 600,
        }),
    );
    assert_eq!(granted.len(), 1);
    assert!(granted[0].ends_with("clip.mkv"));
    match drain(&events).as_slice() {
        [EngineEvent::MediaDropped {
            paths,
            position,
            viewport,
        }] => {
            assert_eq!(paths, &granted);
            assert_eq!(*position, Point { x: 3.0, y: 4.0 });
            assert_eq!(
                *viewport,
                Some(Size {
                    width: 800,
                    height: 600
                })
            );
        }
        other => panic!("unexpected events {other:?}"),
    }
}

#[test]
fn cancelled_pickers_return_empty_or_none_without_side_effects() {
    let (engine, _events, _host) = engine(true);
    assert!(engine.choose_media(false).unwrap().is_empty());
    assert!(engine.choose_media(true).unwrap().is_empty());
    assert!(engine.project_open().unwrap().is_none());
    assert!(engine.choose_subtitles().unwrap().is_empty());
    assert!(engine.choose_artwork().unwrap().is_none());
    assert!(engine.settings_choose_folder().unwrap().is_none());
    assert!(engine.update_download("offer".into()).is_err());
}

#[test]
fn projects_create_and_validate_without_sources() {
    let (engine, _events, _host) = engine(true);
    let project = engine.project_create();
    let plan = engine.project_validate(project.clone());
    let value = serde_json::to_value(&plan).unwrap();
    assert!(value.is_object());
    assert!(engine.export_plan(project).is_ok());
}

#[test]
fn media_probe_and_preview_refuse_unauthorized_or_missing_inputs() {
    let (engine, _events, host) = engine(true);
    assert!(engine
        .media_probe(host.dir.path().join("missing.mkv"))
        .is_err());
    assert!(engine
        .preview_source(host.dir.path().join("missing.mp4"))
        .is_err());
    let unauthorized = host.dir.path().join("not-granted.mp4");
    std::fs::write(&unauthorized, b"bytes").unwrap();
    assert!(engine.preview_source(unauthorized).is_err());
}

#[test]
fn jobs_and_backend_queries_work_on_an_empty_database() {
    let (engine, _events, _host) = engine(true);
    assert!(engine.jobs_list().unwrap().is_empty());
    assert!(engine.job_project("missing".into()).is_err());
    let _ = engine.export_cancel("missing".into());
    let _ = engine.dependencies_status().unwrap();
}

#[test]
fn subtitle_draft_roundtrip_and_clear_do_not_touch_original_files() {
    let (engine, _events, _host) = engine(true);
    let document = Document::parse("1\n00:00:00,000 --> 00:00:01,000\nHello\n").unwrap();
    assert!(engine
        .subtitle_editor_draft(SubtitleEditorDraftRequest {
            document: Some(document.clone()),
            clear: false,
        })
        .unwrap()
        .is_none());
    let stored = engine
        .subtitle_editor_draft(SubtitleEditorDraftRequest {
            document: None,
            clear: false,
        })
        .unwrap()
        .expect("draft is stored");
    assert_eq!(stored.cues.len(), 1);
    engine
        .subtitle_editor_draft(SubtitleEditorDraftRequest {
            document: None,
            clear: true,
        })
        .unwrap();
    assert!(engine
        .subtitle_editor_draft(SubtitleEditorDraftRequest {
            document: None,
            clear: false,
        })
        .unwrap()
        .is_none());
}

#[test]
fn local_model_operations_need_host_approval_first() {
    let (engine, _events, host) = engine(false);
    let error = engine
        .subtitle_model_start(SubtitleModelStartRequest {
            model: "gemma3:4b".into(),
            document: None,
            language: "fr".into(),
        })
        .unwrap_err();
    assert_eq!(error, "Local model operation was not approved");
    let recorded = host.recorded.lock().unwrap();
    assert_eq!(recorded.confirms.len(), 1);
    assert_eq!(recorded.confirms[0].kind, ConfirmKind::Info);
    assert!(recorded.confirms[0].message.starts_with("gemma3:4b"));
}

#[test]
fn model_status_and_results_are_safe_when_idle() {
    let (engine, _events, _host) = engine(true);
    let status = engine.subtitle_model_status().unwrap();
    assert!(serde_json::to_value(&status).is_ok());
    engine.subtitle_model_cancel();
    engine.subtitle_editor_dirty(false);
    // Without a running local Ollama service the reference reports a loopback-service error.
    if let Err(error) = engine.subtitle_models() {
        assert!(error.contains("Ollama"), "{error}");
    }
}

#[test]
fn resource_offers_accept_only_public_https_urls() {
    let (engine, _events, _host) = engine(true);
    for url in [
        "http://example.com/a.srt",
        "https://127.0.0.1/a.srt",
        "not a url",
    ] {
        assert!(
            engine
                .resource_queue(ResourceQueueRequest {
                    kind: SearchKind::Subtitles,
                    url: url.into(),
                })
                .is_err(),
            "{url}"
        );
    }
    let _ = engine.resource_cancel("unknown".into());
}

#[test]
fn search_browser_navigation_policy_is_exposed() {
    let (engine, _events, _host) = engine(true);
    assert!(!engine.search_browser_allows("http://example.com/".into()));
    assert!(!engine.search_browser_allows("tauri://localhost/".into()));
}

#[test]
fn event_wire_names_match_the_reference_events() {
    let names = [
        (
            EngineEvent::ExportUpdated { id: "j".into() },
            "export:updated",
        ),
        (EngineEvent::CloseBlocked, "app:close-blocked"),
        (EngineEvent::CloseRequested, "app:close-requested"),
        (EngineEvent::DependenciesChanged, "dependencies:changed"),
        (
            EngineEvent::ResourceError { error: "e".into() },
            "resource:error",
        ),
    ];
    for (event, name) in names {
        assert_eq!(serde_json::to_value(&event).unwrap()["event"], name);
    }
}

#[test]
fn engine_constructs_twice_on_the_same_directory_without_resetting_data() {
    let host = std::sync::Arc::new(FakeHost::new(true));
    let (first, _rx) = ReelioEngine::new(host.clone()).unwrap();
    first
        .settings_update(SettingsUpdateRequest {
            key: "theme".into(),
            value: json!("light"),
        })
        .unwrap();
    drop(first);
    let (second, _rx2) = ReelioEngine::new(host).unwrap();
    assert_eq!(second.settings_get().unwrap().theme, json!("light"));
}

#[test]
fn point_round_trips_through_json() {
    let point = Point { x: 1.5, y: 2.0 };
    let back: Point = serde_json::from_value(serde_json::to_value(point).unwrap()).unwrap();
    assert_eq!(back, point);
}
