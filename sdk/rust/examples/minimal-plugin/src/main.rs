//! Minimal Pigeon plugin: one workspace view that shows a button and counts clicks.

use pigeon_plugin_sdk::{runtime::PluginSpec, Contribution, Incoming, Node, NodeEvent, Plugin};

fn main() {
    let spec = PluginSpec::new("minimal-rust", "0.1.0")
        .contribution(Contribution::workspace_view("main", "Minimal", "sparkle"));
    let plugin = match Plugin::stdio(spec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("minimal-rust: {e}");
            std::process::exit(1);
        }
    };
    let mut clicks = 0u32;
    plugin.run(move |ctx, msg| match msg {
        Incoming::Visibility { instance, visible: true } => {
            let _ = ctx.set_scene(instance, view(clicks));
        }
        Incoming::UiEvent { instance, event: NodeEvent::Click, .. } => {
            clicks += 1;
            let _ = ctx.set_scene(instance, view(clicks));
        }
        _ => {}
    });
}

fn view(clicks: u32) -> Node {
    Node::column("root").gap(8.0).padding(12.0)
        .child(Node::text("title", "Minimal Rust plugin").style("heading"))
        .child(Node::button("go", &format!("Clicked {clicks} times")).variant("primary").on(&["click"]))
}
