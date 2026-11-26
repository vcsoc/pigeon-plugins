//! `reelio`: the Reelio plugin process (protocol v2, on the vendored Pigeon SDK).
//!
//! Flags: `--plugin` (serve on stdin/stdout), `--setup` (idempotent; nothing to install beyond the
//! binary, so it only reports readiness), `--version`, `--uninstall` (Reelio keeps no external state,
//! so this confirms without touching the user's media or projects).

mod engine_bridge;
mod transport;

use engine_bridge::{EngineBridge, PluginHost, Real, Unavailable};
use pigeon_plugin_sdk::runtime::Plugin;
use std::sync::Arc;

fn run_plugin() -> Result<(), String> {
    let spec = transport::spec()?;
    let plugin = Plugin::stdio(spec).map_err(|e| e.to_string())?;
    let engine: Arc<dyn EngineBridge> = match Real::start(PluginHost::from_env()) {
        Ok(real) => Arc::new(real),
        Err(error) => Arc::new(Unavailable(format!(
            "The Reelio engine could not start: {error}"
        ))),
    };
    let app = transport::App::new(plugin.ctx(), engine);
    transport::start_ticker(&app);
    let handler_app = app.clone();
    plugin.run(move |_ctx, msg| transport::handle(&handler_app, msg));
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
    } else if args.iter().any(|a| a == "--setup") {
        println!("{{\"ok\":true,\"ready\":true}}");
    } else if args.iter().any(|a| a == "--uninstall") {
        println!("{{\"ok\":true,\"removed\":[]}}");
    } else if args.iter().any(|a| a == "--plugin") {
        if let Err(error) = run_plugin() {
            println!("{{\"ok\":false,\"error\":{}}}", serde_json::json!(error));
            std::process::exit(1);
        }
    } else {
        eprintln!("usage: reelio --plugin | --setup | --version | --uninstall");
        std::process::exit(2);
    }
}
