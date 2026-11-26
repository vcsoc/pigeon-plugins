//! `reelio`: the Reelio plugin process (protocol v2, on the vendored Pigeon SDK).
//!
//! Flags: `--plugin` (serve on stdin/stdout), `--setup` (idempotent; nothing to install beyond the
//! binary, so it only reports readiness), `--version`, `--uninstall` (Reelio keeps no external state,
//! so this confirms without touching the user's media or projects).

mod consent;
mod engine_bridge;
mod player;
mod transport;

use engine_bridge::{EngineBridge, PluginHost, Real, Unavailable};
use pigeon_plugin_sdk::runtime::Plugin;
use std::sync::Arc;

fn run_plugin() -> Result<(), String> {
    let spec = transport::spec()?;
    let plugin = Plugin::stdio(spec).map_err(|e| e.to_string())?;
    let (consent_tx, consent_rx) = consent::channel();
    let (engine, events): (Arc<dyn EngineBridge>, _) =
        match Real::start(PluginHost::from_env(plugin.ctx(), consent_tx)) {
            Ok((real, events)) => (Arc::new(real), Some(events)),
            Err(error) => (
                Arc::new(Unavailable(format!(
                    "The Reelio engine could not start: {error}"
                ))),
                None,
            ),
        };
    let app = transport::App::new(plugin.ctx(), engine);
    consent::forward(&app, consent_rx);
    transport::attach_player(&app);
    if let Some(events) = events {
        transport::forward_engine_events(&app, events);
    }
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
