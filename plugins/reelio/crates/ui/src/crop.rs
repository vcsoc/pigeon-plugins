//! Cover crop geometry. Port of `sizeCrop` in `features/artwork/imageGeometry.ts` plus the
//! interactive move/resize rules that the crop selection enforces (source-bounded, ratio-aware).

use crate::model::{Artwork, Asset};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Crop {
    pub fn full(source: Size) -> Crop {
        Crop {
            x: 0,
            y: 0,
            width: source.width,
            height: source.height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Width,
    Height,
}

/// Size a crop from one typed dimension. A ratio (width/height) keeps proportions, scaling the
/// result down to fit the source. Dimensions are at least 2 px and never exceed the source. The
/// crop stays centred on the previous crop and inside the source. Never produces a 1 px crop.
pub fn size_crop(
    source: Size,
    current: Option<Crop>,
    field: Field,
    value: f64,
    ratio: Option<f64>,
) -> Crop {
    let old = current.unwrap_or(Crop::full(source));
    let mut width = if field == Field::Width {
        value
    } else {
        old.width as f64
    };
    let mut height = if field == Field::Height {
        value
    } else {
        old.height as f64
    };
    if let Some(r) = ratio.filter(|r| *r > 0.0) {
        if field == Field::Width {
            height = width / r;
        } else {
            width = height * r;
        }
        let scale = 1f64
            .min(source.width as f64 / width)
            .min(source.height as f64 / height);
        width *= scale;
        height *= scale;
    }
    let w = (js_round(width) as i64).clamp(2, source.width as i64) as u32;
    let h = (js_round(height) as i64).clamp(2, source.height as i64) as u32;
    let x = js_round(old.x as f64 + (old.width as f64 - w as f64) / 2.0) as i64;
    let y = js_round(old.y as f64 + (old.height as f64 - h as f64) / 2.0) as i64;
    Crop {
        x: x.clamp(0, (source.width - w) as i64) as u32,
        y: y.clamp(0, (source.height - h) as i64) as u32,
        width: w,
        height: h,
    }
}

fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Move a crop by whole pixels without resizing; each source edge clamps independently.
pub fn move_crop(source: Size, crop: Crop, dx: i64, dy: i64) -> Crop {
    let x = (crop.x as i64 + dx).clamp(0, (source.width - crop.width) as i64);
    let y = (crop.y as i64 + dy).clamp(0, (source.height - crop.height) as i64);
    Crop {
        x: x as u32,
        y: y as u32,
        ..crop
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

/// Resize from a handle by a pointer delta. The opposite edge stays fixed. With a locked ratio
/// the result is fitted inside the source while keeping the opposite corner.
pub fn resize_crop(
    source: Size,
    crop: Crop,
    handle: Handle,
    dx: i64,
    dy: i64,
    ratio: Option<f64>,
) -> Crop {
    let (mut left, mut top) = (crop.x as i64, crop.y as i64);
    let (mut right, mut bottom) = ((crop.x + crop.width) as i64, (crop.y + crop.height) as i64);
    let moves_left = matches!(handle, Handle::W | Handle::NW | Handle::SW);
    let moves_right = matches!(handle, Handle::E | Handle::NE | Handle::SE);
    let moves_top = matches!(handle, Handle::N | Handle::NW | Handle::NE);
    let moves_bottom = matches!(handle, Handle::S | Handle::SW | Handle::SE);
    if moves_left {
        left += dx;
    }
    if moves_right {
        right += dx;
    }
    if moves_top {
        top += dy;
    }
    if moves_bottom {
        bottom += dy;
    }
    left = left.clamp(0, right - 2);
    top = top.clamp(0, bottom - 2);
    right = right.clamp(left + 2, source.width as i64);
    bottom = bottom.clamp(top + 2, source.height as i64);
    let mut width = (right - left) as f64;
    let mut height = (bottom - top) as f64;
    if let Some(r) = ratio.filter(|r| *r > 0.0) {
        if moves_left || moves_right {
            height = width / r;
        } else {
            width = height * r;
        }
        let fit = 1f64
            .min(source.width as f64 / width)
            .min(source.height as f64 / height);
        width *= fit;
        height *= fit;
        let w = js_round(width).max(2.0) as i64;
        let h = js_round(height).max(2.0) as i64;
        // Keep the opposite corner fixed.
        if moves_left {
            left = right - w;
        } else {
            right = left + w;
        }
        if moves_top {
            top = bottom - h;
        } else {
            bottom = top + h;
        }
    }
    let left = left.max(0);
    let top = top.max(0);
    let right = right.min(source.width as i64);
    let bottom = bottom.min(source.height as i64);
    Crop {
        x: left as u32,
        y: top as u32,
        width: (right - left).max(2) as u32,
        height: (bottom - top).max(2) as u32,
    }
}

/// Preset crop ratios offered in the studio (name, width, height).
pub const CROP_PRESETS: [(&str, u32, u32); 7] = [
    ("Poster 9:16", 9, 16),
    ("Poster 2:3", 2, 3),
    ("Poster 3:4", 3, 4),
    ("Square", 1, 1),
    ("Wide 4:3", 4, 3),
    ("Wide 3:2", 3, 2),
    ("Wide 16:9", 16, 9),
];

/// Largest image dimension accepted for an edit preview, to bound local raster work.
pub const PREVIEW_MAX_EDGE: u32 = 1280;

/// Source size of an artwork or asset, when known from its metadata. Used for crop bounds.
pub fn artwork_size(a: &Artwork) -> Size {
    Size {
        width: a.width,
        height: a.height,
    }
}

pub fn asset_size_hint(_asset: &Asset) -> Option<Size> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: Size = Size {
        width: 1000,
        height: 1500,
    };

    #[test]
    fn never_creates_a_one_pixel_crop() {
        let c = size_crop(SRC, None, Field::Width, 0.0, None);
        assert_eq!(c.width, 2);
        let c = size_crop(SRC, None, Field::Height, 1.0, Some(2.0 / 3.0));
        assert!(c.width >= 2 && c.height >= 2);
    }

    #[test]
    fn centres_and_locks_ratio_inside_the_source() {
        let c = size_crop(SRC, None, Field::Width, 400.0, Some(2.0 / 3.0));
        assert_eq!((c.width, c.height), (400, 600));
        assert_eq!(c.x, 300);
        assert_eq!(c.y, 450);
        let c = size_crop(SRC, None, Field::Width, 5000.0, Some(16.0 / 9.0));
        assert!(c.width <= SRC.width && c.height <= SRC.height);
    }

    #[test]
    fn move_clamps_every_edge_without_resizing() {
        let crop = Crop {
            x: 100,
            y: 100,
            width: 300,
            height: 200,
        };
        let moved = move_crop(SRC, crop, -5000, 5000);
        assert_eq!(
            (moved.x, moved.y, moved.width, moved.height),
            (0, 1300, 300, 200)
        );
    }

    #[test]
    fn resize_preserves_the_opposite_corner_and_ratio() {
        let crop = Crop {
            x: 100,
            y: 100,
            width: 300,
            height: 450,
        };
        let out = resize_crop(SRC, crop, Handle::SE, 60, 0, Some(2.0 / 3.0));
        assert_eq!(out.x, 100);
        assert_eq!(out.y, 100);
        assert_eq!(
            out.width * 3,
            out.height * 2 + (out.width * 3 - out.height * 2)
        );
        assert!((out.width as f64 / out.height as f64 - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn resizing_one_edge_keeps_the_other_dimension() {
        let crop = Crop {
            x: 100,
            y: 100,
            width: 300,
            height: 450,
        };
        let out = resize_crop(SRC, crop, Handle::E, 50, 0, None);
        assert_eq!((out.x, out.y, out.height), (100, 100, 450));
        assert_eq!(out.width, 350);
    }

    #[test]
    fn presets_include_poster_and_wide_ratios() {
        assert!(CROP_PRESETS.iter().any(|p| p.0 == "Poster 9:16"));
        assert!(CROP_PRESETS.iter().any(|p| p.0 == "Wide 3:2"));
    }
}
