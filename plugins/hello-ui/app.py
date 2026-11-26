"""Hello UI: the SDK's reference plugin.

It draws into every slot of protocol section 4, uses every scene node kind, a custom canvas with
pointer, wheel and key handling, a virtualized list, file-ring resources, a library source, a
search provider, commands, and the host services. Everything is deterministic so that the
conformance suite can check it.

Layout of the code: `HelloApp.handle` is the only event entry point; `views` build scenes from
`State`; background work runs on worker threads through `ctx.spawn`.
"""
import json
import math
import os
import sys
import time

import pigeon_plugin as p
from pigeon_plugin.runtime import ServiceError

import views

HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN_ID = "hello-ui"
MAIN = f"{PLUGIN_ID}/main"
ABOUT = f"{PLUGIN_ID}/about"
INFO = f"{PLUGIN_ID}/info"
PREFS = f"{PLUGIN_ID}/prefs"
STATUS = f"{PLUGIN_ID}/status"
SECTION = f"{PLUGIN_ID}/hello-section"
TOOLBAR = f"{PLUGIN_ID}/tb"
TABS = ["nodes", "canvas", "list", "services", "media"]
LIST_COUNT = 10_000
SOURCE_ITEMS = 240


def manifest_contributions():
    with open(os.path.join(HERE, "plugin.json"), encoding="utf-8") as f:
        return json.load(f)["ui"]["contributions"]


class State:
    def __init__(self):
        self.tab = "nodes"
        self.clicks = 0
        self.name = "Clip 01"
        self.name_rev = 1
        self.notify = True
        self.greeting = "Hello"
        self.volume = 0.5
        self.range = (2.0, 7.0)
        self.number = 12.0
        self.choice = "mp4"
        self.radio = "a"
        self.segment = "x"
        self.color = "#3b82f6"
        self.zoom = 40.0          # px per second
        self.playhead = 1.5       # seconds
        self.clips = [{"id": "c1", "start": 0.0, "len": 3.0, "lane": 0},
                      {"id": "c2", "start": 3.5, "len": 2.5, "lane": 0},
                      {"id": "c3", "start": 1.0, "len": 4.0, "lane": 1}]
        self.drag = None          # (clip id, grab offset seconds)
        self.selected = None
        self.selection = []
        self.last_info = "Nothing described yet"
        self.frame = 0
        self.canvas_frame = 0
        self.frames_published = 0
        self.log = []


class HelloApp:
    def __init__(self):
        self.s = State()
        self.theme_mode = "dark"

    # ---------------------------------------------------------------- dispatch

    def handle(self, ctx, msg):
        t = msg.get("t")
        if t == "visibility":
            if msg.get("visible"):
                self.render_instance(ctx, msg["instance"])
        elif t == "ui.event":
            self.on_event(ctx, msg)
        elif t == "command":
            self.on_command(ctx, msg)
        elif t == "drop":
            n = len(msg.get("paths", [])) + len(msg.get("assets", []))
            ctx.toast("info", f"Dropped {n} item(s) on {msg.get('target')}")
            self.s.log.append(f"drop:{n}")
        elif t == "selection.changed":
            self.s.selection = list(msg.get("ids", []))
            self.render_all(ctx)
        elif t == "theme.changed":
            self.theme_mode = msg.get("theme", {}).get("mode", "dark")
            self.render_all(ctx)
        elif t == "req":
            self.on_provider(ctx, msg)
        elif t == "error":
            sys.stderr.write(f"host rejected a message: {msg.get('code')} {msg.get('message')}\n")
        elif t == "shutdown":
            return

    def render_all(self, ctx):
        for instance in (MAIN, INFO, PREFS, STATUS, SECTION, TOOLBAR):
            self.render_instance(ctx, instance)

    def render_instance(self, ctx, instance):
        if instance == MAIN:
            ctx.set_scene(MAIN, views.main(self.s))
        elif instance == INFO:
            ctx.set_scene(INFO, views.info(self.s))
        elif instance == PREFS:
            ctx.set_scene(PREFS, views.prefs(self.s))
        elif instance == STATUS:
            ctx.set_scene(STATUS, views.status(self.s))
        elif instance == SECTION:
            ctx.set_scene(SECTION, views.section(self.s))
        elif instance == TOOLBAR:
            ctx.set_scene(TOOLBAR, views.toolbar(self.s))
        elif instance.startswith(ABOUT):
            ctx.set_scene(instance, views.about(self.s))

    def publish_canvas(self, ctx):
        self.s.canvas_frame += 1
        ctx.canvas_draw(MAIN, "timeline", self.s.canvas_frame, views.timeline_draw(self.s))

    # ---------------------------------------------------------------- events

    def on_event(self, ctx, msg):
        instance, node, kind = msg.get("instance"), msg.get("node"), msg.get("type")
        if instance and instance.startswith(ABOUT):
            if node == "close" and kind == "click":
                ctx.call("window.close_modal", {"instance": instance})
            return
        if instance == PREFS:
            self.on_prefs(ctx, node, kind, msg)
            return
        if instance != MAIN:
            return
        if node == "tabs" and kind == "change":
            self.s.tab = msg.get("value", "nodes")
            self.render_instance(ctx, MAIN)
            return
        if node == "timeline":
            self.on_canvas(ctx, kind, msg)
            return
        if node == "rows" and kind == "list.window":
            self.send_rows(ctx, msg)
            return
        handler = getattr(self, "on_" + node.replace("-", "_"), None) if node else None
        if handler is not None:
            handler(ctx, kind, msg)
            return
        if node in ("go", "toolbar-hello") and kind == "click":
            self.s.clicks += 1
            self.render_all(ctx)

    # ---- controlled inputs (protocol 5.1): accept the host's edit and keep rev ----

    def on_name(self, ctx, kind, msg):
        if kind in ("change", "submit"):
            if msg["rev"] >= self.s.name_rev:
                self.s.name, self.s.name_rev = msg["value"], msg["rev"]
            self.render_instance(ctx, MAIN)

    def on_reset_name(self, ctx, kind, msg):
        if kind == "click":
            self.s.name_rev += 1
            self.s.name = "Clip 01"
            self.render_instance(ctx, MAIN)

    def on_volume(self, ctx, kind, msg):
        if kind == "change":
            self.s.volume = float(msg["value"])
            self.render_instance(ctx, MAIN)

    def on_range(self, ctx, kind, msg):
        if kind == "change":
            self.s.range = tuple(float(v) for v in msg["value"])
            self.render_instance(ctx, MAIN)

    def on_number(self, ctx, kind, msg):
        if kind == "change":
            self.s.number = float(msg["value"])
            self.render_instance(ctx, MAIN)

    def on_choice(self, ctx, kind, msg):
        if kind == "change":
            self.s.choice = msg["value"]
            self.render_instance(ctx, MAIN)

    def on_radio(self, ctx, kind, msg):
        if kind == "change":
            self.s.radio = msg["value"]
            self.render_instance(ctx, MAIN)

    def on_segment(self, ctx, kind, msg):
        if kind == "change":
            self.s.segment = msg["value"]
            self.render_instance(ctx, MAIN)

    def on_color(self, ctx, kind, msg):
        if kind == "change":
            self.s.color = msg["value"]
            self.render_instance(ctx, MAIN)

    def on_toggle(self, ctx, kind, msg):
        if kind == "change":
            self.s.notify = bool(msg["value"])
            self.render_instance(ctx, MAIN)

    # ---- services ----

    def on_toast(self, ctx, kind, msg):
        if kind == "click":
            ctx.toast("success", f"{self.s.greeting}, {self.s.name}")

    def on_progress(self, ctx, kind, msg):
        if kind != "click":
            return

        def work(worker):
            job = worker.progress_start("Hello progress", cancellable=True)
            for i in range(1, 5):
                worker.progress_update(job, i / 4.0, f"step {i} of 4")
                time.sleep(0.02)
            worker.progress_end(job, ok=True, message="done")
            worker.toast("info", "Progress finished")

        ctx.spawn(work)

    def on_dialog(self, ctx, kind, msg):
        if kind != "click":
            return

        def work(worker):
            try:
                paths = worker.open_file_dialog("Pick media", multiple=True)
                worker.toast("info", f"Picked {len(paths)} file(s)")
            except ServiceError as exc:
                worker.toast("error", str(exc))

        ctx.spawn(work)

    def on_secret(self, ctx, kind, msg):
        if kind != "click":
            return

        def work(worker):
            try:
                worker.call("secrets.set", {"key": "api-token", "value": "demo-token"})
                got = worker.call("secrets.get", {"key": "api-token"})
                worker.toast("success", "Token stored" if got.get("value") else "Token missing")
            except ServiceError as exc:
                worker.toast("error", f"{exc.code}: {exc.message}")

        ctx.spawn(work)

    def on_settings(self, ctx, kind, msg):
        if kind == "click":
            self.s.clicks += 1
            ctx.settings_set("clicks", self.s.clicks)
            self.render_all(ctx)

    def on_open_url(self, ctx, kind, msg):
        if kind == "click":
            ctx.open_url("https://example.com/hello")

    def on_clipboard(self, ctx, kind, msg):
        if kind == "click":
            ctx.call("clipboard.write", {"kind": "text", "text": f"{self.s.greeting} {self.s.name}"})
            ctx.toast("info", "Copied to clipboard")

    def on_modal(self, ctx, kind, msg):
        if kind == "click":
            ctx.open_modal("about", {"from": "hello-ui"})

    def on_library(self, ctx, kind, msg):
        if kind == "click":
            def work(worker):
                try:
                    res = worker.library_query({"kind": ["image"]}, limit=5)
                    worker.toast("info", f"{len(res.get('items', []))} library item(s)")
                except ServiceError as exc:
                    worker.toast("error", f"{exc.code}: {exc.message}")

            ctx.spawn(work)

    def on_shell_denied(self, ctx, kind, msg):
        if kind == "click":
            # Calls a method this plugin has no permission for to show the permission_denied path.
            try:
                ctx.call("drag.begin", {"paths": ["/tmp/none"]})
                ctx.toast("info", "drag started")
            except ServiceError as exc:
                ctx.toast("warning", f"{exc.code}: {exc.message}")

    # ---- media tab: resources ----

    def on_thumb_gen(self, ctx, kind, msg):
        if kind != "click":
            return

        def work(worker):
            res = worker.resources
            for n in range(6):
                pixels = views.gradient_pixels(96, 54, n)
                res.put_rgba("hello:frame", 96, 54, pixels)
                self.s.frames_published += 1
            worker.toast("success", f"Published {self.s.frames_published} frames")
            self.render_instance(worker, MAIN)

        ctx.spawn(work)

    # ---- canvas: pointer, wheel, keys (protocol 7) ----

    def on_canvas(self, ctx, kind, msg):
        s = self.s
        if kind == "pointer":
            phase = msg["phase"]
            x, y = msg["x"], msg["y"]
            if phase == "down":
                clip = views.hit_clip(s, x, y)
                s.selected = clip["id"] if clip else None
                s.drag = (clip["id"], x / s.zoom - clip["start"]) if clip else None
            elif phase == "move" and s.drag:
                cid, grab = s.drag
                clip = next(c for c in s.clips if c["id"] == cid)
                clip["start"] = max(0.0, x / s.zoom - grab)
                clip["lane"] = max(0, min(2, int(y // views.LANE_H)))
            elif phase in ("up", "cancel"):
                s.drag = None
            self.publish_canvas(ctx)
        elif kind == "wheel":
            factor = 1.0 + (-msg["dy"]) * 0.05 * msg.get("coalesced", 1)
            s.zoom = max(10.0, min(200.0, s.zoom * factor))
            self.publish_canvas(ctx)
        elif kind == "key" and msg.get("phase") == "down":
            step = {"ArrowLeft": -0.5, "ArrowRight": 0.5}.get(msg.get("key"))
            if step is not None:
                s.playhead = max(0.0, s.playhead + step)
                self.publish_canvas(ctx)
        elif kind == "drop":
            s.log.append(f"canvas-drop:{len(msg.get('paths', []))}")
            ctx.toast("info", f"Dropped {len(msg.get('paths', []))} file(s) on the timeline")

    # ---- list: virtualized window (protocol 5.2) ----

    def send_rows(self, ctx, msg):
        first, last = msg["first"], min(msg["last"], LIST_COUNT - 1)
        first = max(0, first - msg.get("overscan", 0))
        last = min(LIST_COUNT - 1, last + msg.get("overscan", 0))
        rows = [views.list_row(i) for i in range(first, last + 1)]
        ctx.list_rows(MAIN, "rows", first, rows)

    # ---- preferences ----

    def on_prefs(self, ctx, node, kind, msg):
        if node == "greet" and kind in ("change", "submit"):
            if msg["rev"] >= self.s.name_rev:
                self.s.greeting = msg["value"] or "Hello"
        elif node == "notify" and kind == "change":
            self.s.notify = bool(msg["value"])
            ctx.settings_set("notify", self.s.notify)
        self.render_instance(ctx, PREFS)

    # ---- commands and asset actions ----

    def on_command(self, ctx, msg):
        cmd = msg.get("command")
        if cmd == "say":
            ctx.toast("info", f"{self.s.greeting}, world")
        elif cmd == "describe":
            ids = msg.get("context", {}).get("ids", [])

            def work(worker):
                got = worker.call("library.get", {"ids": ids[:1]}) if ids else {"items": []}
                items = got.get("items", [])
                text = f"{items[0]['name']} ({items[0]['kind']})" if items else "no asset"
                self.s.last_info = text
                self.render_instance(worker, INFO)
                worker.toast("info", text)

            ctx.spawn(work)
        elif cmd == "count":
            n = len(msg.get("context", {}).get("ids", []))
            ctx.toast("info", f"{n} asset(s) selected")

    # ---- providers (sections 8.1 and 8.2) ----

    def on_provider(self, ctx, msg):
        rid, method, params = msg["id"], msg["method"], msg.get("params", {})
        if method == "source.describe":
            ctx.answer(rid, {
                "title": "Hello Demo", "icon": "cloud",
                "root": {"id": "root", "title": "Hello Demo", "icon": "cloud", "children": True},
                "capabilities": ["browse", "search"],
            })
        elif method == "source.children":
            node = params.get("node")
            if node == "root":
                nodes = [{"id": f"folder-{k}", "title": f"Folder {k + 1}", "icon": "folder", "children": False,
                          "count": SOURCE_ITEMS // 3} for k in range(3)]
            else:
                nodes = []
            ctx.answer(rid, {"nodes": nodes})
        elif method == "source.list":
            items = [views.source_item(i) for i in range(SOURCE_ITEMS)]
            ctx.answer(rid, {"items": items[:int(params.get("limit", 200))], "total": len(items)})
        elif method == "source.thumbnail":
            path = os.path.join(ctx.session_dir, f"thumb-{params.get('id')}.rgba")
            with open(path, "wb") as f:
                f.write(views.gradient_pixels(64, 36, len(str(params.get("id")))))
            ctx.answer(rid, {"file": path})
        elif method == "source.open":
            ctx.answer_error(rid, "unsupported", "Hello Demo is read-only")
        elif method == "search.query":
            q = str(params.get("query", "")).lower()
            hits = [{"ref": {"source": "demo-source", "id": f"hello-{i}"}, "score": 1.0 - (i % 10) / 10.0,
                     "snippet": views.source_item(i)["name"]}
                    for i in range(SOURCE_ITEMS) if q in views.source_item(i)["name"].lower()]
            ctx.answer(rid, {"results": hits[:50], "status": {"indexed": SOURCE_ITEMS}})
        else:
            ctx.answer_error(rid, "unknown_method", f"hello-ui does not provide {method}")


def run_plugin():
    spec = p.PluginSpec(PLUGIN_ID, "1.0.0", contributions=manifest_contributions())
    plugin = p.Plugin.stdio(spec)
    app = HelloApp()

    def guarded(ctx, msg):
        # One failing handler must not end the plugin: log it and keep serving the host.
        try:
            app.handle(ctx, msg)
        except Exception as exc:  # noqa: BLE001 - the loop must survive any handler bug
            sys.stderr.write(f"hello-ui: {msg.get('t')} handler failed: {exc!r}\n")

    plugin.run(guarded)
    return 0
