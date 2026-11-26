//! Scene builders (protocol v2 section 5). Every function is a pure view of [`State`]; node ids
//! are stable action names (`verb:argument`) that the reducer parses back in `State::dispatch`.
//! Instance ids: `main` (workspace view), `summary` and `project` (inspector panels), and
//! `modal.<name>` for dialogs.

use crate::canvas::{self, LaneDraw};
use crate::engine::job_is_active;
use crate::geometry::{self, clip_bounds, source_duration, timeline_viewport_duration};
use crate::model::{basename, bytes_label, is_image_asset, time_hms, Kind, Track};
use crate::playback::PLAYBACK_SPEEDS;
use crate::state::{action_label, Section, State, LANGUAGES};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Reelio palette shipped as the plugin's theme override (charcoal / blue, Pigeon-derived).
pub fn theme_override() -> Value {
    json!({
        "colors": {
            "bg": "#18191b", "bg_panel": "#202123", "bg_raised": "#272a36", "bg_input": "#191b23",
            "fg": "#e8eaf0", "fg_weak": "#8f95a6", "accent": "#5688ff", "accent_fg": "#b9ccff",
            "danger": "#f16d7a", "warning": "#e6bd79", "success": "#8bd3ac", "border": "#2b2e35",
            "hover": "#2b2e35", "selection": "#34394c", "focus_ring": "#5688ff"
        },
        "radius": 4,
        "spacing": 4
    })
}

fn node(id: &str, kind: &str, props: Value) -> Value {
    let mut v = json!({"id": id, "kind": kind});
    if let (Some(map), Value::Object(extra)) = (v.as_object_mut(), props) {
        map.extend(extra);
    }
    v
}

fn text(id: &str, s: &str, style: &str) -> Value {
    node(id, "text", json!({"text": s, "style": style, "wrap": true}))
}

fn button(id: &str, label: &str, icon: Option<&str>, variant: &str, enabled: bool) -> Value {
    let variant = if variant == "secondary" {
        "default"
    } else {
        variant
    };
    let mut v = node(
        id,
        "button",
        json!({"label": label, "variant": variant, "on": ["click"], "enabled": enabled}),
    );
    if let Some(i) = icon {
        v["icon"] = json!(i);
    }
    v
}

fn icon_button(id: &str, icon: &str, tooltip: &str, enabled: bool) -> Value {
    node(
        id,
        "icon_button",
        json!({"icon": icon, "tooltip": tooltip, "on": ["click"], "enabled": enabled}),
    )
}

fn input(id: &str, value: &str, placeholder: &str, commit: &str) -> Value {
    node(
        id,
        "text_input",
        json!({"value": value, "rev": 0, "placeholder": placeholder, "commit": commit}),
    )
}

fn row(id: &str, children: Vec<Value>, gap: f64) -> Value {
    node(id, "row", json!({"gap": gap, "children": children}))
}

fn column(id: &str, children: Vec<Value>, gap: f64) -> Value {
    node(id, "column", json!({"gap": gap, "children": children}))
}

fn panel(id: &str, children: Vec<Value>) -> Value {
    node(
        id,
        "panel",
        json!({"children": [column(&format!("{id}.body"), children, 8.0)], "padding": 10}),
    )
}

fn section_title(id: &str, title: &str) -> Value {
    node(
        id,
        "text",
        json!({"text": title, "style": "caption", "color": "fg_weak"}),
    )
}

fn pill(id: &str, label: &str, tone: &str) -> Value {
    node(id, "pill", json!({"text": label, "tone": tone}))
}

fn stat(id: &str, label: &str, value: &str) -> Value {
    row(
        id,
        vec![
            node(
                &format!("{id}.k"),
                "text",
                json!({"text": label, "color": "fg_weak"}),
            ),
            node(
                &format!("{id}.v"),
                "text",
                json!({"text": value, "align": "end", "mono": true}),
            ),
        ],
        8.0,
    )
}

/// The active toast stack (notice, error), with dismiss and hover-pause controls.
fn toasts(s: &State) -> Vec<Value> {
    let mut out = vec![];
    for (kind, toast) in [("error", s.error.as_ref()), ("notice", s.notice.as_ref())] {
        if let Some(t) = toast {
            out.push(node(
                &format!("toast:{kind}:hover"),
                "row",
                json!({
                    "role": if t.error { "alert" } else { "status" },
                    "class": if t.error { "toast error" } else { "toast" },
                    "on": ["hover", "blur"],
                    "children": [
                        text(&format!("toast.{kind}.msg"), &t.text, "body"),
                        icon_button(&format!("toast:{kind}:dismiss"), "x", "Dismiss notification", true),
                    ],
                    "gap": 8.0,
                }),
            ));
        }
    }
    out
}

/// Top bar of the workspace: project tools as tabs, undo/redo, settings, export.
fn top_bar(s: &State) -> Value {
    let tabs: Vec<Value> = Section::ALL
        .iter()
        .map(|sec| {
            let mut b = button(
                &format!("nav:{}", sec.id()),
                sec.label(),
                None,
                "ghost",
                true,
            );
            if s.section == *sec {
                b["toggled"] = json!(true);
            }
            b
        })
        .collect();
    let edit_ok = s.store.can_undo();
    row(
        "top",
        vec![
            icon_button(
                "project:menu",
                "menu",
                "Project menu: New, Open, Open media, Save, Quit",
                true,
            ),
            icon_button(
                "layout:left",
                "panel-left",
                "Collapse or expand the media pool",
                true,
            ),
            node("tools", "row", json!({"gap": 2.0, "children": tabs})),
            node("top.spacer", "spacer", json!({"width": {"fill": 1}})),
            icon_button("undo", "undo", "Undo (Ctrl/Cmd+Z)", edit_ok),
            icon_button(
                "redo",
                "redo",
                "Redo (Ctrl/Cmd+Shift+Z)",
                s.store.can_redo(),
            ),
            icon_button("settings:open", "settings", "Preferences", true),
            button(
                "export:validate",
                "Export",
                Some("download"),
                "primary",
                s.busy.is_none(),
            ),
        ],
        6.0,
    )
}

/// Media pool: dropbox, one row per source with a separate add-to-timeline button.
fn media_pool(s: &State) -> Value {
    let p = s.project();
    let rows: Vec<Value> = p
        .assets
        .iter()
        .map(|a| {
            let kind = if is_image_asset(Some(a)) {
                "IMAGE"
            } else if a.streams.iter().any(|x| x.kind == Kind::Video && !x.attached_pic()) {
                "VIDEO"
            } else if a.streams.iter().any(|x| x.kind == Kind::Audio) {
                "AUDIO"
            } else {
                "SUBTITLE"
            };
            let selected = s.source_asset().map(|x| x.id == a.id).unwrap_or(false);
            let mut open = node(
                &format!("pool.open:{}", a.id),
                "button",
                json!({"label": basename(&a.path), "variant": "ghost", "on": ["click", "double_click"], "tooltip": a.path, "toggled": selected}),
            );
            open["kind_label"] = json!(kind);
            row(
                &format!("pool.row:{}", a.id),
                vec![
                    column(
                        &format!("pool.body:{}", a.id),
                        vec![open, text(&format!("pool.kind:{}", a.id), kind, "caption")],
                        2.0,
                    ),
                    icon_button(&format!("pool.add:{}", a.id), "plus", "Add to timeline", s.busy.is_none()),
                ],
                6.0,
            )
        })
        .collect();
    let count = p.assets.len();
    let mut children = vec![
        row(
            "pool.header",
            vec![
                text("pool.title", &format!("MEDIA POOL  {count}"), "caption"),
                node("pool.spacer", "spacer", json!({"width": {"fill": 1}})),
                icon_button(
                    "layout:full_height",
                    "panels-top-left",
                    "Make left panel full height",
                    true,
                ),
                icon_button("pool.add", "plus", "Open media", s.busy.is_none()),
            ],
            6.0,
        ),
        node(
            "pool.dropbox",
            "button",
            json!({"label": "Add video, audio or subtitles\nDrop files here or browse", "variant": "default", "on": ["click"], "enabled": s.busy.is_none()}),
        ),
    ];
    children.push(node(
        "pool.list",
        "scroll",
        json!({"axis": "vertical", "children": [column("pool.rows", rows, 6.0)]}),
    ));
    children.push(text(
        "pool.hint",
        "Drag to timeline · Sources stay untouched",
        "caption",
    ));
    node(
        "pool",
        "column",
        json!({"gap": 8.0, "padding": 10, "width": {"px": s.pool_width}, "children": children}),
    )
}

/// Viewer: project name, source select, frame, seek, transport, volume, speed, notes.
fn viewer(s: &State) -> Value {
    let p = s.project();
    let source = s.source_asset();
    let duration = s.source_duration_ms();
    let sources = s.preview_sources();
    let mut children = vec![row(
        "viewer.head",
        vec![
            text("viewer.label", "VIDEO VIEWER", "caption"),
            input("viewer.name", &p.name, "Project name", "change"),
            pill("viewer.badge", "PREVIEW", "neutral"),
        ],
        8.0,
    )];
    if sources.len() > 1 {
        let options: Vec<Value> = sources
            .iter()
            .map(|a| json!({"value": a.id, "label": basename(&a.path)}))
            .collect();
        children.push(node(
            "viewer.source",
            "select",
            json!({"label": "Source to preview", "options": options, "value": source.map(|a| a.id.clone())}),
        ));
    }
    let frame = match (&s.preview.url, &s.preview.error, source) {
        (_, Some(e), _) => text("viewer.frame.error", e, "body"),
        (Some(_), None, Some(_)) => node(
            "viewer.frame",
            "image",
            json!({"resource": "frame:current", "fit": "contain"}),
        ),
        (None, None, Some(_)) if !s.dependencies_ready() => text(
            "viewer.frame.disabled",
            "Preview disabled: required components unavailable",
            "body",
        ),
        (None, None, Some(_)) => text("viewer.frame.wait", "Preparing source playback...", "body"),
        _ => text(
            "viewer.frame.empty",
            "Open video or audio to preview",
            "body",
        ),
    };
    let mut frame_layers = vec![frame];
    let cues = s.subtitle_overlay();
    if !cues.is_empty() {
        frame_layers.push(node("viewer.captions", "column", json!({"justify":"end", "align":"center", "height":{"fill":1}, "children":[
            node("viewer.caption", "text", json!({"text": cues.join("\n").chars().take(16000).collect::<String>(), "bg":"#000000cc", "fg":"#ffffff", "align":"center", "wrap":true, "padding": 6}))
        ]})));
    }
    children.push(panel(
        "viewer.frame.panel",
        vec![node(
            "viewer.frame.stack",
            "stack",
            json!({"children":frame_layers}),
        )],
    ));
    children.push(node(
        "viewer.seek",
        "slider",
        json!({"value": s.media_time, "min": 0, "max": duration, "step": 1, "on": ["change", "release"], "aria": "Seek"}),
    ));
    children.push(row(
        "viewer.time",
        vec![
            text("viewer.clock", &format!("{} / {}", crate::chapters::chapter_time(s.media_time), crate::chapters::chapter_time(duration)), "mono"),
            button("viewer.play", if s.playing { "Pause" } else { "Play" }, Some(if s.playing { "pause" } else { "play" }), "primary", source.is_some()),
            icon_button("viewer.stop", "square", "Stop and rewind", source.is_some()),
            icon_button("viewer.start", "skip-back", "Beginning", source.is_some()),
            icon_button("viewer.end", "skip-forward", "End", source.is_some()),
            icon_button("viewer.prev_frame", "step-back", "Previous frame", source.is_some()),
            icon_button("viewer.next_frame", "step-forward", "Next frame", source.is_some()),
            icon_button("viewer.add_chapter", "bookmark-plus", "Add chapter at this position", s.busy.is_none()),
            node("viewer.volume", "slider", json!({"value": s.volume, "min": 0, "max": 1, "step": 0.01, "aria": "Volume"})),
            text("viewer.volume.value", &format!("{}%", (s.volume * 100.0).round()), "mono"),
            node(
                "viewer.speed",
                "slider",
                json!({"value": s.speed_index, "min": 0, "max": PLAYBACK_SPEEDS.len() - 1, "step": 1, "aria": "Preview speed", "labels": PLAYBACK_SPEEDS.iter().map(|v| format!("{v}x")).collect::<Vec<_>>()}),
            ),
            text("viewer.speed.value", &format!("{}x", s.speed()), "mono"),
        ],
        6.0,
    ));
    children.push(row(
        "viewer.chapters",
        vec![
            button(
                "viewer.chapter_prev",
                "Previous chapter",
                None,
                "ghost",
                !p.chapters.is_empty(),
            ),
            button(
                "viewer.chapter_next",
                "Next chapter",
                None,
                "ghost",
                !p.chapters.is_empty(),
            ),
            node(
                "viewer.sync",
                "text",
                json!({"text": "Seek: sync timelines", "style": "caption", "color": "fg_weak"}),
            ),
        ],
        8.0,
    ));
    children.push(text(
        "viewer.disclaimer",
        "Source video with synchronized timeline audio and selected subtitle audition. Select a language lane to audition it separately. Rendered video lead-in/trimming is applied at export; codec support varies by platform.",
        "caption",
    ));
    children.push(button(
        "viewer.retry",
        "Retry preview",
        None,
        "secondary",
        source.is_some() && s.dependencies_ready(),
    ));
    node(
        "viewer",
        "column",
        json!({"gap": 8.0, "padding": 10, "fill": 1, "children": children}),
    )
}

/// One timeline lane: header controls plus a canvas that draws ruler, clip, handles and pins.
fn lane(s: &State, t: &Track, view_width: f64) -> Value {
    let p = s.project();
    let kind = p.track_kind(t).unwrap_or(Kind::Data);
    let kind_str = match kind {
        Kind::Video => "video",
        Kind::Audio => "audio",
        _ => "subtitle",
    };
    let letter = match kind {
        Kind::Video => "V",
        Kind::Audio => "A",
        _ => "S",
    };
    let asset_path = p.track_asset(t).map(|a| a.path.clone()).unwrap_or_default();
    let mut controls = vec![];
    if kind == Kind::Video {
        let main = p.main_video_track_id.as_deref() == Some(t.id.as_str());
        let mut b = icon_button(
            &format!("lane.ref:{}", t.id),
            "crosshair",
            if main {
                "Main video reference. Click to clear."
            } else {
                "Use as main video reference (does not change export)"
            },
            s.busy.is_none(),
        );
        b["toggled"] = json!(main);
        controls.push(b);
    }
    if kind == Kind::Audio {
        let mut mute = button(
            &format!("lane.mute:{}", t.id),
            if t.muted { "Muted" } else { "Mute" },
            None,
            "ghost",
            true,
        );
        mute["toggled"] = json!(t.muted);
        controls.push(mute);
        let role = if t.audio_role == Some(crate::model::AudioRole::Mix) {
            "mix"
        } else {
            "separate"
        };
        controls.push(node(
            &format!("lane.role:{}", t.id),
            "select",
            json!({"value": role, "options": [{"value": "mix", "label": "Mix into main audio"}, {"value": "separate", "label": "Separate language track"}]}),
        ));
    }
    if matches!(kind, Kind::Audio | Kind::Subtitle) {
        controls.push(input(
            &format!("lane.lang:{}", t.id),
            &t.language,
            "und",
            "submit",
        ));
    }
    controls.push(node(
        &format!("lane.offset:{}", t.id),
        "number_input",
        json!({"label": "Offset ms", "value": t.offset_ms, "min": -geometry::MAX_OFFSET, "max": geometry::MAX_OFFSET, "step": 10, "commit": "submit"}),
    ));
    let heading = row(
        &format!("lane.head:{}", t.id),
        vec![
            text(&format!("lane.letter:{}", t.id), letter, "title"),
            node(
                &format!("lane.select:{}", t.id),
                "button",
                json!({"label": if t.title.is_empty() { basename(&asset_path) } else { t.title.clone() }, "variant": "ghost", "tooltip": asset_path, "on": ["click"], "toggled": s.selected.as_deref() == Some(t.id.as_str())}),
            ),
            row(&format!("lane.options:{}", t.id), controls, 6.0),
        ],
        6.0,
    );
    let draw = canvas_draw(s, t, kind_str, view_width);
    column(
        &format!("lane:{}", t.id),
        vec![
            heading,
            node(
                &format!("lane.canvas:{}", t.id),
                "canvas",
                json!({
                    "width": {"fill": 1}, "height": {"px": 56}, "cursor": "default",
                    "interactive": {"pointer": true, "wheel": true, "keys": true, "focusable": true, "coalesce": "move"},
                    "draw": draw,
                }),
            ),
        ],
        4.0,
    )
}

/// Lane draw list computed from the shared timeline view (fast-path canvas data).
pub fn canvas_draw(s: &State, t: &Track, kind: &str, width: f64) -> Value {
    let p = s.project();
    let view = s.timeline_view;
    let end = timeline_viewport_duration(p);
    let scale = (width / end) * view.zoom;
    let (start, bounds_end) = clip_bounds(p, t);
    let offset = t.offset_ms as f64;
    let clip_left = ((offset + start).max(0.0)) * scale - view.scroll;
    let clip_width =
        (geometry::MIN_CLIP_WIDTH).max((bounds_end + offset - (offset + start).max(0.0)) * scale);
    let step = geometry::ruler_interval(scale);
    let mut ticks = vec![];
    let first = ((view.scroll / scale) / step).floor().max(0.0) as i64 - 1;
    for n in first..first + 64 {
        let ms = n as f64 * step;
        let x = ms * scale - view.scroll;
        if x > width {
            break;
        }
        if x >= -60.0 {
            ticks.push((x, crate::chapters::chapter_time(ms)));
        }
    }
    let chapters: Vec<f64> = if kind == "video" {
        p.chapters
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let start = match s.drafts.chapter {
                    Some((index, ms)) if index == i => ms,
                    _ => c.start_ms as f64,
                };
                start * scale - view.scroll
            })
            .collect()
    } else {
        vec![]
    };
    let draw = LaneDraw {
        width,
        height: 56.0,
        kind: kind.into(),
        enabled: t.enabled && !t.muted,
        selected: s.selected.as_deref() == Some(t.id.as_str()),
        title: if t.title.is_empty() {
            basename(&p.track_asset(t).map(|a| a.path.clone()).unwrap_or_default())
        } else {
            t.title.clone()
        },
        start_label: crate::chapters::chapter_time((offset + start).max(0.0)),
        end_label: crate::chapters::chapter_time(bounds_end - start.max(-offset)),
        ruler_step_ms: step,
        scale,
        scroll: view.scroll,
        clip_left,
        clip_width,
        trim_handles: source_duration(p, t).map(|d| d >= 10.0).unwrap_or(false),
        ticks,
        chapters,
        playhead_x: s.playhead * scale - view.scroll,
        excluded_pill: if !t.enabled {
            Some("Excluded")
        } else if t.muted {
            Some("Muted")
        } else {
            None
        },
    };
    canvas::draw(s.project_rev, canvas::lane_ops(&draw))
}

/// Timeline editor: header with zoom controls, one lane per included stream, optional cover lane.
fn timeline(s: &State) -> Value {
    let p = s.project();
    let view_width = 700.0;
    let shown: Vec<&Track> = p
        .tracks
        .iter()
        .filter(|t| {
            !is_image_asset(p.track_asset(t))
                && matches!(
                    p.track_kind(t),
                    Some(Kind::Video | Kind::Audio | Kind::Subtitle)
                )
                && !p.track_stream(t).map(|x| x.attached_pic()).unwrap_or(false)
        })
        .collect();
    let mut lanes: Vec<Value> = shown
        .iter()
        .map(|t| lane(s, &s.effective_track(t), view_width))
        .collect();
    if lanes.is_empty() && p.artwork.is_none() {
        lanes.push(text("timeline.empty", "Drop media into the media pool to begin. Every audio and subtitle stream gets its own lane.", "body"));
    }
    if let Some(art) = &p.artwork {
        let mut cover = row(
            "cover:lane",
            vec![
                node(
                    "cover:lane:thumbnail",
                    "image",
                    json!({"resource":"cover:project", "width":{"px":56}, "height":{"px":56}, "fit":"cover", "on":["double_click"]}),
                ),
                column(
                    "cover.text",
                    vec![
                        text(
                            "cover.title",
                            if art.source_label.is_empty() {
                                "Project cover"
                            } else {
                                &art.source_label
                            },
                            "title",
                        ),
                        text(
                            "cover.note",
                            "Container artwork · Not a video overlay",
                            "caption",
                        ),
                    ],
                    2.0,
                ),
                button("cover:edit", "Edit", None, "ghost", s.busy.is_none()),
                button("cover:remove", "Remove", None, "ghost", s.busy.is_none()),
            ],
            8.0,
        );
        cover["on"] = json!(["double_click"]);
        cover["children"][1]["id"] = json!("cover:lane:label");
        cover["children"][1]["on"] = json!(["double_click"]);
        lanes.push(cover);
    }
    let zoom_pct = format!("{}%", (s.timeline_view.zoom * 100.0).round());
    let head = row(
        "timeline.head",
        vec![
            text("timeline.title", "OUTPUT TIMELINE", "caption"),
            button(
                "timeline.add_tracks",
                "Add tracks",
                Some("plus"),
                "secondary",
                s.busy.is_none(),
            ),
            text(
                "timeline.playhead",
                &time_hms(Some(s.playhead as i64)),
                "mono",
            ),
            text(
                "timeline.hint",
                "Drag clips to position · Drag edges to trim · Alt + wheel to zoom all tracks",
                "caption",
            ),
            node("timeline.spacer", "spacer", json!({"width": {"fill": 1}})),
            button("timeline.zoom_out", "-", None, "ghost", true),
            button("timeline.zoom_in", "+", None, "ghost", true),
            button("timeline.zoom_reset", &zoom_pct, None, "ghost", true),
        ],
        8.0,
    );
    column(
        "timeline",
        vec![
            head,
            node(
                "timeline.lanes",
                "scroll",
                json!({"axis": "vertical", "children": [column("timeline.list", lanes, 8.0)]}),
            ),
        ],
        6.0,
    )
}

fn artwork_studio(s: &State) -> Value {
    let empty = crate::poster::PosterState::default();
    let p = s.poster.as_ref().unwrap_or(&empty);
    let page: &[Value] = p.pages.get(p.page).map(|v| v.as_slice()).unwrap_or(&[]);
    let rows: Vec<Value> = page
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let title = r.get("title").and_then(Value::as_str).unwrap_or("Untitled");
            let host = r.get("source_host").and_then(Value::as_str).unwrap_or("");
            let w = r.get("width").and_then(Value::as_u64).unwrap_or(0);
            let h = r.get("height").and_then(Value::as_u64).unwrap_or(0);
            column(
                &format!("art:row:{i}"),
                vec![
                    if let Some(url) = r["thumbnail_url"].as_str().filter(|u| !u.is_empty()) {
                        node(&format!("art.thumb:{i}"), "image", json!({"resource":crate::poster::thumbnail_rid(url), "height":{"px":104}, "fit":"contain"}))
                    } else { node(&format!("art.no_thumb:{i}"),"spacer",json!({"height":{"px":104}})) },
                    column(
                        &format!("art:info:{i}"),
                        vec![
                            text(&format!("art:title:{i}"), title, "body"),
                            text(
                                &format!("art:meta:{i}"),
                                &format!("{host} · {w} × {h}"),
                                "caption",
                            ),
                        ],
                        2.0,
                    ),
                    button(
                        &format!("art:use:{i}"),
                        "Use this cover",
                        None,
                        "primary",
                        s.busy.is_none(),
                    ),
                ],
                6.0,
            )
        })
        .collect();
    let orient = |id: &str, label: &str, dir: &str| {
        let mut b = icon_button(&format!("art:orient:{dir}"), id, label, true);
        b["toggled"] = json!(
            p.proportion.orientation()
                == Some(if dir == "landscape" {
                    crate::posters::Orientation::Landscape
                } else {
                    crate::posters::Orientation::Portrait
                })
        );
        b
    };
    let ratio_options: Vec<Value> = crate::posters::Proportion::ALL
        .iter()
        .map(|r| json!({"value": crate::poster::proportion_key(*r), "label": r.label()}))
        .collect();
    let search_fields = vec![
        input("art:query", &p.query, "Movie title and year", "change"),
        button(
            "art:search",
            "Search posters",
            Some("search"),
            "primary",
            s.busy.is_none(),
        ),
        orient("sparkle", "Landscape", "landscape"),
        orient("sparkle", "Portrait", "portrait"),
        node(
            "art:ratio",
            "select",
            json!({"label": "Image proportions", "value": crate::poster::proportion_key(p.proportion), "options": ratio_options}),
        ),
        node(
            "art:size",
            "select",
            json!({"label": "Results per page", "value": p.page_size.to_string(), "options": ([12,24,48].map(|n|json!({"value":n.to_string(),"label":n.to_string()})))}),
        ),
    ];
    let search = node(
        "art.search",
        "wrap",
        json!({"children":search_fields,"gap":6}),
    );
    let pager = row(
        "art.pager",
        vec![
            button("art:prev", "Previous", None, "ghost", p.page > 0),
            text(
                "art.page",
                &format!("Page {} of {}", p.page + 1, p.pages.len().max(1)),
                "caption",
            ),
            button(
                "art:next",
                "Next",
                None,
                "ghost",
                p.page + 1 < p.pages.len() || (p.has_more && p.pages.len() < 40),
            ),
        ],
        8.0,
    );
    let mut children = vec![row(
        "art.header",
        vec![
            text("art.title", "Poster & cover", "title"),
            button(
                "art:choose",
                "Import local image",
                Some("folder-open"),
                "default",
                s.busy.is_none(),
            ),
            button(
                "art:browser",
                if p.browser && p.editor.is_some() {
                    "Back to image editor"
                } else {
                    "Find artwork"
                },
                Some("search"),
                "ghost",
                true,
            ),
        ],
        6.,
    )];
    if p.browser || p.editor.is_none() {
        children.extend([
            search,
            row(
                "art.link",
                vec![
                    input("art:url", &p.url, "Public HTTPS artwork URL", "change"),
                    button(
                        "art:offer",
                        "Review download",
                        None,
                        "default",
                        !p.url.trim().is_empty(),
                    ),
                ],
                6.,
            ),
            pager,
        ]);
        if p.pending {
            children.push(text("art.busy", "Searching artwork…", "caption"));
        }
        children.push(node("art.list","scroll",json!({"axis":"vertical","height":{"fill":1},"children":[node("art.grid","grid",json!({"columns":if s.window_width<820.0{2}else{4},"gap":8,"children":rows}))]})));
    } else if let Some(ed) = &p.editor {
        children.push(cover_editor(s, ed));
    }
    if let Some(notice) = p.error.as_ref().or(p.notice.as_ref()) {
        children.push(text("art.notice", notice, "caption"));
    }
    column("artwork", children, 8.)
}

fn cover_editor(s: &State, ed: &crate::cover_editor::CoverEditor) -> Value {
    let c = ed.edits.crop;
    let resize = ed.edits.resize;
    let numeric = |id: &str, label: &str, v: u32, max: u32| {
        node(
            id,
            "number_input",
            json!({"value":v,"min":2,"max":max,"step":1,"label":label,"on":["change"]}),
        )
    };
    let toggle = |id: &str, label: &str, active: bool| {
        let mut b = button(id, label, None, "ghost", true);
        b["toggled"] = json!(active);
        b
    };
    let slider = |id: &str, label: &str, v: f64| {
        node(
            id,
            "slider",
            json!({"label":label,"min":0.1,"max":3.,"step":0.01,"value":v,"on":["change","double_click"]}),
        )
    };
    let presets = crate::crop::CROP_PRESETS
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| button(&format!("art:preset:{i}"), name, None, "ghost", true))
        .collect::<Vec<_>>();
    let controls = vec![
        text("art.controls.title", "Image editor", "title"),
        node("art.presets", "wrap", json!({"children":presets,"gap":3})),
        row(
            "art.crop.size",
            vec![
                numeric(
                    "art:crop_w",
                    "Crop width",
                    c.map_or(ed.original.width, |c| c.width),
                    ed.original.width,
                ),
                numeric(
                    "art:crop_h",
                    "Crop height",
                    c.map_or(ed.original.height, |c| c.height),
                    ed.original.height,
                ),
                toggle("art:lock", "Lock crop proportions", ed.locked),
            ],
            4.,
        ),
        row(
            "art.crop.tools",
            vec![
                button("art:clear_crop", "Clear crop", None, "ghost", c.is_some()),
                button("art:rotate:left", "Rotate left", None, "ghost", true),
                button("art:rotate:right", "Rotate right", None, "ghost", true),
                toggle("art:flip", "Flip horizontal", ed.edits.flip),
            ],
            4.,
        ),
        row(
            "art.resize",
            vec![
                numeric(
                    "art:resize_w",
                    "Resize width",
                    resize.map_or(ed.rendered.width, |r| r.width),
                    4096,
                ),
                numeric(
                    "art:resize_h",
                    "Resize height",
                    resize.map_or(ed.rendered.height, |r| r.height),
                    4096,
                ),
                toggle(
                    "art:resize_lock",
                    "Lock resize proportions",
                    ed.resize_locked,
                ),
            ],
            4.,
        ),
        row(
            "art.tones",
            vec![
                toggle("art:grayscale", "Black & white", ed.edits.grayscale),
                toggle("art:negative", "Negative", ed.edits.negative),
                toggle("art:sepia", "Sepia", ed.edits.sepia),
            ],
            4.,
        ),
        slider("art:brightness", "Brightness", ed.edits.brightness),
        slider("art:contrast", "Contrast", ed.edits.contrast),
        row(
            "art.edits",
            vec![
                button(
                    "art:edit_undo",
                    "Undo image edit",
                    None,
                    "ghost",
                    !ed.past.is_empty(),
                ),
                button(
                    "art:edit_redo",
                    "Redo image edit",
                    None,
                    "ghost",
                    !ed.future.is_empty(),
                ),
                button("art:reset", "Reset edits", None, "ghost", true),
            ],
            4.,
        ),
    ];
    let kind = if s.window_width < 820.0 {
        "column"
    } else {
        "row"
    };
    let mut image = node(
        "art.preview",
        "image",
        json!({"resource":"cover:preview","fit":"contain","width":{"px":s.cover_canvas_width()},"height":{"px":220}}),
    );
    if ed.active_crop() != ed.edits.crop {
        if let Some(c) = ed.active_crop() {
            let width = s.cover_canvas_width();
            let k = (width / c.width as f64).min(220. / c.height as f64);
            let (w, h) = (c.width as f64 * k, c.height as f64 * k);
            image = node(
                "art.preview.live",
                "canvas",
                json!({"width":{"px":width},"height":{"px":220},"draw":{"frame":0,"ops":[{"op":"image","rid":"cover:original","src":{"x":c.x,"y":c.y,"w":c.width,"h":c.height},"dst":{"x":(width-w)/2.,"y":(220.-h)/2.,"w":w,"h":h}}]}}),
            );
        }
    }
    node(
        "art.editor",
        kind,
        json!({"gap":10,"children":[
            column("art.original",vec![text("art.original.title","Original image","title"),ed.canvas(s.cover_canvas_width())],6.),
            node("art.controls","scroll",json!({"axis":"vertical","width":{"fill":1},"children":[column("art.control_body",controls,4.)]})),
            column("art.live",vec![row("art.preview.heading",vec![text("art.preview.title","Live preview","title"),button("art:save_image","Save preview image",None,"ghost",ed.can_apply())],4.),image,
                button("art:apply","Apply cover to project",Some("check"),"primary",ed.can_apply()&&s.busy.is_none())],6.),
        ]}),
    )
}

pub fn cue_rows(s: &State, first: u64, end: u64) -> (u64, Vec<Value>) {
    let Some(st) = s.studio.as_ref() else {
        return (0, vec![]);
    };
    let first = first.min(st.doc.cues.len() as u64);
    let count = end.saturating_sub(first).min(200) as usize;
    let rows = st
        .doc
        .cues
        .iter()
        .enumerate()
        .skip(first as usize)
        .take(count)
        .map(|(i, c)| {
            let first: String = c
                .text
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect();
            let mut b = button(
                &format!("sub:select:{}", c.id),
                &format!(
                    "{:>3}  {} → {}  {}",
                    i + 1,
                    crate::subtitles::cue_clock(c.start_ms),
                    crate::subtitles::cue_clock(c.end_ms),
                    first
                ),
                None,
                "ghost",
                true,
            );
            b["toggled"] = json!(st.selected.as_deref() == Some(c.id.as_str()));
            if st.workflow.playing_cue.as_deref() == Some(&c.id) {
                b["bg"] = json!("selection");
            }
            b["on"] = json!(["click", "key"]);
            b
        })
        .collect();
    (first, rows)
}

fn subtitle_studio(s: &State) -> Value {
    let empty = crate::studio::Studio::default();
    let st = s.studio.as_ref().unwrap_or(&empty);
    let doc = &st.doc;
    let w = &st.workflow;
    let editable = !s.studio_model_running();
    let (start, rows) = cue_rows(
        s,
        st.window_start as u64,
        (st.window_start + crate::studio::CUE_WINDOW) as u64,
    );
    let list = node(
        "sub:list",
        "list",
        json!({
            "count": doc.cues.len(), "row_height": 28, "key": "rows", "on":["list.window", "key"],
            "height":{"fill":1}, "window": {"start": start, "rows": rows}
        }),
    );
    let selected = st
        .selected
        .as_ref()
        .and_then(|id| doc.cues.iter().find(|c| &c.id == id));
    let mut fields = vec![];
    if let Some(c) = selected {
        fields.push(input(
            &format!("sub:start:{}", c.id),
            &crate::subtitles::cue_clock(c.start_ms),
            "HH:MM:SS,mmm",
            "submit",
        ));
        fields.push(input(
            &format!("sub:end:{}", c.id),
            &crate::subtitles::cue_clock(c.end_ms),
            "HH:MM:SS,mmm",
            "submit",
        ));
        fields.push(node(
            &format!("sub:text:{}", c.id),
            "text_input",
            json!({"value": c.text, "rev": 0, "multiline": true, "commit": "change", "label": "Subtitle text", "enabled": editable, "max_len":16384}),
        ));
        fields.push(row(
            "sub.cue_actions",
            vec![
                button(
                    &format!("sub:split:{}", c.id),
                    "Split at playhead",
                    None,
                    "secondary",
                    true,
                ),
                button(
                    &format!("sub:merge:{}", c.id),
                    "Merge with next",
                    None,
                    "secondary",
                    true,
                ),
                button(
                    &format!("sub:delete:{}", c.id),
                    "Delete cue",
                    None,
                    "danger",
                    true,
                ),
            ],
            6.0,
        ));
    } else {
        fields.push(text(
            "sub.no_cue",
            "Select a cue to edit its text and timing.",
            "caption",
        ));
    }
    let issues = crate::subtitles::cue_issues(doc);
    let issue_nodes: Vec<Value> = issues
        .iter()
        .take(20)
        .enumerate()
        .map(|(i, t)| text(&format!("sub.issue:{i}"), t, "caption"))
        .collect();
    let toolbar = row(
        "sub.toolbar",
        vec![
            button(
                "sub:open",
                "Open SRT",
                Some("folder-open"),
                "secondary",
                s.busy.is_none(),
            ),
            button(
                "sub:choose",
                "Choose subtitle file",
                None,
                "secondary",
                s.busy.is_none(),
            ),
            button("sub:new", "New", Some("file-plus"), "ghost", true),
            button("sub:add", "Add cue", Some("plus"), "ghost", true),
            button(
                "sub:undo",
                "Undo",
                Some("undo"),
                "ghost",
                !st.past.is_empty(),
            ),
            button(
                "sub:redo",
                "Redo",
                Some("redo"),
                "ghost",
                !st.future.is_empty(),
            ),
            button("sub:discard", "Discard draft", None, "ghost", st.dirty()),
            button(
                "sub:save",
                "Save SRT copy",
                Some("save"),
                "primary",
                !doc.cues.is_empty(),
            ),
        ],
        6.0,
    );
    let tools = row(
        "sub.tools",
        vec![
            input("sub:find", &st.find, "Find", "change"),
            input("sub:replacement", &st.replacement, "Replace with", "change"),
            button(
                "sub:replace",
                "Replace in all cues",
                None,
                "secondary",
                !st.find.is_empty(),
            ),
            node(
                "sub:shift_amount",
                "number_input",
                json!({"label": "Shift all cues (ms)", "value": st.shift_ms, "min": -86400000, "max": 86400000, "step": 100, "commit": "submit"}),
            ),
        ],
        6.0,
    );
    let notice = st.error.clone().or(st.notice.clone()).unwrap_or_default();
    let mut right = vec![
        column("sub.fields", fields, 6.0),
        tools,
        column("sub.issues", issue_nodes, 2.0),
        studio_models(s, st),
    ];
    if w.saved_copy.is_some() {
        right.push(button(
            "sub:add_saved",
            "Add saved SRT to project",
            None,
            "ghost",
            editable,
        ));
    }
    let responsive = if s.window_width < 880.0 {
        "column"
    } else {
        "row"
    };
    let panes = node(
        "sub.body",
        responsive,
        json!({"gap":10, "height":{"fill":1}, "children":[
            node("sub.left","column", json!({"width":{"fill":1},"children":[list]})),
            viewer(s),
            node("sub.right", "scroll", json!({"axis":"vertical", "width":{"fill":1}, "height":{"fill":1}, "children":[column("sub.editing",right,6.0)]}))
        ]}),
    );
    column(
        "subtitles",
        vec![
            row(
                "sub.head",
                vec![
                    text("sub.title", "Subtitle Studio", "title"),
                    text("sub.source", &st.source_label, "caption"),
                ],
                8.0,
            ),
            toolbar,
            button(
                "sub:search_toggle",
                "Search, import & preview",
                Some("search"),
                "ghost",
                editable,
            ),
            if w.search_open {
                studio_search(s, st)
            } else {
                node("sub.search.hidden", "spacer", json!({"height":{"px":0}}))
            },
            text("sub.notice", &notice, "caption"),
            panes,
        ],
        8.0,
    )
}

fn studio_search(s: &State, st: &crate::studio::Studio) -> Value {
    let w = &st.workflow;
    let mut actions = vec![node(
        "sub.provider",
        "wrap",
        json!({"gap":6, "children":[
            input("sub:query", &w.query, "Movie title / year", "change"),
            button("sub:search", "Search OpenSubtitles (browser)", Some("search"), "default", !w.query.trim().is_empty()),
            input("sub:url", &w.url, "Public HTTPS subtitle/ZIP download URL", "change"),
            button("sub:offer", "Review download", None, "default", !w.url.trim().is_empty()),
        ]}),
    )];
    for (i, (name, _)) in w.files.iter().enumerate() {
        actions.push(button(
            &format!("sub:add_file:{i}"),
            &format!("Add & test subtitle: {name}"),
            None,
            "ghost",
            s.busy.is_none(),
        ));
    }
    for track in s
        .project()
        .tracks
        .iter()
        .filter(|t| s.project().track_kind(t) == Some(Kind::Subtitle))
        .take(64)
    {
        actions.push(button(
            &format!("sub:test_track:{}", track.id),
            &format!("Test {}", s.track_title(track)),
            None,
            "ghost",
            s.busy.is_none(),
        ));
    }
    if let Some(test) = &s.subtitle_test {
        let end = s.source_duration_ms() as i64;
        if end > 0 && test.cues.iter().any(|cue| cue.end > end) {
            actions.push(text(
                "sub.duration_warning",
                "Some cues finish after the selected source ends.",
                "caption",
            ));
        }
        for (i, cue) in test.cues.iter().take(100).enumerate() {
            let mut control = button(
                &format!("sub:seek:{}:{i}", cue.start),
                &format!(
                    "{} → {}  {}",
                    crate::subtitles::cue_clock(cue.start),
                    crate::subtitles::cue_clock(cue.end),
                    cue.text.chars().take(130).collect::<String>()
                ),
                None,
                "ghost",
                true,
            );
            control["tooltip"] = json!("Seek source to this subtitle cue");
            actions.push(control);
        }
    }
    node(
        "sub.search",
        "scroll",
        json!({"axis":"vertical", "max_height":180,"children":[column("sub.search.content",actions,4.0)]}),
    )
}

fn studio_models(s: &State, st: &crate::studio::Studio) -> Value {
    let w = &st.workflow;
    let running = s.studio_model_running();
    let mut controls = vec![
        text("sub.model.title", "Local translation (Ollama)", "title"),
        button(
            "sub:models",
            "Refresh local models",
            None,
            "default",
            !running,
        ),
    ];
    if !w.models.is_empty() {
        let opts = w.models.iter().map(|(name,installed)| json!({"value":name,"label":format!("{name} {}",if *installed { "(installed)" } else { "(not installed)" })})).collect::<Vec<_>>();
        controls.extend([
            node(
                "sub:model",
                "select",
                json!({"label":"Local model","options":opts,"value":w.model,"enabled":!running}),
            ),
            button("sub:pull", "Download model plugin", None, "ghost", !running),
            input("sub:language", &w.language, "Translate into", "change"),
            button(
                "sub:translate",
                "Translate document",
                None,
                "primary",
                !running && !st.doc.cues.is_empty() && !w.language.trim().is_empty(),
            ),
        ]);
    }
    if running {
        controls.push(button(
            "sub:model_cancel",
            "Cancel model operation",
            None,
            "danger",
            !w.starting,
        ));
    }
    if !w.job.status.is_empty() {
        controls.push(text(
            "sub.model.status",
            &format!(
                "{}: {} · {} / {} · {}",
                w.job.kind, w.job.status, w.job.done, w.job.total, w.job.message
            ),
            "caption",
        ));
    }
    if let Some(review) = &w.review {
        controls.push(button(
            "sub:review",
            "Review translated cues",
            None,
            "default",
            !running,
        ));
        if w.review_open {
            for cue in review
                .document
                .cues
                .iter()
                .filter(|c| review.translations.contains_key(&c.id))
                .skip(w.review_page * 10)
                .take(10)
            {
                controls.push(node(&format!("sub:review_accept:{}",cue.id), "checkbox", json!({"value":review.accepted.contains(&cue.id),"label":format!("Accept translation {}",cue.id)})));
                controls.push(row(
                    &format!("sub.review.pair:{}", cue.id),
                    vec![
                        text(
                            &format!("sub.review.original:{}", cue.id),
                            &cue.text,
                            "body",
                        ),
                        text(
                            &format!("sub.review.translated:{}", cue.id),
                            &review.translations[&cue.id],
                            "body",
                        ),
                    ],
                    8.0,
                ));
            }
            controls.push(row(
                "sub.review.pager",
                vec![
                    button(
                        "sub:review_prev",
                        "Previous reviewed cues",
                        None,
                        "ghost",
                        w.review_page > 0,
                    ),
                    button(
                        "sub:review_next",
                        "Next reviewed cues",
                        None,
                        "ghost",
                        (w.review_page + 1) * 10 < review.translations.len(),
                    ),
                ],
                4.0,
            ));
            controls.push(button(
                "sub:review_apply",
                "Apply selected translations",
                None,
                "primary",
                !review.accepted.is_empty() && st.doc == review.document,
            ));
            if st.doc != review.document {
                controls.push(text("sub.review.snapshot", "These results belong to a different draft. Replacing it will restore the reviewed source snapshot, with Undo available.","caption"));
                controls.push(button(
                    "sub:review_replace",
                    "Replace with reviewed snapshot",
                    None,
                    "danger",
                    !review.accepted.is_empty(),
                ));
            }
        }
    }
    column("sub.models", controls, 6.0)
}

/// Lower workspace in the center column: timeline or poster & cover studio.
fn lower(s: &State) -> Value {
    let body = match s.section {
        Section::Artwork => artwork_studio(s),
        _ => timeline(s),
    };
    node(
        "lower",
        "column",
        json!({"fill": 1, "gap": 8, "padding": 10, "children": [body]}),
    )
}

/// Workspace view (`main`): the full central area (pool, viewer, lower workspace).
pub fn main_view(s: &State) -> Value {
    let center = if s.section == Section::Subtitles {
        vec![subtitle_studio(s)]
    } else {
        vec![viewer(s), lower(s)]
    };
    let feedback = toasts(s);
    let mut top_children = vec![top_bar(s)];
    if let Some(busy) = &s.busy {
        top_children.push(text("busy", busy, "caption"));
    }
    top_children.extend(feedback);
    let mut body = vec![];
    if !s.layout.left_collapsed && s.section != Section::Subtitles {
        body.push(media_pool(s));
    }
    body.push(node(
        "center",
        "column",
        json!({"width": {"fill":1}, "height":{"fill":1}, "gap": 0, "children": center}),
    ));
    let chrome = node(
        "chrome",
        "column",
        json!({"gap": 6, "padding": 8, "children": top_children}),
    );
    let body_row = node(
        "body",
        "row",
        json!({"fill": 1, "gap": 0, "children": body}),
    );
    let workspace = node(
        "workspace",
        "column",
        json!({"fill": 1, "gap": 6, "children": [chrome, body_row]}),
    );
    // Menus overlay the workspace (z-order `stack`).
    let mut children = vec![workspace];
    children.extend(menu_overlay(s));
    json!({"id": "root", "kind": "stack", "children": children})
}

/// Context or project menu, drawn above the workspace at its anchor point.
fn menu_overlay(s: &State) -> Option<Value> {
    let menu = s.menu.as_ref()?;
    let items: Vec<Value> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut b = button(
                &format!("menu:{}", item.action),
                &item.label,
                None,
                if item.danger { "danger" } else { "ghost" },
                item.enabled,
            );
            b["toggled"] = json!(i == menu.focus);
            if let Some(t) = &item.title {
                b["tooltip"] = json!(t);
            }
            b
        })
        .collect();
    Some(node(
        "menu",
        "panel",
        json!({"x": menu.x, "y": menu.y, "width": {"px": 200}, "role": "menu", "label": menu.label,
               "children": [column("menu.items", items, 2.0)], "padding": 4}),
    ))
}

/// Inspector: poster/cover, selected lane, output container, at a glance, validation.
pub fn summary_panel(s: &State) -> Value {
    let p = s.project();
    let mut children = vec![section_title("summary.cover.title", "POSTER / COVER")];
    children.push(match &p.artwork {
        Some(_) => node("cover:preview", "image", json!({"resource":"cover:project", "fit":"contain", "height":{"px":140}, "on":["double_click","context","drop"], "tooltip":"Drop one still image to apply it as cover"})),
        None => node("cover:preview", "button", json!({"label":"Select a cover", "variant":"ghost", "on":["click","drop"], "tooltip":"Drop one still image to apply it as cover"})),
    });
    children.push(row(
        "summary.cover.actions",
        vec![
            button("cover:edit", "Edit", None, "ghost", s.busy.is_none()),
            button("cover:remove", "Remove", None, "ghost", p.artwork.is_some()),
        ],
        6.0,
    ));
    if let Some(t) = s.selected_track() {
        let kind = p.track_kind(t);
        children.push(section_title(
            "summary.lane.title",
            &format!(
                "SELECTED {} LANE",
                match kind {
                    Some(Kind::Video) => "VIDEO",
                    Some(Kind::Audio) => "AUDIO",
                    _ => "SUBTITLE",
                }
            ),
        ));
        children.push(input(
            &format!("track.title:{}", t.id),
            &t.title,
            "Title",
            "change",
        ));
        if matches!(kind, Some(Kind::Audio | Kind::Subtitle)) {
            children.push(input(
                &format!("track.language:{}", t.id),
                &t.language,
                "Language",
                "submit",
            ));
        }
        children.push(text(
            "summary.lane.position",
            &format!(
                "Position: {} ms. Drag the clip or edit its timeline offset. Drag either clip edge to trim; extend an edge to restore source material.",
                t.offset_ms
            ),
            "caption",
        ));
        if t.trim_start_ms > 0 || t.trim_end_ms.is_some() {
            children.push(button(
                &format!("lane.trim_reset:{}", t.id),
                "Reset clip trims",
                None,
                "secondary",
                true,
            ));
        }
    }
    children.push(section_title("summary.container.title", "OUTPUT CONTAINER"));
    let containers: Vec<Value> = [("matroska", "MKV"), ("mp4", "MP4"), ("m4v", "M4V")]
        .iter()
        .map(|(id, label)| {
            let mut b = button(&format!("container:{id}"), label, None, "secondary", true);
            let current = match p.container {
                crate::model::Container::Matroska => "matroska",
                crate::model::Container::Mp4 => "mp4",
                crate::model::Container::M4v => "m4v",
            };
            b["toggled"] = json!(current == *id);
            b
        })
        .collect();
    children.push(row("summary.containers", containers, 4.0));
    children.push(text(
        "summary.container.note",
        if p.container == crate::model::Container::Matroska {
            "Flexible stream preservation."
        } else {
            "Broad compatibility. Codec restrictions apply."
        },
        "caption",
    ));
    children.push(section_title("summary.glance.title", "AT A GLANCE"));
    let duration = s
        .plan
        .as_ref()
        .and_then(|x| x.duration_ms)
        .or_else(|| s.source_asset().and_then(|a| a.duration_ms));
    let total: u64 = p.assets.iter().map(|a| a.size).sum();
    let processing = match &s.plan {
        Some(plan)
            if plan
                .tracks
                .iter()
                .any(|t| t.compatibility.action == "render_video") =>
        {
            "H.264 render"
        }
        Some(plan)
            if plan
                .tracks
                .iter()
                .any(|t| t.compatibility.action == "transcode") =>
        {
            "Blocked"
        }
        _ => "Stream copy",
    };
    children.push(stat(
        "summary.glance.duration",
        "Duration",
        &time_hms(duration),
    ));
    children.push(stat(
        "summary.glance.size",
        "Source size",
        &if p.assets.is_empty() {
            "--".into()
        } else {
            bytes_label(total)
        },
    ));
    children.push(stat(
        "summary.glance.tracks",
        "Output tracks",
        &s.plan
            .as_ref()
            .map(|x| x.tracks.len())
            .unwrap_or(p.tracks.iter().filter(|t| t.enabled).count())
            .to_string(),
    ));
    children.push(stat(
        "summary.glance.processing",
        "Video processing",
        processing,
    ));
    children.push(section_title("summary.validation.title", "VALIDATION"));
    let status = match (&s.plan, p.assets.is_empty()) {
        (_, true) => "Import media to inspect compatibility.".to_string(),
        (Some(plan), _) if !plan.errors.is_empty() => {
            format!("{} items need attention", plan.errors.len())
        }
        (Some(_), _) => "Track settings look compatible".into(),
        (None, _) => "Checking project...".into(),
    };
    children.push(text("summary.validation.status", &status, "body"));
    children.push(text(
        "summary.note",
        "A new container. Originals stay intact.",
        "caption",
    ));
    node(
        "summary",
        "column",
        json!({"gap": 8, "padding": 12, "children": children}),
    )
}

/// Project tools inspector: track settings, metadata, chapters, and export & history.
pub fn project_panel(s: &State) -> Value {
    let p = s.project();
    let body = match s.section {
        Section::Tracks | Section::Audio => track_table(s),
        Section::Metadata => metadata(s),
        Section::Chapters => chapters(s),
        Section::Export => export(s),
        Section::Media => media_info(s),
        _ => text(
            "project.hint",
            &format!("{} tracks · {} sources", p.tracks.len(), p.assets.len()),
            "caption",
        ),
    };
    node(
        "project",
        "column",
        json!({"gap": 8, "padding": 12, "children": [section_title("project.title", Section::from_id(s.section.id()).map(|x| x.label()).unwrap_or("Project")), body]}),
    )
}

fn track_table(s: &State) -> Value {
    let p = s.project();
    let audio_only = s.section == Section::Audio;
    let rows: Vec<Value> = p
        .tracks
        .iter()
        .filter(|t| !audio_only || p.track_kind(t) == Some(Kind::Audio))
        .map(|t| {
            let action = s
                .plan
                .as_ref()
                .and_then(|pl| pl.tracks.iter().find(|x| x.track_id == t.id))
                .map(|x| action_label(&x.compatibility.action))
                .unwrap_or(if t.enabled { "Checking" } else { "Excluded" });
            row(
                &format!("track.row:{}", t.id),
                vec![
                    node(
                        &format!("track.enabled:{}", t.id),
                        "checkbox",
                        json!({"value": t.enabled, "label": "Include", "on": ["change"]}),
                    ),
                    node(
                        &format!("lane.select:{}", t.id),
                        "button",
                        json!({"label": s.track_title(t), "variant": "ghost", "on": ["click"]}),
                    ),
                    select_language(t),
                    node(
                        &format!("track.default:{}", t.id),
                        "checkbox",
                        json!({"value": t.default, "label": "Default", "on": ["change"]}),
                    ),
                    text(&format!("track.output:{}", t.id), action, "caption"),
                    icon_button(
                        &format!("track.move:{}:-1", t.id),
                        "arrow-up",
                        "Move up",
                        true,
                    ),
                    icon_button(
                        &format!("track.move:{}:1", t.id),
                        "arrow-down",
                        "Move down",
                        true,
                    ),
                ],
                6.0,
            )
        })
        .collect();
    let mut children = vec![node(
        "tracks.list",
        "scroll",
        json!({"axis": "vertical", "children": [column("tracks.rows", rows, 4.0)]}),
    )];
    if let Some(t) = s.selected_track() {
        children.push(input(
            &format!("track.title:{}", t.id),
            &t.title,
            "Track title",
            "change",
        ));
        children.push(node(
            &format!("track.offset:{}", t.id),
            "number_input",
            json!({"label": "Offset (milliseconds)", "value": t.offset_ms, "min": -geometry::MAX_OFFSET, "max": geometry::MAX_OFFSET, "step": 1, "commit": "submit"}),
        ));
        children.push(node(
            &format!("track.forced:{}", t.id),
            "checkbox",
            json!({"value": t.forced, "label": "Forced subtitle", "on": ["change"]}),
        ));
        children.push(node(
            &format!("track.commentary:{}", t.id),
            "checkbox",
            json!({"value": t.commentary, "label": "Commentary", "on": ["change"]}),
        ));
        children.push(node(
            &format!("track.descriptive:{}", t.id),
            "checkbox",
            json!({"value": t.descriptive, "label": "Descriptive audio", "on": ["change"]}),
        ));
        children.push(button(
            &format!("lane.remove:{}", t.id),
            "Remove from project",
            Some("trash"),
            "danger",
            s.busy.is_none(),
        ));
    }
    column("track.table", children, 8.0)
}

fn select_language(t: &Track) -> Value {
    node(
        &format!("track.language:{}", t.id),
        "select",
        json!({"value": t.language, "options": LANGUAGES.iter().map(|l| json!({"value": l, "label": l})).collect::<Vec<_>>(), "searchable": true}),
    )
}

fn metadata(s: &State) -> Value {
    const FIELDS: [&str; 21] = [
        "title",
        "artist",
        "album",
        "date",
        "genre",
        "description",
        "synopsis",
        "copyright",
        "comment",
        "show",
        "season_number",
        "episode_sort",
        "episode_id",
        "network",
        "director",
        "cast",
        "studio",
        "content_rating",
        "sort_title",
        "original_title",
        "external_id",
    ];
    let p = s.project();
    let mut fields: Vec<Value> = FIELDS
        .iter()
        .map(|k| {
            let value = p.metadata.get(*k).cloned().unwrap_or_default();
            input(&format!("meta:{k}"), &value, &k.replace('_', " "), "change")
        })
        .collect();
    let mut extra: BTreeMap<&String, &String> = BTreeMap::new();
    for (k, v) in &p.metadata {
        if !FIELDS.contains(&k.as_str()) {
            extra.insert(k, v);
        }
    }
    for (k, v) in extra {
        fields.push(input(&format!("meta:{k}"), v, k, "change"));
    }
    column(
        "metadata",
        vec![
            text(
                "meta.note",
                "Container-aware metadata. Your source tags remain untouched.",
                "caption",
            ),
            column("meta.fields", fields, 6.0),
        ],
        8.0,
    )
}

fn chapters(s: &State) -> Value {
    let p = s.project();
    let mut rows = vec![];
    for (i, c) in p.chapters.iter().enumerate() {
        rows.push(row(
            &format!("chapter.row:{i}"),
            vec![
                text(&format!("chapter.no:{i}"), &format!("{:02}", i + 1), "mono"),
                input(&format!("chapter:title:{i}"), &c.title, "Title", "change"),
                input(
                    &format!("chapter:time:{i}"),
                    &crate::chapters::chapter_time(c.start_ms as f64),
                    "00:00:00.000",
                    "submit",
                ),
                icon_button(
                    &format!("chapter:delete:{i}"),
                    "trash",
                    &format!("Delete chapter {}", i + 1),
                    s.busy.is_none(),
                ),
            ],
            6.0,
        ));
    }
    column(
        "chapters",
        vec![
            row("chapters.tools", vec![button("chapter:add_playhead", "At playhead", Some("plus"), "secondary", s.busy.is_none()), button("chapter:sort", "Sort by timestamp", None, "ghost", true)], 6.0),
            column("chapters.rows", rows, 4.0),
            text("chapters.note", "Start times use HH:MM:SS.mmm and must be unique and increasing. Chapter ends follow the next start or the source duration automatically.", "caption"),
        ],
        8.0,
    )
}

fn export(s: &State) -> Value {
    let p = s.project();
    let mut children = vec![row(
        "export.head",
        vec![
            text("export.title", "Ready for a new container", "title"),
            button(
                "export:validate",
                "Validate sources",
                None,
                "secondary",
                s.busy.is_none(),
            ),
        ],
        6.0,
    )];
    if let Some(plan) = &s.plan {
        for e in &plan.errors {
            children.push(node(
                "export.error",
                "text",
                json!({"text": e, "style": "body", "color": "warning"}),
            ));
        }
        for t in &plan.tracks {
            children.push(row(
                &format!("export.plan:{}", t.track_id),
                vec![
                    text(
                        &format!("export.plan.name:{}", t.track_id),
                        &format!(
                            "#{} {}",
                            t.output_index + 1,
                            p.tracks
                                .iter()
                                .find(|x| x.id == t.track_id)
                                .map(|x| x.title.clone())
                                .unwrap_or_default()
                        ),
                        "body",
                    ),
                    text(
                        &format!("export.plan.action:{}", t.track_id),
                        action_label(&t.compatibility.action),
                        "caption",
                    ),
                ],
                6.0,
            ));
        }
    }
    children.push(button(
        "export:start",
        "Choose destination & export",
        Some("download"),
        "primary",
        s.export_enabled(),
    ));
    let jobs: Vec<Value> = s
        .jobs
        .iter()
        .filter(|j| j.project_id.as_deref() == Some(p.id.as_str()) || j.project_id.is_none())
        .map(|j| {
            let mut cells = vec![
                text(
                    &format!("job.name:{}", j.id),
                    &basename(&j.destination),
                    "body",
                ),
                text(&format!("job.status:{}", j.id), &j.status, "caption"),
            ];
            if job_is_active(&j.status) {
                cells.push(button(
                    &format!("export:cancel:{}", j.id),
                    "Cancel",
                    None,
                    "ghost",
                    true,
                ));
            }
            column(&format!("job:{}", j.id), cells, 4.0)
        })
        .collect();
    children.push(text("export.history.title", "Export history", "title"));
    children.push(if jobs.is_empty() {
        text(
            "export.history.empty",
            "Your completed and failed exports will appear here.",
            "caption",
        )
    } else {
        column("export.history", jobs, 6.0)
    });
    column("export", children, 8.0)
}

fn media_info(s: &State) -> Value {
    let rows: Vec<Value> = s
        .project()
        .assets
        .iter()
        .map(|a| {
            column(
                &format!("media:{}", a.id),
                vec![
                    text(&format!("media.name:{}", a.id), &basename(&a.path), "title"),
                    text(&format!("media.path:{}", a.id), &a.path, "mono"),
                    stat(&format!("media.format:{}", a.id), "Container", &a.format),
                    stat(
                        &format!("media.size:{}", a.id),
                        "Size",
                        &bytes_label(a.size),
                    ),
                    stat(
                        &format!("media.duration:{}", a.id),
                        "Duration",
                        &time_hms(a.duration_ms),
                    ),
                    stat(
                        &format!("media.streams:{}", a.id),
                        "Streams",
                        &a.streams.len().to_string(),
                    ),
                ],
                4.0,
            )
        })
        .collect();
    column("media.list", rows, 10.0)
}

/// Dialogs (`modal.<name>` instances), shown only while their flag is set.
pub fn modals(s: &State) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if s.dialogs.settings {
        let theme = if s.settings.theme.is_empty() {
            "dark"
        } else {
            s.settings.theme.as_str()
        };
        let deps = s.deps.as_ref();
        let missing = deps
            .map(|d| d.missing_components.join(", "))
            .unwrap_or_default();
        out.insert(
            "modal.settings".into(),
            column(
                "settings",
                vec![
                    text("settings.title", "Make yourself at home", "title"),
                    node("settings.theme", "select", json!({"label": "Appearance", "value": theme, "options": [
                        {"value": "system", "label": "Follow system"}, {"value": "light", "label": "Light"}, {"value": "dark", "label": "Dark"}
                    ]})),
                    text("settings.folder", &format!("Default folder: {}", s.settings.default_folder.clone().unwrap_or_else(|| "Your Videos folder".into())), "caption"),
                    text("settings.components", &if s.deps_checking { "Checking required components...".into() } else if missing.is_empty() { "Required components verified.".into() } else { format!("Components need attention: {missing}") }, "body"),
                    row("settings.actions", vec![
                        button("dep:recheck", "Check required components", None, "secondary", s.busy.is_none()),
                        button("dep:repair", "Install or repair components", None, "secondary", s.busy.is_none() && deps.map(|d| d.repair_supported).unwrap_or(false)),
                    ], 6.0),
                    text("settings.backend", &s.backend.as_ref().map(|b| b.ffmpeg_version.clone()).unwrap_or_else(|| "FFmpeg not connected. Install FFmpeg and make it available on PATH, or configure it in Preferences.".into()), "caption"),
                    button("settings:close", "Close", None, "primary", true),
                ],
                10.0,
            ),
        );
    }
    // Preferences: backend paths, default folder and image format (reference settings dialog).
    if let Some(Value::Array(children)) = out
        .get_mut("modal.settings")
        .and_then(|m| m.get_mut("children"))
    {
        let b = s.settings.backend.clone().unwrap_or_default();
        let formats: Vec<Value> = ["png", "jpeg", "webp"]
            .iter()
            .map(|f| json!({"value": f, "label": f.to_uppercase()}))
            .collect();
        children.extend([
            input("settings:ffmpeg", b.ffmpeg.as_deref().unwrap_or(""), "FFmpeg executable (absolute path)", "change"),
            input("settings:ffprobe", b.ffprobe.as_deref().unwrap_or(""), "FFprobe executable (absolute path)", "change"),
            button("backend:save", "Save backend paths", None, "secondary", s.busy.is_none()),
            row("settings.folder_actions", vec![
                button("folder:choose", "Browse folder", None, "secondary", s.busy.is_none()),
                button("folder:reset", "Reset to Videos", None, "ghost", s.busy.is_none()),
            ], 6.0),
            node("settings:image_format", "select", json!({"label": "Default image format", "value": s.settings.image_format.clone().unwrap_or_else(|| "png".into()), "options": formats})),
        ]);
    }
    if s.dialogs.about {
        out.insert(
            "modal.about".into(),
            column(
                "about",
                vec![
                    text("about.title", "Reelio", "title"),
                    text(
                        "about.version",
                        &format!("Version {} (Pigeon plugin)", env!("CARGO_PKG_VERSION")),
                        "body",
                    ),
                    text("about.credit", "Developed by Chris Visser", "body"),
                    text("about.link", "github.com/vcsoc/reelio", "caption"),
                    button(
                        "about:check_updates",
                        "Check for updates",
                        None,
                        "secondary",
                        s.busy.is_none(),
                    ),
                    button("about:close", "Close", None, "primary", true),
                ],
                10.0,
            ),
        );
    }
    if s.dialogs.quit_confirm {
        out.insert(
            "modal.quit".into(),
            column(
                "quit",
                vec![
                    text("quit.text", "Close Reelio and discard unsaved project changes? Choose Cancel to save your project first.", "body"),
                    row("quit.actions", vec![button("quit:cancel", "Cancel", None, "secondary", true), button("quit:confirm", "Close Reelio", None, "danger", true)], 6.0),
                ],
                10.0,
            ),
        );
    }
    if let Some((id, title, message)) = &s.dialogs.engine_consent {
        out.insert(
            "modal.consent".into(),
            confirm_scene(
                title,
                std::slice::from_ref(message),
                &format!("consent:cancel:{id}"),
                &format!("consent:approve:{id}"),
                "Continue",
                true,
            ),
        );
    } else if let Some(lines) = &s.dialogs.export_review {
        out.insert(
            "modal.export_review".into(),
            confirm_scene(
                "Export review",
                lines,
                "export:review_cancel",
                "export:confirm",
                "Continue with export",
                true,
            ),
        );
    } else if let Some(r) = s.resources.front() {
        out.insert(
            "modal.download".into(),
            confirm_scene(
                &format!("Download {}?", r.name),
                &[format!(
                    "From {}. The file is checked before it becomes available to import.",
                    r.host
                )],
                &format!("resource:dismiss:{}", r.id),
                &format!("resource:import:{}", r.id),
                "Download and validate",
                s.busy.is_none(),
            ),
        );
    }
    out
}

fn confirm_scene(
    title: &str,
    lines: &[String],
    cancel: &str,
    approve: &str,
    action: &str,
    enabled: bool,
) -> Value {
    let copy = lines
        .iter()
        .take(512)
        .enumerate()
        .map(|(i, line)| {
            node(
                &format!("confirm.line:{i}"),
                "text",
                json!({"text":line, "wrap":true}),
            )
        })
        .collect::<Vec<_>>();
    column(
        "confirm",
        vec![
            text("confirm.title", title, "title"),
            node(
                "confirm.copy",
                "scroll",
                json!({"axis":"vertical", "max_height":380, "children":[column("confirm.lines",copy,6.0)]}),
            ),
            row(
                "confirm.actions",
                vec![
                    button(cancel, "Cancel", None, "default", true),
                    button(approve, action, None, "primary", enabled),
                ],
                6.0,
            ),
        ],
        10.0,
    )
}

/// All scenes for the slot instances, keyed by instance id.
pub fn all(s: &State) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    out.insert("main".into(), main_view(s));
    out.insert("summary".into(), summary_panel(s));
    out.insert("project".into(), project_panel(s));
    out.extend(modals(s));
    out
}

/// Timeline duration used by summary and export (kept here for scene consumers).
pub fn timeline_length(s: &State) -> f64 {
    timeline_viewport_duration(s.project())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &Value, out: &mut Vec<String>) {
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            out.push(id.to_string());
        }
        for key in ["children"] {
            if let Some(children) = v.get(key).and_then(Value::as_array) {
                for c in children {
                    ids(c, out);
                }
            }
        }
    }

    #[test]
    fn scene_node_ids_are_unique_within_each_instance() {
        let s = State::new();
        for (instance, scene) in all(&s) {
            let mut found = vec![];
            ids(&scene, &mut found);
            let mut sorted = found.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(found.len(), sorted.len(), "duplicate ids in {instance}");
        }
    }

    #[test]
    fn empty_project_shows_placeholder_and_no_lanes() {
        let s = State::new();
        let scene = main_view(&s).to_string();
        assert!(scene.contains("Drop media into the media pool to begin"));
        assert!(!scene.contains("lane.canvas:"));
    }

    #[test]
    fn modal_scenes_appear_only_when_their_flag_is_set() {
        let mut s = State::new();
        assert!(!modals(&s).contains_key("modal.settings"));
        s.dialogs.settings = true;
        assert!(modals(&s).contains_key("modal.settings"));
    }
}
