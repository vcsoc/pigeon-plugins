//! Pointer and keyboard handling for timeline lane canvases. Port of the drag machines in
//! `features/timeline/Timeline.tsx` (`Lane`: clip drag, trim drag, chapter drag, ruler seek).
//! A completed gesture is one undoable edit; a cancelled gesture leaves the project untouched.
//! Live feedback is kept in [`Drafts`] and read back by the canvas builder.

use crate::canvas::RULER_H;
use crate::geometry::{
    self, clip_bounds, offset_after_drag, source_duration, trim_after_drag, Edge, MAX_OFFSET,
    MIN_CLIP_WIDTH,
};
use crate::model::{Track, TrackPatch};
use crate::state::State;
use serde_json::Value;

/// Trim/clip/chapter feedback shown while a gesture is in progress.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drafts {
    pub offset: Option<(String, i64)>,
    pub trim: Option<(String, TrackPatch)>,
    pub chapter: Option<(usize, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    Clip {
        track: String,
        pointer_x: f64,
        original: i64,
        moved: bool,
    },
    Trim {
        track: String,
        edge: Edge,
        pointer_x: f64,
        start: f64,
        end: f64,
        duration: f64,
        scale: f64,
        snap: Option<geometry::TrimSnap>,
        patch: Option<TrackPatch>,
        moved: bool,
    },
    Chapter {
        index: usize,
        pointer_x: f64,
        original: f64,
        current: f64,
        moved: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub phase: Phase,
    pub x: f64,
    pub y: f64,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Where a pointer lands inside a lane canvas.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Ruler,
    TrimStart,
    TrimEnd,
    Clip,
    Chapter(usize),
    Surface,
}

const HANDLE_W: f64 = 10.0;

fn world_x(state: &State, x: f64) -> f64 {
    x + state.timeline_view.scroll
}

/// Hit test in lane canvas coordinates (`x` is the canvas x, scroll applied here).
pub fn hit(state: &State, track: &Track, x: f64, y: f64) -> Hit {
    let p = state.project();
    let scale = state.timeline_scale();
    // Chapter pins sit on the top edge of video lanes, over the ruler band.
    if y < RULER_H + 6.0 && p.track_kind(track) == Some(crate::model::Kind::Video) {
        for (i, c) in p.chapters.iter().enumerate() {
            let cx = c.start_ms as f64 * scale - state.timeline_view.scroll;
            if (x - cx).abs() <= 6.0 {
                return Hit::Chapter(i);
            }
        }
    }
    if y < RULER_H {
        return Hit::Ruler;
    }
    let (start, end) = clip_bounds(p, track);
    let offset = track.offset_ms as f64;
    let left = (offset + start).max(0.0) * scale - state.timeline_view.scroll;
    let width = MIN_CLIP_WIDTH.max((end + offset - (offset + start).max(0.0)) * scale);
    let trimmable = source_duration(p, track)
        .map(|d| d >= 10.0)
        .unwrap_or(false);
    if trimmable {
        if x >= left && x <= left + HANDLE_W {
            return Hit::TrimStart;
        }
        if x >= left + width - HANDLE_W && x <= left + width {
            return Hit::TrimEnd;
        }
    }
    if x >= left && x <= left + width {
        return Hit::Clip;
    }
    Hit::Surface
}

/// Pointer events from a lane canvas (`lane.canvas:<track>`).
pub fn pointer(state: &mut State, track_id: &str, ev: Pointer) {
    let Some(track) = state.project().track(track_id).cloned() else {
        return;
    };
    match ev.phase {
        Phase::Down => {
            let x = world_x(state, ev.x);
            match hit(state, &track, ev.x, ev.y) {
                Hit::Ruler => {
                    let ms = (x / state.timeline_scale()).max(0.0);
                    state.seek_playhead(ms);
                }
                Hit::Clip => {
                    state.select_track_by_id(track_id);
                    if ev.ctrl && ev.alt {
                        // Ctrl+Alt+click: clip to video (applied on release, see below).
                        return;
                    }
                    state.gesture = Some(Gesture::Clip {
                        track: track_id.into(),
                        pointer_x: ev.x,
                        original: track.offset_ms,
                        moved: false,
                    });
                }
                Hit::TrimStart | Hit::TrimEnd => {
                    let edge = if hit_is_start(state, &track, ev.x, ev.y) {
                        Edge::Start
                    } else {
                        Edge::End
                    };
                    let (start, end) = clip_bounds(state.project(), &track);
                    let scale = state.timeline_scale();
                    state.select_track_by_id(track_id);
                    state.gesture = Some(Gesture::Trim {
                        track: track_id.into(),
                        edge,
                        pointer_x: ev.x,
                        start,
                        end,
                        duration: source_duration(state.project(), &track).unwrap_or(0.0),
                        scale,
                        snap: if edge == Edge::End
                            && track.id
                                != state
                                    .project()
                                    .main_video_track_id
                                    .clone()
                                    .unwrap_or_default()
                        {
                            geometry::video_trim_snap(state.project(), &track, scale)
                        } else {
                            None
                        },
                        patch: None,
                        moved: false,
                    });
                }
                Hit::Chapter(i) => {
                    let original = state.project().chapters[i].start_ms as f64;
                    state.gesture = Some(Gesture::Chapter {
                        index: i,
                        pointer_x: ev.x,
                        original,
                        current: original,
                        moved: false,
                    });
                }
                Hit::Surface => {
                    // Background clicks on video lanes add a chapter on release when not dragged.
                    let _ = x;
                }
            }
        }
        Phase::Move => {
            let gesture = state.gesture.take();
            state.gesture = gesture.map(|g| match g {
                Gesture::Clip {
                    track,
                    pointer_x,
                    original,
                    moved,
                } => {
                    let dx = ev.x - pointer_x;
                    let moved = moved || dx.abs() > 3.0;
                    let next = offset_after_drag(original, dx, state.timeline_scale());
                    state.drafts.offset = Some((track.clone(), next));
                    Gesture::Clip {
                        track,
                        pointer_x,
                        original,
                        moved,
                    }
                }
                Gesture::Trim {
                    track,
                    edge,
                    pointer_x,
                    start,
                    end,
                    duration,
                    scale,
                    snap,
                    moved,
                    ..
                } => {
                    let dx = ev.x - pointer_x;
                    let moved = moved || dx.abs() > 3.0;
                    let t = state
                        .project()
                        .track(&track)
                        .cloned()
                        .unwrap_or_else(blank_track);
                    let patch =
                        trim_after_drag(edge, start, end, duration, t.offset_ms, dx / scale, snap);
                    state.drafts.trim = Some((track.clone(), patch.clone()));
                    Gesture::Trim {
                        track,
                        edge,
                        pointer_x,
                        start,
                        end,
                        duration,
                        scale,
                        snap,
                        patch: Some(patch),
                        moved,
                    }
                }
                Gesture::Chapter {
                    index,
                    pointer_x,
                    original,
                    moved,
                    ..
                } => {
                    let dx = ev.x - pointer_x;
                    let moved = moved || dx.abs() > 3.0;
                    let max = geometry::timeline_duration(state.project()) - 1.0;
                    let current = (original + dx / state.timeline_scale())
                        .round()
                        .clamp(0.0, max.max(0.0));
                    state.drafts.chapter = Some((index, current));
                    Gesture::Chapter {
                        index,
                        pointer_x,
                        original,
                        current,
                        moved,
                    }
                }
            });
        }
        Phase::Up => finish(state, track_id, ev, false),
        Phase::Cancel => finish(state, track_id, ev, true),
    }
}

fn hit_is_start(state: &State, track: &Track, x: f64, y: f64) -> bool {
    matches!(hit(state, track, x, y), Hit::TrimStart)
}

fn blank_track() -> Track {
    Track {
        id: String::new(),
        asset_id: String::new(),
        stream_index: 0,
        enabled: false,
        title: String::new(),
        language: String::new(),
        default: false,
        forced: false,
        commentary: false,
        descriptive: false,
        offset_ms: 0,
        trim_start_ms: 0,
        trim_end_ms: None,
        muted: false,
        audio_role: None,
    }
}

fn finish(state: &mut State, track_id: &str, ev: Pointer, cancel: bool) {
    let Some(gesture) = state.gesture.take() else {
        // A plain release on a video lane surface adds a chapter at the pointer.
        if ev.phase == Phase::Up {
            if let Some(t) = state.project().track(track_id).cloned() {
                if state.project().track_kind(&t) == Some(crate::model::Kind::Video)
                    && hit(state, &t, ev.x, ev.y) == Hit::Surface
                {
                    let ms = world_x(state, ev.x) / state.timeline_scale();
                    state.add_chapter(ms);
                }
            }
        }
        return;
    };
    let drafts = std::mem::take(&mut state.drafts);
    if cancel {
        return;
    }
    match gesture {
        Gesture::Clip { track, moved, .. } => {
            if let (true, Some((draft_track, next))) = (moved, drafts.offset) {
                if draft_track == track {
                    state.edit_track_offset(&track, next);
                }
            }
        }
        Gesture::Trim {
            track,
            patch,
            moved,
            ..
        } => {
            if let (true, Some(patch)) = (moved, patch) {
                state.edit_track_patch(&track, patch);
            }
        }
        Gesture::Chapter {
            index,
            original,
            current,
            moved,
            ..
        } => {
            if moved && current != original {
                state.move_chapter(index, current);
            } else {
                state.seek_playhead(current);
            }
        }
    }
}

/// Keyboard handling for focused clips and trim handles: arrows move by 100 ms (Shift 1 s, Alt 10 ms).
pub fn key(state: &mut State, track_id: &str, key: &str, shift: bool, alt: bool) {
    let step = if alt {
        10.0
    } else if shift {
        1000.0
    } else {
        100.0
    };
    let Some(track) = state.project().track(track_id).cloned() else {
        return;
    };
    match key {
        "ArrowLeft" | "ArrowRight" => {
            let sign = if key == "ArrowLeft" { -1.0 } else { 1.0 };
            let next = (track.offset_ms as f64 + sign * step)
                .clamp(-(MAX_OFFSET as f64), MAX_OFFSET as f64) as i64;
            state.edit_track_offset(track_id, next);
        }
        _ => {}
    }
}

/// Wheel: Alt zooms the shared timeline around the pointer, otherwise scrolls horizontally.
pub fn wheel(state: &mut State, dx: f64, dy: f64, x: f64, alt: bool) {
    if alt {
        let duration = geometry::timeline_viewport_duration(state.project());
        let playhead = state.playhead;
        state.timeline_view = geometry::zoom_timeline(
            state.timeline_view,
            dy,
            state.timeline_view_width(),
            duration,
            playhead,
            Some(x),
        );
    } else {
        let max = state.timeline_view_width() * (state.timeline_view.zoom - 1.0).max(0.0);
        state.timeline_view.scroll = (state.timeline_view.scroll + dx + dy).clamp(0.0, max);
    }
}

/// Decode a JSON pointer payload from the host into a [`Pointer`].
pub fn parse_pointer(value: &Value) -> Option<Pointer> {
    let phase = match value.get("phase")?.as_str()? {
        "down" => Phase::Down,
        "move" => Phase::Move,
        "up" => Phase::Up,
        "cancel" => Phase::Cancel,
        _ => return None,
    };
    let mods = value.get("mods");
    let flag = |k: &str| {
        mods.and_then(|m| m.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Some(Pointer {
        phase,
        x: value.get("x")?.as_f64()?,
        y: value.get("y")?.as_f64()?,
        ctrl: flag("ctrl"),
        alt: flag("alt"),
        shift: flag("shift"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Event;
    use serde_json::json;

    fn video_state() -> State {
        use crate::model::{Asset, Kind, Stream};
        use std::collections::BTreeMap;
        let mut s = State::new();
        let stream = Stream {
            index: 0,
            kind: Kind::Video,
            codec: "h264".into(),
            title: String::new(),
            language: "und".into(),
            duration_ms: Some(10_000),
            default: false,
            forced: false,
            properties: BTreeMap::new(),
        };
        let asset = Asset {
            id: "v".into(),
            path: "/m/v.mp4".into(),
            format: "mp4".into(),
            size: 1,
            modified_ms: 0,
            duration_ms: Some(10_000),
            streams: vec![stream],
            metadata: BTreeMap::new(),
            chapters: vec![],
        };
        s.edit(|p| {
            p.assets.push(asset);
            p.tracks.push(Track {
                id: "tv".into(),
                asset_id: "v".into(),
                stream_index: 0,
                enabled: true,
                ..blank_track()
            });
        });
        s
    }

    fn ptr(phase: Phase, x: f64, y: f64) -> Pointer {
        Pointer {
            phase,
            x,
            y,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn clip_drag_is_one_undo_step_and_cancel_discards() {
        let mut s = video_state();
        let scale = s.timeline_scale();
        let history_before = s.store.history_len().0;
        pointer(&mut s, "tv", ptr(Phase::Down, 100.0, 40.0));
        pointer(&mut s, "tv", ptr(Phase::Move, 100.0 + 50.0, 40.0));
        pointer(&mut s, "tv", ptr(Phase::Up, 150.0, 40.0));
        let expected = offset_after_drag(0, 50.0, scale);
        assert_eq!(s.project().track("tv").unwrap().offset_ms, expected);
        assert_eq!(
            s.store.history_len().0,
            history_before + 1,
            "one undo step per completed drag"
        );

        let before = s.project().track("tv").unwrap().offset_ms;
        pointer(&mut s, "tv", ptr(Phase::Down, 100.0, 40.0));
        pointer(&mut s, "tv", ptr(Phase::Move, 300.0, 40.0));
        pointer(&mut s, "tv", ptr(Phase::Cancel, 300.0, 40.0));
        assert_eq!(
            s.project().track("tv").unwrap().offset_ms,
            before,
            "cancel discards"
        );
        assert!(s.drafts.offset.is_none());
    }

    #[test]
    fn trim_drag_snaps_to_the_video_end_and_escape_discards() {
        let mut s = video_state();
        // A second, audio track that is trimmed short; the video is the reference.
        s.edit(|p| {
            p.main_video_track_id = Some("tv".into());
        });
        let mut audio = blank_track();
        audio.id = "ta".into();
        audio.asset_id = "a".into();
        s.edit(|p| {
            use crate::model::{Asset, Kind, Stream};
            use std::collections::BTreeMap;
            p.assets.push(Asset {
                id: "a".into(),
                path: "/m/a.m4a".into(),
                format: "mp4".into(),
                size: 1,
                modified_ms: 0,
                duration_ms: Some(12_000),
                streams: vec![Stream {
                    index: 1,
                    kind: Kind::Audio,
                    codec: "aac".into(),
                    title: String::new(),
                    language: "und".into(),
                    duration_ms: Some(12_000),
                    default: false,
                    forced: false,
                    properties: BTreeMap::new(),
                }],
                metadata: BTreeMap::new(),
                chapters: vec![],
            });
            p.tracks.push(Track {
                id: "ta".into(),
                asset_id: "a".into(),
                stream_index: 1,
                enabled: true,
                trim_end_ms: Some(9_900),
                ..blank_track()
            });
        });
        let scale = s.timeline_scale();
        // Right edge of the audio clip sits near 9.9 s; drag it 0.1 s right and it snaps to 10 s.
        let (_, end) = clip_bounds(s.project(), &s.project().track("ta").unwrap().clone());
        let right_px = end * scale;
        let trim = right_px - 4.0;
        pointer(&mut s, "ta", ptr(Phase::Down, trim, 40.0));
        pointer(&mut s, "ta", ptr(Phase::Move, trim + (100.0 * scale), 40.0));
        assert_eq!(
            s.drafts.trim.as_ref().and_then(|(_, p)| p.trim_end_ms),
            Some(Some(10_000)),
            "snapped to the exact video end"
        );
        pointer(&mut s, "ta", ptr(Phase::Cancel, trim, 40.0));
        assert_eq!(
            s.project().track("ta").unwrap().trim_end_ms,
            Some(9_900),
            "cancelled trim is discarded"
        );

        pointer(&mut s, "ta", ptr(Phase::Down, trim, 40.0));
        pointer(&mut s, "ta", ptr(Phase::Move, trim + 100.0 * scale, 40.0));
        pointer(&mut s, "ta", ptr(Phase::Up, trim + 100.0 * scale, 40.0));
        assert_eq!(s.project().track("ta").unwrap().trim_end_ms, Some(10_000));
    }

    #[test]
    fn arrow_keys_move_clips_by_100_ms_shift_one_second_alt_ten_ms() {
        let mut s = video_state();
        key(&mut s, "tv", "ArrowRight", false, false);
        assert_eq!(s.project().track("tv").unwrap().offset_ms, 100);
        key(&mut s, "tv", "ArrowLeft", true, false);
        assert_eq!(s.project().track("tv").unwrap().offset_ms, -900);
        key(&mut s, "tv", "ArrowRight", false, true);
        assert_eq!(s.project().track("tv").unwrap().offset_ms, -890);
    }

    #[test]
    fn alt_wheel_zooms_and_plain_wheel_scrolls_within_bounds() {
        let mut s = video_state();
        wheel(&mut s, 0.0, -120.0, 200.0, true);
        assert!(s.timeline_view.zoom > 1.0);
        wheel(&mut s, 0.0, 10_000.0, 200.0, false);
        assert!(s.timeline_view.scroll <= 700.0 * (s.timeline_view.zoom - 1.0));
    }

    #[test]
    fn chapter_drag_repositions_with_one_step_and_a_click_seeks() {
        let mut s = video_state();
        s.add_chapter(2000.0);
        let index = 0;
        let scale = s.timeline_scale();
        let x = 2000.0 * scale - s.timeline_view.scroll;
        pointer(&mut s, "tv", ptr(Phase::Down, x, 4.0));
        pointer(&mut s, "tv", ptr(Phase::Move, x + 200.0, 4.0));
        pointer(&mut s, "tv", ptr(Phase::Up, x + 200.0, 4.0));
        assert!(s.project().chapters[index].start_ms > 2000);
        let steps = s.store.history_len().0;
        pointer(&mut s, "tv", ptr(Phase::Down, x, 4.0));
        pointer(&mut s, "tv", ptr(Phase::Up, x, 4.0));
        assert_eq!(
            s.store.history_len().0,
            steps,
            "a click without movement does not edit"
        );
    }

    #[test]
    fn host_pointer_payloads_parse_and_unknown_phases_are_ignored() {
        let v = json!({"type": "pointer", "phase": "down", "x": 4.0, "y": 8.0, "mods": {"ctrl": true, "alt": true}});
        let p = parse_pointer(&v).unwrap();
        assert!(p.ctrl && p.alt && !p.shift);
        assert!(parse_pointer(&json!({"phase": "hover", "x": 1, "y": 1})).is_none());
        let _ = Event::Tick { now: 0.0 };
    }
}
