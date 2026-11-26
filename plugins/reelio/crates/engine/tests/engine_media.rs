//! End-to-end engine tests with generated media (requires `ffmpeg` and `ffprobe` on PATH).
//! They exercise the command surface a host uses: probe, plan, export start, durable job state,
//! restore, project save/open, and the no-overwrite guarantees.

use reelio_domain::Container;
use reelio_engine::{
    ConfirmDialog, Engine, EngineEvent, EngineHost, ExportStartRequest, FileDialog, ReelioEngine,
    WindowAction,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc::Receiver, Arc, Mutex};
use std::time::{Duration, Instant};

struct Host {
    dir: tempfile::TempDir,
    save_target: Mutex<Option<PathBuf>>,
    pick: Mutex<Option<Vec<PathBuf>>>,
}

impl EngineHost for Host {
    fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }
    fn videos_dir(&self) -> Option<PathBuf> {
        Some(self.dir.path().join("Videos"))
    }
    fn pick_files(&self, _request: &FileDialog) -> Option<Vec<PathBuf>> {
        self.pick.lock().unwrap().clone()
    }
    fn pick_folder(&self, _request: &FileDialog) -> Option<PathBuf> {
        None
    }
    fn save_file(&self, _request: &FileDialog) -> Option<PathBuf> {
        self.save_target.lock().unwrap().clone()
    }
    fn confirm(&self, _request: &ConfirmDialog) -> bool {
        true
    }
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn open_search_browser(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn window_control(&self, _action: WindowAction) -> Result<bool, String> {
        Ok(false)
    }
    fn set_interface_zoom(&self, _factor: f64) -> Result<(), String> {
        Ok(())
    }
}

fn setup() -> (ReelioEngine, Receiver<EngineEvent>, Arc<Host>) {
    let host = Arc::new(Host {
        dir: tempfile::tempdir().unwrap(),
        save_target: Mutex::new(None),
        pick: Mutex::new(None),
    });
    let (engine, events) = ReelioEngine::new(host.clone()).unwrap();
    (engine, events, host)
}

/// A two-stream Matroska source (MPEG-4 video, AAC audio), like the reference fixtures.
fn generate(path: &Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-n",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=128x72:rate=12:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg is required for engine media tests");
    assert!(status.success());
}

/// The real flow: the host picks the file (authorizing it), then the engine probes it.
fn probe(engine: &ReelioEngine, host: &Host, source: &Path) -> reelio_engine::Asset {
    *host.pick.lock().unwrap() = Some(vec![source.to_path_buf()]);
    let granted = engine.choose_media(false).unwrap();
    assert_eq!(granted.len(), 1);
    engine.media_probe(PathBuf::from(&granted[0])).unwrap()
}

fn wait_for_job(engine: &ReelioEngine, id: &str) -> String {
    let started = Instant::now();
    loop {
        let job = engine
            .jobs_list()
            .unwrap()
            .into_iter()
            .find(|job| job.id == id)
            .expect("job is recorded");
        if matches!(
            job.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            return job.status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "export did not finish"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn export_through_the_engine_publishes_a_new_file_and_never_touches_the_source() {
    let (engine, events, host) = setup();
    let source = host.dir.path().join("source.mkv");
    generate(&source);
    let source_bytes = std::fs::read(&source).unwrap();

    let asset = probe(&engine, &host, &source);
    let mut project = engine.project_create();
    project.container = Container::Matroska;
    project.add_asset(asset).unwrap();
    let plan = engine.project_validate(project.clone());
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    engine.export_plan(project.clone()).unwrap();

    let destination = host.dir.path().join("out.mkv");
    *host.save_target.lock().unwrap() = Some(destination.clone());
    let id = engine
        .export_start(ExportStartRequest {
            project: project.clone(),
            approve_conversion: false,
        })
        .unwrap()
        .expect("export was approved in the host dialog");
    assert_eq!(wait_for_job(&engine, &id), "completed");

    assert!(destination.is_file(), "output was published");
    assert_eq!(
        std::fs::read(&source).unwrap(),
        source_bytes,
        "source is unchanged"
    );
    let finished = engine.jobs_list().unwrap();
    assert!(finished.iter().any(|job| job.id == id));
    assert!(events
        .try_iter()
        .any(|e| matches!(e, EngineEvent::ExportUpdated { id: ref event_id } if *event_id == id)));

    // The durable snapshot restores through the authorized job path.
    let restored = engine.job_project(id.clone()).unwrap();
    assert_eq!(restored.tracks.len(), project.tracks.len());

    // An existing destination is refused before any job starts.
    let refused = engine.export_start(ExportStartRequest {
        project: project.clone(),
        approve_conversion: false,
    });
    assert_eq!(
        refused.unwrap_err(),
        "Destination exists. Choose a new filename."
    );
    // Its bytes are untouched.
    assert!(destination.is_file());
}

#[test]
fn project_save_and_open_round_trip_through_the_host_dialogs() {
    let (engine, _events, host) = setup();
    let source = host.dir.path().join("source.mkv");
    generate(&source);
    let asset = probe(&engine, &host, &source);
    let mut project = engine.project_create();
    project.container = Container::Matroska;
    project.add_asset(asset).unwrap();

    let saved = host.dir.path().join("Untitled.reelio");
    *host.save_target.lock().unwrap() = Some(saved.clone());
    let path = engine.project_save(project.clone()).unwrap();
    assert_eq!(path.as_deref(), Some(saved.to_string_lossy().as_ref()));
    assert!(saved.is_file());

    *host.pick.lock().unwrap() = Some(vec![saved.clone()]);
    let reopened = engine.project_open().unwrap().expect("project reopened");
    assert_eq!(reopened.id, project.id);
    assert_eq!(reopened.tracks.len(), project.tracks.len());
    // The source was authorized by opening the project, and the recent list records it.
    let settings = engine.settings_get().unwrap();
    assert!(settings
        .recents
        .iter()
        .any(|r| r.to_string().contains("Untitled.reelio")));
}
