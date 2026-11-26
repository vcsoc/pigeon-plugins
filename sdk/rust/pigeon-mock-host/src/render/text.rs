//! Glyph text with the fonts egui ships (Ubuntu-Light for UI text, Hack for monospace), so golden
//! images match across machines.

use std::sync::OnceLock;

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};

static UI: OnceLock<FontRef<'static>> = OnceLock::new();
static MONO: OnceLock<FontRef<'static>> = OnceLock::new();

/// The font for UI text or monospace text.
pub fn font(mono: bool) -> &'static FontRef<'static> {
    if mono {
        MONO.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).expect("bundled Hack font"))
    } else {
        UI.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("bundled Ubuntu font"))
    }
}

/// Advance width of `text` at `px` device pixels.
pub fn measure(font: &FontRef<'static>, text: &str, px: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(px));
    text.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum()
}

/// Height of one line at `px` device pixels (ascent plus descent).
pub fn line_height(font: &FontRef<'static>, px: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(px));
    scaled.ascent() - scaled.descent()
}

/// Draw `text` with its line top at `top_y` (device px). `cb(x, y, coverage)` receives every
/// covered pixel.
pub fn draw(font: &FontRef<'static>, text: &str, left: f32, top_y: f32, px: f32, cb: &mut dyn FnMut(i64, i64, f32)) {
    let scaled = font.as_scaled(PxScale::from(px));
    let baseline = top_y + scaled.ascent();
    let mut x = left;
    for c in text.chars() {
        let id = font.glyph_id(c);
        let glyph = id.with_scale_and_position(PxScale::from(px), point(x, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let ix = b.min.x as i64 + gx as i64;
                let iy = b.min.y as i64 + gy as i64;
                if cov > 0.0 {
                    cb(ix, iy, cov);
                }
            });
        }
        x += scaled.h_advance(id);
    }
}
