//! `pigeon-plugin-sdk`: build a Pigeon plugin that draws itself into host slots (protocol v2).
//!
//! The crate is dependency-light (serde, serde_json, std threads; no async runtime). Modules:
//!
//! * [`protocol`]: typed messages in both directions, limits.
//! * [`scene`]: scene nodes, sizes, colors, menus and the fluent node builders.
//! * [`canvas`]: draw ops and the draw-list builder.
//! * [`contribution`]: slot contributions and `applies_to`.
//! * [`runtime`]: the process runtime: protocol threads, app loop, service calls, lifecycle.
//! * [`resources`]: the file ring for images and frames.
//! * [`transport`]: bounded line reading and the outbound queue.
//!
//! Specification: `docs/architecture/plugin-ui-protocol.md`; SDK guide: `docs/architecture/sdk.md`.

pub mod canvas;
pub mod contribution;
pub mod protocol;
pub mod resources;
pub mod runtime;
pub mod scene;
pub mod transport;

pub use canvas::{DrawList, DrawOp, PathCmd};
pub use contribution::{AppliesTo, Contribution};
pub use protocol::{Incoming, NodeEvent, Outgoing, ProtocolError, PROTOCOL_VERSION};
pub use runtime::{Ctx, Plugin, PluginSpec, Reply, ServiceError};
pub use scene::{Color, MenuItem, Node, Size};
