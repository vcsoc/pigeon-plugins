//! Scene builders (protocol v2 section 5). Pure functions of view data, so the behaviour is testable
//! without a host. Input rule (section 5.1): every text input carries `rev`; the host reports changes with
//! the `rev` they were based on, and the plugin adopts a change only when its `rev` is not older.

use serde_json::{json, Value};

pub fn text(id: &str, value: &str, style: &str) -> Value {
    json!({"id":id,"kind":"text","style":style,"text":value,"wrap":true,"selectable":true})
}

pub fn button(id: &str, label: &str, variant: &str, enabled: bool) -> Value {
    json!({"id":id,"kind":"button","label":label,"variant":variant,"enabled":enabled,"on":["click"]})
}

pub fn text_input(id: &str, value: &str, rev: u64, placeholder: &str, password: bool) -> Value {
    json!({"id":id,"kind":"text_input","value":value,"rev":rev,"placeholder":placeholder,"password":password,
           "commit":"change","max_len":2048,"accessibility":{"label":placeholder,"role":"textbox"}})
}

pub fn checkbox(id: &str, label: &str, checked: bool) -> Value {
    json!({"id":id,"kind":"checkbox","label":label,"checked":checked,"on":["change"]})
}

fn column(id: &str, children: Vec<Value>) -> Value {
    json!({"id":id,"kind":"column","gap":8,"padding":12,"children":children})
}

fn row(id: &str, children: Vec<Value>) -> Value {
    json!({"id":id,"kind":"row","gap":8,"children":children})
}

fn scroll(id: &str, child: Value) -> Value {
    json!({"id":id,"kind":"scroll","axis":"vertical","show_bars":true,"height":{"fill":1},"children":[child]})
}

pub fn progress(id: &str, value: Option<f64>) -> Value {
    json!({"id":id,"kind":"progress","value":value})
}

/// The preferences page.
pub struct PrefsView {
    pub enabled: bool,
    pub server: String,
    pub email: String,
    pub remember: bool,
    pub rev: u64,
    pub connected: bool,
    pub images: u64,
    pub tags: u64,
    pub status: String,
    pub busy: bool,
}

pub fn prefs(view: &PrefsView) -> Value {
    let summary = if view.connected {
        format!("Connected: {} images, {} tags. Originals stay on the server.", view.images, view.tags)
    } else {
        "Not connected.".to_owned()
    };
    column(
        "root",
        vec![
            text("title", "Immich", "heading"),
            text(
                "intro",
                "Browse server thumbnails. Download original files only when explicitly transferred to a local folder.",
                "body",
            ),
            checkbox("enabled", "Enable Immich", view.enabled),
            text_input("server", &view.server, view.rev, "https://photos.example.com", false),
            text_input("email", &view.email, view.rev, "Email", false),
            text_input("password", "", view.rev, "Stored in system keyring when remembered", true),
            checkbox("remember", "Remember credentials securely in the system keyring", view.remember),
            text(
                "remember_note",
                "Session-only logins keep the password in memory for this session; nothing is written to disk.",
                "caption",
            ),
            row(
                "actions",
                vec![
                    button("connect", "Connect / save", "primary", !view.busy),
                    button("test", "Test connection", "default", !view.busy),
                    button("sync", "Sync now", "default", !view.busy),
                    button("disconnect", "Disconnect", "danger", view.connected),
                ],
            ),
            if view.busy { progress("busy", None) } else { json!({"id":"busy","kind":"spacer"}) },
            text("summary", &summary, "body"),
            text("status", &view.status, "caption"),
        ],
    )
}

/// Compact status line for the status bar.
pub fn status_item(connected: bool, images: u64) -> Value {
    let label = if connected {
        format!("Immich: {images} images")
    } else {
        "Immich: off".to_owned()
    };
    json!({"id":"status","kind":"row","gap":6,"children":[text("label", &label, "caption")]})
}

/// One row of the transfer dialog.
pub struct ItemView {
    pub name: String,
    pub state: String,
    pub done: bool,
}

pub struct TransferView {
    pub heading: String,
    pub upload: bool,
    pub running: bool,
    pub items: Vec<ItemView>,
    pub status: String,
}

pub fn transfer(view: &TransferView) -> Value {
    let total = view.items.len();
    let complete = view.items.iter().filter(|i| i.done).count();
    let mut body = vec![
        text("heading", &view.heading, "heading"),
        text(
            "explain",
            if view.upload {
                "Original bytes are checksum-verified. Pigeon metadata is written and read back before a file counts as done."
            } else {
                "Original bytes are checksum-verified. Complete metadata is retained in immutable sidecars next to each download."
            },
            "caption",
        ),
        progress("progress", Some(if total == 0 { 0.0 } else { complete as f64 / total as f64 })),
        text("count", &format!("{complete} of {total} verified"), "body"),
    ];
    let rows: Vec<Value> = view
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            json!({"id":format!("item-{index}"),"kind":"row","gap":8,"children":[
                text(&format!("name-{index}"), &item.name, "body"),
                text(&format!("state-{index}"), &item.state, "caption"),
            ]})
        })
        .collect();
    body.push(scroll("items", column("items-column", rows)));
    body.push(text("status", &view.status, "caption"));
    let actions = if view.running {
        vec![button("stop", "Stop after current file", "default", true)]
    } else if complete < total {
        vec![
            text("incomplete", "Batch incomplete. Source files remain unchanged; retry only incomplete files.", "caption"),
            button("retry", "Retry", "primary", true),
        ]
    } else {
        vec![text("done", "All originals and metadata verified", "body")]
    };
    body.push(row("actions", actions));
    column("root", body)
}

/// Tag picker for "add an existing Immich tag to the selected references".
pub struct TagView {
    pub count: usize,
    pub tags: Vec<(String, String)>,
    pub selected: Option<String>,
    pub status: String,
}

pub fn tag_picker(view: &TagView) -> Value {
    let options: Vec<Value> = view.tags.iter().map(|(id, name)| json!({"value":id,"label":name})).collect();
    column(
        "root",
        vec![
            text("heading", "Add Immich tag", "heading"),
            text(
                "explain",
                &format!("Add an existing tag to {} selected Immich image(s). Tags are created on Immich from Preferences or from Pigeon's tag editor.", view.count),
                "body",
            ),
            json!({"id":"tag","kind":"select","options":options,"value":view.selected,"searchable":true,"enabled":!options.is_empty()}),
            row("actions", vec![button("apply", "Apply", "primary", view.selected.is_some()), button("cancel", "Close", "ghost", true)]),
            text("status", &view.status, "caption"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(scene: &Value, out: &mut Vec<String>) {
        if let Some(id) = scene["id"].as_str() {
            out.push(id.to_owned());
        }
        if let Some(children) = scene["children"].as_array() {
            for child in children {
                ids(child, out);
            }
        }
    }

    #[test]
    fn node_ids_are_unique_in_every_scene() {
        let scenes = [
            prefs(&PrefsView { enabled: true, server: "https://x.test".into(), email: "a@x.test".into(), remember: true, rev: 3, connected: true, images: 2, tags: 1, status: "ok".into(), busy: false }),
            transfer(&TransferView { heading: "h".into(), upload: false, running: true, items: vec![ItemView { name: "a".into(), state: "Queued".into(), done: false }, ItemView { name: "b".into(), state: "ok".into(), done: true }], status: String::new() }),
            tag_picker(&TagView { count: 2, tags: vec![("t".into(), "travel".into())], selected: None, status: String::new() }),
        ];
        for scene in scenes {
            let mut all = Vec::new();
            ids(&scene, &mut all);
            let mut sorted = all.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), all.len(), "duplicate node ids in {scene}");
        }
    }

    #[test]
    fn password_field_is_never_prefilled() {
        let scene = prefs(&PrefsView { enabled: true, server: String::new(), email: String::new(), remember: true, rev: 1, connected: false, images: 0, tags: 0, status: String::new(), busy: false });
        let password = scene["children"].as_array().unwrap().iter().find(|c| c["id"] == "password").unwrap();
        assert_eq!(password["value"], "");
        assert_eq!(password["password"], true);
    }

    #[test]
    fn transfer_dialog_offers_stop_while_running_and_retry_when_incomplete() {
        let running = transfer(&TransferView { heading: "h".into(), upload: true, running: true, items: vec![], status: String::new() });
        assert!(running.to_string().contains("Stop after current file"));
        let incomplete = transfer(&TransferView { heading: "h".into(), upload: true, running: false, items: vec![ItemView { name: "a".into(), state: "failed".into(), done: false }], status: String::new() });
        assert!(incomplete.to_string().contains("\"retry\""));
        let done = transfer(&TransferView { heading: "h".into(), upload: true, running: false, items: vec![ItemView { name: "a".into(), state: "ok".into(), done: true }], status: String::new() });
        assert!(done.to_string().contains("All originals and metadata verified"));
        assert!(!done.to_string().contains("\"retry\""));
    }
}
