//! Golden images: advisory visual checks. A mismatch reports the fraction of differing pixels
//! and the largest channel difference. `UPDATE_GOLDENS=1` rewrites the golden instead.

use std::path::Path;

use super::png;
use super::raster::Surface;

/// The result of comparing a rendering against a golden file.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub differing_fraction: f64,
    pub max_channel_delta: u8,
    pub size_matches: bool,
    pub written: bool,
}

impl Comparison {
    pub fn passes(&self, max_fraction: f64) -> bool {
        self.size_matches && self.differing_fraction <= max_fraction
    }
}

/// Compare `surface` with the PNG at `golden`. Creates or overwrites it when `UPDATE_GOLDENS=1` or
/// when the file does not exist yet (first run).
pub fn compare_png(surface: &Surface, golden: &Path) -> Result<Comparison, String> {
    let update = std::env::var("UPDATE_GOLDENS").map(|v| v == "1").unwrap_or(false);
    if update || !golden.exists() {
        if let Some(dir) = golden.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(golden, png::encode(surface)?).map_err(|e| e.to_string())?;
        return Ok(Comparison { differing_fraction: 0.0, max_channel_delta: 0, size_matches: true, written: true });
    }
    let bytes = std::fs::read(golden).map_err(|e| e.to_string())?;
    let expected = png::decode(&bytes)?;
    if expected.w != surface.w || expected.h != surface.h {
        return Ok(Comparison { differing_fraction: 1.0, max_channel_delta: 255, size_matches: false, written: false });
    }
    let mut differing = 0usize;
    let mut max_delta = 0u8;
    for (a, b) in surface.px.chunks_exact(4).zip(expected.rgba.chunks_exact(4)) {
        let mut pixel_differs = false;
        for k in 0..4 {
            let d = a[k].abs_diff(b[k]);
            if d > 0 {
                pixel_differs = true;
            }
            max_delta = max_delta.max(d);
        }
        if pixel_differs {
            differing += 1;
        }
    }
    let total = (surface.w as usize * surface.h as usize).max(1);
    Ok(Comparison { differing_fraction: differing as f64 / total as f64, max_channel_delta: max_delta, size_matches: true, written: false })
}
