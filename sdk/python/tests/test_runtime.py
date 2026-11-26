"""pigeon_plugin runtime over real OS pipes: handshake, ping while stalled, coalescing, services,
provider requests, resources (ack-gated ring), oversized lines and lifecycle."""
import io
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import pigeon_plugin as p  # noqa: E402
from pigeon_plugin import runtime  # noqa: E402


class Host:
    """Plays the host: writes hello and events into a pipe, reads what the plugin wrote."""

    def __init__(self, session_dir):
        self.r, self.w = os.pipe()
        self.out = io.BytesIO()
        self.lock = threading.Lock()
        self.session = session_dir
        self.hello = {"v": 2, "t": "hello", "host": {"name": "Pigeon", "version": "0.4.1"},
                      "protocol": {"min": 2, "max": 2}, "theme": {"mode": "dark", "colors": {"accent": "#3b82f6"}},
                      "locale": "en", "scale": 2.0, "session_dir": session_dir, "data_dir": "/tmp/d",
                      "features": ["canvas", "scene.patch"]}
        self.send(self.hello)

    def send(self, msg):
        data = (json.dumps(msg) if isinstance(msg, dict) else msg).encode() + b"\n"
        os.write(self.w, data)

    def lines(self):
        with self.lock:
            text = self.out.getvalue().decode()
        return [json.loads(line) for line in text.splitlines() if line]

    def wait_for(self, pred, timeout=5.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for msg in self.lines():
                if pred(msg):
                    return msg
            time.sleep(0.005)
        raise AssertionError(f"timed out; output: {self.lines()}")

    def close(self):
        os.close(self.w)


class StreamWriter:
    """Binary writer that appends to the host's capture buffer under a lock."""

    def __init__(self, host):
        self.host = host

    def write(self, data):
        with self.host.lock:
            self.host.out.write(data)

    def flush(self):
        pass


def start(tag, spec=None):
    session = tempfile.mkdtemp(prefix=f"pigeon-py-{tag}-")
    host = Host(session)
    reader = os.fdopen(host.r, "rb", buffering=0)
    spec = spec or p.PluginSpec("test-plugin", "1.2.3", [p.Contribution.workspace_view("main", "Test", "sparkle")])
    plugin = p.Plugin.start(reader, StreamWriter(host), spec)
    return host, plugin, session


class RuntimeTests(unittest.TestCase):
    def finish(self, host, thread):
        host.send({"v": 2, "t": "shutdown"})
        thread.join(5)
        self.assertFalse(thread.is_alive())

    def test_handshake_sends_ready(self):
        host, plugin, _ = start("hs")
        ready = host.wait_for(lambda m: m["t"] == "ready")
        self.assertEqual(ready["plugin"], {"id": "test-plugin", "version": "1.2.3"})
        self.assertEqual(ready["protocol"], 2)
        self.assertEqual(ready["contributions"][0]["slot"], "workspace.view")
        self.assertEqual(plugin.ctx.theme_color("accent"), "#3b82f6")
        self.assertTrue(plugin.ctx.has_feature("canvas"))
        self.assertFalse(plugin.ctx.has_feature("source"))
        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: None,))
        t.start()
        self.finish(host, t)

    def test_shutdown_stops_io_threads_even_with_host_stdin_open(self):
        host, plugin, _ = start("open-shutdown")
        seen = []
        thread = threading.Thread(target=plugin.run, args=(lambda ctx, msg: seen.append(msg["t"]),))
        thread.start()
        try:
            self.finish(host, thread)
            plugin._threads[0].join(1)
            self.assertFalse(any(worker.is_alive() for worker in plugin._threads))
            self.assertEqual(seen, ["shutdown"])
        finally:
            host.close()

    def test_refuses_unsupported_protocol(self):
        hello = {"v": 2, "t": "hello", "host": {"name": "Pigeon", "version": "9"}, "protocol": {"min": 3, "max": 4},
                 "theme": {}, "locale": "en", "scale": 1.0, "session_dir": tempfile.gettempdir(), "features": []}
        r, w = os.pipe()
        os.write(w, (json.dumps(hello) + "\n").encode())
        with self.assertRaises(runtime.StartError) as ctx:
            p.Plugin.start(os.fdopen(r, "rb", buffering=0), io.BytesIO(), p.PluginSpec("x", "1"))
        self.assertIn("protocol 3-4", str(ctx.exception))
        os.close(w)

    def test_ping_is_answered_while_the_handler_is_blocked(self):
        host, plugin, _ = start("ping")
        gate = threading.Event()
        started = threading.Event()

        def handler(ctx, msg):
            if msg["t"] == "focus":
                started.set()
                gate.wait(5)

        t = threading.Thread(target=plugin.run, args=(handler,))
        t.start()
        host.send({"v": 2, "t": "focus", "has": True})
        self.assertTrue(started.wait(5))
        host.send({"v": 2, "t": "ping", "id": "p-7"})
        pong = host.wait_for(lambda m: m["t"] == "pong")
        self.assertEqual(pong["id"], "p-7")
        gate.set()
        self.finish(host, t)

    def test_pointer_moves_and_wheel_ticks_coalesce_presses_do_not(self):
        host, plugin, _ = start("coalesce")
        gate = threading.Event()
        started = threading.Event()
        seen = []

        def handler(ctx, msg):
            if msg["t"] == "focus":
                started.set()
                gate.wait(5)
                return
            seen.append(msg)

        t = threading.Thread(target=plugin.run, args=(handler,))
        t.start()
        host.send({"v": 2, "t": "focus", "has": True})
        self.assertTrue(started.wait(5))

        def pointer(phase, x, buttons=1):
            return {"v": 2, "t": "ui.event", "instance": "t/main", "node": "tl", "type": "pointer", "phase": phase,
                    "x": x, "y": 1.0, "button": 0, "buttons": buttons, "mods": {}, "clicks": 0, "pointer_id": 1,
                    "coalesced": 1}

        host.send(pointer("down", 0.0))
        for i in range(1, 51):
            host.send(pointer("move", float(i)))
        for _ in range(10):
            host.send({"v": 2, "t": "ui.event", "instance": "t/main", "node": "tl", "type": "wheel",
                       "dx": 0, "dy": -1, "x": 7, "y": 8, "mods": {}, "coalesced": 1})
        host.send(pointer("up", 50.0, 0))
        host.send({"v": 2, "t": "ping", "id": "sync"})
        host.wait_for(lambda m: m["t"] == "pong" and m.get("id") == "sync")
        gate.set()
        self.finish(host, t)
        moves = [(m["x"], m["coalesced"]) for m in seen if m.get("type") == "pointer" and m["phase"] == "move"]
        wheels = [(m["dy"], m["coalesced"], m["x"]) for m in seen if m.get("type") == "wheel"]
        presses = [m["phase"] for m in seen if m.get("type") == "pointer" and m["phase"] != "move"]
        self.assertEqual(moves, [(50.0, 50)])
        self.assertEqual(wheels, [(-10, 10, 7)])
        self.assertEqual(presses, ["down", "up"])

    def test_blocking_call_and_timeout(self):
        host, plugin, _ = start("call")
        result = {}

        def worker(ctx):
            try:
                result["ok"] = ctx.call("library.query", {"filter": {}, "limit": 2})
            except runtime.ServiceError as exc:
                result["err"] = exc

        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: None,))
        t.start()
        plugin.ctx.spawn(worker)
        req = host.wait_for(lambda m: m["t"] == "req")
        self.assertEqual(req["method"], "library.query")
        host.send({"v": 2, "t": "res", "id": req["id"], "result": {"items": [], "total": 0}})
        deadline = time.monotonic() + 5
        while "ok" not in result and time.monotonic() < deadline:
            time.sleep(0.005)
        self.assertEqual(result["ok"]["total"], 0)
        with self.assertRaises(runtime.ServiceError) as ctx2:
            plugin.ctx.call("progress.start", {"title": "x"}, timeout=0.05)
        self.assertEqual(ctx2.exception.code, "timeout")
        self.finish(host, t)

    def test_host_error_is_raised_with_code(self):
        host, plugin, _ = start("err")
        out = {}

        def worker(ctx):
            try:
                ctx.call("secrets.get", {"key": "k"})
            except runtime.ServiceError as exc:
                out["code"] = exc.code

        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: None,))
        t.start()
        plugin.ctx.spawn(worker)
        req = host.wait_for(lambda m: m["t"] == "req")
        host.send({"v": 2, "t": "res", "id": req["id"], "error": {"code": "permission_denied", "message": "no"}})
        deadline = time.monotonic() + 5
        while "code" not in out and time.monotonic() < deadline:
            time.sleep(0.005)
        self.assertEqual(out["code"], "permission_denied")
        self.finish(host, t)

    def test_async_callback_runs_on_the_app_thread(self):
        host, plugin, _ = start("async")
        seen = {}
        plugin.ctx.call_async("selection.get", {}, lambda ctx, result, error: seen.update(
            result=result, thread=threading.get_ident(), error=error))
        req = host.wait_for(lambda m: m["t"] == "req")
        host.send({"v": 2, "t": "res", "id": req["id"], "result": {"ids": ["a"]}})
        app_thread_ids = {}

        def app():
            app_thread_ids["id"] = threading.get_ident()
            plugin.run(lambda ctx, m: None)

        t = threading.Thread(target=app)
        t.start()
        self.finish(host, t)
        self.assertEqual(seen["result"]["ids"], ["a"])
        self.assertEqual(seen["thread"], app_thread_ids["id"])

    def test_provider_requests_are_answered(self):
        host, plugin, _ = start("provider")

        def handler(ctx, msg):
            if msg["t"] == "req":
                ctx.answer(msg["id"], {"title": "Cloud", "echo": msg["params"]})

        t = threading.Thread(target=plugin.run, args=(handler,))
        t.start()
        host.send({"v": 2, "t": "req", "id": "host-1", "method": "source.describe", "params": {"source": "cloud"}})
        res = host.wait_for(lambda m: m["t"] == "res" and m["id"] == "host-1")
        self.assertEqual(res["result"]["title"], "Cloud")
        self.assertEqual(res["result"]["echo"], {"source": "cloud"})
        self.finish(host, t)

    def test_scenes_are_validated_before_sending(self):
        host, plugin, _ = start("validate")
        with self.assertRaises(p.ProtocolError):
            plugin.ctx.set_scene("t/main", p.column("root").child(p.button("a", "A")).child(p.button("a", "B")))
        plugin.ctx.set_scene("t/main", p.column("root").child(p.button("ok", "OK")))
        host.wait_for(lambda m: m["t"] == "ui.set")
        self.assertEqual(sum(1 for m in host.lines() if m["t"] == "ui.set"), 1)
        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: None,))
        t.start()
        self.finish(host, t)

    def test_resource_ring_waits_for_ack_then_publishes(self):
        host, plugin, session = start("ring")
        res = p.Resources(plugin.ctx, ack_wait=0.08)
        pixels = bytes([9]) * (4 * 4 * 4)
        self.assertEqual([res.put_rgba("thumb:1", 4, 4, pixels) for _ in range(3)], [1, 2, 3])
        with self.assertRaises(runtime.ResourceError):
            res.put_rgba("thumb:1", 4, 4, pixels)
        host.send({"v": 2, "t": "resource.ack", "rid": "thumb:1", "seq": 1})
        self.assertEqual(res.put_rgba("thumb:1", 4, 4, pixels), 4)
        put = host.wait_for(lambda m: m["t"] == "resource.put" and m["seq"] == 4)
        self.assertEqual((put["kind"], put["stride"], put["w"]), ("rgba", 16, 4))
        self.assertTrue(put["file"].startswith(session))
        with open(put["file"], "rb") as f:
            self.assertEqual(f.read(), pixels)
        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: None,))
        t.start()
        self.finish(host, t)

    def test_oversized_lines_are_skipped(self):
        host, plugin, _ = start("oversize")
        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: None,))
        t.start()
        host.send('{"v":2,"t":"ping","pad":"' + "x" * (1024 * 1024 + 10) + '"}')
        host.send({"v": 2, "t": "ping", "id": "after"})
        host.wait_for(lambda m: m["t"] == "pong" and m.get("id") == "after")
        self.finish(host, t)

    def test_end_of_input_ends_the_loop(self):
        host, plugin, _ = start("eof")
        seen = []
        t = threading.Thread(target=plugin.run, args=(lambda ctx, m: seen.append(m["t"]),))
        t.start()
        host.close()
        t.join(5)
        self.assertFalse(t.is_alive())
        self.assertEqual(seen[-1], "shutdown")

    def test_coalesce_helper(self):
        items = [("event", {"t": "ui.event", "instance": "i", "node": "n", "type": "wheel", "dx": 0, "dy": 1, "x": 1, "y": 1, "coalesced": 1}),
                 ("event", {"t": "ui.event", "instance": "i", "node": "n", "type": "wheel", "dx": 0, "dy": 2, "x": 2, "y": 2, "coalesced": 1}),
                 ("event", {"t": "ui.event", "instance": "i", "node": "n", "type": "click"}),
                 ("event", {"t": "ui.event", "instance": "i", "node": "n", "type": "click"})]
        out = runtime._coalesce(items)
        self.assertEqual(len(out), 3)
        self.assertEqual(out[0][1]["dy"], 3)
        self.assertEqual(out[0][1]["coalesced"], 2)


if __name__ == "__main__":
    unittest.main()
