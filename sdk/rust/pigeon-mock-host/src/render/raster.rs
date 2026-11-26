//! Deterministic software rasterizer for draw ops. Pixels are tested at their centers (no
//! anti-aliasing), so output is identical on every machine. Text coverage is the only blended
//! edge. Coordinates are logical px; `scale` maps them to device px.

use std::collections::HashMap;

use pigeon_plugin_sdk::canvas::{DrawOp, PathCmd};
use pigeon_plugin_sdk::protocol::Rect;
use pigeon_plugin_sdk::scene::Color;

use super::theme::{Rgba, Theme};

/// An RGBA8 image, straight alpha.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Surface {
    pub fn new(w: u32, h: u32, fill: Rgba) -> Surface {
        let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
        for chunk in px.chunks_exact_mut(4) {
            chunk.copy_from_slice(&fill.0);
        }
        Surface { w, h, px }
    }

    pub fn pixel(&self, x: u32, y: u32) -> Rgba {
        let i = ((y * self.w + x) * 4) as usize;
        Rgba([self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]])
    }

    /// Source-over blend of one pixel with coverage `cov` (0..1).
    fn blend(&mut self, x: i64, y: i64, c: Rgba, cov: f32) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let a = (c.0[3] as f32 / 255.0) * cov.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        for k in 0..3 {
            let dst = self.px[i + k] as f32;
            self.px[i + k] = (c.0[k] as f32 * a + dst * (1.0 - a)).round() as u8;
        }
        let da = self.px[i + 3] as f32 / 255.0;
        self.px[i + 3] = ((a + da * (1.0 - a)) * 255.0).round() as u8;
    }
}

/// Paint for fills: a solid color, or a linear gradient in device px.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Solid(Rgba),
    Linear { x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<(f32, Rgba)> },
}

impl Paint {
    fn at(&self, x: f32, y: f32) -> Rgba {
        match self {
            Paint::Solid(c) => *c,
            Paint::Linear { x0, y0, x1, y1, stops } => {
                let (dx, dy) = (x1 - x0, y1 - y0);
                let len2 = (dx * dx + dy * dy).max(1e-6);
                let t = (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0);
                let mut prev = stops.first().copied().unwrap_or((0.0, Rgba([0, 0, 0, 255])));
                for s in stops {
                    if t <= s.0 {
                        let span = (s.0 - prev.0).max(1e-6);
                        let k = ((t - prev.0) / span).clamp(0.0, 1.0);
                        let mut out = [0u8; 4];
                        for i in 0..4 {
                            out[i] = (prev.1 .0[i] as f32 + (s.1 .0[i] as f32 - prev.1 .0[i] as f32) * k).round() as u8;
                        }
                        return Rgba(out);
                    }
                    prev = *s;
                }
                prev.1
            }
        }
    }
}

#[derive(Debug, Clone)]
struct GState {
    clip: Option<(f32, f32, f32, f32)>,
    tx: f32,
    ty: f32,
    sx: f32,
    sy: f32,
    shadow: Option<(f32, f32, Rgba)>,
    gradient: Option<Paint>,
}

/// Draws ops onto a [`Surface`] with a state stack (save/restore, clip, translate, scale).
pub struct Painter<'a> {
    pub surf: &'a mut Surface,
    pub theme: &'a Theme,
    pub images: &'a HashMap<String, ImageRef>,
    state: GState,
    stack: Vec<GState>,
    pub scale: f32,
}

/// A decoded RGBA image available to `image` ops and `image` nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

impl<'a> Painter<'a> {
    pub fn new(surf: &'a mut Surface, theme: &'a Theme, images: &'a HashMap<String, ImageRef>, scale: f32) -> Painter<'a> {
        Painter {
            surf,
            theme,
            images,
            state: GState { clip: None, tx: 0.0, ty: 0.0, sx: 1.0, sy: 1.0, shadow: None, gradient: None },
            stack: Vec::new(),
            scale,
        }
    }

    /// Device coordinates of a logical point.
    fn dev(&self, x: f32, y: f32) -> (f32, f32) {
        let k = self.scale;
        ((x * self.state.sx + self.state.tx) * k, (y * self.state.sy + self.state.ty) * k)
    }

    fn dev_rect(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
        let (x0, y0) = self.dev(x, y);
        let (x1, y1) = self.dev(x + w, y + h);
        (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }

    fn clip_bounds(&self) -> (i64, i64, i64, i64) {
        let (mut x0, mut y0, mut x1, mut y1) = (0.0f32, 0.0f32, self.surf.w as f32, self.surf.h as f32);
        if let Some((cx, cy, cw, ch)) = self.state.clip {
            x0 = x0.max(cx);
            y0 = y0.max(cy);
            x1 = x1.min(cx + cw);
            y1 = y1.min(cy + ch);
        }
        (x0.floor() as i64, y0.floor() as i64, x1.ceil() as i64, y1.ceil() as i64)
    }

    fn paint_for(&self, color: &Color) -> Paint {
        if let Some(g) = &self.state.gradient {
            return g.clone();
        }
        Paint::Solid(self.theme.resolve(&color.0))
    }

    /// Fill a region described by `inside(px, py)` with a paint. Shadows are drawn first.
    fn fill_region(&mut self, bbox: (f32, f32, f32, f32), paint: &Paint, inside: &dyn Fn(f32, f32) -> bool) {
        if let Some((dx, dy, color)) = self.state.shadow {
            let k = self.scale;
            let off = (dx * k, dy * k);
            let (bx0, by0, bx1, by1) = bbox;
            let shifted = (bx0 + off.0, by0 + off.1, bx1 + off.0, by1 + off.1);
            let shadow_paint = Paint::Solid(color);
            self.fill_raw(shifted, &shadow_paint, &|x, y| inside(x - off.0, y - off.1));
        }
        self.fill_raw(bbox, paint, inside);
    }

    fn fill_raw(&mut self, bbox: (f32, f32, f32, f32), paint: &Paint, inside: &dyn Fn(f32, f32) -> bool) {
        let (cx0, cy0, cx1, cy1) = self.clip_bounds();
        let x0 = (bbox.0.floor() as i64).max(cx0);
        let y0 = (bbox.1.floor() as i64).max(cy0);
        let x1 = (bbox.2.ceil() as i64).min(cx1);
        let y1 = (bbox.3.ceil() as i64).min(cy1);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                if inside(fx, fy) {
                    let c = paint.at(fx, fy);
                    self.surf.blend(px, py, c, 1.0);
                }
            }
        }
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: &Color, radius: f32) {
        let paint = self.paint_for(color);
        self.rect_paint(x, y, w, h, &paint, radius);
    }

    /// A rectangle in logical px with an explicit paint (used by the scene renderer).
    pub fn rect_paint(&mut self, x: f32, y: f32, w: f32, h: f32, paint: &Paint, radius: f32) {
        let bbox = self.dev_rect(x, y, w, h);
        let k = self.scale;
        let r = (radius * self.state.sx.abs() * k).min(((bbox.2 - bbox.0).min(bbox.3 - bbox.1)) / 2.0).max(0.0);
        let (bx0, by0, bx1, by1) = bbox;
        let inside = move |px: f32, py: f32| {
            if r <= 0.0 {
                return px >= bx0 && px < bx1 && py >= by0 && py < by1;
            }
            if px < bx0 || px >= bx1 || py < by0 || py >= by1 {
                return false;
            }
            let cx = px.clamp(bx0 + r, bx1 - r);
            let cy = py.clamp(by0 + r, by1 - r);
            let (dx, dy) = (px - cx, py - cy);
            dx * dx + dy * dy <= r * r
        };
        self.fill_region(bbox, paint, &inside);
    }

    /// Stroke a rectangle outline of width `w` (logical px).
    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: &Color, width: f32) {
        let c = self.theme.resolve(&color.0);
        let t = width.max(1.0);
        self.rect_paint(x, y, w, t, &Paint::Solid(c), 0.0);
        self.rect_paint(x, y + h - t, w, t, &Paint::Solid(c), 0.0);
        self.rect_paint(x, y, t, h, &Paint::Solid(c), 0.0);
        self.rect_paint(x + w - t, y, t, h, &Paint::Solid(c), 0.0);
    }

    /// A thick segment in logical px.
    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, color: &Color, width: f32) {
        let paint = self.paint_for(color);
        let (ax, ay) = self.dev(x1, y1);
        let (bx, by) = self.dev(x2, y2);
        let half = (width * self.state.sx.abs() * self.scale).max(1.0) / 2.0;
        let bbox = (ax.min(bx) - half, ay.min(by) - half, ax.max(bx) + half, ay.max(by) + half);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        let inside = move |px: f32, py: f32| {
            let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
            let (qx, qy) = (ax + dx * t - px, ay + dy * t - py);
            qx * qx + qy * qy <= half * half
        };
        self.fill_region(bbox, &paint, &inside);
    }

    /// Fill a polygon (even-odd) given one or more closed rings in logical px. Non-anti-aliased:
    /// each row's pixel centers are filled between sorted edge crossings.
    pub fn fill_polygons(&mut self, rings: &[Vec<(f32, f32)>], paint: &Paint) {
        let dev: Vec<Vec<(f32, f32)>> = rings.iter().map(|r| r.iter().map(|&(x, y)| self.dev(x, y)).collect()).collect();
        let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for r in &dev {
            for &(x, y) in r {
                bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            }
        }
        if bbox.0 > bbox.2 {
            return;
        }
        let (cx0, cy0, cx1, cy1) = self.clip_bounds();
        let y0 = (bbox.1.floor() as i64).max(cy0);
        let y1 = (bbox.3.ceil() as i64).min(cy1);
        for py in y0..y1 {
            let fy = py as f32 + 0.5;
            let mut xs: Vec<f32> = Vec::new();
            for r in &dev {
                for i in 0..r.len() {
                    let (ax, ay) = r[i];
                    let (bx, by) = r[(i + 1) % r.len()];
                    if (ay <= fy && by > fy) || (by <= fy && ay > fy) {
                        xs.push(ax + (fy - ay) * (bx - ax) / (by - ay));
                    }
                }
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for pair in xs.chunks(2) {
                if pair.len() != 2 {
                    continue;
                }
                let sx = (pair[0] + 0.5).floor().max(cx0 as f32) as i64;
                let ex = (pair[1] + 0.5).floor().min(cx1 as f32) as i64;
                for px in sx..ex {
                    let c = paint.at(px as f32 + 0.5, fy);
                    self.surf.blend(px, py, c, 1.0);
                }
            }
        }
    }

    pub fn fill_ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, paint: &Paint) {
        let (ox, oy) = self.dev(cx, cy);
        let (rxd, ryd) = ((rx * self.state.sx * self.scale).abs(), (ry * self.state.sy * self.scale).abs());
        let bbox = (ox - rxd, oy - ryd, ox + rxd, oy + ryd);
        let inside = move |px: f32, py: f32| {
            if rxd <= 0.0 || ryd <= 0.0 {
                return false;
            }
            let (dx, dy) = ((px - ox) / rxd, (py - oy) / ryd);
            dx * dx + dy * dy <= 1.0
        };
        self.fill_region(bbox, paint, &inside);
    }

    /// Arc of a circle as a ring (stroke) or a pie wedge (fill), radians clockwise from +x.
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, start: f32, end: f32, paint: &Paint, ring_width: Option<f32>) {
        let (ox, oy) = self.dev(cx, cy);
        let rd = r * self.scale * self.state.sx.abs();
        let (w0, w1) = (start.min(end), start.max(end));
        let inner = ring_width.map(|w| (rd - w * self.scale).max(0.0));
        let bbox = (ox - rd, oy - rd, ox + rd, oy + rd);
        let inside = move |px: f32, py: f32| {
            let (dx, dy) = (px - ox, py - oy);
            let d2 = dx * dx + dy * dy;
            let inner_ok = match inner {
                Some(ri) => d2 >= ri * ri,
                None => true,
            };
            if d2 > rd * rd || !inner_ok {
                return false;
            }
            let mut a = dy.atan2(dx);
            if a < 0.0 {
                a += std::f32::consts::TAU;
            }
            let (lo, hi) = (w0.rem_euclid(std::f32::consts::TAU), w1.rem_euclid(std::f32::consts::TAU));
            let span = (w1 - w0).min(std::f32::consts::TAU);
            if span >= std::f32::consts::TAU - 1e-4 {
                return true;
            }
            if lo <= hi { a >= lo && a <= hi } else { a >= lo || a <= hi }
        };
        self.fill_region(bbox, paint, &inside);
    }

    /// Text in logical px. `size` is the em size in logical px. Returns the advance width.
    pub fn text(&mut self, x: f32, y: f32, text: &str, size: f32, color: Rgba, mono: bool, align: &str) -> f32 {
        let font = super::text::font(mono);
        let (dx, dy) = self.dev(x, y);
        let k = self.scale * self.state.sx.abs();
        let px_size = size * k;
        let width = super::text::measure(font, text, px_size);
        let start = match align {
            "center" => dx - width / 2.0,
            "right" => dx - width,
            _ => dx,
        };
        let clip = self.clip_bounds();
        super::text::draw(font, text, start, dy, px_size, &mut |px, py, cov| {
            if px >= clip.0 && py >= clip.1 && px < clip.2 && py < clip.3 {
                self.surf.blend(px, py, color, cov);
            }
        });
        width / k
    }

    /// Draw a decoded image into a destination rectangle (nearest neighbour), optionally a source sub-rect.
    pub fn image(&mut self, img: &ImageRef, src: Option<Rect>, dst: Rect, opacity: f32, tint: Option<Rgba>) {
        let (sx0, sy0, sw, sh) = match src {
            Some(r) => (r.x, r.y, r.w.max(1.0), r.h.max(1.0)),
            None => (0.0, 0.0, img.w as f32, img.h as f32),
        };
        let (dx0, dy0) = self.dev(dst.x, dst.y);
        let (dx1, dy1) = self.dev(dst.x + dst.w, dst.y + dst.h);
        let (clip_x0, clip_y0, clip_x1, clip_y1) = self.clip_bounds();
        let (x0, y0) = (dx0.min(dx1).floor() as i64, dy0.min(dy1).floor() as i64);
        let (x1, y1) = (dx0.max(dx1).ceil() as i64, dy0.max(dy1).ceil() as i64);
        let (dw, dh) = ((x1 - x0).max(1) as f32, (y1 - y0).max(1) as f32);
        for py in y0.max(clip_y0)..y1.min(clip_y1) {
            for px in x0.max(clip_x0)..x1.min(clip_x1) {
                let u = ((px - x0) as f32 + 0.5) / dw;
                let v = ((py - y0) as f32 + 0.5) / dh;
                let ix = ((sx0 + u * sw) as i64).clamp(0, img.w as i64 - 1) as usize;
                let iy = ((sy0 + v * sh) as i64).clamp(0, img.h as i64 - 1) as usize;
                let i = (iy * img.w as usize + ix) * 4;
                let mut c = Rgba([img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]);
                if let Some(t) = tint {
                    for k in 0..3 {
                        c.0[k] = ((c.0[k] as u16 * t.0[k] as u16) / 255) as u8;
                    }
                }
                self.surf.blend(px, py, c, opacity.clamp(0.0, 1.0));
            }
        }
    }

    pub fn save(&mut self) {
        self.stack.push(self.state.clone());
    }

    pub fn restore(&mut self) {
        if let Some(s) = self.stack.pop() {
            self.state = s;
        }
    }

    pub fn clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (a, b, c, d) = self.dev_rect(x, y, w, h);
        self.state.clip = Some(match self.state.clip {
            Some((cx, cy, cw, ch)) => {
                let x0 = a.max(cx);
                let y0 = b.max(cy);
                let x1 = c.min(cx + cw).max(x0);
                let y1 = d.min(cy + ch).max(y0);
                (x0, y0, x1 - x0, y1 - y0)
            }
            None => (a, b, c - a, d - b),
        });
    }

    /// Current clip rectangle in logical px, for layout code that needs it.
    pub fn translate(&mut self, x: f32, y: f32) {
        self.state.tx += x * self.state.sx;
        self.state.ty += y * self.state.sy;
    }

    pub fn scale_by(&mut self, x: f32, y: f32) {
        self.state.sx *= x;
        self.state.sy *= y;
    }

    pub fn set_shadow(&mut self, dx: f32, dy: f32, color: &Color) {
        let c = self.theme.resolve(&color.0);
        self.state.shadow = Some((dx, dy, c));
    }

    pub fn set_gradient(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, stops: &[(f32, Color)]) {
        let (a, b) = self.dev(x0, y0);
        let (c, d) = self.dev(x1, y1);
        let stops = stops.iter().map(|(t, col)| (*t, self.theme.resolve(&col.0))).collect();
        self.state.gradient = Some(Paint::Linear { x0: a, y0: b, x1: c, y1: d, stops });
    }

    /// Apply one draw op. Images are looked up through the painter's resource callback.
    pub fn apply(&mut self, op: &DrawOp) {
        match op {
            DrawOp::Rect { x, y, w, h, fill, stroke, stroke_w, radius } => {
                if let Some(f) = fill {
                    let paint = self.paint_for(f);
                    self.rect_paint(*x, *y, *w, *h, &paint, radius.unwrap_or(0.0));
                }
                if let Some(s) = stroke {
                    let t = stroke_w.unwrap_or(1.0);
                    self.stroke_rect(*x, *y, *w, *h, s, t);
                }
            }
            DrawOp::Line { x1, y1, x2, y2, stroke, stroke_w } => self.line(*x1, *y1, *x2, *y2, stroke, *stroke_w),
            DrawOp::Polyline { pts, stroke, stroke_w } => {
                for pair in pts.windows(2) {
                    self.line(pair[0][0], pair[0][1], pair[1][0], pair[1][1], stroke, *stroke_w);
                }
            }
            DrawOp::Polygon { pts, fill, stroke, stroke_w } => {
                let ring: Vec<(f32, f32)> = pts.iter().map(|p| (p[0], p[1])).collect();
                if let Some(f) = fill {
                    let paint = self.paint_for(f);
                    self.fill_polygons(std::slice::from_ref(&ring), &paint);
                }
                if let Some(s) = stroke {
                    let w = stroke_w.unwrap_or(1.0);
                    let mut closed = ring.clone();
                    if let Some(first) = ring.first() {
                        closed.push(*first);
                    }
                    for pair in closed.windows(2) {
                        self.line(pair[0].0, pair[0].1, pair[1].0, pair[1].1, s, w);
                    }
                }
            }
            DrawOp::Path { cmds, fill, stroke, stroke_w } => {
                let rings = flatten(cmds);
                if let Some(f) = fill {
                    let paint = self.paint_for(f);
                    self.fill_polygons(&rings, &paint);
                }
                if let Some(s) = stroke {
                    let w = stroke_w.unwrap_or(1.0);
                    for ring in &rings {
                        for pair in ring.windows(2) {
                            self.line(pair[0].0, pair[0].1, pair[1].0, pair[1].1, s, w);
                        }
                    }
                }
            }
            DrawOp::Circle { cx, cy, r, fill, stroke, stroke_w } => {
                if let Some(f) = fill {
                    let paint = self.paint_for(f);
                    self.fill_ellipse(*cx, *cy, *r, *r, &paint);
                }
                if let Some(s) = stroke {
                    let w = stroke_w.unwrap_or(1.0);
                    let paint = Paint::Solid(self.theme.resolve(&s.0));
                    self.arc(*cx, *cy, *r, 0.0, std::f32::consts::TAU, &paint, Some(w));
                }
            }
            DrawOp::Ellipse { cx, cy, rx, ry, fill, stroke, .. } => {
                if let Some(f) = fill {
                    let paint = self.paint_for(f);
                    self.fill_ellipse(*cx, *cy, *rx, *ry, &paint);
                }
                if let Some(s) = stroke {
                    let paint = Paint::Solid(self.theme.resolve(&s.0));
                    self.fill_ellipse(*cx, *cy, *rx, *ry, &paint);
                }
            }
            DrawOp::Arc { cx, cy, r, start, end, fill, stroke, stroke_w } => {
                if let Some(f) = fill {
                    let paint = self.paint_for(f);
                    self.arc(*cx, *cy, *r, *start, *end, &paint, None);
                }
                if let Some(s) = stroke {
                    let paint = Paint::Solid(self.theme.resolve(&s.0));
                    self.arc(*cx, *cy, *r, *start, *end, &paint, Some(stroke_w.unwrap_or(1.0)));
                }
            }
            DrawOp::Text { x, y, text, size, color, align, font, .. } => {
                let c = color.as_ref().map(|c| self.theme.resolve(&c.0)).unwrap_or(self.theme.token("fg", "#e6e8ee"));
                let mono = font.as_deref() == Some("mono");
                self.text(*x, *y, text, *size, c, mono, align.as_deref().unwrap_or("left"));
            }
            DrawOp::Image { rid, src, dst, tint, opacity } => {
                let images: &'a HashMap<String, ImageRef> = self.images;
                if let Some(img) = images.get(rid) {
                    let tint = tint.as_ref().map(|t| self.theme.resolve(&t.0));
                    self.image(img, *src, *dst, opacity.unwrap_or(1.0), tint);
                } else {
                    let grey = Color::rgb(90, 90, 90);
                    self.fill_rect(dst.x, dst.y, dst.w, dst.h, &grey, 0.0);
                }
            }
            DrawOp::Clip { x, y, w, h } => self.clip(*x, *y, *w, *h),
            DrawOp::Unclip => self.state.clip = None,
            DrawOp::Translate { x, y } => self.translate(*x, *y),
            DrawOp::Scale { x, y } => self.scale_by(*x, *y),
            DrawOp::Save => self.save(),
            DrawOp::Restore => self.restore(),
            DrawOp::Shadow { dx, dy, color, .. } => self.set_shadow(*dx, *dy, color),
            DrawOp::Gradient { x0, y0, x1, y1, stops } => self.set_gradient(*x0, *y0, *x1, *y1, stops),
        }
    }
}

/// Flatten path commands into closed-or-open rings (quadratics and cubics in 12 steps).
pub fn flatten(cmds: &[PathCmd]) -> Vec<Vec<(f32, f32)>> {
    let mut rings: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let mut pen = (0.0f32, 0.0f32);
    for cmd in cmds {
        match *cmd {
            PathCmd::MoveTo(x, y) => {
                if cur.len() > 1 {
                    rings.push(std::mem::take(&mut cur));
                }
                cur.clear();
                cur.push((x, y));
                pen = (x, y);
            }
            PathCmd::LineTo(x, y) => {
                cur.push((x, y));
                pen = (x, y);
            }
            PathCmd::QuadTo(cx, cy, x, y) => {
                for i in 1..=12 {
                    let t = i as f32 / 12.0;
                    let u = 1.0 - t;
                    cur.push((u * u * pen.0 + 2.0 * u * t * cx + t * t * x, u * u * pen.1 + 2.0 * u * t * cy + t * t * y));
                }
                pen = (x, y);
            }
            PathCmd::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                for i in 1..=12 {
                    let t = i as f32 / 12.0;
                    let u = 1.0 - t;
                    let a = u * u * u;
                    let b = 3.0 * u * u * t;
                    let c = 3.0 * u * t * t;
                    let d = t * t * t;
                    cur.push((a * pen.0 + b * c1x + c * c2x + d * x, a * pen.1 + b * c1y + c * c2y + d * y));
                }
                pen = (x, y);
            }
            PathCmd::Close => {
                if let Some(first) = cur.first().copied() {
                    cur.push(first);
                }
            }
        }
    }
    if cur.len() > 1 {
        rings.push(cur);
    }
    rings
}
