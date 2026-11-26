//! Project model. Mirrors `apps/desktop/src/types/index.ts` and the factories in
//! `stores/projectStore.ts`. JSON field names match the reference `.reelio` schema 3 so
//! projects stay interchangeable with the reference application.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    #[default]
    Mp4,
    M4v,
    #[serde(rename = "matroska")]
    Matroska,
}

impl Container {
    /// Labels shown in the output-container buttons (`MKV`, `MP4`, `M4V`).
    pub fn label(self) -> &'static str {
        match self {
            Container::Mp4 => "MP4",
            Container::M4v => "M4V",
            Container::Matroska => "MKV",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Data,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    pub index: i64,
    pub kind: Kind,
    pub codec: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub forced: bool,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

impl Stream {
    pub fn prop(&self, key: &str) -> &str {
        self.properties.get(key).map(String::as_str).unwrap_or("")
    }
    pub fn attached_pic(&self) -> bool {
        self.prop("attached_pic") == "1"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub path: String,
    pub format: String,
    pub size: u64,
    #[serde(default)]
    pub modified_ms: i64,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    pub streams: Vec<Stream>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioRole {
    Mix,
    Separate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub asset_id: String,
    pub stream_index: i64,
    pub enabled: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub forced: bool,
    #[serde(default)]
    pub commentary: bool,
    #[serde(default)]
    pub descriptive: bool,
    #[serde(default)]
    pub offset_ms: i64,
    #[serde(default)]
    pub trim_start_ms: i64,
    /// `None` means the clip runs to the source end (JSON `null`).
    #[serde(default)]
    pub trim_end_ms: Option<i64>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub audio_role: Option<AudioRole>,
}

/// A partial track update. `trim_end_ms: Some(None)` sets the clip back to the source end.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackPatch {
    pub enabled: Option<bool>,
    pub title: Option<String>,
    pub language: Option<String>,
    pub default: Option<bool>,
    pub forced: Option<bool>,
    pub commentary: Option<bool>,
    pub descriptive: Option<bool>,
    pub offset_ms: Option<i64>,
    pub trim_start_ms: Option<i64>,
    pub trim_end_ms: Option<Option<i64>>,
    pub muted: Option<bool>,
    pub audio_role: Option<AudioRole>,
}

impl TrackPatch {
    pub fn apply(&self, t: &mut Track) {
        if let Some(v) = self.enabled {
            t.enabled = v;
        }
        if let Some(v) = &self.title {
            t.title = v.clone();
        }
        if let Some(v) = &self.language {
            t.language = v.clone();
        }
        if let Some(v) = self.default {
            t.default = v;
        }
        if let Some(v) = self.forced {
            t.forced = v;
        }
        if let Some(v) = self.commentary {
            t.commentary = v;
        }
        if let Some(v) = self.descriptive {
            t.descriptive = v;
        }
        if let Some(v) = self.offset_ms {
            t.offset_ms = v;
        }
        if let Some(v) = self.trim_start_ms {
            t.trim_start_ms = v;
        }
        if let Some(v) = self.trim_end_ms {
            t.trim_end_ms = v;
        }
        if let Some(v) = self.muted {
            t.muted = v;
        }
        if let Some(v) = self.audio_role {
            t.audio_role = Some(v);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artwork {
    pub jpeg: String,
    pub width: u32,
    pub height: u32,
    pub source_label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub main_video_track_id: Option<String>,
    #[serde(default)]
    pub artwork: Option<Artwork>,
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub container: Container,
    pub assets: Vec<Asset>,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
}

/// Identifier generator. The engine/host supplies UUID-like values; tests use a counter.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Mirrors `isImageAsset`: still pictures (JPEG/PNG/WebP pipes, AVIF brand, or a still stream)
/// with no audio and no multi-frame video.
pub fn is_image_asset(asset: Option<&Asset>) -> bool {
    let Some(a) = asset else { return false };
    if a.streams
        .iter()
        .any(|s| s.kind == Kind::Audio || s.prop("nb_frames").parse::<i64>().unwrap_or(0) > 1)
    {
        return false;
    }
    ["jpeg_pipe", "png_pipe", "webp_pipe"].contains(&a.format.as_str())
        || a.metadata
            .get("major_brand")
            .map(|b| b.trim() == "avif")
            .unwrap_or(false)
        || a.streams.iter().any(|s| s.prop("still_image") == "1")
}

pub fn empty_project() -> Project {
    Project {
        main_video_track_id: None,
        artwork: None,
        schema_version: 3,
        id: new_id(),
        name: "Untitled project".into(),
        container: Container::Mp4,
        assets: vec![],
        tracks: vec![],
        metadata: BTreeMap::new(),
        chapters: vec![],
    }
}

impl Project {
    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }
    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
    pub fn track_asset(&self, t: &Track) -> Option<&Asset> {
        self.asset(&t.asset_id)
    }
    /// Stream backing a track (`sourceStream` in the reference).
    pub fn track_stream(&self, t: &Track) -> Option<&Stream> {
        self.track_asset(t)?
            .streams
            .iter()
            .find(|s| s.index == t.stream_index)
    }
    pub fn track_kind(&self, t: &Track) -> Option<Kind> {
        self.track_stream(t).map(|s| s.kind)
    }
    /// Stream kind for an asset and stream index (`sourceStream` without the track).
    pub fn track_kind_of(&self, asset_id: &str, index: i64) -> Option<Kind> {
        self.asset(asset_id)?
            .streams
            .iter()
            .find(|s| s.index == index)
            .map(|s| s.kind)
    }
}

/// Mirrors `assetTracks`: one track per stream with the reference's default-enable rules.
pub fn asset_tracks(p: &Project, asset: &Asset) -> Vec<Track> {
    if is_image_asset(Some(asset)) {
        return vec![];
    }
    let asset_has_video = asset
        .streams
        .iter()
        .any(|x| x.kind == Kind::Video && !x.attached_pic());
    let project_has_video = p.assets.iter().any(|a| {
        a.streams
            .iter()
            .any(|x| x.kind == Kind::Video && !x.attached_pic())
    });
    let first_audio = asset
        .streams
        .iter()
        .find(|x| x.kind == Kind::Audio)
        .map(|s| s.index);
    asset
        .streams
        .iter()
        .map(|s| {
            let kind_ok = matches!(s.kind, Kind::Video | Kind::Audio | Kind::Subtitle);
            let enabled = kind_ok && !s.attached_pic() && !(asset_has_video && project_has_video);
            // Only the first existing default of this kind stays default.
            let other_default = p
                .tracks
                .iter()
                .any(|t| t.enabled && t.default && p.track_kind(t) == Some(s.kind));
            let audio_role = if s.kind == Kind::Audio && first_audio == Some(s.index) {
                AudioRole::Mix
            } else {
                AudioRole::Separate
            };
            Track {
                id: new_id(),
                asset_id: asset.id.clone(),
                stream_index: s.index,
                enabled,
                title: s.title.clone(),
                language: s.language.clone(),
                default: s.default && !other_default,
                forced: s.forced,
                commentary: s.prop("comment") == "1",
                descriptive: s.prop("visual_impaired") == "1",
                offset_ms: 0,
                trim_start_ms: 0,
                trim_end_ms: None,
                muted: false,
                audio_role: Some(audio_role),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    Duplicate,
}

impl AddError {
    pub fn message(&self) -> &'static str {
        match self {
            AddError::Duplicate => {
                "This file is already in the project. No duplicate tracks were added."
            }
        }
    }
}

/// Mirrors `addAsset`: rejects duplicate paths, names an empty project after its first media
/// file, and adopts metadata and chapters from the first non-image source.
pub fn add_asset(p: &Project, asset: &Asset) -> Result<Project, AddError> {
    if p.assets.iter().any(|a| a.path == asset.path) {
        return Err(AddError::Duplicate);
    }
    let has_media =
        is_image_asset(Some(asset)) || p.assets.iter().any(|i| !is_image_asset(Some(i)));
    let mut next = p.clone();
    if !has_media {
        if let Some(stem) = file_stem(&asset.path) {
            next.name = stem;
        }
        next.metadata = asset.metadata.clone();
        next.chapters = asset.chapters.clone();
    }
    let tracks = asset_tracks(p, asset);
    next.assets.push(asset.clone());
    next.tracks.extend(tracks);
    Ok(next)
}

/// `basename` from `types/index.ts`: last path segment, splitting on both separators.
pub fn basename(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn file_stem(path: &str) -> Option<String> {
    let name = basename(path);
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name.as_str(),
    };
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

/// `time()` from `types/index.ts`: `HH:MM:SS`, or `--:--` when unknown.
pub fn time_hms(ms: Option<i64>) -> String {
    match ms {
        None => "--:--".into(),
        Some(ms) => {
            let ms = ms.max(0);
            format!(
                "{:02}:{:02}:{:02}",
                ms / 3_600_000,
                (ms / 60_000) % 60,
                (ms / 1000) % 60
            )
        }
    }
}

/// `bytes()` from `types/index.ts`.
pub fn bytes_label(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.2} GB", n as f64 / 1e9)
    } else {
        format!("{:.1} MB", n as f64 / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(index: i64, kind: Kind) -> Stream {
        Stream {
            index,
            kind,
            codec: "h264".into(),
            title: String::new(),
            language: "und".into(),
            duration_ms: Some(10_000),
            default: false,
            forced: false,
            properties: BTreeMap::new(),
        }
    }

    fn asset(id: &str, path: &str, streams: Vec<Stream>) -> Asset {
        Asset {
            id: id.into(),
            path: path.into(),
            format: "matroska,webm".into(),
            size: 5_000_000,
            modified_ms: 0,
            duration_ms: Some(10_000),
            streams,
            metadata: BTreeMap::new(),
            chapters: vec![],
        }
    }

    #[test]
    fn image_assets_have_no_tracks_and_never_replace_movie_metadata() {
        let mut image = asset("img", "/x/bunny.webp", vec![stream(0, Kind::Video)]);
        image.format = "webp_pipe".into();
        image.streams[0]
            .properties
            .insert("nb_frames".into(), "1".into());
        assert!(is_image_asset(Some(&image)));
        assert!(asset_tracks(&empty_project(), &image).is_empty());

        let mut p = empty_project();
        p = add_asset(
            &p,
            &asset("v", "/x/movie.mkv", vec![stream(0, Kind::Video)]),
        )
        .unwrap();
        p.metadata.insert("title".into(), "Movie".into());
        assert_eq!(
            p.name, "movie",
            "the first media file names an empty project"
        );
        let next = add_asset(&p, &image).unwrap();
        assert_eq!(
            next.metadata.get("title").map(String::as_str),
            Some("Movie")
        );
        assert_eq!(next.name, "movie");
    }

    #[test]
    fn duplicate_sources_are_rejected_without_tracks() {
        let p = add_asset(
            &empty_project(),
            &asset("v", "/x/a.mkv", vec![stream(0, Kind::Video)]),
        )
        .unwrap();
        let err =
            add_asset(&p, &asset("v2", "/x/a.mkv", vec![stream(0, Kind::Video)])).unwrap_err();
        assert_eq!(
            err.message(),
            "This file is already in the project. No duplicate tracks were added."
        );
    }

    #[test]
    fn cover_pictures_are_excluded_from_video_mapping() {
        let mut attached = stream(1, Kind::Video);
        attached
            .properties
            .insert("attached_pic".into(), "1".into());
        let p = add_asset(
            &empty_project(),
            &asset("v", "/x/a.mkv", vec![stream(0, Kind::Video)]),
        )
        .unwrap();
        let tracks = asset_tracks(&p, &asset("w", "/x/b.mkv", vec![attached]));
        assert!(!tracks[0].enabled);
    }

    #[test]
    fn first_audio_is_mix_and_others_are_separate() {
        let a = asset(
            "v",
            "/x/a.mkv",
            vec![
                stream(0, Kind::Video),
                stream(1, Kind::Audio),
                stream(2, Kind::Audio),
            ],
        );
        let tracks = asset_tracks(&empty_project(), &a);
        assert_eq!(tracks[1].audio_role, Some(AudioRole::Mix));
        assert_eq!(tracks[2].audio_role, Some(AudioRole::Separate));
    }

    #[test]
    fn time_and_bytes_labels_match_reference() {
        assert_eq!(time_hms(None), "--:--");
        assert_eq!(time_hms(Some(3_723_000)), "01:02:03");
        assert_eq!(bytes_label(5_300_000), "5.3 MB");
        assert_eq!(bytes_label(2_500_000_000), "2.50 GB");
        assert_eq!(basename("C:\\media\\x.mp4"), "x.mp4");
    }
}
