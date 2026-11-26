#!/usr/bin/env python3
"""Regenerate the protocol v2 fixtures (valid/ and invalid/) with the Python SDK builders.

  python3 schema/ui-v2/examples/generate.py

Each valid fixture is named <direction>__<message>.json and must validate against the matching
schema (to_host.schema.json or to_plugin.schema.json, or manifest-ui.schema.json for manifest_ui).
Invalid fixtures must fail validation; sdk/python/tests/test_schemas.py checks both.
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "sdk" / "python"))

import pigeon_plugin as p  # noqa: E402
from pigeon_plugin import draw  # noqa: E402
from pigeon_plugin.scene import MenuItem  # noqa: E402

V = ROOT / "schema/ui-v2/examples/valid"
I = ROOT / "schema/ui-v2/examples/invalid"


def write(folder, name, obj):
    folder.mkdir(parents=True, exist_ok=True)
    with open(folder / name, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=2, ensure_ascii=False)
        f.write("\n")


def env(t, **kw):
    out = {"v": 2, "t": t}
    out.update(kw)
    return out


def all_kinds_scene():
    """One scene that uses every node kind (the reference for scene.schema.json)."""
    root = p.column("root").gap(8).padding(12)
    root.child(p.text("title", "Reelio", "heading"))
    root.child(p.row("buttons").gap(6).children_from([
        p.button("go", "Open media", icon="folder-open", variant="primary").on("click"),
        p.icon_button("refresh", "arrows-clockwise", "Refresh"),
        p.badge("count", "3"),
        p.pill("tag", "beta"),
    ]))
    root.child(p.icon("ic", "film"))
    root.child(p.image("thumb", "thumb:42", "cover"))
    root.child(p.checkbox("cb", "Auto-save", True))
    root.child(p.switch("sw", "Dark", False))
    root.child(p.radio_group("rg", [("a", "A"), ("b", "B")], "a"))
    root.child(p.segmented("seg", [("x", "X"), ("y", "Y")], "x"))
    root.child(p.text_input("name", "Clip", rev=2).placeholder("Name").commit("submit"))
    root.child(p.number_input("n", 12.5, rev=1))
    root.child(p.slider("vol", 0.4, 0, 1, rev=3))
    root.child(p.range_slider("r", 1, 4, 0, 10, rev=1))
    root.child(p.select("sel", [("mp4", "MP4"), ("mkv", "MKV")], "mp4"))
    root.child(p.color_picker("col", "#3b82f6"))
    root.child(p.progress("bar", 0.5))
    root.child(p.progress("busy", None))
    root.child(p.spinner("spin"))
    root.child(p.menu("m", [
        MenuItem.item("open", "Open").shortcut("Cmd+O"),
        MenuItem.separator(),
        MenuItem.submenu("more", "More", [MenuItem.item("x", "X").checked(True)]),
    ]))
    root.child(p.context_menu("cm", [MenuItem.item("rename", "Rename")]))
    root.child(p.Node("tip", "tooltip", text="Hint"))
    root.child(p.table("tbl", [{"id": "name", "title": "Name", "width": 200}], 2))
    root.child(p.tree("tree", [{"id": "root", "title": "Root", "children": True}]))
    root.child(p.list_node("rows", "rows", 1000, row_height=24))
    root.child(p.rich_text("rt", [{"text": "Read "}, {"text": "docs", "style": "link"}]))
    root.child(p.code("code", "print(1)", "python"))
    canvas = (p.canvas("tl", p.Size.fill(1), p.Size.px(120))
              .interactive(pointer=True, wheel=True, keys=True, drop=True)
              .cursor("crosshair")
              .draw(p.DrawList("bg_input").add(draw.rect(0, 0, 100, 20, fill="accent", radius=4),
                                                draw.text(4, 4, "clip", size=11)), 1))
    root.child(canvas)
    root.child(p.split("sp", "horizontal", [0.3, 0.7]).children_from([p.panel("left"), p.panel("right")]))
    root.child(p.scroll("sc", "vertical").children_from([p.spacer("gap"), p.divider("div")]))
    root.child(p.tabs("tabs", [("a", "A"), ("b", "B")], "a").children_from([p.text("ta", "Tab A"), p.text("tb", "Tab B")]))
    root.child(p.collapsible("col2", "Advanced", True).child(p.grid("g", columns=3).child(p.text("g1", "1"))))
    root.child(p.grid("g2", min_cell=120).child(p.text("g2a", "x")))
    root.child(p.wrap("wr").child(p.spinner("s2")))
    root.child(p.stack("st").child(p.panel("pn")))
    return root


def main():
    write(V, "to_host__ui_set_all_kinds.json",
          env("ui.set", instance="reelio/main", scene=all_kinds_scene().to_dict()))
    write(V, "to_host__ready.json", env("ready", id="1", plugin={"id": "hello-ui", "version": "1.0.0"}, protocol=2, contributions=[
        p.Contribution.sidebar_section("sec", "Reelio", "film", 10),
        p.Contribution.sidebar_entry("nav", "Reelio", "film", "main"),
        {"slot": "toolbar.item", "id": "tb", "group": "ai", "order": 2},
        {"slot": "workspace.view", "id": "main", "title": "Reelio", "icon": "film", "toolbar": True},
        {"slot": "inspector.panel", "id": "insp", "title": "Info", "icon": "info", "order": 1,
         "applies_to": {"kinds": ["video"], "min": 1, "max": None, "ext": []}},
        p.Contribution.preferences_page("prefs", "Reelio", "gear", 3),
        p.Contribution.modal("export", "Export", "large", True),
        p.Contribution.statusbar_item("sb", "right", 1),
        {"slot": "asset.action", "id": "open", "title": "Open in Reelio", "icon": "film",
         "applies_to": {"kinds": ["video", "audio"], "min": 1, "max": 1, "ext": ["mkv"]},
         "selection": "one", "group": "open", "shortcut": "Cmd+Shift+R"},
        {"slot": "library.source", "id": "src", "title": "Cloud", "icon": "cloud",
         "root": {"id": "root", "title": "Cloud"}, "capabilities": ["browse", "search", "upload", "download"]},
        {"slot": "search.provider", "id": "sem", "title": "Semantic", "icon": "sparkle", "mode_id": "semantic"},
        {"slot": "drop.target", "id": "dz", "accepts": ["files", "assets"], "where": "workspace"},
        p.Contribution.command("cmd", "Show hello", "Cmd+Alt+H"),
    ]))
    write(V, "to_host__contribute.json", env("contribute", contributions=[
        p.Contribution.sidebar_entry("nav2", "Two", "film", "main")]))
    write(V, "to_host__withdraw.json", env("withdraw", ids=["nav2"]))
    write(V, "to_host__ui_patch.json", env("ui.patch", instance="hello-ui/main", ops=[
        {"op": "set", "path": "title/text", "value": "Hello"},
        {"op": "insert", "path": "buttons", "index": 0, "node": {"id": "new", "kind": "button", "label": "New"}},
        {"op": "remove", "path": "buttons/go"},
        {"op": "move", "path": "buttons/new", "index": 2}]))
    write(V, "to_host__ui_focus.json", env("ui.focus", instance="hello-ui/main", node="name"))
    write(V, "to_host__ui_scroll_to.json", env("ui.scroll_to", instance="hello-ui/main", node="rows", align="center"))
    write(V, "to_host__ui_beep.json", env("ui.beep"))
    ops = [
        draw.rect(0, 0, 640, 360, fill="#1b1d22", stroke="#2a2d34", stroke_w=1, radius=6),
        draw.line(0, 180, 640, 180, "fg_weak", 1),
        draw.polyline([[0, 0], [10, 5], [20, 0]], "accent", 2),
        draw.polygon([[0, 0], [10, 0], [5, 9]], fill="danger"),
        draw.path([["M", 0, 0], ["L", 10, 10], ["Q", 20, 0, 30, 10], ["C", 1, 2, 3, 4, 5, 6], ["Z"]], fill="warning"),
        draw.circle(50, 50, 10, fill="accent", stroke="fg", stroke_w=2),
        draw.ellipse(90, 50, 20, 10, stroke="fg"),
        draw.arc(120, 50, 20, 0, 3.14, stroke="accent", stroke_w=3),
        draw.text(10, 10, "00:12:03", size=12, weight=600, color="fg", align="left", baseline="top", max_w=200, font="mono"),
        draw.image("thumb:42", {"x": 0, "y": 0, "w": 64, "h": 36}, src={"x": 0, "y": 0, "w": 640, "h": 360},
                   tint="#ffffff", opacity=0.8),
        draw.clip(0, 0, 100, 100), draw.unclip(), draw.translate(5, 5), draw.scale(2, 2), draw.save(),
        draw.shadow(0, 2, 6, "#00000066"), draw.rect(0, 0, 10, 10, fill="accent"), draw.restore(),
        draw.gradient(0, 0, 0, 100, [[0, "#ffffff"], [1, "#000000"]]),
    ]
    write(V, "to_host__canvas_draw.json", env("canvas.draw", instance="hello-ui/main", node="tl", frame=13, ops=ops,
                                              region={"x": 0, "y": 0, "w": 640, "h": 180}))
    write(V, "to_host__list_rows.json", env("list.rows", instance="hello-ui/main", node="rows", start=10,
                                             rows=[{"id": "row-10", "kind": "row", "children": [
                                                 {"id": "row-10-t", "kind": "text", "text": "Clip 10"}]}]))
    write(V, "to_host__resource_put.json", env("resource.put", rid="thumb:42", kind="rgba", file="/tmp/s/thumb-42-0.rgba",
                                                w=640, h=360, stride=2560, seq=7, premultiplied=False))
    write(V, "to_host__resource_put_png.json", env("resource.put", rid="icon:logo", kind="png", file="/tmp/s/logo.png", w=64, h=64, seq=1))
    write(V, "to_host__resource_drop.json", env("resource.drop", rid="thumb:42"))
    write(V, "to_host__req_library_query.json", env("req", id="7", method="library.query",
                                                     params={"filter": {"kind": ["video"]}, "sort": {"key": "taken_at", "dir": "desc"}, "limit": 50}))
    write(V, "to_host__req_progress_start.json", env("req", id="8", method="progress.start", params={"title": "Exporting", "cancellable": True}))
    write(V, "to_host__res_ok.json", env("res", id="r1", result={"items": [], "total": 0}))
    write(V, "to_host__res_error.json", env("res", id="r2", error={"code": "permission_denied", "message": "plugin lacks library.read"}))
    write(V, "to_host__pong.json", env("pong", id="p1"))
    write(V, "to_host__source_changed.json", env("source.changed", source="cloud", kind="invalidate", node="album-1"))
    write(V, "to_host__search_status.json", env("search.status", mode_id="semantic", status={"state": "indexing", "done": 120, "total": 900}))

    write(V, "to_plugin__hello.json", env("hello", host={"name": "Pigeon", "version": "0.4.1"}, protocol={"min": 2, "max": 2},
          theme={"mode": "dark", "scale": 2.0, "font": {"ui": "Inter", "mono": "JetBrains Mono"},
                 "colors": {"bg": "#1b1d22", "accent": "#3b82f6", "fg": "#e6e8ee"}, "radius": 4, "spacing": 4},
          locale="en", scale=2.0, data_dir="/Users/me/.pigeon/plugins/hello-ui/.data", session_dir="/tmp/pigeon-session-1",
          features=["scene.patch", "canvas", "list.window", "resources.file", "source", "search"]))
    write(V, "to_plugin__ping.json", env("ping", id="p1"))
    mods = {"shift": False, "ctrl": False, "alt": False, "meta": False}
    write(V, "to_plugin__ui_event_click.json", env("ui.event", instance="hello-ui/main", node="go", type="click"))
    write(V, "to_plugin__ui_event_change.json", env("ui.event", instance="hello-ui/main", node="name", type="change", value="Clip 2", rev=2))
    write(V, "to_plugin__ui_event_submit.json", env("ui.event", instance="hello-ui/main", node="name", type="submit", value="Clip 2", rev=3))
    write(V, "to_plugin__ui_event_pointer.json", env("ui.event", instance="hello-ui/main", node="tl", type="pointer", phase="down",
                                                      x=120.5, y=30, button=0, buttons=1, mods=mods, clicks=1, pointer_id=1, coalesced=1))
    write(V, "to_plugin__ui_event_wheel.json", env("ui.event", instance="hello-ui/main", node="tl", type="wheel", dx=0, dy=-3,
                                                    x=10, y=20, mods={"ctrl": True}, coalesced=4))
    write(V, "to_plugin__ui_event_key.json", env("ui.event", instance="hello-ui/main", node="tl", type="key", phase="down",
                                                  key="ArrowLeft", code="ArrowLeft", text="", mods={}, repeat=False))
    write(V, "to_plugin__ui_event_list_window.json", env("ui.event", instance="hello-ui/main", node="rows", type="list.window",
                                                          first=0, last=24, overscan=8))
    write(V, "to_plugin__ui_event_drop.json", env("ui.event", instance="hello-ui/main", node="tl", type="drop",
                                                   paths=["/Users/me/a.mp4"], assets=[], x=3, y=4))
    write(V, "to_plugin__resource_ack.json", env("resource.ack", rid="thumb:42", seq=7))
    write(V, "to_plugin__res.json", env("res", id="7", result={"items": [{"id": "a", "name": "a.mp4", "kind": "video"}], "total": 1}))
    write(V, "to_plugin__req_source_list.json", env("req", id="s1", method="source.list",
                                                     params={"source": "cloud", "node": "album-1", "cursor": None, "limit": 200,
                                                             "sort": {"key": "taken_at", "dir": "desc"}, "filter": {}}))
    write(V, "to_plugin__visibility.json", env("visibility", instance="hello-ui/main", visible=True))
    write(V, "to_plugin__focus.json", env("focus", has=True))
    write(V, "to_plugin__resize.json", env("resize", instance="hello-ui/main", w=1280, h=720, scale=2.0))
    write(V, "to_plugin__theme_changed.json", env("theme.changed", theme={"mode": "light", "scale": 1.0, "colors": {"bg": "#ffffff"}}))
    write(V, "to_plugin__locale_changed.json", env("locale.changed", locale="de"))
    write(V, "to_plugin__selection_changed.json", env("selection.changed", ids=["a", "b"], kinds=["video"], paths=["/x/a.mp4"]))
    write(V, "to_plugin__cancel.json", env("cancel", job="job-1"))
    write(V, "to_plugin__command.json", env("command", command="show-hello", args={}, context={"ids": ["a"]}))
    write(V, "to_plugin__drop.json", env("drop", target="dz", paths=["/x/a.mp4"], assets=[]))
    write(V, "to_plugin__error.json", env("error", code="canvas_too_large", message="canvas has 21000 ops (max 20000)", ref="tl"))
    write(V, "to_plugin__shutdown.json", env("shutdown"))
    write(V, "manifest_ui.json", {"protocol": 2, "activation": "visible", "contributions": [
        {"slot": "sidebar.entry", "id": "nav", "title": "Hello UI", "icon": "sparkle", "opens": "main"},
        {"slot": "workspace.view", "id": "main", "title": "Hello UI", "icon": "sparkle"}],
        "permissions": ["selection", "dialogs"]})

    # invalid: each must fail validation
    write(I, "to_host__ready_missing_plugin.json", env("ready", protocol=2, contributions=[]))
    write(I, "to_host__ui_set_bad_kind.json", env("ui.set", instance="a/b", scene={"id": "root", "kind": "carousel"}))
    write(I, "to_host__ui_set_bad_size.json", env("ui.set", instance="a/b", scene={"id": "root", "kind": "column", "width": {"px": "wide"}}))
    write(I, "to_host__ui_set_split_one_child.json", env("ui.set", instance="a/b", scene={"id": "root", "kind": "split", "direction": "horizontal", "sizes": [1], "children": [{"id": "x", "kind": "panel"}]}))
    write(I, "to_host__canvas_bad_op.json", env("canvas.draw", instance="a/b", node="c", frame=1, ops=[{"op": "rect", "x": 1}]))
    write(I, "to_host__canvas_bad_path.json", env("canvas.draw", instance="a/b", node="c", frame=1, ops=[{"op": "path", "cmds": [["M", 1]]}]))
    write(I, "to_host__req_bad_method.json", env("req", id="1", method="Library Query", params={}))
    write(I, "to_host__res_both.json", env("res", id="1", result={}, error={"code": "x", "message": "y"}))
    write(I, "to_host__wrong_version.json", {"v": 1, "t": "pong"})
    write(I, "to_host__ui_set_bad_color.json", env("ui.set", instance="a/b", scene={"id": "root", "kind": "text", "text": "x", "fg": "#12345"}))
    write(I, "to_host__ui_set_text_input_no_rev.json", env("ui.set", instance="a/b", scene={"id": "root", "kind": "text_input", "value": "x"}))
    write(I, "to_host__contribution_bad_slot.json", env("contribute", contributions=[{"slot": "sidebar.menu", "id": "x"}]))
    write(I, "to_host__asset_action_no_selection.json", env("contribute", contributions=[
        {"slot": "asset.action", "id": "x", "title": "X", "icon": "film", "applies_to": {"kinds": ["video"]}}]))
    write(I, "to_plugin__hello_no_theme.json", env("hello", host={"name": "Pigeon", "version": "0.4.1"}, protocol={"min": 2, "max": 2},
                                                    locale="en", scale=2.0, session_dir="/tmp/s", features=[]))
    write(I, "to_plugin__ui_event_change_no_rev.json", env("ui.event", instance="a/b", node="n", type="change", value="x"))
    write(I, "to_plugin__ui_event_pointer_bad_phase.json", env("ui.event", instance="a/b", node="n", type="pointer", phase="hover", x=1, y=2))
    write(I, "to_plugin__res_neither.json", env("res", id="1"))
    write(I, "to_plugin__instance_no_slash.json", env("ui.event", instance="nojoin", node="n", type="click"))
    print("fixtures written")


if __name__ == "__main__":
    main()
