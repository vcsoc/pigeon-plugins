//! Engine bridge. Maps each UI [`Request`] onto the Reelio engine. The real engine is WP4's
//! `reelio-engine` crate; until it is published on `arch/reelio-engine` this build ships
//! [`UnconnectedEngine`], which answers every command with an explicit, user-visible error so the
//! UI degrades honestly (no silent success). Tests use their own fake implementing [`EngineBridge`].

use reelio_ui::engine::{Reply, Request, RuntimeDependencies};
use serde_json::{json, Value};

/// The engine seam. Implementations must be thread-safe: the host runs each request on a worker.
pub trait EngineBridge: Send + Sync + 'static {
    fn call(&self, request: Request) -> Reply;
}

/// Stand-in used until `reelio-engine` is linked in.
pub struct UnconnectedEngine;

const NOT_CONNECTED: &str = "The Reelio engine is not connected in this plugin build. Install a Reelio engine build to edit media.";

impl EngineBridge for UnconnectedEngine {
    fn call(&self, request: Request) -> Reply {
        match request {
            Request::SettingsGet => Ok(json!({"theme": "dark", "recents": []})),
            Request::BackendStatus => Err(NOT_CONNECTED.into()),
            Request::DependenciesStatus => Ok(serde_json::to_value(RuntimeDependencies {
                preview_available: false,
                network_available: false,
                backend_available: false,
                missing_components: vec!["Reelio engine".into()],
                repair_supported: false,
                repair_hint: NOT_CONNECTED.into(),
                packages: vec![],
            })
            .unwrap_or(Value::Null)),
            _ => Err(NOT_CONNECTED.into()),
        }
    }
}
