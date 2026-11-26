//! Canvas draw lists (section 7). Coordinates are logical px with the origin at the canvas'
//! top-left corner.
//!
//! ```
//! use pigeon_plugin_sdk::canvas::{DrawList, DrawOp};
//! use pigeon_plugin_sdk::scene::Color;
//! let mut list = DrawList::new().clear(Color::token("bg_input"));
//! list.add(DrawOp::rect(10.0, 10.0, 100.0, 24.0).fill(Color::token("accent")).radius(4.0));
//! list.add(DrawOp::text(14.0, 26.0, "clip 01").size(12.0).color(Color::token("fg")));
//! assert_eq!(list.len(), 2);
//! ```
//!
//! Path commands serialize as arrays: `["M",x,y]`, `["L",x,y]`, `["Q",cx,cy,x,y]`,
//! `["C",c1x,c1y,c2x,c2y,x,y]`, `["Z"]`.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::protocol::{ProtocolError, MAX_CANVAS_BYTES, MAX_CANVAS_OPS};
use crate::scene::Color;

/// One command of a `path` op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCmd {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

impl Serialize for PathCmd {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v: Vec<Value> = match *self {
            PathCmd::MoveTo(x, y) => vec!["M".into(), x.into(), y.into()],
            PathCmd::LineTo(x, y) => vec!["L".into(), x.into(), y.into()],
            PathCmd::QuadTo(a, b, x, y) => vec!["Q".into(), a.into(), b.into(), x.into(), y.into()],
            PathCmd::CubicTo(a, b, c, d, x, y) => {
                vec!["C".into(), a.into(), b.into(), c.into(), d.into(), x.into(), y.into()]
            }
            PathCmd::Close => vec!["Z".into()],
        };
        v.serialize(s)
    }
}

impl<'de> Deserialize<'de> for PathCmd {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<PathCmd, D::Error> {
        let v = Vec::<Value>::deserialize(d)?;
        let tag = v.first().and_then(Value::as_str).ok_or_else(|| D::Error::custom("path command needs a tag"))?;
        let n = |i: usize| -> Result<f32, D::Error> {
            v.get(i).and_then(Value::as_f64).map(|f| f as f32).ok_or_else(|| D::Error::custom("path coordinate"))
        };
        match tag {
            "M" => Ok(PathCmd::MoveTo(n(1)?, n(2)?)),
            "L" => Ok(PathCmd::LineTo(n(1)?, n(2)?)),
            "Q" => Ok(PathCmd::QuadTo(n(1)?, n(2)?, n(3)?, n(4)?)),
            "C" => Ok(PathCmd::CubicTo(n(1)?, n(2)?, n(3)?, n(4)?, n(5)?, n(6)?)),
            "Z" => Ok(PathCmd::Close),
            other => Err(D::Error::custom(format!("unknown path command {other}"))),
        }
    }
}

/// One draw operation. Serialized with an `op` tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DrawOp {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_w: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<f32>,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: Color,
        stroke_w: f32,
    },
    Polyline {
        pts: Vec<[f32; 2]>,
        stroke: Color,
        stroke_w: f32,
    },
    Polygon {
        pts: Vec<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_w: Option<f32>,
    },
    Path {
        cmds: Vec<PathCmd>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_w: Option<f32>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_w: Option<f32>,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_w: Option<f32>,
    },
    /// Arc of a circle from `start` to `end` radians (clockwise from +x).
    Arc {
        cx: f32,
        cy: f32,
        r: f32,
        start: f32,
        end: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_w: Option<f32>,
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Color>,
        /// `left`, `center` or `right`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<String>,
        /// `top`, `middle`, `alphabetic` or `bottom`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        baseline: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_w: Option<f32>,
        /// `ui` (default) or `mono`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<String>,
    },
    Image {
        rid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        src: Option<crate::protocol::Rect>,
        dst: crate::protocol::Rect,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tint: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f32>,
    },
    Clip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    Unclip,
    Translate {
        x: f32,
        y: f32,
    },
    Scale {
        x: f32,
        y: f32,
    },
    Save,
    Restore,
    /// Shadow for the following fills until `restore`.
    Shadow {
        dx: f32,
        dy: f32,
        blur: f32,
        color: Color,
    },
    /// Linear gradient paint for the following fills until `restore`. `stops` are `[offset, color]`.
    Gradient {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stops: Vec<(f32, Color)>,
    },
}

impl DrawOp {
    /// `rect` with no paint yet (set `fill`/`stroke`).
    pub fn rect(x: f32, y: f32, w: f32, h: f32) -> DrawOp {
        DrawOp::Rect { x, y, w, h, fill: None, stroke: None, stroke_w: None, radius: None }
    }
    pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, stroke: impl Into<Color>, stroke_w: f32) -> DrawOp {
        DrawOp::Line { x1, y1, x2, y2, stroke: stroke.into(), stroke_w }
    }
    pub fn polyline(pts: &[[f32; 2]], stroke: impl Into<Color>, stroke_w: f32) -> DrawOp {
        DrawOp::Polyline { pts: pts.to_vec(), stroke: stroke.into(), stroke_w }
    }
    pub fn polygon(pts: &[[f32; 2]]) -> DrawOp {
        DrawOp::Polygon { pts: pts.to_vec(), fill: None, stroke: None, stroke_w: None }
    }
    pub fn path(cmds: Vec<PathCmd>) -> DrawOp {
        DrawOp::Path { cmds, fill: None, stroke: None, stroke_w: None }
    }
    pub fn circle(cx: f32, cy: f32, r: f32) -> DrawOp {
        DrawOp::Circle { cx, cy, r, fill: None, stroke: None, stroke_w: None }
    }
    pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> DrawOp {
        DrawOp::Ellipse { cx, cy, rx, ry, fill: None, stroke: None, stroke_w: None }
    }
    pub fn arc(cx: f32, cy: f32, r: f32, start: f32, end: f32) -> DrawOp {
        DrawOp::Arc { cx, cy, r, start, end, fill: None, stroke: None, stroke_w: None }
    }
    pub fn text(x: f32, y: f32, text: &str) -> DrawOp {
        DrawOp::Text {
            x,
            y,
            text: text.into(),
            size: 13.0,
            weight: None,
            color: None,
            align: None,
            baseline: None,
            max_w: None,
            font: None,
        }
    }
    pub fn image(rid: &str, dst: crate::protocol::Rect) -> DrawOp {
        DrawOp::Image { rid: rid.into(), src: None, dst, tint: None, opacity: None }
    }
    pub fn clip(x: f32, y: f32, w: f32, h: f32) -> DrawOp {
        DrawOp::Clip { x, y, w, h }
    }
    pub fn translate(x: f32, y: f32) -> DrawOp {
        DrawOp::Translate { x, y }
    }
    pub fn scale(x: f32, y: f32) -> DrawOp {
        DrawOp::Scale { x, y }
    }
    pub fn shadow(dx: f32, dy: f32, blur: f32, color: impl Into<Color>) -> DrawOp {
        DrawOp::Shadow { dx, dy, blur, color: color.into() }
    }
    pub fn gradient(x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<(f32, Color)>) -> DrawOp {
        DrawOp::Gradient { x0, y0, x1, y1, stops }
    }

    /// Set the fill (rect, polygon, path, circle, ellipse, arc). No-op for other ops.
    pub fn fill(self, c: impl Into<Color>) -> DrawOp {
        let c = Some(c.into());
        match self {
            DrawOp::Rect { x, y, w, h, stroke, stroke_w, radius, .. } => {
                DrawOp::Rect { x, y, w, h, fill: c, stroke, stroke_w, radius }
            }
            DrawOp::Polygon { pts, stroke, stroke_w, .. } => DrawOp::Polygon { pts, fill: c, stroke, stroke_w },
            DrawOp::Path { cmds, stroke, stroke_w, .. } => DrawOp::Path { cmds, fill: c, stroke, stroke_w },
            DrawOp::Circle { cx, cy, r, stroke, stroke_w, .. } => DrawOp::Circle { cx, cy, r, fill: c, stroke, stroke_w },
            DrawOp::Ellipse { cx, cy, rx, ry, stroke, stroke_w, .. } => {
                DrawOp::Ellipse { cx, cy, rx, ry, fill: c, stroke, stroke_w }
            }
            DrawOp::Arc { cx, cy, r, start, end, stroke, stroke_w, .. } => {
                DrawOp::Arc { cx, cy, r, start, end, fill: c, stroke, stroke_w }
            }
            other => other,
        }
    }

    /// Set the stroke and its width. Line and polyline always stroke; this changes their color too.
    pub fn stroke(self, c: impl Into<Color>, width: f32) -> DrawOp {
        let c = Some(c.into());
        let w = Some(width);
        match self {
            DrawOp::Rect { x, y, w: rw, h, fill, radius, .. } => {
                DrawOp::Rect { x, y, w: rw, h, fill, stroke: c, stroke_w: w, radius }
            }
            DrawOp::Polygon { pts, fill, .. } => DrawOp::Polygon { pts, fill, stroke: c, stroke_w: w },
            DrawOp::Path { cmds, fill, .. } => DrawOp::Path { cmds, fill, stroke: c, stroke_w: w },
            DrawOp::Circle { cx, cy, r, fill, .. } => DrawOp::Circle { cx, cy, r, fill, stroke: c, stroke_w: w },
            DrawOp::Ellipse { cx, cy, rx, ry, fill, .. } => {
                DrawOp::Ellipse { cx, cy, rx, ry, fill, stroke: c, stroke_w: w }
            }
            DrawOp::Arc { cx, cy, r, start, end, fill, .. } => {
                DrawOp::Arc { cx, cy, r, start, end, fill, stroke: c, stroke_w: w }
            }
            DrawOp::Line { x1, y1, x2, y2, .. } => {
                DrawOp::Line { x1, y1, x2, y2, stroke: c.unwrap_or_else(|| Color::token("fg")), stroke_w: width }
            }
            DrawOp::Polyline { pts, .. } => {
                DrawOp::Polyline { pts, stroke: c.unwrap_or_else(|| Color::token("fg")), stroke_w: width }
            }
            other => other,
        }
    }

    /// Corner radius for `rect`. No-op otherwise.
    pub fn radius(self, r: f32) -> DrawOp {
        match self {
            DrawOp::Rect { x, y, w, h, fill, stroke, stroke_w, .. } => {
                DrawOp::Rect { x, y, w, h, fill, stroke, stroke_w, radius: Some(r) }
            }
            other => other,
        }
    }

    /// Font size for `text`. No-op otherwise.
    pub fn size(self, px: f32) -> DrawOp {
        match self {
            DrawOp::Text { x, y, text, weight, color, align, baseline, max_w, font, .. } => {
                DrawOp::Text { x, y, text, size: px, weight, color, align, baseline, max_w, font }
            }
            other => other,
        }
    }

    /// Text color for `text`. No-op otherwise.
    pub fn color(self, c: impl Into<Color>) -> DrawOp {
        match self {
            DrawOp::Text { x, y, text, size, weight, align, baseline, max_w, font, .. } => DrawOp::Text {
                x,
                y,
                text,
                size,
                weight,
                color: Some(c.into()),
                align,
                baseline,
                max_w,
                font,
            },
            other => other,
        }
    }

    /// Font weight (100-900) for `text`. No-op otherwise.
    pub fn weight(self, w: u16) -> DrawOp {
        match self {
            DrawOp::Text { x, y, text, size, color, align, baseline, max_w, font, .. } => {
                DrawOp::Text { x, y, text, size, weight: Some(w), color, align, baseline, max_w, font }
            }
            other => other,
        }
    }

    /// Horizontal alignment (`left`, `center`, `right`) for `text`. No-op otherwise.
    pub fn align(self, a: &str) -> DrawOp {
        match self {
            DrawOp::Text { x, y, text, size, weight, color, baseline, max_w, font, .. } => DrawOp::Text {
                x,
                y,
                text,
                size,
                weight,
                color,
                align: Some(a.into()),
                baseline,
                max_w,
                font,
            },
            other => other,
        }
    }

    /// Use the monospace font for `text`. No-op otherwise.
    pub fn mono(self) -> DrawOp {
        match self {
            DrawOp::Text { x, y, text, size, weight, color, align, baseline, max_w, .. } => DrawOp::Text {
                x,
                y,
                text,
                size,
                weight,
                color,
                align,
                baseline,
                max_w,
                font: Some("mono".into()),
            },
            other => other,
        }
    }

    /// Opacity (0..=1) for `image`. No-op otherwise.
    pub fn opacity(self, a: f32) -> DrawOp {
        match self {
            DrawOp::Image { rid, src, dst, tint, .. } => DrawOp::Image { rid, src, dst, tint, opacity: Some(a) },
            other => other,
        }
    }
}

/// An ordered list of draw ops plus the clear color.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrawList {
    pub clear: Option<Color>,
    pub ops: Vec<DrawOp>,
}

impl DrawList {
    pub fn new() -> DrawList {
        DrawList::default()
    }

    /// Clear the canvas with this color before drawing.
    pub fn clear(mut self, c: impl Into<Color>) -> DrawList {
        self.clear = Some(c.into());
        self
    }

    /// Append an op.
    pub fn add(&mut self, op: DrawOp) -> &mut DrawList {
        self.ops.push(op);
        self
    }

    /// Append an op, builder-style.
    pub fn with(mut self, op: DrawOp) -> DrawList {
        self.ops.push(op);
        self
    }

    /// Append many ops.
    pub fn extend<I: IntoIterator<Item = DrawOp>>(&mut self, ops: I) -> &mut DrawList {
        self.ops.extend(ops);
        self
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The `draw` prop of a canvas node: `{frame, clear?, ops}`.
    pub fn to_draw_value(&self, frame: u64) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("frame".into(), Value::from(frame));
        if let Some(c) = &self.clear {
            m.insert("clear".into(), Value::from(c.0.clone()));
        }
        m.insert("ops".into(), serde_json::to_value(&self.ops).unwrap_or(Value::Array(vec![])));
        Value::Object(m)
    }

    /// Check the documented limits (20,000 ops, 4 MiB of JSON).
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.ops.len() > MAX_CANVAS_OPS {
            return Err(ProtocolError::Limit(format!(
                "canvas_too_large: {} ops (max {MAX_CANVAS_OPS})",
                self.ops.len()
            )));
        }
        let bytes = serde_json::to_vec(&self.ops).map(|v| v.len()).unwrap_or(usize::MAX);
        if bytes > MAX_CANVAS_BYTES {
            return Err(ProtocolError::Limit(format!("canvas_too_large: {bytes} bytes (max {MAX_CANVAS_BYTES})")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_serialize_with_op_tag_and_skip_unset_paint() {
        let v = serde_json::to_value(DrawOp::rect(1.0, 2.0, 3.0, 4.0).fill("accent").radius(2.0)).unwrap();
        assert_eq!(v, serde_json::json!({"op":"rect","x":1.0,"y":2.0,"w":3.0,"h":4.0,"fill":"accent","radius":2.0}));
        let t = serde_json::to_value(DrawOp::text(0.0, 0.0, "hi").size(12.0).color("fg").mono()).unwrap();
        assert_eq!(t["op"], "text");
        assert_eq!(t["font"], "mono");
        assert!(t.get("weight").is_none());
    }

    #[test]
    fn path_commands_are_arrays() {
        let op = DrawOp::path(vec![PathCmd::MoveTo(0.0, 0.0), PathCmd::CubicTo(1.0, 1.0, 2.0, 2.0, 3.0, 3.0), PathCmd::Close]);
        let v = serde_json::to_value(&op).unwrap();
        assert_eq!(v["cmds"][0], serde_json::json!(["M", 0.0, 0.0]));
        assert_eq!(v["cmds"][1], serde_json::json!(["C", 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]));
        assert_eq!(v["cmds"][2], serde_json::json!(["Z"]));
        let back: DrawOp = serde_json::from_value(v).unwrap();
        assert_eq!(back, op);
    }

    #[test]
    fn unit_ops_and_gradients_roundtrip() {
        for op in [
            DrawOp::Unclip,
            DrawOp::Save,
            DrawOp::Restore,
            DrawOp::gradient(0.0, 0.0, 0.0, 10.0, vec![(0.0, Color::token("accent")), (1.0, Color::rgb(0, 0, 0))]),
            DrawOp::shadow(0.0, 2.0, 4.0, "#00000080"),
        ] {
            let v = serde_json::to_value(&op).unwrap();
            let back: DrawOp = serde_json::from_value(v.clone()).unwrap();
            assert_eq!(back, op, "{v}");
        }
    }

    #[test]
    fn validate_enforces_op_limit() {
        let mut list = DrawList::new();
        list.extend((0..MAX_CANVAS_OPS + 1).map(|_| DrawOp::Unclip));
        let err = list.validate().unwrap_err();
        assert!(err.to_string().contains("canvas_too_large"));
    }

    #[test]
    fn draw_value_has_frame_clear_and_ops() {
        let list = DrawList::new().clear("bg").with(DrawOp::Unclip);
        let v = list.to_draw_value(7);
        assert_eq!(v["frame"], 7);
        assert_eq!(v["clear"], "bg");
        assert_eq!(v["ops"][0]["op"], "unclip");
    }
}
