//! Artwork, poster search, subtitle import/test, and confirmed browser downloads
//! (reference `apps/desktop/src-tauri/src/resources.rs`, Tauri-free).

use crate::context::Ctx;
use crate::host::FileDialog;
use crate::types::*;
use reelio_application::{artwork, resources};
use reelio_ffmpeg::MediaBackend;
use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

fn dialog(title: &str, directory: PathBuf, filters: Vec<(String, Vec<String>)>) -> FileDialog {
    FileDialog {
        title: Some(title.into()),
        directory: Some(directory),
        file_name: None,
        filters,
        multiple: false,
    }
}

fn filter(label: &str, extensions: &[&str]) -> (String, Vec<String>) {
    (
        label.into(),
        extensions.iter().map(|e| (*e).to_string()).collect(),
    )
}

/// Former `choose_artwork`.
pub(crate) fn choose_artwork(ctx: &Ctx) -> EngineResult<Option<Artwork>> {
    let Some(files) = ctx.host.pick_files(&dialog(
        "Choose a poster or cover",
        ctx.default_folder()?,
        vec![filter(
            "Cover image",
            &["jpg", "jpeg", "png", "webp", "avif"],
        )],
    )) else {
        return Ok(None);
    };
    let Some(path) = files.into_iter().next() else {
        return Ok(None);
    };
    let path = ctx.grant(&path)?;
    load_cover(ctx, &path).map(|result| Some(result.artwork))
}

/// Reads a still cover image with the bounded probe/decode rules of the reference.
fn load_cover(ctx: &Ctx, path: &Path) -> EngineResult<ImportedCover> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > artwork::MAX_DOWNLOAD as u64 {
        return Err("Choose a regular still image smaller than 8 MiB".into());
    }
    let asset = ctx.app.backend.probe(path)?;
    if !asset.is_still_image() {
        return Err(
            "Drop a JPEG, PNG, WebP or still AVIF image into the cover area, not a video".into(),
        );
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(artwork::MAX_DOWNLOAD as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Local cover");
    let art = if asset
        .metadata
        .get("major_brand")
        .is_some_and(|brand| brand.trim() == "avif")
    {
        artwork::prepare(&ctx.app.backend.cover_image_bytes(&asset)?, label)?
    } else {
        artwork::prepare(&bytes, label)?
    };
    if reelio_ffmpeg::fingerprint(path)? != (asset.size, asset.modified_ms) {
        return Err("Image changed while preparing the cover".into());
    }
    Ok(ImportedCover {
        asset,
        artwork: art,
    })
}

/// Former `artwork_import`.
pub(crate) fn artwork_import(ctx: &Ctx, path: PathBuf) -> EngineResult<ImportedCover> {
    let path = ctx.app.require_authorized(&path)?;
    load_cover(ctx, &path)
}

/// Former `poster_search`.
pub(crate) fn poster_search(
    query: String,
    proportion: resources::PosterProportion,
    offset: u32,
) -> EngineResult<resources::PosterBatch> {
    resources::poster_search_page(&query, proportion, offset)
}

/// Former `poster_image`.
pub(crate) fn poster_image(
    url: String,
    proportion: Option<resources::PosterProportion>,
) -> EngineResult<Artwork> {
    let host = Url::parse(&url)
        .map_err(|_| "Invalid image URL")?
        .host_str()
        .unwrap_or("Web artwork")
        .to_string();
    let art = artwork::prepare(&resources::poster_image(&url)?, &host)?;
    let proportion = proportion.unwrap_or_default();
    if !proportion.matches(art.width, art.height) {
        return Err("The downloaded image does not match the selected proportions. Choose another result; your existing cover is unchanged.".into());
    }
    if let Some((w, h)) = proportion.ratio() {
        artwork::crop_to_ratio(&art, w, h)
    } else {
        Ok(art)
    }
}

/// Former `artwork_save_copy`: a new file in the default image format, never overwriting.
pub(crate) fn artwork_save_copy(ctx: &Ctx, artwork: Artwork) -> EngineResult<Option<String>> {
    artwork::jpeg_bytes(&artwork)?;
    let value = ctx
        .app
        .storage
        .lock()
        .map_err(|_| "Storage lock failed")?
        .setting("image_format")?
        .unwrap_or(serde_json::json!("png"));
    let format: artwork::ImageFileFormat = serde_json::from_value(value)
        .map_err(|_| "Invalid default image format; choose PNG, JPEG or WebP in Settings")?;
    let Some(mut path) = ctx.host.save_file(&FileDialog {
        title: Some("Save preview image to a new file".into()),
        directory: Some(ctx.default_folder()?),
        file_name: Some(format!("cover-preview.{}", format.extension())),
        filters: vec![filter("Preview image", &[format.extension()])],
        multiple: false,
    }) else {
        return Ok(None);
    };
    if path.extension().is_none() {
        path.set_extension(format.extension());
    }
    artwork::save_copy(&artwork, &path, format)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Former `artwork_edit`.
pub(crate) fn artwork_edit(
    artwork: Artwork,
    edits: reelio_artwork::ImageEdits,
) -> EngineResult<Artwork> {
    artwork::edit(&artwork, &edits)
}

/// Former `choose_subtitles`: at most 32 authorized text subtitle files.
pub(crate) fn choose_subtitles(ctx: &Ctx) -> EngineResult<Vec<String>> {
    let Some(files) = ctx.host.pick_files(&FileDialog {
        title: Some("Choose subtitles to test".into()),
        directory: Some(ctx.default_folder()?),
        file_name: None,
        filters: vec![filter("Text subtitles", &["srt", "ass", "ssa", "vtt"])],
        multiple: true,
    }) else {
        return Ok(vec![]);
    };
    files
        .into_iter()
        .take(32)
        .map(|path| Ok(ctx.grant(&path)?.to_string_lossy().into_owned()))
        .collect()
}

/// Former `subtitle_preview`: WebVTT preview of a zero-offset text subtitle track.
pub(crate) fn subtitle_preview(ctx: &Ctx, request: SubtitlePreviewRequest) -> EngineResult<String> {
    let SubtitlePreviewRequest { project, track_id } = request;
    ctx.app.validate_sources(&project)?;
    let track = project
        .tracks
        .iter()
        .find(|t| t.id == track_id)
        .ok_or("Subtitle track not found")?;
    if track.offset_ms != 0 {
        return Err(
            "Nonzero subtitle offsets are not implemented; set the offset to zero before testing"
                .into(),
        );
    }
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == track.asset_id)
        .ok_or("Subtitle source not found")?;
    ctx.app
        .backend
        .subtitle_preview(&asset.path, track.stream_index)
}

/// Former `resource_queue`: an opaque, expiring offer for a public HTTPS URL.
pub(crate) fn resource_queue(
    ctx: &Ctx,
    request: ResourceQueueRequest,
) -> EngineResult<PendingInfo> {
    ctx.app.resources.remember(
        Url::parse(&request.url).map_err(|_| "Enter a valid public HTTPS download URL")?,
        request.kind,
        String::new(),
    )
}

/// Former `resource_cancel`.
pub(crate) fn resource_cancel(ctx: &Ctx, id: String) -> EngineResult<()> {
    ctx.app.resources.forget(&id)
}

/// Former `resource_download`: downloads a confirmed offer and validates it.
pub(crate) fn resource_download(ctx: &Ctx, id: String) -> EngineResult<ImportResult> {
    let download = ctx.app.resources.download(&id)?;
    match download.kind {
        SearchKind::Artwork => Ok(ImportResult::Artwork {
            artwork: artwork::prepare(&download.bytes, &download.source_label)?,
        }),
        SearchKind::Subtitles => {
            let files = ctx
                .app
                .resources
                .save_subtitles(&download.bytes, &download.filename)?;
            for file in &files {
                ctx.grant(&file.path)?;
            }
            Ok(ImportResult::Subtitles { files })
        }
    }
}

/// Former `search_browser_open`. Requires playback and HTTPS components first; the host opens
/// the isolated browser at the engine-built search URL.
pub(crate) fn search_browser_open(ctx: &Ctx, request: SearchBrowserRequest) -> EngineResult<()> {
    let url = resources::search_url(request.kind, &request.query)?;
    let report = ctx.app.dependencies.status(&ctx.app.backend);
    if !(report.preview_available && report.network_available) {
        ctx.emit(EngineEvent::DependenciesChanged);
        return Err("Install the required playback and HTTPS components before opening the search browser. Use the workspace repair control.".into());
    }
    ctx.host.open_search_browser(url.as_str())
}

/// Former browser navigation/popup policy (`resources::allowed_url`).
pub(crate) fn search_browser_allows(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| resources::allowed_url(&url))
}

/// Former browser download hook (`remember` with the captured cookie, then notify).
pub(crate) fn search_browser_download(
    ctx: &Ctx,
    request: SearchBrowserDownloadRequest,
) -> EngineResult<PendingInfo> {
    let parsed =
        Url::parse(&request.url).map_err(|_| "Enter a valid public HTTPS download URL".to_string());
    let result = parsed.and_then(|url| {
        ctx.app
            .resources
            .remember(url, request.kind, request.cookie)
    });
    match result {
        Ok(info) => {
            ctx.emit(EngineEvent::ResourceDownloadRequested { info: info.clone() });
            Ok(info)
        }
        Err(error) => {
            ctx.emit(EngineEvent::ResourceError {
                error: error.clone(),
            });
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_navigation_is_limited_to_public_https_search_pages() {
        assert!(!search_browser_allows("tauri://localhost/index.html"));
        assert!(!search_browser_allows("http://example.com/"));
        assert!(!search_browser_allows("https://127.0.0.1/"));
        assert!(!search_browser_allows("not a url"));
    }
}
