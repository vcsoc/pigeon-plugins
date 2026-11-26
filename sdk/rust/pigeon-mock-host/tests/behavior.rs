use std::{path::Path, sync::Arc, time::Duration};

use pigeon_mock_host::{LaunchOptions, Schemas, Session};
use pigeon_plugin_sdk::protocol::NodeEvent;
use serde_json::json;

#[test]
fn hello_ui_events_advance_snapshots_and_update_materialized_rows() {
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../plugins/hello-ui");
    let host = Session::launch(&plugin, Arc::new(Schemas::load()), LaunchOptions::default()).unwrap();
    let timeout = Duration::from_secs(5);
    host.wait_ready(timeout).unwrap();
    let id = "hello-ui/main";
    host.show(id).unwrap();
    let first = host.wait_next_scene(id, 0, timeout).unwrap();
    assert_eq!(first.assert_text("Click me")["id"], "go");
    assert_eq!(first.assert_node("count")["text"], "0 clicks");
    let (_, images) = host.render_snapshot(id).unwrap();
    assert_eq!((images["hello:frame"].w, images["hello:frame"].h), (96, 54));

    let before = host.event(id, "go", NodeEvent::Click).unwrap();
    let clicked = host.wait_scene_matching(id, before, timeout, |s| s.by_text("1 clicks").is_some()).unwrap();
    assert!(clicked.revision > first.revision);
    let before = host.event(id, "tabs", NodeEvent::Change { value: json!("list"), rev: 0 }).unwrap();
    host.wait_scene_matching(id, before, timeout, |s| s.assert_node("tabs")["active"] == "list").unwrap();
    let before = host.event(id, "rows", NodeEvent::ListWindow { first: 40, last: 43, overscan: 0 }).unwrap();
    let updated = host.wait_next_scene(id, before, timeout).unwrap();
    assert_eq!(updated.assert_text("Row 00041")["id"], "row-40-name");
    assert!(host.shutdown(timeout));
    host.assert_no_violations();
}
