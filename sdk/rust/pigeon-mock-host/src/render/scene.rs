//! Scene layout and painting for protocol v2 nodes, and canvas rendering.
//!
//! The layout is intentionally simple and deterministic: fixed metrics, px/fill/auto sizes, gap and
//! padding, and the layout kinds of section 5. It is a test renderer, not a pixel copy of Pigeon:
//! its job is to show that a scene is well-formed and to catch regressions in plugin output.

use std::collections::HashMap;

use pigeon_plugin_sdk::canvas::DrawOp;
use pigeon_plugin_sdk::scene::Color;
use serde_json::Value;

use super::raster::{ImageRef, Paint, Painter, Surface};
use super::text;
use super::theme::{Rgba, Theme};

const ROW_H: f32 = 28.0;
const TITLE_H: f32 = 26.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Len {
    Px(f32),
    Fill(f32),
    Auto,
}

fn len_of(node: &Value, key: &str) -> Option<Len> {
    let v = node.get(key)?;
    if v == "auto" {
        return Some(Len::Auto);
    }
    if let Some(n) = v["px"].as_f64() {
        return Some(Len::Px(n as f32));
    }
    v["fill"].as_f64().map(|n| Len::Fill(n as f32))
}

fn num(node: &Value, key: &str) -> Option<f32> {
    node.get(key).and_then(Value::as_f64).map(|n| n as f32)
}

fn text_of<'a>(node: &'a Value, key: &str) -> &'a str {
    node.get(key).and_then(Value::as_str).unwrap_or("")
}

fn color_of(theme: &Theme, node: &Value, key: &str) -> Option<Rgba> {
    node.get(key).and_then(Value::as_str).map(|c| theme.resolve(c))
}

fn kids(node: &Value) -> &[Value] {
    node.get("children").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// Kinds whose cross-axis size fills the parent by default.
fn stretches(kind: &str) -> bool {
    matches!(kind, "panel" | "canvas" | "list" | "table" | "split" | "tabs" | "scroll" | "grid" | "divider" | "text_input" | "progress" | "slider" | "range_slider" | "code" | "rich_text" | "collapsible" | "select" | "number_input" | "tree")
}

/// Natural size of a node (used when its width or height is auto).
fn natural(node: &Value, theme: &Theme, p: &Painter, scale: f32) -> (f32, f32) {
    let _ = (theme, scale);
    let font = |mono: bool| text::font(mono);
    let size_of = |s: &str, size: f32, mono: bool| text::measure(font(mono), s, size * scale) / scale;
    let kind = node["kind"].as_str().unwrap_or("");
    let (w, h) = match kind {
        "text" => {
            let style = text_of(node, "style");
            let size = text_size(style);
            let lines: Vec<&str> = text_of(node, "text").split('\n').collect();
            let w = lines.iter().map(|l| size_of(l, size, style == "mono" || style == "code")).fold(0.0, f32::max);
            (w, size * 1.4 * lines.len() as f32)
        }
        "button" | "pill" | "badge" => (size_of(text_of(node, "label").max(text_of(node, "text")), 13.0, false) + 24.0, ROW_H),
        "icon" | "icon_button" | "color_picker" | "spinner" => (16.0 + if kind == "icon_button" { 12.0 } else { 0.0 }, ROW_H.min(20.0)),
        "checkbox" => (size_of(text_of(node, "label"), 13.0, false) + 22.0, ROW_H),
        "switch" => (size_of(text_of(node, "label"), 13.0, false) + 44.0, ROW_H),
        "text_input" | "select" | "number_input" => (180.0, ROW_H),
        "slider" | "range_slider" | "progress" => (160.0, 14.0),
        "radio_group" | "segmented" => (200.0, ROW_H),
        "divider" => (1.0, 1.0),
        "spacer" => (0.0, 0.0),
        "image" => (120.0, 90.0),
        "canvas" => (200.0, 120.0),
        "list" => (200.0, 200.0),
        "table" => (300.0, 120.0),
        "tree" => (200.0, 120.0),
        "code" | "rich_text" => (260.0, 60.0),
        "tooltip" => (size_of(text_of(node, "text"), 11.0, false) + 12.0, 22.0),
        "menu" | "context_menu" => (180.0, 24.0 * items_count(node) as f32 + 8.0),
        "split" => (200.0, 120.0),
        "row" | "column" | "stack" | "wrap" | "grid" | "panel" | "scroll" | "tabs" | "collapsible" => container_natural(node, theme, p, scale),
        _ => (120.0, ROW_H),
    };
    (w, h)
}

fn items_count(node: &Value) -> usize {
    node.get("items").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)
}

fn container_natural(node: &Value, theme: &Theme, p: &Painter, scale: f32) -> (f32, f32) {
    let kind = node["kind"].as_str().unwrap_or("");
    let pad = num(node, "padding").unwrap_or(0.0);
    let gap = num(node, "gap").unwrap_or(0.0);
    let children = kids(node);
    let sizes: Vec<(f32, f32)> = children.iter().map(|c| natural(c, theme, p, scale)).collect();
    let (w, h) = match kind {
        "row" => (
            sizes.iter().map(|s| s.0).sum::<f32>() + gap * (sizes.len().saturating_sub(1)) as f32,
            sizes.iter().map(|s| s.1).fold(0.0, f32::max),
        ),
        "tabs" => (
            sizes.iter().map(|s| s.0).fold(0.0, f32::max).max(160.0),
            ROW_H + sizes.iter().map(|s| s.1).fold(0.0, f32::max),
        ),
        "collapsible" => (
            sizes.iter().map(|s| s.0).fold(0.0, f32::max).max(160.0),
            TITLE_H + sizes.iter().map(|s| s.1 + gap).sum::<f32>(),
        ),
        "grid" => {
            let cols = num(node, "columns").unwrap_or(2.0).max(1.0);
            let cell_w = sizes.iter().map(|s| s.0).fold(0.0, f32::max).max(80.0);
            (cell_w * cols, ROW_H * (children.len() as f32 / cols).ceil())
        }
        _ => (
            sizes.iter().map(|s| s.0).fold(0.0, f32::max),
            sizes.iter().map(|s| s.1).sum::<f32>() + gap * (sizes.len().saturating_sub(1)) as f32,
        ),
    };
    (w + 2.0 * pad, h + 2.0 * pad)
}

fn text_size(style: &str) -> f32 {
    match style {
        "heading" => 20.0,
        "title" => 16.0,
        "caption" => 11.0,
        "code" | "mono" => 12.0,
        _ => 13.0,
    }
}

/// Render a scene (the root node) into a surface of `size` logical px at `scale`.
pub fn render_scene(scene: &Value, size: (u32, u32), theme: &Theme, images: &HashMap<String, ImageRef>, scale: f32) -> Surface {
    let bg = theme.token("bg", "#1b1d22");
    let mut surf = Surface::new((size.0 as f32 * scale) as u32, (size.1 as f32 * scale) as u32, bg);
    {
        let mut p = Painter::new(&mut surf, theme, images, scale);
        let rect = Rect { x: 0.0, y: 0.0, w: size.0 as f32, h: size.1 as f32 };
        paint_node(&mut p, scene, rect, scale);
    }
    surf
}

/// Render a canvas draw value (`{clear?, ops}`) at `size` logical px.
pub fn render_canvas(draw: &Value, size: (u32, u32), theme: &Theme, images: &HashMap<String, ImageRef>, scale: f32) -> Result<Surface, String> {
    let mut surf = Surface::new((size.0 as f32 * scale) as u32, (size.1 as f32 * scale) as u32, theme.token("bg", "#1b1d22"));
    let mut p = Painter::new(&mut surf, theme, images, scale);
    paint_draw_value(&mut p, draw, 0.0, 0.0, size.0 as f32, size.1 as f32)?;
    Ok(surf)
}

fn paint_draw_value(p: &mut Painter, draw: &Value, x: f32, y: f32, w: f32, h: f32) -> Result<(), String> {
    if let Some(clear) = draw.get("clear").and_then(Value::as_str) {
        let c = p.theme.resolve(clear);
        p.rect_paint(x, y, w, h, &Paint::Solid(c), 0.0);
    }
    p.canvas(x, y, w, h, |p| {
        for op in draw["ops"].as_array().into_iter().flatten() {
            let typed: DrawOp = serde_json::from_value(op.clone()).map_err(|e| format!("bad draw op {op}: {e}"))?;
            p.apply(&typed);
        }
        Ok(())
    })
}

fn paint_node(p: &mut Painter, node: &Value, r: Rect, scale: f32) {
    if node.get("visible").and_then(Value::as_bool) == Some(false) {
        return;
    }
    let kind = node["kind"].as_str().unwrap_or("");
    let theme = p.theme;
    let radius = num(node, "radius").unwrap_or(theme.radius);
    if let Some(bg) = color_of(theme, node, "bg") {
        p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(bg), radius);
    }
    match kind {
        "panel" => {
            let bg = color_of(theme, node, "bg").unwrap_or_else(|| theme.token("bg_panel", "#23262d"));
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(bg), radius);
            p.stroke_rect(r.x, r.y, r.w, r.h, &Color(node["border"].as_str().unwrap_or("border").into()), 1.0);
            let mut body = inset(r, num(node, "padding").unwrap_or(8.0));
            if let Some(title) = node["title"].as_str() {
                p.text(body.x, body.y, title, 16.0, theme.token("fg", "#e6e8ee"), false, "left");
                body.y += TITLE_H;
                body.h = (body.h - TITLE_H).max(0.0);
            }
            layout_children(p, node, body, scale);
        }
        "row" | "column" | "wrap" | "scroll" => {
            let pad = num(node, "padding").unwrap_or(0.0);
            if kind == "scroll" {
                p.save();
                p.clip(r.x, r.y, r.w, r.h);
                let mut content = inset(r, pad);
                let offset = num(node, "scroll_to").unwrap_or_default();
                if node["axis"] == "horizontal" { content.x -= offset; } else { content.y -= offset; }
                layout_children(p, node, content, scale);
                p.restore();
            } else {
                layout_children(p, node, inset(r, pad), scale);
            }
        }
        "stack" => {
            for c in kids(node) {
                paint_node(p, c, r, scale);
            }
        }
        "grid" => {
            let children = kids(node);
            let cols = match num(node, "columns") {
                Some(c) => c.max(1.0) as usize,
                None => ((r.w / num(node, "min_cell").unwrap_or(120.0)).floor().max(1.0)) as usize,
            };
            let cell_w = r.w / cols as f32;
            for (i, c) in children.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let cell = Rect { x: r.x + col as f32 * cell_w, y: r.y + row as f32 * ROW_H, w: cell_w, h: ROW_H };
                paint_node(p, c, cell, scale);
            }
        }
        "split" => {
            let children = kids(node);
            let sizes: Vec<f32> = node.get("sizes").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).map(|f| f as f32).collect()).unwrap_or_default();
            let horizontal = node.get("direction").and_then(Value::as_str) != Some("vertical");
            let total = if horizontal { r.w } else { r.h };
            let mut offset = 0.0;
            for (i, c) in children.iter().enumerate() {
                let frac = sizes.get(i).copied().unwrap_or(1.0 / children.len().max(1) as f32);
                let len = (total * frac).floor();
                let cr = if horizontal {
                    Rect { x: r.x + offset, y: r.y, w: len, h: r.h }
                } else {
                    Rect { x: r.x, y: r.y + offset, w: r.w, h: len }
                };
                paint_node(p, c, cr, scale);
                offset += len;
                if i + 1 < children.len() {
                    if horizontal {
                        p.line(r.x + offset, r.y, r.x + offset, r.y + r.h, &Color::token("border"), 1.0);
                    } else {
                        p.line(r.x, r.y + offset, r.x + r.w, r.y + offset, &Color::token("border"), 1.0);
                    }
                }
            }
        }
        "tabs" => {
            let tabs = node.get("tabs").and_then(Value::as_array).cloned().unwrap_or_default();
            let active = text_of(node, "active");
            let tw = r.w / tabs.len().max(1) as f32;
            for (i, t) in tabs.iter().enumerate() {
                let x = r.x + i as f32 * tw;
                let is_active = t["id"].as_str() == Some(active);
                let label = t["label"].as_str().unwrap_or("");
                let color = if is_active { theme.token("fg", "#e6e8ee") } else { theme.token("fg_weak", "#9aa3b2") };
                p.text(x + 8.0, r.y + 5.0, label, 13.0, color, false, "left");
                if is_active {
                    p.rect_paint(x, r.y + ROW_H - 2.0, tw, 2.0, &Paint::Solid(theme.token("accent", "#3b82f6")), 0.0);
                }
            }
            let content = Rect { x: r.x, y: r.y + ROW_H, w: r.w, h: r.h - ROW_H };
            for c in kids(node) {
                if c["id"].as_str() == Some(active) {
                    paint_node(p, c, content, scale);
                }
            }
        }
        "collapsible" => {
            let open = node.get("open").and_then(Value::as_bool).unwrap_or(true);
            p.text(r.x + 6.0, r.y + 5.0, &format!("{} {}", if open { "v" } else { ">" }, text_of(node, "title")), 13.0, theme.token("fg", "#e6e8ee"), false, "left");
            if open {
                let body = Rect { x: r.x, y: r.y + TITLE_H, w: r.w, h: r.h - TITLE_H };
                layout_children(p, node, body, scale);
            }
        }
        "text" => paint_text(p, node, r),
        "button" => paint_button(p, node, r),
        "icon" => paint_icon(p, node, r),
        "icon_button" => {
            paint_icon(p, &serde_json::json!({"icon": node.get("icon").cloned().unwrap_or(Value::Null)}), Rect { x: r.x + 6.0, y: r.y + 6.0, ..r });
            p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("border"), 1.0);
        }
        "badge" | "pill" => {
            let bgc = if kind == "badge" { theme.token("accent", "#3b82f6") } else { theme.token("bg_raised", "#2b2f38") };
            let fgc = if kind == "badge" { theme.token("accent_fg", "#ffffff") } else { theme.token("fg", "#e6e8ee") };
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(bgc), r.h / 2.0);
            p.text(r.x + 12.0, r.y + (r.h - 11.0 * 1.2) / 2.0, text_of(node, "text"), 11.0, fgc, false, "left");
        }
        "checkbox" => {
            let on = node.get("value").and_then(Value::as_bool).unwrap_or(false);
            let bx = Rect { x: r.x, y: r.y + (r.h - 14.0) / 2.0, w: 14.0, h: 14.0 };
            if on {
                p.rect_paint(bx.x, bx.y, bx.w, bx.h, &Paint::Solid(theme.token("accent", "#3b82f6")), 3.0);
                p.line(bx.x + 3.0, bx.y + 7.0, bx.x + 6.0, bx.y + 10.0, &Color::token("accent_fg"), 2.0);
                p.line(bx.x + 6.0, bx.y + 10.0, bx.x + 11.0, bx.y + 4.0, &Color::token("accent_fg"), 2.0);
            } else {
                p.rect_paint(bx.x, bx.y, bx.w, bx.h, &Paint::Solid(theme.token("bg_input", "#16181c")), 3.0);
                p.stroke_rect(bx.x, bx.y, bx.w, bx.h, &Color::token("border"), 1.0);
            }
            p.text(r.x + 22.0, r.y + 6.0, text_of(node, "label"), 13.0, theme.token("fg", "#e6e8ee"), false, "left");
        }
        "switch" => {
            let on = node.get("value").and_then(Value::as_bool).unwrap_or(false);
            let track = Rect { x: r.x, y: r.y + 6.0, w: 32.0, h: 16.0 };
            let tc = if on { theme.token("accent", "#3b82f6") } else { theme.token("border", "#343945") };
            p.rect_paint(track.x, track.y, track.w, track.h, &Paint::Solid(tc), 8.0);
            let kx = if on { track.x + 18.0 } else { track.x + 2.0 };
            p.fill_ellipse(kx + 6.0, track.y + 8.0, 6.0, 6.0, &Paint::Solid(theme.token("fg", "#e6e8ee")));
            p.text(r.x + 40.0, r.y + 6.0, text_of(node, "label"), 13.0, theme.token("fg", "#e6e8ee"), false, "left");
        }
        "radio_group" | "segmented" => paint_choice(p, node, r, kind == "segmented"),
        "text_input" => {
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_input", "#16181c")), radius);
            p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("border"), 1.0);
            let value = text_of(node, "value");
            let (shown, color) = if value.is_empty() {
                (text_of(node, "placeholder").to_string(), theme.token("fg_weak", "#9aa3b2"))
            } else if node.get("password").and_then(Value::as_bool) == Some(true) {
                ("*".repeat(value.chars().count()), theme.token("fg", "#e6e8ee"))
            } else {
                (value.to_string(), theme.token("fg", "#e6e8ee"))
            };
            let mono = node.get("monospace").and_then(Value::as_bool) == Some(true);
            p.text(r.x + 8.0, r.y + 6.0, &shown, 13.0, color, mono, "left");
        }
        "number_input" => {
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_input", "#16181c")), radius);
            p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("border"), 1.0);
            let v = node.get("value").and_then(Value::as_f64).unwrap_or(0.0);
            let prec = node.get("precision").and_then(Value::as_u64).unwrap_or(if v.fract() == 0.0 { 0 } else { 2 }) as usize;
            let unit = text_of(node, "unit");
            p.text(r.x + 8.0, r.y + 6.0, &format!("{v:.prec$}{unit}"), 13.0, theme.token("fg", "#e6e8ee"), false, "left");
        }
        "slider" | "range_slider" => paint_slider(p, node, r),
        "select" => {
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_input", "#16181c")), radius);
            p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("border"), 1.0);
            let value = text_of(node, "value");
            let label = node.get("options").and_then(Value::as_array).and_then(|o| o.iter().find(|x| x["value"] == value)).and_then(|x| x["label"].as_str()).unwrap_or(value);
            p.text(r.x + 8.0, r.y + 6.0, label, 13.0, theme.token("fg", "#e6e8ee"), false, "left");
            let tri = vec![(r.x + r.w - 14.0, r.y + 10.0), (r.x + r.w - 6.0, r.y + 10.0), (r.x + r.w - 10.0, r.y + 16.0)];
            p.fill_polygons(&[tri], &Paint::Solid(theme.token("fg_weak", "#9aa3b2")));
        }
        "color_picker" => {
            let c = color_of(theme, node, "value").unwrap_or(Rgba([255, 0, 255, 255]));
            p.rect_paint(r.x, r.y, r.w.min(28.0), r.h.min(28.0), &Paint::Solid(c), 4.0);
            p.stroke_rect(r.x, r.y, r.w.min(28.0), r.h.min(28.0), &Color::token("border"), 1.0);
        }
        "progress" => {
            let track = Rect { x: r.x, y: r.y + (r.h - 8.0) / 2.0, w: r.w, h: 8.0 };
            p.rect_paint(track.x, track.y, track.w, track.h, &Paint::Solid(theme.token("bg_input", "#16181c")), 4.0);
            let (x, w) = match node.get("value").and_then(Value::as_f64) {
                Some(v) => (track.x, track.w * v.clamp(0.0, 1.0) as f32),
                None => (track.x + track.w * 0.35, track.w * 0.3),
            };
            p.rect_paint(x, track.y, w, track.h, &Paint::Solid(theme.token("accent", "#3b82f6")), 4.0);
        }
        "spinner" => {
            p.arc(r.x + 8.0, r.y + 8.0, 7.0, 0.0, 4.71, &Paint::Solid(theme.token("accent", "#3b82f6")), Some(2.0));
        }
        "divider" => p.rect_paint(r.x, r.y, r.w, 1.0, &Paint::Solid(theme.token("border", "#343945")), 0.0),
        "image" => paint_image(p, node, r),
        "canvas" => {
            let draw = node.get("draw").cloned().unwrap_or(Value::Null);
            if let Err(e) = paint_draw_value(p, &draw, r.x, r.y, r.w, r.h) {
                p.text(r.x + 4.0, r.y + 4.0, &e, 10.0, Rgba([255, 80, 80, 255]), false, "left");
            }
        }
        "list" => paint_list(p, node, r, scale),
        "table" => paint_table(p, node, r),
        "tree" => {
            let nodes = node.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
            for (i, n) in nodes.iter().enumerate() {
                let y = r.y + i as f32 * ROW_H;
                p.text(r.x + 6.0, y + 6.0, n["title"].as_str().unwrap_or(""), 13.0, theme.token("fg", "#e6e8ee"), false, "left");
            }
        }
        "menu" | "context_menu" => paint_menu(p, node, r),
        "tooltip" => {
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_raised", "#2b2f38")), radius);
            p.text(r.x + 6.0, r.y + 4.0, text_of(node, "text"), 11.0, theme.token("fg", "#e6e8ee"), false, "left");
        }
        "rich_text" => {
            let mut x = r.x;
            for span in node.get("spans").and_then(Value::as_array).cloned().unwrap_or_default() {
                let style = span["style"].as_str().unwrap_or("body");
                let color = theme.token(if style == "link" { "accent" } else { "fg" }, "#e6e8ee");
                let size = text_size(style);
                x += p.text(x, r.y + 2.0, span["text"].as_str().unwrap_or(""), size, color, false, "left");
            }
        }
        "code" => {
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_input", "#16181c")), radius);
            for (i, line) in text_of(node, "text").split('\n').enumerate() {
                p.text(r.x + 8.0, r.y + 6.0 + i as f32 * 15.0, line, 12.0, theme.token("fg", "#e6e8ee"), true, "left");
            }
        }
        _ => {}
    }
}

fn inset(r: Rect, pad: f32) -> Rect {
    Rect { x: r.x + pad, y: r.y + pad, w: (r.w - 2.0 * pad).max(0.0), h: (r.h - 2.0 * pad).max(0.0) }
}

/// Lay out children along the container's main axis (row: x, column/others: y), with gap.
fn layout_children(p: &mut Painter, node: &Value, r: Rect, scale: f32) {
    let theme = p.theme;
    let horizontal = node["kind"] == "row" || (node["kind"] == "scroll" && node["axis"] == "horizontal");
    let gap = num(node, "gap").unwrap_or(0.0);
    let children = kids(node);
    if children.is_empty() {
        return;
    }
    let naturals: Vec<(f32, f32)> = children.iter().map(|c| natural(c, theme, p, scale)).collect();
    let fixed: f32 = children
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let key = if horizontal { "width" } else { "height" };
            match len_of(c, key) {
                Some(Len::Px(n)) => n,
                Some(Len::Fill(_)) => 0.0,
                _ => {
                    if horizontal { naturals[i].0 } else { naturals[i].1 }
                }
            }
        })
        .sum();
    let fills: f32 = children
        .iter()
        .filter_map(|c| {
            let key = if horizontal { "width" } else { "height" };
            match len_of(c, key) {
                Some(Len::Fill(w)) => Some(w),
                _ => None,
            }
        })
        .sum();
    let total = if horizontal { r.w } else { r.h };
    let remaining = (total - fixed - gap * (children.len().saturating_sub(1)) as f32).max(0.0);
    let mut cursor = if horizontal { r.x } else { r.y };
    for (i, c) in children.iter().enumerate() {
        let key = if horizontal { "width" } else { "height" };
        let main = match len_of(c, key) {
            Some(Len::Px(n)) => n,
            Some(Len::Fill(w)) if fills > 0.0 => remaining * w / fills,
            Some(Len::Fill(_)) => 0.0,
            _ => {
                if horizontal { naturals[i].0 } else { naturals[i].1 }
            }
        };
        let cross_key = if horizontal { "height" } else { "width" };
        let cross_total = if horizontal { r.h } else { r.w };
        let cross = match len_of(c, cross_key) {
            Some(Len::Px(n)) => n,
            Some(Len::Fill(_)) => cross_total,
            _ => {
                if stretches(c["kind"].as_str().unwrap_or("")) {
                    cross_total
                } else if horizontal {
                    naturals[i].1
                } else {
                    naturals[i].0
                }
            }
        };
        let cr = if horizontal {
            Rect { x: cursor, y: r.y, w: main, h: cross.min(r.h.max(cross)) }
        } else {
            Rect { x: r.x, y: cursor, w: cross.min(r.w.max(cross)), h: main }
        };
        paint_node(p, c, cr, scale);
        cursor += main + gap;
    }
}

fn paint_text(p: &mut Painter, node: &Value, r: Rect) {
    let theme = p.theme;
    let style = text_of(node, "style");
    let size = text_size(style);
    let color = color_of(theme, node, "color").or_else(|| node.get("fg").and_then(Value::as_str).map(|c| theme.resolve(c))).unwrap_or(match style {
        "caption" => theme.token("fg_weak", "#9aa3b2"),
        _ => theme.token("fg", "#e6e8ee"),
    });
    let mono = style == "mono" || style == "code";
    let align = node.get("align").and_then(Value::as_str).unwrap_or("left");
    let x = match align {
        "center" => r.x + r.w / 2.0,
        "right" => r.x + r.w,
        _ => r.x,
    };
    for (i, line) in text_of(node, "text").split('\n').enumerate() {
        p.text(x, r.y + i as f32 * size * 1.4, line, size, color, mono, align);
    }
}

fn paint_button(p: &mut Painter, node: &Value, r: Rect) {
    let theme = p.theme;
    let variant = node.get("variant").and_then(Value::as_str).unwrap_or("default");
    let enabled = node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let (bg, fg, border): (Option<Rgba>, Rgba, Option<Rgba>) = match variant {
        "primary" => (Some(theme.token("accent", "#3b82f6")), theme.token("accent_fg", "#ffffff"), None),
        "danger" => (Some(theme.token("danger", "#ef4444")), theme.token("accent_fg", "#ffffff"), None),
        "ghost" => (None, theme.token("fg", "#e6e8ee"), None),
        "link" => (None, theme.token("accent", "#3b82f6"), None),
        _ => (Some(theme.token("bg_raised", "#2b2f38")), theme.token("fg", "#e6e8ee"), Some(theme.token("border", "#343945"))),
    };
    let toggled = node.get("toggled").and_then(Value::as_bool) == Some(true);
    if let Some(bg) = bg {
        let bg = if enabled { bg } else { Rgba([bg.0[0] / 2, bg.0[1] / 2, bg.0[2] / 2, 255]) };
        p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(bg), theme.radius);
    }
    if let Some(b) = border {
        p.stroke_rect(r.x, r.y, r.w, r.h, &Color(format!("#{:02x}{:02x}{:02x}", b.0[0], b.0[1], b.0[2])), 1.0);
    }
    if toggled {
        p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("focus_ring"), 2.0);
    }
    let label = text_of(node, "label");
    let w = text::measure(text::font(false), label, 13.0);
    p.text(r.x + (r.w - w) / 2.0, r.y + (r.h - 13.0 * 1.2) / 2.0, label, 13.0, if enabled { fg } else { Rgba([fg.0[0], fg.0[1], fg.0[2], 120]) }, false, "left");
}

fn paint_icon(p: &mut Painter, node: &Value, r: Rect) {
    // Built-in icons are not drawn from a font here: each name gets a stable glyph (a square with
    // a dot pattern from its name), so icons are visible and deterministic in golden images.
    let name = text_of(node, "icon");
    let hash = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let size = 14.0f32;
    let (x, y) = (r.x, r.y + (r.h - size).max(0.0) / 2.0);
    p.rect_paint(x, y, size, size, &Paint::Solid(p.theme.token("fg_weak", "#9aa3b2")), 2.0);
    for i in 0..4u32 {
        if hash >> i & 1 == 1 {
            let (dx, dy) = ((i % 2) as f32 * 7.0 + 3.0, (i / 2) as f32 * 7.0 + 3.0);
            p.rect_paint(x + dx, y + dy, 4.0, 4.0, &Paint::Solid(p.theme.token("bg", "#1b1d22")), 0.0);
        }
    }
}

fn paint_choice(p: &mut Painter, node: &Value, r: Rect, segmented: bool) {
    let theme = p.theme;
    let value = text_of(node, "value");
    let options = node.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut x = r.x;
    for o in &options {
        let label = o["label"].as_str().unwrap_or("");
        let selected = o["value"].as_str() == Some(value);
        let w = text::measure(text::font(false), label, 13.0) + 26.0;
        if segmented {
            let bg = if selected { theme.token("accent", "#3b82f6") } else { theme.token("bg_raised", "#2b2f38") };
            p.rect_paint(x, r.y, w, ROW_H, &Paint::Solid(bg), 0.0);
            let fg = if selected { theme.token("accent_fg", "#ffffff") } else { theme.token("fg", "#e6e8ee") };
            p.text(x + 13.0, r.y + 6.0, label, 13.0, fg, false, "left");
        } else {
            let ring = Paint::Solid(theme.token("border", "#343945"));
            p.arc(x + 8.0, r.y + 14.0, 7.0, 0.0, std::f32::consts::TAU, &ring, Some(2.0));
            if selected {
                p.fill_ellipse(x + 8.0, r.y + 14.0, 3.5, 3.5, &Paint::Solid(theme.token("accent", "#3b82f6")));
            }
            p.text(x + 20.0, r.y + 6.0, label, 13.0, theme.token("fg", "#e6e8ee"), false, "left");
        }
        x += w + if segmented { 2.0 } else { 16.0 };
    }
}

fn paint_slider(p: &mut Painter, node: &Value, r: Rect) {
    let theme = p.theme;
    let min = num(node, "min").unwrap_or(0.0);
    let max = num(node, "max").unwrap_or(1.0).max(min + 1e-6);
    let frac = |v: f32| ((v - min) / (max - min)).clamp(0.0, 1.0);
    let track = Rect { x: r.x + 6.0, y: r.y + r.h / 2.0 - 2.0, w: (r.w - 12.0).max(0.0), h: 4.0 };
    p.rect_paint(track.x, track.y, track.w, track.h, &Paint::Solid(theme.token("border", "#343945")), 2.0);
    let values: Vec<f32> = match node.get("value") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_f64).map(|f| f as f32).collect(),
        Some(v) => vec![v.as_f64().unwrap_or(0.0) as f32],
        None => vec![min],
    };
    let lo = if values.len() > 1 { frac(values[0]) } else { 0.0 };
    let hi = frac(*values.last().unwrap_or(&min));
    p.rect_paint(track.x + track.w * lo, track.y, track.w * (hi - lo), track.h, &Paint::Solid(theme.token("accent", "#3b82f6")), 2.0);
    for v in &values {
        let kx = track.x + track.w * frac(*v);
        p.fill_ellipse(kx, r.y + r.h / 2.0, 6.0, 6.0, &Paint::Solid(theme.token("fg", "#e6e8ee")));
    }
}

fn paint_image(p: &mut Painter, node: &Value, r: Rect) {
    let theme = p.theme;
    let rid = text_of(node, "resource");
    let fit = node.get("fit").and_then(Value::as_str).unwrap_or("contain");
    let images: &HashMap<String, ImageRef> = p.images;
    let img = images.get(rid);
    match img {
        None => {
            p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_input", "#16181c")), 4.0);
            p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("border"), 1.0);
            p.text(r.x + 6.0, r.y + 6.0, &format!("image {rid}"), 11.0, theme.token("fg_weak", "#9aa3b2"), false, "left");
        }
        Some(img) => {
            let (iw, ih) = (img.w as f32, img.h as f32);
            let dst = match fit {
                "fill" => r,
                "none" => Rect { x: r.x, y: r.y, w: iw, h: ih },
                "cover" => {
                    let k = (r.w / iw).max(r.h / ih);
                    let (w, h) = (iw * k, ih * k);
                    Rect { x: r.x + (r.w - w) / 2.0, y: r.y + (r.h - h) / 2.0, w, h }
                }
                _ => {
                    let k = (r.w / iw).min(r.h / ih);
                    let (w, h) = (iw * k, ih * k);
                    Rect { x: r.x + (r.w - w) / 2.0, y: r.y + (r.h - h) / 2.0, w, h }
                }
            };
            let clip_r = r;
            p.save();
            p.clip(clip_r.x, clip_r.y, clip_r.w, clip_r.h);
            p.image(img, None, pigeon_plugin_sdk::protocol::Rect { x: dst.x, y: dst.y, w: dst.w, h: dst.h }, 1.0, None);
            p.restore();
        }
    }
}

fn paint_list(p: &mut Painter, node: &Value, r: Rect, scale: f32) {
    let theme = p.theme;
    let row_h = num(node, "row_height").unwrap_or(ROW_H);
    let count = node.get("count").and_then(Value::as_u64).unwrap_or(0);
    let window = node.get("window");
    let start = window.and_then(|w| w.get("start")).and_then(Value::as_u64).unwrap_or(0);
    let rows = window.and_then(|w| w.get("rows")).and_then(Value::as_array);
    p.save();
    p.clip(r.x, r.y, r.w, r.h);
    let row_h = row_h.max(1.0);
    let visible = (r.h / row_h).ceil() as usize;
    for i in 0..visible.min(count.saturating_sub(start) as usize) {
        let y = r.y + i as f32 * row_h;
        let row = rows.and_then(|rows| rows.get(i));
        match row {
            Some(row) => {
                let rr = Rect { x: r.x, y, w: r.w, h: row_h };
                paint_node(p, row, rr, scale);
            }
            None => {
                p.rect_paint(r.x + 4.0, y + 6.0, r.w * 0.6, row_h - 12.0, &Paint::Solid(theme.token("bg_raised", "#2b2f38")), 3.0);
            }
        }
    }
    p.restore();
}

fn paint_table(p: &mut Painter, node: &Value, r: Rect) {
    let theme = p.theme;
    let columns = node.get("columns").and_then(Value::as_array).cloned().unwrap_or_default();
    p.rect_paint(r.x, r.y, r.w, ROW_H, &Paint::Solid(theme.token("bg_raised", "#2b2f38")), 0.0);
    let cw = r.w / columns.len().max(1) as f32;
    for (i, c) in columns.iter().enumerate() {
        p.text(r.x + i as f32 * cw + 6.0, r.y + 6.0, c["title"].as_str().unwrap_or(""), 12.0, theme.token("fg_weak", "#9aa3b2"), false, "left");
    }
    let count = node.get("count").and_then(Value::as_u64).unwrap_or(0);
    let rows = node.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
    for i in 0..count.min(((r.h - ROW_H) / ROW_H).floor() as u64) {
        let y = r.y + ROW_H * (i as f32 + 1.0);
        let row = rows.iter().find(|x| x["index"].as_u64() == Some(i));
        for (ci, c) in columns.iter().enumerate() {
            let cell = row.and_then(|x| x["cells"][c["id"].as_str().unwrap_or("")].as_str()).unwrap_or("");
            p.text(r.x + ci as f32 * cw + 6.0, y + 6.0, cell, 13.0, theme.token("fg", "#e6e8ee"), false, "left");
        }
        p.rect_paint(r.x, y + ROW_H - 1.0, r.w, 1.0, &Paint::Solid(theme.token("border", "#343945")), 0.0);
    }
}

fn paint_menu(p: &mut Painter, node: &Value, r: Rect) {
    let theme = p.theme;
    p.rect_paint(r.x, r.y, r.w, r.h, &Paint::Solid(theme.token("bg_raised", "#2b2f38")), theme.radius);
    p.stroke_rect(r.x, r.y, r.w, r.h, &Color::token("border"), 1.0);
    let items = node.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    for (i, item) in items.iter().enumerate() {
        let y = r.y + 4.0 + i as f32 * 24.0;
        if item["separator"].as_bool() == Some(true) {
            p.rect_paint(r.x + 6.0, y + 11.0, r.w - 12.0, 1.0, &Paint::Solid(theme.token("border", "#343945")), 0.0);
            continue;
        }
        let enabled = item["enabled"].as_bool().unwrap_or(true);
        let fg = if enabled { theme.token("fg", "#e6e8ee") } else { theme.token("fg_weak", "#9aa3b2") };
        if item["checked"].as_bool() == Some(true) {
            p.line(r.x + 8.0, y + 12.0, r.x + 11.0, y + 15.0, &Color::token("accent"), 2.0);
            p.line(r.x + 11.0, y + 15.0, r.x + 16.0, y + 9.0, &Color::token("accent"), 2.0);
        }
        p.text(r.x + 24.0, y + 6.0, item["label"].as_str().unwrap_or(""), 13.0, fg, false, "left");
        if let Some(sc) = item["shortcut"].as_str() {
            let w = text::measure(text::font(false), sc, 11.0);
            p.text(r.x + r.w - w - 10.0, y + 7.0, sc, 11.0, theme.token("fg_weak", "#9aa3b2"), false, "left");
        }
    }
}
