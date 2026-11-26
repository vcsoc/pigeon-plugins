//! Timeline geometry. Faithful port of `features/timeline/geometry.ts`. Numeric rounding follows
//! JavaScript `Math.round` (half toward +infinity) so the same drags and trims land on the same
//! milliseconds as the reference application.

use crate::model::{is_image_asset, Kind, Project, Track, TrackPatch};

pub const MAX_OFFSET: i64 = 86_400_000;
pub const MIN_CLIP_WIDTH: f64 = 28.0;
/// Minimum trimmable length in milliseconds.
pub const MIN_TRIM_MS: f64 = 10.0;
/// Pixel distance within which a trim edge snaps to the reference video end.
pub const SNAP_PIXELS: f64 = 8.0;

/// JavaScript `Math.round`.
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

const TEXT_CODECS: [&str; 6] = ["subrip", "ass", "ssa", "webvtt", "mov_text", "text"];

fn is_text_codec(codec: &str) -> bool {
    TEXT_CODECS.contains(&codec)
}

pub fn offset_after_drag(original: i64, pixels: f64, pixels_per_ms: f64) -> i64 {
    let raw = js_round((original as f64 + pixels / pixels_per_ms) / 10.0) * 10.0;
    raw.clamp(-(MAX_OFFSET as f64), MAX_OFFSET as f64) as i64
}

/// Returns `(zoom, scroll)` after an exponential zoom around `pointer`.
pub fn zoom_at(zoom: f64, delta: f64, scroll: f64, pointer: f64) -> (f64, f64) {
    let next = (zoom * (-delta * 0.002).exp()).clamp(0.25, 512.0);
    (next, ((scroll + pointer) * next / zoom - pointer).max(0.0))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: f64,
    pub scroll: f64,
}

impl Default for View {
    fn default() -> Self {
        View {
            zoom: 1.0,
            scroll: 0.0,
        }
    }
}

/// Mirrors `zoomTimeline`. `pointer` is the cursor x inside the lane viewport, if hovered.
pub fn zoom_timeline(
    view: View,
    delta: f64,
    width: f64,
    duration: f64,
    playhead: f64,
    pointer: Option<f64>,
) -> View {
    let marker = (playhead * width / duration) * view.zoom;
    let position = marker - view.scroll;
    let anchor = pointer.unwrap_or(if position < 16.0 || position > width - 16.0 {
        width / 2.0
    } else {
        position
    });
    let base = if pointer.is_none() {
        marker - anchor
    } else {
        view.scroll
    };
    let (zoom, scroll) = zoom_at(view.zoom, delta, base, anchor);
    View {
        zoom,
        scroll: scroll.min(width * (zoom - 1.0).max(0.0)).max(0.0),
    }
}

/// Ruler tick spacing in milliseconds for a scale (pixels per millisecond).
pub fn ruler_interval(pixels_per_ms: f64) -> f64 {
    let minimum = 110.0 / pixels_per_ms.max(f64::EPSILON);
    let unit = 10f64.powf(minimum.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|n| n * unit >= minimum)
        .unwrap_or(10.0)
        * unit
}

/// Stream duration, else asset duration, else the first video asset's duration, else 10 s.
pub fn track_duration(p: &Project, t: &Track) -> f64 {
    source_duration(p, t).unwrap_or_else(|| {
        p.assets
            .iter()
            .find(|a| a.streams.iter().any(|s| s.kind == Kind::Video))
            .and_then(|a| a.duration_ms)
            .map(|d| d as f64)
            .unwrap_or(10_000.0)
    })
}

/// Stream duration, else asset duration; `None` when unknown.
pub fn source_duration(p: &Project, t: &Track) -> Option<f64> {
    let asset = p.asset(&t.asset_id)?;
    asset
        .streams
        .iter()
        .find(|s| s.index == t.stream_index)
        .and_then(|s| s.duration_ms)
        .or(asset.duration_ms)
        .map(|d| d as f64)
}

/// `(start, end)` retained source interval of a clip.
pub fn clip_bounds(p: &Project, t: &Track) -> (f64, f64) {
    (
        t.trim_start_ms as f64,
        t.trim_end_ms
            .map(|v| v as f64)
            .unwrap_or_else(|| track_duration(p, t)),
    )
}

fn is_image(p: &Project, t: &Track) -> bool {
    is_image_asset(p.asset(&t.asset_id))
}

/// Timeline fit extent: retained clip ends (including excluded clips), plus 15 percent.
pub fn timeline_viewport_duration(p: &Project) -> f64 {
    let max_end = p
        .tracks
        .iter()
        .filter(|t| {
            if is_image(p, t) {
                return false;
            }
            p.track_stream(t)
                .map(|s| {
                    matches!(s.kind, Kind::Video | Kind::Audio | Kind::Subtitle)
                        && !s.attached_pic()
                })
                .unwrap_or(false)
        })
        .map(|t| clip_bounds(p, t).1 + t.offset_ms as f64)
        .fold(10_000.0_f64, f64::max);
    max_end * 1.15
}

fn is_real_video_stream(p: &Project, t: &Track) -> bool {
    p.asset(&t.asset_id)
        .map(|a| {
            a.streams
                .iter()
                .any(|s| s.index == t.stream_index && s.kind == Kind::Video && !s.attached_pic())
        })
        .unwrap_or(false)
}

fn valid_duration(d: Option<f64>) -> Option<f64> {
    d.filter(|d| d.is_finite() && *d > 0.0)
}

/// Latest included video end in output time; `None` when unknown or no video is included.
pub fn video_timeline_end(p: &Project) -> Option<f64> {
    let videos: Vec<&Track> = p
        .tracks
        .iter()
        .filter(|t| t.enabled && !t.muted && !is_image(p, t) && is_real_video_stream(p, t))
        .collect();
    if videos.is_empty()
        || videos
            .iter()
            .any(|v| valid_duration(source_duration(p, v)).is_none())
    {
        return None;
    }
    let end = videos
        .iter()
        .map(|v| clip_bounds(p, v).1 + v.offset_ms as f64)
        .fold(f64::NEG_INFINITY, f64::max);
    (end > 0.0).then_some(end)
}

/// End of the main video reference when one is chosen, else `video_timeline_end`.
pub fn reference_video_end(p: &Project) -> Option<f64> {
    let Some(main) = &p.main_video_track_id else {
        return video_timeline_end(p);
    };
    let t = p.track(main)?;
    let asset = p.asset(&t.asset_id);
    let stream = p.track_stream(t)?;
    if stream.kind != Kind::Video || stream.attached_pic() || is_image_asset(asset) {
        return None;
    }
    valid_duration(source_duration(p, t))?;
    let end = clip_bounds(p, t).1 + t.offset_ms as f64;
    (end > 0.0).then_some(end)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrimSnap {
    /// Snap target in the track's source time (reference end minus the track offset).
    pub end: f64,
    /// Distance in milliseconds (8 screen pixels at this scale).
    pub tolerance: f64,
    /// Trim end value at which the drag engages the snap.
    pub activation_end: f64,
}

/// Mirrors `videoTrimSnap`.
pub fn video_trim_snap(p: &Project, t: &Track, scale: f64) -> Option<TrimSnap> {
    let stream = p.track_stream(t)?;
    let end = reference_video_end(p)?;
    if !scale.is_finite() || scale <= 0.0 || source_duration(p, t).is_none() {
        return None;
    }
    let eligible = stream.kind == Kind::Audio
        || (stream.kind == Kind::Video
            && p.main_video_track_id.is_some()
            && Some(&t.id) != p.main_video_track_id.as_ref())
        || (stream.kind == Kind::Subtitle && is_text_codec(&stream.codec));
    if !eligible {
        return None;
    }
    let visible_right = |track: &Track| {
        let (start, track_end) = clip_bounds(p, track);
        let left = (start + track.offset_ms as f64).max(0.0);
        left * scale + MIN_CLIP_WIDTH.max((track_end + track.offset_ms as f64 - left) * scale)
    };
    let anchor = p.tracks.iter().find(|track| {
        let video_ok = match &p.main_video_track_id {
            Some(main) => &track.id == main,
            None => track.enabled && !track.muted,
        };
        let Some(s) = p.track_stream(track) else {
            return false;
        };
        video_ok
            && s.kind == Kind::Video
            && !is_image(p, track)
            && !s.attached_pic()
            && clip_bounds(p, track).1 + track.offset_ms as f64 == end
    })?;
    let (_, t_end) = clip_bounds(p, t);
    Some(TrimSnap {
        end: end - t.offset_ms as f64,
        tolerance: SNAP_PIXELS / scale,
        activation_end: t_end + (visible_right(anchor) - visible_right(t)) / scale,
    })
}

/// Result of `clipToVideo`: the trim change (if any) and the explanation shown to the user.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipToVideo {
    pub patch: Option<TrackPatch>,
    pub reason: String,
}

/// Mirrors `clipToVideo`: shorten a clip's right edge to the included video's retained end.
pub fn clip_to_video(p: &Project, t: &Track) -> ClipToVideo {
    let unavailable = |reason: &str| ClipToVideo {
        patch: None,
        reason: reason.into(),
    };
    let Some(stream) = p.track_stream(t) else {
        return unavailable("Clip to video applies to video, audio and text-subtitle clips.");
    };
    if !matches!(stream.kind, Kind::Video | Kind::Audio | Kind::Subtitle) || is_image(p, t) {
        return unavailable("Clip to video applies to video, audio and text-subtitle clips.");
    }
    if p.main_video_track_id.as_ref() == Some(&t.id) {
        return unavailable("This is the main video reference; choose another clip to fit.");
    }
    if stream.kind == Kind::Subtitle && !is_text_codec(&stream.codec) {
        return unavailable("Image-subtitle trimming is not supported.");
    }
    let Some(video_end) = reference_video_end(p) else {
        return unavailable(
            "Select a main video or include a video with a known, nonempty duration first.",
        );
    };
    let Some(duration) = valid_duration(source_duration(p, t)) else {
        return unavailable("This clip's source duration is not available.");
    };
    let (start, end_bound) = clip_bounds(p, t);
    let end = js_round(video_end - t.offset_ms as f64);
    if end - start.max(-(t.offset_ms as f64)).max(0.0) < MIN_TRIM_MS {
        return unavailable("This clip has no trimmable overlap with the video.");
    }
    if end >= end_bound || end > duration {
        return unavailable(
            "This clip already ends at or before the video; source material will not be extended.",
        );
    }
    ClipToVideo {
        patch: Some(TrackPatch {
            trim_end_ms: Some(Some(end as i64)),
            ..Default::default()
        }),
        reason: "Shorten this clip to end with the video (Ctrl+Alt+click).".into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// Mirrors `trimAfterDrag`. `delta` is in milliseconds.
pub fn trim_after_drag(
    edge: Edge,
    start: f64,
    end: f64,
    duration: f64,
    offset: i64,
    delta: f64,
    snap: Option<TrimSnap>,
) -> TrackPatch {
    let minimum = (-(offset as f64)).max(0.0);
    if let (Edge::End, Some(snap)) = (edge, snap) {
        let target = snap.activation_end;
        if (end + delta - target).abs() <= snap.tolerance
            && snap.end >= start.max(minimum) + MIN_TRIM_MS
            && snap.end <= duration
        {
            return TrackPatch {
                trim_end_ms: Some(if snap.end >= duration {
                    None
                } else {
                    Some(snap.end as i64)
                }),
                ..Default::default()
            };
        }
    }
    let base = match edge {
        Edge::Start => start.max(minimum),
        Edge::End => end,
    };
    let value = js_round((base + delta) / 10.0) * 10.0;
    match edge {
        Edge::Start => TrackPatch {
            trim_start_ms: Some(minimum.max((end - MIN_TRIM_MS).min(value)) as i64),
            ..Default::default()
        },
        Edge::End => {
            let next = (start.max(minimum) + MIN_TRIM_MS).max(duration.min(value));
            TrackPatch {
                trim_end_ms: Some(if next >= duration {
                    None
                } else {
                    Some(next as i64)
                }),
                ..Default::default()
            }
        }
    }
}

/// Mirrors `timelineDuration`: at least one second, else the furthest retained clip end.
pub fn timeline_duration(p: &Project) -> f64 {
    p.tracks
        .iter()
        .filter(|t| t.enabled && !t.muted && !is_image(p, t))
        .map(|t| clip_bounds(p, t).1 + t.offset_ms as f64)
        .fold(1000.0_f64, f64::max)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
}

/// Mirrors `subtitleAtOffset`: clip cues to the retained interval, shift by the offset.
pub fn subtitle_at_offset<T: Clone>(
    cues: &[T],
    cue_of: impl Fn(&T) -> Cue,
    with: impl Fn(&T, Cue) -> T,
    offset: f64,
    trim_start: f64,
    trim_end: f64,
) -> Vec<T> {
    cues.iter()
        .filter(|c| {
            let c = cue_of(c);
            c.end.min(trim_end) > c.start.max(trim_start)
        })
        .map(|c| {
            let cue = cue_of(c);
            with(
                c,
                Cue {
                    start: 0f64.max(cue.start.max(trim_start) + offset),
                    end: cue.end.min(trim_end) + offset,
                },
            )
        })
        .filter(|c| {
            let c = cue_of(c);
            c.end > c.start
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Asset, Stream, Track};
    use std::collections::BTreeMap;

    fn stream(index: i64, kind: Kind, codec: &str, duration: Option<i64>) -> Stream {
        Stream {
            index,
            kind,
            codec: codec.into(),
            title: String::new(),
            language: "und".into(),
            duration_ms: duration,
            default: false,
            forced: false,
            properties: BTreeMap::new(),
        }
    }

    fn track(id: &str, asset: &str, index: i64, offset: i64) -> Track {
        Track {
            id: id.into(),
            asset_id: asset.into(),
            stream_index: index,
            enabled: true,
            title: id.into(),
            language: "und".into(),
            default: false,
            forced: false,
            commentary: false,
            descriptive: false,
            offset_ms: offset,
            trim_start_ms: 0,
            trim_end_ms: None,
            muted: false,
            audio_role: None,
        }
    }

    fn project() -> Project {
        let video = Asset {
            id: "v".into(),
            path: "/m/v.mp4".into(),
            format: "mp4".into(),
            size: 1,
            modified_ms: 0,
            duration_ms: Some(10_000),
            streams: vec![stream(0, Kind::Video, "h264", Some(10_000))],
            metadata: BTreeMap::new(),
            chapters: vec![],
        };
        let audio = Asset {
            id: "a".into(),
            path: "/m/a.mka".into(),
            format: "matroska".into(),
            size: 1,
            modified_ms: 0,
            duration_ms: Some(12_000),
            streams: vec![stream(0, Kind::Audio, "aac", Some(12_000))],
            metadata: BTreeMap::new(),
            chapters: vec![],
        };
        Project {
            main_video_track_id: None,
            artwork: None,
            schema_version: 3,
            id: "p".into(),
            name: "p".into(),
            container: crate::model::Container::Mp4,
            assets: vec![video, audio],
            tracks: vec![track("tv", "v", 0, 0), track("ta", "a", 0, 0)],
            metadata: BTreeMap::new(),
            chapters: vec![],
        }
    }

    #[test]
    fn js_round_rounds_halves_up() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(2.4), 2.0);
    }

    #[test]
    fn drags_quantize_to_ten_milliseconds_and_clamp() {
        assert_eq!(
            offset_after_drag(0, 11.0, 0.1),
            110,
            "11 px at 0.1 px/ms is 110 ms"
        );
        assert_eq!(offset_after_drag(0, 1e12, 0.1), MAX_OFFSET);
        assert_eq!(offset_after_drag(0, -1e12, 0.1), -MAX_OFFSET);
    }

    #[test]
    fn ruler_interval_is_a_nice_step() {
        assert_eq!(
            ruler_interval(0.1),
            2000.0,
            "1100 ms minimum rounds up to the 2 s step"
        );
        assert!(ruler_interval(1000.0) <= 10.0);
    }

    #[test]
    fn zoom_keeps_the_pointer_time_stationary() {
        // Time under the pointer is (scroll + pointer) / zoom; it must not change while zooming.
        let (zoom, scroll) = zoom_at(1.0, -100.0, 0.0, 300.0);
        let before = (0.0 + 300.0) / 1.0;
        let after = (scroll + 300.0) / zoom;
        assert!((before - after).abs() < 1e-9);
        assert!(zoom > 1.0, "negative delta zooms in");
    }

    #[test]
    fn retained_interval_fit_ignores_hours_long_original_subtitles() {
        let mut p = project();
        p.tracks[1].trim_end_ms = Some(4_000);
        // Video ends at 10 s; the trimmed 12 s audio contributes only its retained 4 s.
        assert!((timeline_viewport_duration(&p) - 10_000.0 * 1.15).abs() < 1e-6);
    }

    #[test]
    fn trim_drag_never_crosses_edges_or_source_bounds() {
        let p = project();
        let t = &p.tracks[1];
        let patch = trim_after_drag(Edge::Start, 0.0, 12_000.0, 12_000.0, 0, 1e9, None);
        assert_eq!(patch.trim_start_ms, Some(11_990));
        let patch = trim_after_drag(Edge::End, 0.0, 12_000.0, 12_000.0, 0, 1e9, None);
        assert_eq!(
            patch.trim_end_ms,
            Some(None),
            "reaching the source end sets the clip back to the source end"
        );
        let _ = t;
    }

    #[test]
    fn clip_to_video_shortens_only_with_real_overlap() {
        let p = project();
        let action = clip_to_video(&p, &p.tracks[1]);
        assert_eq!(action.patch.unwrap().trim_end_ms, Some(Some(10_000)));
        let mut p2 = p.clone();
        p2.tracks[0].enabled = false;
        assert!(clip_to_video(&p2, &p2.tracks[1]).patch.is_none());
    }

    #[test]
    fn snap_engages_within_eight_pixels_of_the_video_end() {
        let mut p = project();
        p.main_video_track_id = Some("tv".into());
        p.tracks[1].trim_end_ms = Some(9_950);
        let snap = video_trim_snap(&p, &p.tracks[1], 0.1).expect("snap available");
        assert!((snap.tolerance - 80.0).abs() < 1e-9);
        assert_eq!(snap.end, 10_000.0);
    }

    #[test]
    fn subtitle_cues_clip_and_shift() {
        #[derive(Clone)]
        struct C(f64, f64);
        let cues = vec![C(0.0, 500.0), C(900.0, 2000.0), C(3000.0, 4000.0)];
        let out = subtitle_at_offset(
            &cues,
            |c| Cue {
                start: c.0,
                end: c.1,
            },
            |_, cue| C(cue.start, cue.end),
            -1000.0,
            0.0,
            f64::INFINITY,
        );
        assert_eq!(out.len(), 2, "cue entirely before zero is dropped");
        assert_eq!(out[0].0, 0.0);
        assert_eq!(out[1].0, 2000.0);
    }
}
