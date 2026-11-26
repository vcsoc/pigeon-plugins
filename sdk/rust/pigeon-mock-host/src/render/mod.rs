//! Software renderer for scenes and canvases (see `docs/architecture/sdk.md`, "Mock host").
//!
//! * [`raster`]: pixel primitives and the draw-op interpreter.
//! * [`scene`]: layout and painting of protocol scene nodes.
//! * [`text`]: glyph text with the bundled fonts.
//! * [`png`]: PNG encoding and decoding.
//! * [`golden`]: golden-image comparison (`UPDATE_GOLDENS=1` writes new goldens).

pub mod golden;
pub mod png;
pub mod raster;
pub mod scene;
pub mod text;
pub mod theme;

pub use golden::{compare_png, Comparison};
pub use raster::{ImageRef, Painter, Surface};
pub use scene::{render_canvas, render_scene};
pub use theme::{dark_theme_protocol, Rgba, Theme};
