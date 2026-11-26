//! `reelio-engine`: the Tauri-free headless Reelio backend.
//!
//! * [`Engine`] (in `api`): one method per former Tauri command, with plain serde types.
//! * [`EngineEvent`]: job progress, preview readiness, repair progress and notifications,
//!   delivered over a standard channel. Events are hints; state is re-read through the API.
//! * [`EngineHost`]: the few services that belong to the Pigeon host (native dialogs, opening
//!   URLs, window chrome, interface zoom, data directory).
//!
//! The crate depends on no UI toolkit and no plugin SDK. Status: the trait and types are the
//! published M1 contract; the implementation (`ReelioEngine`) is being filled in per milestone.

pub mod api;
pub mod host;
pub mod types;

pub use api::Engine;
pub use host::{ConfirmDialog, ConfirmKind, EngineHost, FileDialog};
pub use types::*;
