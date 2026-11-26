//! Copy and decode resource files before acknowledging them. The sender may recycle the file after ack.

use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::Path;

use serde_json::Value;

use super::{png, ImageRef};

const FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SIDE: u32 = 8192;
pub(crate) const CACHE_BYTES: usize = 128 * 1024 * 1024;

pub(crate) fn load(root: &Path, put: &Value) -> Result<ImageRef, String> {
    let file = Path::new(put["file"].as_str().ok_or("resource requires a file")?);
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = file.canonicalize().map_err(|e| e.to_string())?;
    if !file.is_absolute() || !path.starts_with(&root) {
        return Err("resource file must be absolute and inside PIGEON_SESSION_DIR".into());
    }
    let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > FILE_BYTES as u64 {
        return Err("resource must be a regular file of at most 64 MiB".into());
    }
    let mut bytes = Vec::new();
    File::open(path).and_then(|f| f.take(FILE_BYTES as u64 + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
    if bytes.len() > FILE_BYTES { return Err("resource exceeds 64 MiB".into()); }
    match put["kind"].as_str() {
        Some("rgba") => rgba(put, &bytes),
        Some("png") => png::decode(&bytes),
        Some("jpeg") => encoded(&bytes, image::ImageFormat::Jpeg),
        Some("webp") => encoded(&bytes, image::ImageFormat::WebP),
        Some("svg") => svg(&bytes),
        _ => Err("unknown resource kind".into()),
    }
}

pub(super) fn check_dimensions(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) * 4 > FILE_BYTES as u64 {
        Err(format!("image dimensions out of bounds: {w}x{h}"))
    } else {
        Ok(())
    }
}

fn rgba(put: &Value, bytes: &[u8]) -> Result<ImageRef, String> {
    let dim = |key: &str| put[key].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid image dimensions");
    let (w, h) = (dim("w")?, dim("h")?);
    check_dimensions(w, h)?;
    let row_bytes = w as usize * 4;
    let stride = usize::try_from(put["stride"].as_u64().unwrap_or(row_bytes as u64)).map_err(|_| "invalid RGBA stride")?;
    if stride < row_bytes || stride.checked_mul(h as usize) != Some(bytes.len()) {
        return Err("RGBA resource does not match dimensions and stride".into());
    }
    let mut rgba = Vec::with_capacity(row_bytes * h as usize);
    for row in bytes.chunks_exact(stride) { rgba.extend_from_slice(&row[..row_bytes]); }
    if put["premultiplied"] == true { unpremultiply(&mut rgba); }
    Ok(ImageRef { w, h, rgba })
}

fn encoded(bytes: &[u8], format: image::ImageFormat) -> Result<ImageRef, String> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(FILE_BYTES as u64 * 2);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (w, h) = image::ImageDecoder::dimensions(&decoder);
    check_dimensions(w, h)?;
    let rgba = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?.into_rgba8().into_raw();
    Ok(ImageRef { w, h, rgba })
}

fn svg(bytes: &[u8]) -> Result<ImageRef, String> {
    use resvg::{tiny_skia, usvg};
    // SVG resources are self-contained vectors; don't resolve external files or embedded bitmaps.
    if !bytes.starts_with(b"<") && !bytes.starts_with(b"\xef\xbb\xbf") && !bytes.first().is_some_and(u8::is_ascii_whitespace) {
        return Err("expected plain SVG XML".into());
    }
    let options = usvg::Options { image_href_resolver: usvg::ImageHrefResolver {
        resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None),
    }, ..usvg::Options::default() };
    let tree = usvg::Tree::from_data(bytes, &options).map_err(|e| e.to_string())?;
    let size = tree.size().to_int_size();
    let (w, h) = (size.width(), size.height());
    check_dimensions(w, h)?;
    let mut pixels = tiny_skia::Pixmap::new(w, h).ok_or("cannot allocate SVG pixels")?;
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixels.as_mut());
    let mut rgba = pixels.take();
    unpremultiply(&mut rgba);
    Ok(ImageRef { w, h, rgba })
}

fn unpremultiply(rgba: &mut [u8]) {
    for c in rgba.chunks_exact_mut(4) {
        let alpha = u16::from(c[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut c[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}
