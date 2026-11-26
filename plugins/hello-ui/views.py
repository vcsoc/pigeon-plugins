"""Scene and canvas builders for Hello UI. Pure functions of the State: no I/O, no host calls."""
import math

import pigeon_plugin as p
from pigeon_plugin import draw
from pigeon_plugin.scene import MenuItem

LANE_H = 60
LIST_COUNT = 10_000
FRAME_W, FRAME_H = 96, 54


def _tabs_node(state):
    return p.tabs("tabs", [("nodes", "Nodes"), ("canvas", "Canvas"), ("list", "List"),
                           ("services", "Services"), ("media", "Media")], state.tab)


def main(s):
    """The workspace view: a header and the tabs (each tab's content has the tab's id)."""
    header = p.row("header").gap(8).children_from([
        p.text("title", "Hello UI", "heading"),
        p.badge("count", f"{s.clicks} clicks"),
        p.button("go", "Click me", variant="primary").on("click"),
    ])
    return p.column("root").gap(8).padding(12).children_from([header, _tabs_with_children(s)])


def _tabs_with_children(s):
    node = _tabs_node(s)
    node.children = [nodes_tab(s), canvas_tab(s), list_tab(s), services_tab(s), media_tab(s)]
    return node


def nodes_tab(s):
    """Every scene node kind, in one place (the reference for scene.schema.json)."""
    kinds = p.column("nodes").gap(8).children_from([
        p.text("t-heading", "Heading", "heading"),
        p.text("t-title", "Title", "title"),
        p.text("t-body", "Body text, wrapping is off", "body"),
        p.text("t-caption", "Caption", "caption"),
        p.text("t-code", "print('code')", "code"),
        p.text("t-mono", "mono 00:12:03", "mono"),
        p.row("r-icons").gap(8).children_from([
            p.icon("ic", "film"), p.icon_button("ib", "arrows-clockwise", "Refresh"),
            p.badge("bd", "3"), p.pill("pl", "beta")]),
        p.row("r-buttons").gap(8).children_from([
            p.button("b-default", "Default"), p.button("b-primary", "Primary", variant="primary"),
            p.button("b-danger", "Danger", variant="danger"), p.button("b-ghost", "Ghost", variant="ghost"),
            p.button("b-link", "Link", variant="link"), p.button("b-toggled", "Toggled").toggled(True)]),
        p.row("r-inputs").gap(8).children_from([
            p.text_input("name", s.name, rev=s.name_rev).placeholder("Clip name").commit("submit"),
            p.button("reset-name", "Reset name")]),
        p.row("r-numbers").gap(8).children_from([
            p.number_input("number", s.number, rev=1).set("min", 0).set("max", 100).set("unit", "s"),
            p.slider("volume", s.volume, 0, 1, rev=1),
            p.range_slider("range", s.range[0], s.range[1], 0, 10, rev=1)]),
        p.row("r-choice").gap(8).children_from([
            p.select("choice", [("mp4", "MP4"), ("mkv", "MKV")], s.choice),
            p.radio_group("radio", [("a", "A"), ("b", "B")], s.radio),
            p.segmented("segment", [("x", "X"), ("y", "Y")], s.segment)]),
        p.row("r-toggles").gap(8).children_from([
            p.checkbox("cb", "Checkbox", True), p.switch("toggle", "Notifications", s.notify),
            p.color_picker("color", s.color)]),
        p.row("r-progress").gap(8).children_from([
            p.progress("bar", s.volume), p.progress("busy", None), p.spinner("spin")]),
        p.image("frame", "hello:frame", "contain").set("height", p.Size.px(FRAME_H * 2)).set("width", p.Size.px(FRAME_W * 2)),
        p.menu("menu", [MenuItem.item("open", "Open").shortcut("Cmd+O").icon("folder-open"),
                        MenuItem.separator(),
                        MenuItem.submenu("more", "More", [MenuItem.item("x", "X").checked(True)])]),
        p.context_menu("ctx", [MenuItem.item("rename", "Rename")]).set("hint", "right-click the label"),
        p.Node("tip", "tooltip", text="Tooltip text"),
        p.table("tbl", [{"id": "name", "title": "Name", "width": 200}, {"id": "kind", "title": "Kind", "width": 90}], 2),
        p.tree("tree", [{"id": "root", "title": "Root", "children": True, "expanded": True}]),
        p.rich_text("rich", [{"text": "Rich "}, {"text": "bold", "style": "heading"}, {"text": " text"}]),
        p.code("code", "def hello():\n    return 'world'", "python"),
        p.split("split", "horizontal", [0.4, 0.6]).children_from([
            p.panel("split-left").children_from([p.text("sl", "Left panel")]),
            p.panel("split-right").children_from([p.text("sr", "Right panel")]),
        ]).set("height", p.Size.px(60)),
        p.collapsible("more", "Collapsible", True).children_from([
            p.grid("grid", columns=3).children_from([p.text(f"g{i}", f"cell {i}") for i in range(3)]),
        ]),
        p.wrap("wrap").children_from([p.badge(f"w{i}", str(i)) for i in range(4)]),
        p.stack("stack").children_from([p.panel("back"), p.text("front", "Overlay")]),
        p.scroll("scroll", "vertical").children_from([p.text("sc", "Scrolled content")]).set("height", p.Size.px(40)),
        p.spacer("sp"),
        p.divider("div"),
    ])
    return kinds


def canvas_tab(s):
    return p.column("canvas").gap(8).children_from([
        p.text("canvas-help", "Drag clips, wheel to zoom, arrows move the playhead, drop files on the lane."),
        p.canvas("timeline", p.Size.fill(1), p.Size.px(3 * LANE_H)).interactive(
            pointer=True, wheel=True, keys=True, drop=True).cursor("crosshair").draw(timeline_draw(s), s.canvas_frame + 1),
    ])


def list_tab(s):
    rows = [list_row(i) for i in range(20)]
    lst = p.list_node("rows", "rows", LIST_COUNT, row_height=28).set("window", {"start": 0, "rows": rows})
    return p.column("list").gap(8).children_from([
        p.text("list-help", f"{LIST_COUNT} rows, virtualized: the host asks for the visible window."),
        lst.set("height", p.Size.px(300)),
    ])


def services_tab(s):
    return p.column("services").gap(8).children_from([
        p.row("svc-1").gap(8).children_from([
            p.button("toast", "Toast"), p.button("progress", "Progress job"), p.button("dialog", "Open file dialog"),
        ]),
        p.row("svc-2").gap(8).children_from([
            p.button("secret", "Store token"), p.button("settings", f"Settings clicks: {s.clicks}"),
            p.button("open-url", "Open URL"),
        ]),
        p.row("svc-3").gap(8).children_from([
            p.button("clipboard", "Copy greeting"), p.button("modal", "About (modal)"),
            p.button("library", "Query library"), p.button("shell-denied", "Denied call"),
        ]),
        p.text("svc-last", f"Last: {s.log[-1] if s.log else 'nothing yet'}", "caption"),
    ])


def media_tab(s):
    return p.column("media").gap(8).children_from([
        p.text("media-help", "Six frames are published to one resource id; the ring waits for acks."),
        p.row("media-buttons").gap(8).children_from([p.button("thumb-gen", "Publish frames", variant="primary")]),
        p.canvas("wave", p.Size.fill(1), p.Size.px(80)).draw(wave_draw(), 1),
    ])


def toolbar(s):
    return p.row("toolbar").gap(6).children_from([
        p.button("toolbar-hello", f"Hello ({s.clicks})", icon="sparkle").on("click"),
    ])


def section(s):
    return p.column("section").gap(4).padding(6).children_from([
        p.badge("section-count", str(s.clicks)),
        p.button("section-open", "Open Hello UI", icon="sparkle"),
    ])


def status(s):
    return p.row("status").gap(8).children_from([
        p.text("status-text", f"Hello UI: {s.clicks} clicks, {len(s.selection)} selected", "caption"),
    ])


def info(s):
    return p.column("info").gap(6).padding(10).children_from([
        p.text("info-title", "Selected asset", "title"),
        p.text("info-body", s.last_info, "body").wrap_text(True),
        p.badge("info-count", f"{len(s.selection)} selected"),
    ])


def prefs(s):
    return p.column("prefs").gap(10).padding(12).children_from([
        p.text("prefs-title", "Hello UI preferences", "title"),
        p.text_input("greet", s.greeting, rev=s.name_rev).placeholder("Greeting"),
        p.switch("notify", "Show notifications", s.notify),
    ])


def about(s):
    return p.column("about").gap(8).padding(12).children_from([
        p.text("about-title", "Hello UI 1.0.0", "heading"),
        p.text("about-body", "Reference plugin for the Pigeon plugin SDK."),
        p.button("close", "Close", variant="primary").on("click"),
    ])


def list_row(i):
    return p.row(f"row-{i}").gap(8).padding(4).children_from([
        p.text(f"row-{i}-name", f"Row {i + 1:05d}"),
        p.badge(f"row-{i}-badge", "even" if i % 2 == 0 else "odd"),
    ])


def clip_rect(s, clip):
    x = clip["start"] * s.zoom
    w = max(4.0, clip["len"] * s.zoom)
    y = clip["lane"] * LANE_H + 8
    return x, y, w, LANE_H - 16


def hit_clip(s, x, y):
    for clip in reversed(s.clips):
        cx, cy, cw, ch = clip_rect(s, clip)
        if cx <= x <= cx + cw and cy <= y <= cy + ch:
            return clip
    return None


def timeline_draw(s):
    """The timeline: lanes, a ruler, clips, the playhead. Depends only on State."""
    width = 1000
    dl = p.DrawList("bg_input")
    for lane in range(3):
        dl.add(draw.rect(0, lane * LANE_H, width, LANE_H, fill="bg_panel" if lane % 2 == 0 else "bg"))
    for sec in range(0, int(width / s.zoom) + 2):
        x = sec * s.zoom
        dl.add(draw.line(x, 0, x, 3 * LANE_H, stroke="border", stroke_w=1))
        dl.add(draw.text(x + 3, 2, f"{sec}s", size=10, color="fg_weak", font="mono"))
    for clip in s.clips:
        x, y, w, h = clip_rect(s, clip)
        selected = s.selected == clip["id"]
        dl.add(draw.rect(x, y, w, h, fill="accent" if selected else "selection", stroke="focus_ring" if selected else None,
                         stroke_w=2 if selected else None, radius=4))
        dl.add(draw.text(x + 6, y + 6, clip["id"], size=11, color="accent_fg"))
    px = s.playhead * s.zoom
    dl.add(draw.line(px, 0, px, 3 * LANE_H, stroke="danger", stroke_w=2))
    dl.add(draw.circle(px, 0, 4, fill="danger"))
    return dl


def wave_draw():
    dl = p.DrawList("bg_input")
    pts = [[x * 4.0, 40 + 28 * math.sin(x / 6.0)] for x in range(0, 250)]
    dl.add(draw.polyline(pts, "accent", 2))
    dl.add(draw.path([["M", 0, 70], ["Q", 200, 10, 400, 70], ["L", 400, 80], ["Z"]], fill="selection"))
    return dl


def gradient_pixels(w, h, n):
    """A deterministic RGBA frame; n changes the hue."""
    out = bytearray(w * h * 4)
    for y in range(h):
        for x in range(w):
            i = (y * w + x) * 4
            out[i] = (x * 255 // max(1, w - 1) + n * 30) & 255
            out[i + 1] = (y * 255 // max(1, h - 1)) & 255
            out[i + 2] = (128 + n * 20) & 255
            out[i + 3] = 255
    return bytes(out)


def source_item(i):
    kinds = ["image", "video", "audio", "document"]
    kind = kinds[i % 4]
    item = {"id": f"hello-{i}", "name": f"Demo {kind} {i:03d}", "kind": kind, "size": 1000 + i * 17,
            "tags": ["demo"], "favorite": i % 7 == 0}
    if kind in ("image", "video"):
        item.update({"w": 1920, "h": 1080})
    return item
