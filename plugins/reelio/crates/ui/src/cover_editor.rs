//! Non-destructive cover edit history and source-coordinate canvas interaction.
use crate::crop::{self, Crop, Field, Handle, Size};
use crate::{
    engine::Request,
    model::Artwork,
    state::{Pending, Section, State},
};
use serde_json::{json, Value};

const HANDLES: [(Handle, f64, f64); 8] = [
    (Handle::NW, 0., 0.),
    (Handle::N, 0.5, 0.),
    (Handle::NE, 1., 0.),
    (Handle::E, 1., 0.5),
    (Handle::SE, 1., 1.),
    (Handle::S, 0.5, 1.),
    (Handle::SW, 0., 1.),
    (Handle::W, 0., 0.5),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Edits {
    pub crop: Option<Crop>,
    pub rotation: u32,
    pub flip: bool,
    pub grayscale: bool,
    pub negative: bool,
    pub sepia: bool,
    pub brightness: f64,
    pub contrast: f64,
    pub resize: Option<Size>,
}
impl Default for Edits {
    fn default() -> Self {
        Self {
            crop: None,
            rotation: 0,
            flip: false,
            grayscale: false,
            negative: false,
            sepia: false,
            brightness: 1.,
            contrast: 1.,
            resize: None,
        }
    }
}
impl Edits {
    fn value(&self) -> Value {
        json!({"crop":self.crop.map(|c| json!({"x":c.x,"y":c.y,"width":c.width,"height":c.height})),"rotation":self.rotation,"flip":self.flip,"grayscale":self.grayscale,"negative":self.negative,"sepia":self.sepia,"brightness":self.brightness,"contrast":self.contrast,"resize":self.resize.map(|s|json!({"width":s.width,"height":s.height}))})
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Create,
    Move,
    Resize(Handle),
    Clear,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Gesture {
    hit: Hit,
    start: (i64, i64),
    before: Option<Crop>,
    live: Option<Crop>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoverEditor {
    pub original: Artwork,
    pub rendered: Artwork,
    pub edits: Edits,
    pub past: Vec<Edits>,
    pub future: Vec<Edits>,
    pub locked: bool,
    pub resize_locked: bool,
    pub revision: u64,
    pub render_due: bool,
    pub rendering: bool,
    pub ready: bool,
    gesture: Option<Gesture>,
    focus: Option<Handle>,
}
impl CoverEditor {
    fn new(original: Artwork) -> Self {
        Self {
            rendered: original.clone(),
            original,
            edits: Edits::default(),
            past: vec![],
            future: vec![],
            locked: false,
            resize_locked: true,
            revision: 0,
            render_due: false,
            rendering: false,
            ready: true,
            gesture: None,
            focus: None,
        }
    }
    fn size(&self) -> Size {
        crop::artwork_size(&self.original)
    }
    fn ratio(&self) -> Option<f64> {
        self.locked.then(|| {
            self.edits.crop.map_or(
                self.original.width as f64 / self.original.height as f64,
                |c| c.width as f64 / c.height as f64,
            )
        })
    }
    fn commit(&mut self, next: Edits) {
        if next == self.edits {
            return;
        }
        self.past.push(std::mem::replace(&mut self.edits, next));
        if self.past.len() > 40 {
            self.past.remove(0);
        }
        self.future.clear();
        self.invalidate();
    }
    fn invalidate(&mut self) {
        self.revision += 1;
        self.render_due = true;
        self.ready = false;
    }
    pub fn can_apply(&self) -> bool {
        self.ready && self.gesture.is_none()
    }
    pub fn active_crop(&self) -> Option<Crop> {
        match self.gesture {
            Some(g) => g.live,
            None => self.edits.crop,
        }
    }
    pub fn canvas_box(&self, width: f64) -> (f64, f64, f64, f64, f64) {
        let height = 220.0;
        let scale = (width / self.original.width as f64).min(height / self.original.height as f64);
        let w = self.original.width as f64 * scale;
        let h = self.original.height as f64 * scale;
        ((width - w) / 2., (height - h) / 2., w, h, scale)
    }
    pub fn canvas(&self, width: f64) -> Value {
        let (x, y, w, h, k) = self.canvas_box(width);
        let mut ops =
            vec![json!({"op":"image","rid":"cover:original","dst":{"x":x,"y":y,"w":w,"h":h}})];
        if let Some(c) = self.active_crop() {
            let (cx, cy, cw, ch) = (
                x + c.x as f64 * k,
                y + c.y as f64 * k,
                c.width as f64 * k,
                c.height as f64 * k,
            );
            for (l, t, ww, hh) in [
                (x, y, w, cy - y),
                (x, cy, cx - x, ch),
                (cx + cw, cy, (x + w - cx - cw).max(0.), ch),
                (x, cy + ch, w, (y + h - cy - ch).max(0.)),
            ] {
                ops.push(json!({"op":"rect","x":l,"y":t,"w":ww,"h":hh,"fill":"#00000099"}));
            }
            ops.push(
                json!({"op":"rect","x":cx,"y":cy,"w":cw,"h":ch,"stroke":"#83acff","stroke_w":1.5}),
            );
            for (handle, hx, hy) in HANDLES {
                ops.push(json!({"op":"rect","x":cx+cw*hx-3.5,"y":cy+ch*hy-3.5,"w":7,"h":7,"fill":if self.focus==Some(handle) {"#5688ff"} else {"#ffffff"}}));
            }
            ops.push(json!({"op":"text","x":cx+cw-13.,"y":cy+12.,"text":"×","color":"#ffffff","size":12.}));
        }
        json!({"id":"art:canvas","kind":"canvas","width":{"px":width},"height":{"px":220},"interactive":{"pointer":true,"keys":true,"focusable":true,"coalesce":"move"},"draw":{"frame":0,"clear":"#141517","ops":ops}})
    }
    fn pointer(&mut self, value: &Value, width: f64) {
        let phase = value["phase"].as_str().unwrap_or_default();
        if phase == "cancel" {
            self.gesture = None;
            return;
        }
        let (ox, oy, w, h, k) = self.canvas_box(width);
        let Some((x, y)) = value["x"].as_f64().zip(value["y"].as_f64()) else {
            return;
        };
        let source = self.size();
        let p = (
            ((x - ox) / k).round().clamp(0., source.width as f64) as i64,
            ((y - oy) / k).round().clamp(0., source.height as f64) as i64,
        );
        if phase == "down" {
            if x < ox - 6. || y < oy - 6. || x > ox + w + 6. || y > oy + h + 6. {
                return;
            }
            let hit = self.edits.crop.map_or(Hit::Create, |c| {
                let (l, t, w, h) = (
                    ox + c.x as f64 * k,
                    oy + c.y as f64 * k,
                    c.width as f64 * k,
                    c.height as f64 * k,
                );
                if w > 25.
                    && h > 25.
                    && ((x - (l + w - 12.)).abs()).max((y - (t + 10.)).abs()) <= 6.
                {
                    return Hit::Clear;
                }
                HANDLES
                    .into_iter()
                    .find(|(_, px, py)| ((x - l - w * px).abs()).max((y - t - h * py).abs()) <= 7.)
                    .map_or_else(
                        || {
                            if x >= l && x <= l + w && y >= t && y <= t + h {
                                Hit::Move
                            } else {
                                Hit::Create
                            }
                        },
                        |(h, _, _)| Hit::Resize(h),
                    )
            });
            self.focus = if let Hit::Resize(h) = hit {
                Some(h)
            } else {
                None
            };
            self.gesture = Some(Gesture {
                hit,
                start: p,
                before: self.edits.crop,
                live: self.edits.crop,
            });
            return;
        }
        let Some(mut g) = self.gesture else { return };
        let ratio = self.ratio();
        let (dx, dy) = (p.0 - g.start.0, p.1 - g.start.1);
        g.live = match g.hit {
            Hit::Clear => None,
            Hit::Move => g.before.map(|c| crop::move_crop(source, c, dx, dy)),
            Hit::Resize(h) => g
                .before
                .map(|c| crop::resize_crop(source, c, h, dx, dy, ratio)),
            Hit::Create if dx.abs() < 2 || dy.abs() < 2 => g.before,
            Hit::Create => {
                let (x, y) = (p.0.min(g.start.0) as u32, p.1.min(g.start.1) as u32);
                let (width, height) = (dx.unsigned_abs() as u32, dy.unsigned_abs() as u32);
                let c = Crop {
                    x,
                    y,
                    width,
                    height,
                };
                Some(if ratio.is_some() {
                    crop::size_crop(source, Some(c), Field::Width, width as f64, ratio)
                } else {
                    c
                })
            }
        };
        if phase == "up" {
            self.gesture = None;
            self.commit(Edits {
                crop: g.live,
                ..self.edits.clone()
            });
        } else {
            self.gesture = Some(g);
        }
    }
    fn key(&mut self, value: &Value) {
        if value["phase"] != "down" {
            return;
        }
        let key = value["key"].as_str().unwrap_or_default();
        if key == "Escape" {
            self.gesture = None;
            return;
        }
        let step = if value["mods"]["shift"] == true {
            10
        } else {
            1
        };
        let (dx, dy) = match key {
            "ArrowLeft" | "Left" => (-step, 0),
            "ArrowRight" | "Right" => (step, 0),
            "ArrowUp" | "Up" => (0, -step),
            "ArrowDown" | "Down" => (0, step),
            "Delete" | "Backspace" => {
                self.commit(Edits {
                    crop: None,
                    ..self.edits.clone()
                });
                return;
            }
            _ => return,
        };
        if let Some(c) = self.edits.crop {
            let next = if let Some(h) = self.focus {
                crop::resize_crop(self.size(), c, h, dx, dy, self.ratio())
            } else {
                crop::move_crop(self.size(), c, dx, dy)
            };
            self.commit(Edits {
                crop: Some(next),
                ..self.edits.clone()
            });
        }
    }
}

impl State {
    pub fn cover_canvas_width(&self) -> f64 {
        if self.window_width < 820.0 {
            (self.window_width - 45.).clamp(180., 430.)
        } else {
            ((self.window_width - 310.) / 3.).clamp(160., 360.)
        }
    }
    pub fn load_cover_editor(&mut self, art: Artwork) {
        if art.width < 2 || art.height < 2 {
            return;
        }
        let p = self.poster_mut();
        p.generation += 1;
        p.editor = Some(CoverEditor::new(art));
        p.browser = false;
        self.section = Section::Artwork;
    }
    pub fn open_applied_cover(&mut self) {
        if let Some(art) = self.project().artwork.clone() {
            self.load_cover_editor(art);
        } else {
            self.section = Section::Artwork;
            self.poster_mut().browser = true;
        }
    }
    pub fn cover_edit_action(&mut self, verb: &str, rest: &str, v: &Value) {
        if verb == "choose" {
            self.engine_request_with(Request::ChooseArtwork, Pending::CoverChoose);
            return;
        }
        if verb == "url" {
            self.poster_mut().url = v.as_str().unwrap_or_default().chars().take(4096).collect();
            return;
        }
        if verb == "offer" {
            let url = self.poster_mut().url.trim().to_owned();
            if !url.is_empty() {
                self.engine_request_with(
                    Request::SearchBrowserDownload {
                        kind: "artwork".into(),
                        url,
                    },
                    Pending::CoverOffer,
                );
            }
            return;
        }
        if verb == "apply" || verb == "save_image" {
            let Some(art) = self
                .poster
                .as_ref()
                .and_then(|p| p.editor.as_ref())
                .filter(|e| e.can_apply())
                .map(|e| e.rendered.clone())
            else {
                return;
            };
            if verb == "apply" {
                self.apply_cover_artwork(
                    art,
                    "Cover applied to the project. Sources are unchanged.",
                );
                self.section = Section::Timeline;
            } else {
                self.engine_request_with(
                    Request::ArtworkSaveCopy {
                        artwork: serde_json::to_value(art).unwrap_or_default(),
                    },
                    Pending::CoverSave,
                );
            }
            return;
        }
        let width = self.cover_canvas_width();
        let Some(ed) = self.poster_mut().editor.as_mut() else {
            return;
        };
        let mut next = ed.edits.clone();
        match verb {
            "canvas" => {
                if v["type"] == "key" {
                    ed.key(v);
                } else {
                    ed.pointer(v, width);
                }
                return;
            }
            "rotate" => {
                next.rotation = (next.rotation + if rest == "left" { 270 } else { 90 }) % 360
            }
            "flip" => next.flip = !next.flip,
            "grayscale" => next.grayscale = !next.grayscale,
            "negative" => next.negative = !next.negative,
            "sepia" => next.sepia = !next.sepia,
            "brightness" => next.brightness = v.as_f64().unwrap_or(1.).clamp(0.1, 3.),
            "contrast" => next.contrast = v.as_f64().unwrap_or(1.).clamp(0.1, 3.),
            "lock" => {
                ed.locked = !ed.locked;
                return;
            }
            "resize_lock" => {
                ed.resize_locked = !ed.resize_locked;
                return;
            }
            "clear_crop" => next.crop = None,
            "crop_w" | "crop_h" => {
                next.crop = Some(crop::size_crop(
                    ed.size(),
                    next.crop,
                    if verb == "crop_w" {
                        Field::Width
                    } else {
                        Field::Height
                    },
                    v.as_f64().unwrap_or(2.),
                    ed.ratio(),
                ))
            }
            "preset" => {
                if let Some((_, w, h)) = rest
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| crop::CROP_PRESETS.get(i))
                {
                    let src = ed.size();
                    let n = (src.width / w).min(src.height / h);
                    if n * w >= 2 && n * h >= 2 {
                        next.crop = Some(Crop {
                            x: (src.width - n * w) / 2,
                            y: (src.height - n * h) / 2,
                            width: n * w,
                            height: n * h,
                        });
                    }
                }
            }
            "resize_w" | "resize_h" => {
                let c = next.crop.unwrap_or_else(|| Crop::full(ed.size()));
                let base = if matches!(next.rotation, 90 | 270) {
                    Size {
                        width: c.height,
                        height: c.width,
                    }
                } else {
                    Size {
                        width: c.width,
                        height: c.height,
                    }
                };
                let old = next.resize.unwrap_or(base);
                let n = v.as_u64().unwrap_or(2).clamp(2, 4096) as u32;
                let mut size = if verb == "resize_w" {
                    Size {
                        width: n,
                        height: old.height,
                    }
                } else {
                    Size {
                        width: old.width,
                        height: n,
                    }
                };
                if ed.resize_locked {
                    if verb == "resize_w" {
                        size.height = ((n as f64 * old.height as f64 / old.width as f64).round()
                            as u32)
                            .clamp(2, 4096);
                    } else {
                        size.width = ((n as f64 * old.width as f64 / old.height as f64).round()
                            as u32)
                            .clamp(2, 4096);
                    }
                }
                next.resize = Some(size);
            }
            "reset" => next = Edits::default(),
            "edit_undo" => {
                if let Some(prior) = ed.past.pop() {
                    ed.future.push(std::mem::replace(&mut ed.edits, prior));
                    ed.invalidate();
                }
                return;
            }
            "edit_redo" => {
                if let Some(later) = ed.future.pop() {
                    ed.past.push(std::mem::replace(&mut ed.edits, later));
                    ed.invalidate();
                }
                return;
            }
            _ => return,
        }
        ed.commit(next);
    }
    pub fn cover_tick(&mut self) {
        let Some(p) = self.poster.as_mut() else {
            return;
        };
        let Some(ed) = p.editor.as_mut().filter(|e| e.render_due && !e.rendering) else {
            return;
        };
        ed.render_due = false;
        if ed.edits == Edits::default() {
            ed.rendered = ed.original.clone();
            ed.ready = true;
            return;
        }
        ed.rendering = true;
        let (generation, revision) = (p.generation, ed.revision);
        let request = Request::ArtworkEdit {
            artwork: serde_json::to_value(&ed.original).unwrap_or_default(),
            edits: ed.edits.value(),
        };
        self.engine_request_with(
            request,
            Pending::ArtworkEdit {
                generation,
                revision,
            },
        );
    }
}
