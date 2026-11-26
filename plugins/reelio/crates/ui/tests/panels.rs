use reelio_ui::{
    engine::{Job, Progress, Request},
    scene,
    state::{Effect, Event, Section, State},
};
use serde_json::{json, Value};
fn ui(s: &mut State, node: &str, value: Value) {
    s.dispatch(Event::Ui {
        node: node.into(),
        value,
    });
}
#[test]
fn export_history_discloses_live_progress_adjustments_and_snapshot_restore() {
    let mut s = State::new();
    s.section = Section::Export;
    s.drain();
    let job =
        |id: &str, status: &str, project_id: Option<String>, progress: Option<Progress>| Job {
            id: id.into(),
            status: status.into(),
            project_id,
            progress,
            sequence: 4,
            destination: format!("/output/{id}.mp4"),
            error: None,
            created_at: "2026-01-01".into(),
        };
    let pid = Some(s.project().id.clone());
    s.jobs = vec![
        job(
            "active",
            "processing",
            pid.clone(),
            Some(Progress {
                phase: "muxing".into(),
                percent: Some(32.5),
                ..Default::default()
            }),
        ),
        job(
            "done",
            "completed",
            pid.clone(),
            Some(Progress {
                notices: vec!["Cropped audio to fit video".into()],
                ..Default::default()
            }),
        ),
        job("broken", "failed", pid, None),
        job("other", "failed", Some("other-project".into()), None),
    ];
    let scene = scene::project_panel(&s).to_string();
    for text in [
        "job.progress:active",
        "0.325",
        "Cropped audio to fit video",
        "export:restore:broken",
    ] {
        assert!(scene.contains(text), "missing {text}");
    }
    assert!(!scene.contains("other.mp4"));
    s.jobs.remove(0);
    s.edit(|p| p.name = "unsaved".into());
    ui(&mut s, "export:restore:broken", Value::Null);
    assert!(scene::modals(&s).contains_key("modal.restore"));
    assert!(s.drain().is_empty());
    ui(&mut s, "export:restore_confirm", Value::Null);
    assert!(s
        .drain()
        .iter()
        .any(|e| matches!(e,Effect::Engine{request:Request::JobProject{id},..} if id=="broken")));
}
#[test]
fn toast_is_overlay_and_hover_and_focus_pause_are_independent() {
    let mut s = State::new();
    s.dispatch(Event::EngineEvent(
        json!({"event":"notification","level":"info","text":"Saved"}),
    ));
    let v = scene::main_view(&s);
    assert_eq!(v["children"][1]["id"], "toast.overlay");
    ui(&mut s, "toast:notice:hover", json!({"type":"enter"}));
    ui(&mut s, "toast:notice:dismiss", json!({"type":"focus"}));
    ui(&mut s, "toast:notice:hover", json!({"type":"leave"}));
    s.dispatch(Event::Tick { now: 6000. });
    assert!(s.notice.is_some());
    ui(&mut s, "toast:notice:dismiss", json!({"type":"blur"}));
    s.dispatch(Event::Tick { now: 11000. });
    assert!(s.notice.is_none());
}
#[test]
fn dependency_panel_shows_failures_and_guards_repair() {
    let mut s = State::new();
    let id = s
        .drain()
        .into_iter()
        .find_map(|e| match e {
            Effect::Engine {
                id,
                request: Request::DependenciesStatus,
            } => Some(id),
            _ => None,
        })
        .unwrap();
    s.dispatch(Event::Engine{id, reply:Ok(json!({"preview_available":false,"network_available":true,"backend_available":false,"missing_components":["FFmpeg not found"],"repair_supported":false,"repair_hint":"Install manually","packages":[]}))});
    ui(&mut s, "dep:open", Value::Null);
    let v = scene::modals(&s)["modal.dependencies"].to_string();
    assert!(v.contains("FFmpeg not found"));
    assert!(v.contains("Install manually"));
    ui(&mut s, "dep:repair", Value::Null);
    assert!(s.drain().is_empty());
}
