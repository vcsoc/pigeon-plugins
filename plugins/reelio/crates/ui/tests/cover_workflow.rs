use reelio_ui::{
    engine::Request,
    model::Artwork,
    state::{Effect, Event, Section, State},
};
use serde_json::{json, Value};
fn event(s: &mut State, node: &str, value: Value) {
    s.dispatch(Event::Ui {
        node: node.into(),
        value,
    });
}
fn art(name: &str, width: u32, height: u32) -> Artwork {
    Artwork {
        jpeg: "example".into(),
        width,
        height,
        source_label: name.into(),
    }
}
fn answer(s: &mut State, filter: impl Fn(&Request) -> bool, v: Value) {
    let id = s
        .drain()
        .into_iter()
        .find_map(|e| match e {
            Effect::Engine { id, request } if filter(&request) => Some(id),
            _ => None,
        })
        .expect("expected request");
    s.dispatch(Event::Engine { id, reply: Ok(v) });
}
#[test]
fn crop_drag_resizes_once_and_lane_reopens_the_applied_original() {
    let mut s = State::new();
    s.drain();
    s.edit(|p| p.artwork = Some(art("applied", 360, 220)));
    let before = s.store.history_len();
    event(
        &mut s,
        "cover:lane:thumbnail",
        json!({"type":"double_click"}),
    );
    assert_eq!(s.section, Section::Artwork);
    let ptr = |phase, x, y| json!({"type":"pointer","phase":phase,"x":x,"y":y});
    event(&mut s, "art:canvas", ptr("down", 75, 35));
    event(&mut s, "art:canvas", ptr("move", 190, 130));
    assert!(s
        .poster
        .as_ref()
        .unwrap()
        .editor
        .as_ref()
        .unwrap()
        .edits
        .crop
        .is_none());
    event(&mut s, "art:canvas", ptr("up", 190, 130));
    let e = s.poster.as_ref().unwrap().editor.as_ref().unwrap();
    assert_eq!(e.past.len(), 1);
    assert!(e.edits.crop.unwrap().width > 110);
    let first_crop = e.edits.crop;
    event(&mut s, "art:canvas", ptr("down", 190, 130));
    event(&mut s, "art:canvas", ptr("move", 240, 165));
    event(&mut s, "art:canvas", ptr("cancel", 240, 165));
    assert_eq!(
        s.poster
            .as_ref()
            .unwrap()
            .editor
            .as_ref()
            .unwrap()
            .edits
            .crop,
        first_crop
    );
    event(&mut s, "art:canvas", ptr("down", 190, 130));
    event(&mut s, "art:canvas", ptr("up", 240, 165));
    let e = s.poster.as_ref().unwrap().editor.as_ref().unwrap();
    assert_eq!(e.past.len(), 2);
    assert!(e.edits.crop.unwrap().width > first_crop.unwrap().width);
    assert_eq!(s.store.history_len(), before);
    event(&mut s, "cover:lane", json!({"type":"double_click"}));
    assert!(s
        .poster
        .as_ref()
        .unwrap()
        .editor
        .as_ref()
        .unwrap()
        .edits
        .crop
        .is_none());
}
#[test]
fn cover_drop_requires_one_authorized_file_and_keeps_previous_cover_on_failure() {
    let mut s = State::new();
    s.edit(|p| p.artwork = Some(art("existing", 90, 90)));
    s.drain();
    s.dispatch(Event::CoverDrop {
        paths: vec!["one.png".into(), "two.png".into()],
    });
    assert!(s.drain().is_empty());
    s.dispatch(Event::CoverDrop {
        paths: vec!["one.png".into()],
    });
    answer(
        &mut s,
        |r| matches!(r, Request::MediaDropped { .. }),
        json!(["/canonical/one.png"]),
    );
    let request=s.drain().into_iter().find(|e|matches!(e,Effect::Engine{request:Request::ArtworkImport{path},..} if path=="/canonical/one.png")).unwrap();
    let Effect::Engine { id, .. } = request else {
        unreachable!()
    };
    s.dispatch(Event::Engine {
        id,
        reply: Err("Invalid artwork".into()),
    });
    assert_eq!(s.cover().unwrap().source_label, "existing");
}
#[test]
fn exact_ratio_is_forwarded_and_thumbnails_are_referenced_not_inlined() {
    let mut s = State::new();
    s.section = Section::Artwork;
    s.drain();
    event(&mut s, "art:query", json!("Movie"));
    event(&mut s, "art:ratio", json!("3:2"));
    answer(
        &mut s,
        |r| matches!(r,Request::PosterSearch{proportion,..} if proportion=="3:2"),
        json!({"results":[{"title":"Fixture","image_url":"https://example.org/cover.jpg","thumbnail_url":"https://example.org/thumb.jpg"}],"next_offset":1,"has_more":false}),
    );
    let scene = reelio_ui::scene::main_view(&s).to_string();
    assert!(scene.contains(&reelio_ui::poster::thumbnail_rid(
        "https://example.org/thumb.jpg"
    )));
    assert!(!scene.contains("https://example.org/thumb.jpg"));
    event(&mut s, "art:use:0", Value::Null);
    answer(
        &mut s,
        |r| matches!(r,Request::PosterImage{proportion:Some(p),..} if p=="3:2"),
        serde_json::to_value(art("cropped", 300, 200)).unwrap(),
    );
    assert_eq!(s.cover().map(|a| (a.width, a.height)), Some((300, 200)));
}
