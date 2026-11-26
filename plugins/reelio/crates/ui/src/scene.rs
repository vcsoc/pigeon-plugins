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
    children.push(panel("viewer.frame.panel", vec![frame]));
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
        lanes.push(row(
            "lane.cover",
            vec![
                node(
                    "cover.thumb",
                    "image",
                    json!({"inline_jpeg": art.jpeg, "width": 56, "height": 56, "fit": "cover"}),
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
                button("cover.edit", "Edit", None, "ghost", s.busy.is_none()),
                button("cover.remove", "Remove", None, "ghost", s.busy.is_none()),
            ],
            8.0,
        ));
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
    let p = s.project();
    let cover = match &p.artwork {
        Some(_) => node("art.preview", "image", json!({"resource": "cover:project", "fit": "contain", "height": {"px": 260}})),
        None => text("art.empty", "No cover applied. Find artwork or drop a JPEG, PNG, WebP or AVIF image on the inspector.", "body"),
    };
    let presets: Vec<Value> = crate::crop::CROP_PRESETS
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| button(&format!("art.preset:{i}"), name, None, "ghost", true))
        .collect();
    column(
        "artwork",
        vec![
            row(
                "art.search",
                vec![
                    input("art.query", "", "Movie title and year", "submit"),
                    button(
                        "art.search_go",
                        "Search posters",
                        Some("search"),
                        "primary",
                        s.busy.is_none(),
                    ),
                    node(
                        "art.orient.landscape",
                        "icon_button",
                        json!({"icon": "landscape", "tooltip": "Landscape", "on": ["click"]}),
                    ),
                    node(
                        "art.orient.portrait",
                        "icon_button",
                        json!({"icon": "portrait", "tooltip": "Portrait", "on": ["click"]}),
                    ),
                ],
                6.0,
            ),
            row(
                "art.body",
                vec![cover, column("art.controls", presets, 4.0)],
                12.0,
            ),
            button(
                "art.apply",
                "Apply cover to project",
                Some("check"),
                "primary",
                s.busy.is_none(),
            ),
        ],
        8.0,
    )
}

fn subtitle_studio(s: &State) -> Value {
    let empty = crate::studio::Studio::default();
    let st = s.studio.as_ref().unwrap_or(&empty);
    let doc = &st.doc;
    let rows: Vec<Value> = doc
        .cues
        .iter()
        .enumerate()
        .skip(st.window_start)
        .take(crate::studio::CUE_WINDOW)
        .map(|(i, c)| {
            let first = c.text.lines().next().unwrap_or("");
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
            b
        })
        .collect();
    let list = node(
        "sub:list",
        "list",
        json!({
            "count": doc.cues.len(), "row_height": 28, "key": "rows",
            "window": {"start": st.window_start, "rows": rows}
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
            json!({"value": c.text, "rev": 0, "multiline": true, "commit": "change", "label": "Subtitle text"}),
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
            tools,
            if notice.is_empty() {
                text("sub.notice", "", "caption")
            } else {
                text("sub.notice", &notice, "caption")
            },
            row(
                "sub.body",
                vec![
                    column("sub.left", vec![list], 4.0),
                    column(
                        "sub.right",
                        vec![
                            column("sub.fields", fields, 6.0),
                            column("sub.issues", issue_nodes, 2.0),
                        ],
                        6.0,
                    ),
                ],
                10.0,
            ),
        ],
        8.0,
    )
}

/// Lower workspace in the center column: timeline, poster & cover studio, or subtitle studio.
fn lower(s: &State) -> Value {
    let body = match s.section {
        Section::Artwork => artwork_studio(s),
        Section::Subtitles => subtitle_studio(s),
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
    let mut center = vec![viewer(s), lower(s)];
    let feedback = toasts(s);
    let mut top_children = vec![top_bar(s)];
    if let Some(busy) = &s.busy {
        top_children.push(text("busy", busy, "caption"));
    }
    top_children.extend(feedback);
    let mut body = vec![];
    if !s.layout.left_collapsed {
        body.push(media_pool(s));
    }
    body.push(node(
        "center",
        "column",
        json!({"fill": 1, "gap": 0, "children": std::mem::take(&mut center)}),
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
        Some(_) => node("summary.cover", "image", json!({"resource": "cover:project", "fit": "contain", "height": {"px": 140}, "on": ["double_click", "context"]})),
        None => node("summary.cover.empty", "button", json!({"label": "Select a cover", "variant": "ghost", "on": ["click"]})),
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
    if let Some(lines) = &s.dialogs.export_review {
        let mut children = vec![text("review.title", "Export review", "title")];
        children.extend(
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| text(&format!("review.line:{i}"), l, "body")),
        );
        children.push(row(
            "review.actions",
            vec![
                button("export:review_cancel", "Cancel", None, "secondary", true),
                button(
                    "export:confirm",
                    "Continue with export",
                    None,
                    "primary",
                    true,
                ),
            ],
            6.0,
        ));
        out.insert(
            "modal.export_review".into(),
            column("export_review", children, 10.0),
        );
    }
    if let Some(r) = s.resources.front() {
        out.insert(
            "modal.download".into(),
            column(
                "download",
                vec![
                    text("download.title", &format!("Download {}?", r.name), "title"),
                    text("download.host", &format!("From {}", r.host), "caption"),
                    row(
                        "download.actions",
                        vec![
                            button("resource:dismiss", "Cancel", None, "secondary", true),
                            button(
                                "resource:import",
                                "Use this",
                                None,
                                "primary",
                                s.busy.is_none(),
                            ),
                        ],
                        6.0,
                    ),
                ],
                10.0,
            ),
        );
    }
    out
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
