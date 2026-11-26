use reelio_ui::{
    engine::Request,
    scene,
    state::{Effect, Event, State},
};
use serde_json::{json, Value};

fn ui(s: &mut State, node: &str, value: Value) {
    s.dispatch(Event::Ui {
        node: node.into(),
        value,
    });
}
fn response(s: &mut State, want: impl Fn(&Request) -> bool, value: Value) {
    let id = s
        .drain()
        .into_iter()
        .find_map(|e| match e {
            Effect::Engine { id, request } if want(&request) => Some(id),
            _ => None,
        })
        .expect("expected engine request");
    s.dispatch(Event::Engine {
        id,
        reply: Ok(value),
    });
}

#[test]
fn download_requires_the_current_offer_and_never_imports_tracks_automatically() {
    let mut s = State::new();
    s.drain();
    ui(&mut s, "sub:url", json!("https://example.org/movie.zip"));
    ui(&mut s, "sub:offer", Value::Null);
    response(
        &mut s,
        |r| matches!(r, Request::SearchBrowserDownload { kind, url } if kind == "subtitles" && url == "https://example.org/movie.zip"),
        json!({"id":"first","kind":"subtitles","host":"example.org","name":"movie.zip"}),
    );
    assert!(scene::modals(&s).contains_key("modal.download"));
    ui(&mut s, "resource:import:some-other-offer", Value::Null);
    assert!(s.drain().is_empty());
    ui(&mut s, "resource:import:first", Value::Null);
    response(
        &mut s,
        |r| matches!(r, Request::ResourceDownload { id } if id == "first"),
        json!({"kind":"subtitles","files":[{"path":"/authorized/en.srt","name":"English"}]}),
    );
    assert!(s.project().tracks.is_empty());
    assert!(s.resources.is_empty());
    ui(&mut s, "sub:add_file:0", Value::Null);
    assert!(s.drain().iter().any(|e| matches!(e, Effect::Engine { request: Request::MediaProbe { path }, .. } if path == "/authorized/en.srt")));
}

#[test]
fn translation_review_changes_only_accepted_text_with_one_undo_step() {
    let mut s = State::new();
    let source = json!({"cues":[{"id":"a","start_ms":1000,"end_ms":3000,"text":"Hello"},{"id":"b","start_ms":4000,"end_ms":6000,"text":"Goodbye"}]});
    s.studio_load(
        serde_json::from_value(source.clone()).unwrap(),
        "Original",
        None,
    );
    ui(&mut s, "sub:models", Value::Null);
    response(
        &mut s,
        |r| matches!(r, Request::SubtitleModels),
        json!({"installed":[{"name":"gemma3:4b"}],"supported":["gemma3:4b"]}),
    );
    ui(&mut s, "sub:language", json!("French"));
    ui(&mut s, "sub:translate", Value::Null);
    response(
        &mut s,
        |r| matches!(r, Request::SubtitleModelStart { document: Some(doc), language, .. } if *doc == source && language == "French"),
        json!({"id":"translation","kind":"translate","status":"running","done":0,"total":2,"message":"Working"}),
    );
    assert!(s.studio_model_running());
    s.dispatch(Event::Tick { now: 500. });
    response(
        &mut s,
        |r| matches!(r, Request::SubtitleModelStatus),
        json!({"id":"translation","kind":"translate","status":"completed","done":2,"total":2,"message":"Review"}),
    );
    response(
        &mut s,
        |r| matches!(r, Request::SubtitleModelResults { id } if id == "translation"),
        json!({"document":source,"translations":[{"id":"a","text":"Bonjour"},{"id":"b","text":"Au revoir"}]}),
    );
    assert_eq!(s.studio_cue("a").unwrap().text, "Hello");
    ui(&mut s, "sub:review", Value::Null);
    ui(&mut s, "sub:review_accept:b", json!(false));
    ui(&mut s, "sub:review_apply", Value::Null);
    let a = s.studio_cue("a").unwrap();
    assert_eq!(
        (a.start_ms, a.end_ms, a.text.as_str()),
        (1000, 3000, "Bonjour")
    );
    assert_eq!(s.studio_cue("b").unwrap().text, "Goodbye");
    ui(&mut s, "sub:undo", Value::Null);
    assert_eq!(s.studio_cue("a").unwrap().text, "Hello");
}

#[test]
fn overlay_follows_media_time_and_keyboard_navigation_leaves_timing_unchanged() {
    let mut s = State::new();
    ui(&mut s, "nav:subtitles", Value::Null);
    let doc = json!({"cues":[{"id":"a","start_ms":0,"end_ms":700,"text":"Hello"},{"id":"b","start_ms":700,"end_ms":1500,"text":"Again"}]});
    s.studio_load(serde_json::from_value(doc).unwrap(), "Original", None);
    assert_eq!(s.subtitle_overlay(), ["Hello"]);
    ui(
        &mut s,
        "sub:select:a",
        json!({"type":"key","phase":"down","key":"ArrowRight"}),
    );
    assert_eq!(s.studio.as_ref().unwrap().selected.as_deref(), Some("b"));
    s.media_time = 700.0;
    assert_eq!(s.subtitle_overlay(), ["Again"]);
    s.media_time = 1500.0;
    assert!(s.subtitle_overlay().is_empty());
    assert!(!s.studio.as_ref().unwrap().dirty());
}
