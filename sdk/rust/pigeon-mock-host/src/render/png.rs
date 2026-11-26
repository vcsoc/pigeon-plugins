//! PNG encode and decode for surfaces and resources (8-bit RGB, RGBA and grayscale).

use super::raster::Surface;
use super::raster::ImageRef;

/// Encode a surface as an RGBA8 PNG.
pub fn encode(surface: &Surface) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, surface.w, surface.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&surface.px).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decode a PNG into RGBA8 (RGB and grayscale are expanded; 16-bit is reduced to 8).
pub fn decode(bytes: &[u8]) -> Result<ImageRef, String> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    super::resource::check_dimensions(reader.info().width, reader.info().height)?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        other => return Err(format!("unsupported PNG color type {other:?}")),
    };
    Ok(ImageRef { w: info.width, h: info.height, rgba })
}
