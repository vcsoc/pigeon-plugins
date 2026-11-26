//! Canvas draw-list builders (protocol v2 section 7). Each op is a JSON object with the `op` tag.
//! Colours are the Reelio palette: charcoal surfaces, blue selection, green audio, ochre subtitles.

use serde_json::{json, Value};

pub const CLR_RULER_TEXT: &str = "#8f95a6";
pub const CLR_RULER_TICK: &str = "#3a3d46";
pub const CLR_LANE: &str = "#1c1d24";
pub const CLR_PLAYHEAD: &str = "#f16d7a";
pub const CLR_CHAPTER: &str = "#e6bd79";
pub const CLR_VIDEO: &str = "#395278";
pub const CLR_VIDEO_EDGE: &str = "#5688ff";
pub const CLR_AUDIO: &str = "#24513f";
pub const CLR_AUDIO_EDGE: &str = "#8bd3ac";
pub const CLR_SUBTITLE: &str = "#725f3a";
pub const CLR_SUBTITLE_EDGE: &str = "#e6bd79";
pub const CLR_EXCLUDED: &str = "#34363d";
pub const CLR_TEXT: &str = "#e8eaf0";
pub const CLR_MUTED: &str = "#8f95a6";
pub const CLR_SNAP: &str = "#9db8ff";

pub fn rect(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    fill: &str,
    stroke: Option<&str>,
    radius: f64,
) -> Value {
    let mut v =
        json!({"op": "rect", "x": x, "y": y, "w": w, "h": h, "fill": fill, "radius": radius});
    if let Some(s) = stroke {
        v["stroke"] = json!(s);
        v["stroke_w"] = json!(1.0);
    }
    v
}

pub fn line(x1: f64, y1: f64, x2: f64, y2: f64, stroke: &str, width: f64) -> Value {
    json!({"op": "line", "x1": x1, "y1": y1, "x2": x2, "y2": y2, "stroke": stroke, "stroke_w": width})
}

pub fn text(
    x: f64,
    y: f64,
    s: &str,
    size: f64,
    color: &str,
    mono: bool,
    max_w: Option<f64>,
) -> Value {
    let mut v = json!({
        "op": "text", "x": x, "y": y, "text": s, "size": size, "color": color,
        "weight": 400, "align": "left", "baseline": "top",
        "font": if mono { "mono" } else { "ui" }
    });
    if let Some(w) = max_w {
        v["max_w"] = json!(w);
    }
    v
}

/// Everything a timeline lane needs to draw itself; computed by the reducer's geometry layer.
#[derive(Debug, Clone)]
pub struct LaneDraw {
    pub width: f64,
    pub height: f64,
    /// Kind: "video" | "audio" | "subtitle".
    pub kind: String,
    pub enabled: bool,
    pub selected: bool,
    pub title: String,
    pub start_label: String,
    pub end_label: String,
    pub ruler_step_ms: f64,
    pub scale: f64,
    pub scroll: f64,
    pub clip_left: f64,
    pub clip_width: f64,
    pub trim_handles: bool,
    /// Visible ruler labels as (x, text) pairs (already virtualized).
    pub ticks: Vec<(f64, String)>,
    /// Chapter pin x positions (video lanes only).
    pub chapters: Vec<f64>,
    pub playhead_x: f64,
    pub excluded_pill: Option<&'static str>,
}

pub const RULER_H: f64 = 22.0;

/// Draw ops for one lane canvas: ruler, clip body, trim handles, chapter pins and the playhead.
pub fn lane_ops(d: &LaneDraw) -> Vec<Value> {
    let mut ops = vec![];
    ops.push(rect(0.0, 0.0, d.width, d.height, CLR_LANE, None, 0.0));
    ops.push(rect(0.0, 0.0, d.width, RULER_H, "#202227", None, 0.0));
    for (x, label) in &d.ticks {
        ops.push(line(*x, RULER_H - 6.0, *x, RULER_H, CLR_RULER_TICK, 1.0));
        ops.push(text(*x + 4.0, 4.0, label, 10.0, CLR_RULER_TEXT, true, None));
    }
    let (fill, edge) = match d.kind.as_str() {
        "video" => (CLR_VIDEO, CLR_VIDEO_EDGE),
        "audio" => (CLR_AUDIO, CLR_AUDIO_EDGE),
        _ => (CLR_SUBTITLE, CLR_SUBTITLE_EDGE),
    };
    let body_fill = if d.enabled { fill } else { CLR_EXCLUDED };
    let body_edge = if d.enabled && d.selected {
        edge
    } else {
        "#4a4e58"
    };
    let top = RULER_H + 2.0;
    let h = d.height - top - 10.0;
    ops.push(rect(
        d.clip_left,
        top,
        d.clip_width,
        h,
        body_fill,
        Some(body_edge),
        4.0,
    ));
    if d.trim_handles {
        ops.push(rect(d.clip_left, top, 6.0, h, edge, None, 2.0));
        ops.push(rect(
            d.clip_left + d.clip_width - 6.0,
            top,
            6.0,
            h,
            edge,
            None,
            2.0,
        ));
    }
    ops.push(text(
        d.clip_left + 12.0,
        top + 6.0,
        &d.title,
        12.0,
        CLR_TEXT,
        false,
        Some((d.clip_width - 24.0).max(0.0)),
    ));
    ops.push(text(
        d.clip_left + 12.0,
        top + 24.0,
        &format!("Start {} · {}", d.start_label, d.end_label),
        10.0,
        CLR_MUTED,
        true,
        Some((d.clip_width - 24.0).max(0.0)),
    ));
    if let Some(pill) = d.excluded_pill {
        ops.push(rect(
            d.clip_left + d.clip_width - 84.0,
            top + 6.0,
            74.0,
            16.0,
            "#4a4e58",
            None,
            8.0,
        ));
        ops.push(text(
            d.clip_left + d.clip_width - 78.0,
            top + 8.0,
            pill,
            10.0,
            CLR_TEXT,
            false,
            None,
        ));
    }
    for x in &d.chapters {
        ops.push(rect(*x - 4.0, 2.0, 8.0, 8.0, CLR_CHAPTER, None, 1.0));
    }
    ops.push(line(
        d.playhead_x,
        0.0,
        d.playhead_x,
        d.height,
        CLR_PLAYHEAD,
        2.0,
    ));
    ops
}

/// Wrap a list of ops into a canvas `draw` object.
pub fn draw(frame: u64, ops: Vec<Value>) -> Value {
    json!({"frame": frame, "clear": CLR_LANE, "ops": ops})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(kind: &str, enabled: bool) -> LaneDraw {
        LaneDraw {
            width: 800.0,
            height: 56.0,
            kind: kind.into(),
            enabled,
            selected: true,
            title: "Movie".into(),
            start_label: "00:00:00.000".into(),
            end_label: "00:00:10.000".into(),
            ruler_step_ms: 1000.0,
            scale: 0.1,
            scroll: 0.0,
            clip_left: 10.0,
            clip_width: 400.0,
            trim_handles: true,
            ticks: vec![(0.0, "00:00:00.000".into())],
            chapters: vec![40.0],
            playhead_x: 120.0,
            excluded_pill: if enabled { None } else { Some("Excluded") },
        }
    }

    #[test]
    fn lane_ops_use_kind_colours_and_grey_excluded_clips() {
        let video = lane_ops(&lane("video", true));
        assert!(video.iter().any(|o| o["fill"] == CLR_VIDEO));
        let excluded = lane_ops(&lane("audio", false));
        assert!(excluded.iter().any(|o| o["fill"] == CLR_EXCLUDED));
        assert!(excluded.iter().any(|o| o["text"] == "Excluded"));
    }

    #[test]
    fn playhead_is_always_drawn_last_so_it_stays_above_clips() {
        let ops = lane_ops(&lane("video", true));
        assert_eq!(ops.last().unwrap()["stroke"], CLR_PLAYHEAD);
    }
}
