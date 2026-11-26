//! Renderer basics: a filled rect lands on the right pixels, text draws coverage, PNG round-trips.

use std::collections::HashMap;

use pigeon_mock_host::render::{self, Painter, Rgba, Surface, Theme};
use pigeon_plugin_sdk::canvas::DrawOp;
use pigeon_plugin_sdk::scene::Color;
use serde_json::json;

#[test]
fn filled_rect_covers_exactly_its_pixels() {
    let theme = Theme::dark();
    let images = HashMap::new();
    let mut surf = Surface::new(20, 20, Rgba([0, 0, 0, 255]));
    {
        let mut p = Painter::new(&mut surf, &theme, &images, 1.0);
        p.apply(&DrawOp::rect(2.0, 3.0, 4.0, 5.0).fill(Color::rgb(255, 0, 0)));
    }
    assert_eq!(surf.pixel(2, 3), Rgba([255, 0, 0, 255]));
    assert_eq!(surf.pixel(5, 7), Rgba([255, 0, 0, 255]));
    assert_eq!(surf.pixel(6, 3), Rgba([0, 0, 0, 255]), "x is exclusive");
    assert_eq!(surf.pixel(2, 8), Rgba([0, 0, 0, 255]), "y is exclusive");
}

#[test]
fn text_draws_non_background_pixels() {
    let theme = Theme::dark();
    let images = HashMap::new();
    let bg = Rgba([0, 0, 0, 255]);
    let mut surf = Surface::new(120, 30, bg);
    {
        let mut p = Painter::new(&mut surf, &theme, &images, 1.0);
        p.apply(&DrawOp::text(4.0, 4.0, "Hello").size(16.0).color(Color::rgb(255, 255, 255)));
    }
    let lit = surf.px.chunks_exact(4).filter(|c| c[0] > 40).count();
    assert!(lit > 50, "text should light up pixels, got {lit}");
}

#[test]
fn scene_renders_and_png_round_trips() {
    let theme = Theme::dark();
    let scene = json!({"id":"root","kind":"column","padding":8,"children":[
        {"id":"t","kind":"text","style":"heading","text":"Hi"},
        {"id":"go","kind":"button","label":"Go","variant":"primary"}]});
    let surf = render::render_scene(&scene, (200, 120), &theme, &HashMap::new(), 1.0);
    assert_eq!((surf.w, surf.h), (200, 120));
    let bytes = render::png::encode(&surf).unwrap();
    let back = render::png::decode(&bytes).unwrap();
    assert_eq!((back.w, back.h), (200, 120));
    assert_eq!(back.rgba, surf.px);
}

#[test]
fn draw_clip_can_be_removed_but_cannot_escape_its_canvas() {
    let scene = json!({"id":"root","kind":"column","padding":2,"children":[{
        "id":"canvas","kind":"canvas","width":{"px":8},"height":{"px":8},"draw":{"frame":1,"ops":[
            {"op":"clip","x":0,"y":0,"w":4,"h":4},
            {"op":"rect","x":0,"y":0,"w":8,"h":8,"fill":"#ff0000"},
            {"op":"unclip"}, {"op":"restore"},
            {"op":"rect","x":4,"y":0,"w":30,"h":30,"fill":"#00ff00"}
        ]}
    }]});
    let theme = Theme::dark();
    let surf = render::render_scene(&scene, (16, 16), &theme, &HashMap::new(), 1.0);
    assert_eq!(surf.pixel(2, 2), Rgba([255, 0, 0, 255]));
    assert_eq!(surf.pixel(9, 9), Rgba([0, 255, 0, 255]));
    assert_eq!(surf.pixel(10, 10), theme.resolve("bg"));
}

#[test]
fn list_windows_render_non_numeric_node_ids() {
    let scene = json!({"id":"list","kind":"list","key":"entries","count":10,"row_height":20,
        "window":{"start":4,"rows":[{"id":"book-uuid","kind":"text","text":"", "bg":"#ff0000"}]}});
    let surf = render::render_scene(&scene, (60, 40), &Theme::dark(), &HashMap::new(), 1.0);
    assert_eq!(surf.pixel(10, 10), Rgba([255, 0, 0, 255]));
}
