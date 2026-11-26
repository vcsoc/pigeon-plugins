//! Headless Pigeon host for plugin tests.
//!
//! * [`schemas`]: validation against the `schema/ui-v2` JSON Schemas embedded at build time.
//! * [`session`]: launches a plugin (`run --setup`, `run --plugin`), validates and records every
//!   message, answers host services and provider requests, simulates stalls and crashes.
//! * [`services`]: fakes for the section-10 host services.
//! * [`render`]: software renderer for scenes and canvases (PNG, themeable, golden tests).
//! * [`conformance`]: the case runner used by `pigeon-mock conformance` and by the real host.

pub mod conformance;
pub mod render;
pub mod schemas;
pub mod services;
pub mod session;

pub use schemas::{Schemas, Violation};
pub use session::{LaunchOptions, Session};
