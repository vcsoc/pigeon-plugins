//! Reelio plugin UI for Pigeon (protocol v2).
//!
//! * [`model`], [`store`], [`geometry`], [`chapters`], [`playback`], [`subtitles`], [`posters`],
//!   [`crop`], [`layout`]: pure logic ported from the reference front end (`apps/desktop/src`).
//! * [`engine`]: the engine-neutral request/reply contract (one variant per former Tauri command).
//! * [`state`]: the reducer. Events in, effects out; no I/O on the caller's thread.
//! * [`scene`] and [`canvas`]: builders for protocol-v2 scenes and canvas draw lists.

pub mod canvas;
pub mod chapters;
pub mod crop;
pub mod engine;
pub mod geometry;
pub mod layout;
pub mod menus;
pub mod model;
pub mod playback;
pub mod poster;
pub mod posters;
pub mod scene;
pub mod state;
pub mod store;
pub mod studio;
pub mod subtitles;
pub mod timeline_input;
