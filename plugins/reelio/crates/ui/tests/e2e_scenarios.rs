//! Compact behavioural ports of the four Playwright specs in `apps/desktop/e2e`. Each test sends the
//! same user events the spec performs (as `ui.event` node/value pairs), then asserts on reducer
//! state and on the scene the host would draw. They replace browser clicks; they do not exercise
//! the native shell.

use reelio_ui::engine::Reply;
use reelio_ui::model::{Asset, Kind, Stream, Track};
use reelio_ui::scene;
use reelio_ui::state::{Effect, Event, State};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn ui(s: &mut State, node: &str, value: Value) {
    s.dispatch(Event::Ui {
        node: node.into(),
        value,
    });
}

fn scene_text(s: &State) -> String {
    scene::all(s)
        .values()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reply to the newest engine request (silent dirty-sync requests are skipped).
fn reply_to_last(s: &mut State, reply: Reply) {
    let id = s
        .drain()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Engine { id, request }
                if !format!("{request:?}").starts_with("SubtitleEditorDirty") =>
            {
                Some(id)
            }
            _ => None,
        })
        .last()
        .expect("an engine request was queued");
    s.dispatch(Event::Engine { id, reply });
}

/// One 10 s, 1920x1080 video source with its lane `tv`, as after a successful import.
fn video_project() -> State {
    let mut s = State::new();
    let stream = Stream {
        index: 0,
        kind: Kind::Video,
        codec: "h264".into(),
        title: String::new(),
        language: "und".into(),
        duration_ms: Some(10_000),
        default: false,
        forced: false,
        properties: BTreeMap::from([
            ("width".into(), "1920".into()),
            ("height".into(), "1080".into()),
            ("r_frame_rate".into(), "25/1".into()),
        ]),
    };
    let asset = Asset {
        id: "v".into(),
        path: "/m/bunny.mp4".into(),
        format: "mp4".into(),
        size: 5_300_000,
        modified_ms: 0,
        duration_ms: Some(10_000),
        streams: vec![stream],
        metadata: BTreeMap::new(),
        chapters: vec![],
    };
    s.edit(|p| {
        p.assets.push(asset);
        p.tracks.push(Track {
            id: "tv".into(),
            asset_id: "v".into(),
            stream_index: 0,
            enabled: true,
            title: "Big Buck Bunny".into(),
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
        });
    });
    s.drain();
    s
}

/// preview-transport.spec: transport icons, the speed ladder, frame steps and chapter insertion.
#[test]
fn preview_transport_scenario() {
    let mut s = video_project();
    ui(&mut s, "viewer:add_chapter", Value::Null);
    assert_eq!(
        s.project().chapters.len(),
        1,
        "the chapter icon inserts at the playhead"
    );
    ui(&mut s, "viewer:speed", json!(9));
    assert_eq!(s.speed(), 10.0);
    ui(&mut s, "viewer:speed", json!(0));
    assert_eq!(s.speed(), 0.1);
    ui(&mut s, "viewer:next_frame", Value::Null);
    assert!(
        (s.media_time - 40.0).abs() < 1e-6,
        "one nominal 25 fps frame"
    );
    ui(&mut s, "viewer:prev_frame", Value::Null);
    ui(&mut s, "viewer:prev_frame", Value::Null);
    assert_eq!(s.media_time, 0.0, "frame steps clamp at the source start");
    ui(&mut s, "viewer:end", Value::Null);
    assert_eq!(s.media_time, 10_000.0);
    assert!(scene_text(&s).contains("Retry preview"));
}

/// subtitle-editor.spec: open, edit, undo/redo, merge, save a copy (a new file, never the original).
#[test]
fn subtitle_editor_scenario() {
    let mut s = video_project();
    ui(&mut s, "nav:subtitles", Value::Null);
    ui(&mut s, "sub:open", Value::Null);
    reply_to_last(
        &mut s,
        Ok(json!({"document": {"cues": [
            {"id": "a", "start_ms": 1000, "end_ms": 4000, "text": "One two"},
            {"id": "b", "start_ms": 5000, "end_ms": 7000, "text": "Three"}
        ]}, "path": "/subs/movie.srt"})),
    );
    assert_eq!(s.studio.as_ref().unwrap().doc.cues.len(), 2);
    ui(&mut s, "sub:text:a", json!("One two three"));
    ui(&mut s, "sub:undo", Value::Null);
    assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].text, "One two");
    ui(&mut s, "sub:redo", Value::Null);
    ui(&mut s, "sub:merge:a", Value::Null);
    assert_eq!(
        s.studio.as_ref().unwrap().doc.cues.len(),
        1,
        "merge joins adjacent cues"
    );
    ui(&mut s, "sub:save", Value::Null);
    reply_to_last(&mut s, Ok(json!("/subs/movie.copy.srt")));
    let st = s.studio.as_ref().unwrap();
    assert!(!st.dirty());
    assert!(st.notice.as_ref().unwrap().contains("movie.copy.srt"));
}

/// window-feedback.spec: errors last longer, hover pauses them, and quitting a dirty project asks.
#[test]
fn window_feedback_scenario() {
    let mut s = video_project();
    s.dispatch(Event::Tick { now: 0.0 });
    ui(&mut s, "export:validate", Value::Null);
    reply_to_last(&mut s, Err("boom".into()));
    assert!(s.error.is_some());
    ui(&mut s, "toast:error:hover", json!(true));
    s.dispatch(Event::Tick { now: 20_000.0 });
    assert!(s.error.is_some(), "hover pauses the error toast");
    ui(&mut s, "toast:error:hover", json!(false));
    s.dispatch(Event::Tick { now: 30_000.0 });
    assert!(s.error.is_none(), "errors dismiss after eight seconds");
    s.edit(|p| p.name = "Edited".into());
    s.dispatch(Event::Quit);
    assert!(s.dialogs.quit_confirm, "unsaved edits ask before quitting");
}

/// workspace.spec: trim a clip (one undo step), then the clip menu's Clip to video.
#[test]
fn workspace_editing_scenario() {
    let mut s = video_project();
    ui(
        &mut s,
        "lane:trim:tv",
        json!({"edge": "end", "ms": -2000, "snap": false}),
    );
    assert_eq!(s.project().track("tv").unwrap().trim_end_ms, Some(8000));
    ui(&mut s, "undo", Value::Null);
    assert_eq!(
        s.project().track("tv").unwrap().trim_end_ms,
        None,
        "the trim is one undo step"
    );
    ui(
        &mut s,
        "lane:trim:tv",
        json!({"edge": "end", "ms": -2000, "snap": false}),
    );
    let scene = scene_text(&s);
    assert!(
        scene.contains("lane.canvas:tv"),
        "the lane renders on its canvas"
    );
    s.dispatch(Event::Ui {
        node: "lane.canvas:tv".into(),
        value: json!({"type": "context", "x": 10.0, "y": 40.0}),
    });
    assert!(s.menu.is_some(), "right-click opens the clip menu");
}
