"""Plugin runtime (protocol v2) for Python plugins. Standard library only.

Threads, as in the Rust SDK (section 9 of the protocol):

* reader: reads stdin, answers `ping` itself, resolves `res` replies, records `resource.ack`,
  theme and locale changes, and queues everything else for the app. It never waits on the app.
* writer: drains the outbox; control lines (pong, responses) are written before normal lines.
* app: the thread that calls `Plugin.run(handler)`; receives batches, coalesces pointer moves and
  wheel ticks, and calls `handler(ctx, message)` where message is the parsed dict.

    from pigeon_plugin import Plugin, PluginSpec, Contribution, Node

    plugin = Plugin.stdio(PluginSpec("hello-ui", "1.0.0", [
        Contribution.workspace_view("main", "Hello", "sparkle")]))

    def handle(ctx, msg):
        if msg["t"] == "visibility" and msg["visible"]:
            ctx.set_scene(msg["instance"], Node("root", "text", text="Hi"))

    plugin.run(handle)
"""
import collections
import json
import os
import queue
import sys
import tempfile
import threading
import time

from .protocol import (HOST_QUEUE_CAPACITY, MAX_CANVAS_BYTES, MAX_LINE_BYTES, PROTOCOL_VERSION,
                       ProtocolError, encode_outgoing, parse_incoming)
from .scene import Node
from .draw import DrawList

DEFAULT_CALL_TIMEOUT = 30.0
DEFAULT_ACK_WAIT = 2.0
RING_SLOTS = 3
MAX_QUEUED_EVENTS = 4096
COALESCIBLE_EVENTS = {"pointer", "wheel", "hover", "list.window", "scroll"}
COALESCIBLE_POINTER_PHASES = {"move", "enter", "leave"}


class PluginSpec:
    """Identity and static contributions, sent in `ready`."""

    def __init__(self, plugin_id, version, contributions=()):
        if not plugin_id or not version:
            raise ValueError("plugin_id and version are required")
        self.plugin_id = plugin_id
        self.version = version
        self.contributions = list(contributions)


class ServiceError(Exception):
    """A host service call failed: code is e.g. permission_denied; timeout/closed are local."""

    def __init__(self, code, message):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class StartError(Exception):
    pass


class _Outbox:
    def __init__(self, capacity):
        self._cond = threading.Condition()
        self._control = collections.deque()
        self._normal = collections.deque()
        self._capacity = max(1, capacity)
        self._closed = False

    def send_control(self, line):
        with self._cond:
            if self._closed:
                return False
            self._control.append(line)
            self._cond.notify_all()
            return True

    def send(self, line):
        with self._cond:
            while not self._closed and len(self._normal) >= self._capacity:
                self._cond.wait()
            if self._closed:
                return False
            self._normal.append(line)
            self._cond.notify_all()
            return True

    def next(self):
        with self._cond:
            while True:
                if self._control:
                    return self._control.popleft()
                if self._normal:
                    self._cond.notify_all()
                    return self._normal.popleft()
                if self._closed:
                    return None
                self._cond.wait()

    def close(self):
        with self._cond:
            self._closed = True
            self._cond.notify_all()


class _Shared:
    def __init__(self, plugin_id, version, session_dir):
        self.plugin_id = plugin_id
        self.version = version
        self.outbox = _Outbox(HOST_QUEUE_CAPACITY)
        self.lock = threading.Lock()
        self.pending = {}          # id -> (Event, box) or ("callback", fn)
        self.next_id = 1
        self.inbound = queue.Queue()
        self.queued = 0
        self.dropped = 0
        self.theme = {}
        self.locale = "en"
        self.scale = 1.0
        self.features = []
        self.session_dir = session_dir
        self.data_dir = None
        self.host = {}
        self.acks = {}
        self.acks_cond = threading.Condition(self.lock)
        self.rings = {}
        self.publish_lock = threading.Lock()
        self.closed = False


class Ctx:
    """A handle to the connection. Safe to use from worker threads."""

    def __init__(self, shared):
        self._s = shared

    # ---- identity and host state ----
    @property
    def plugin_id(self):
        return self._s.plugin_id

    @property
    def version(self):
        return self._s.version

    @property
    def theme(self):
        return dict(self._s.theme)

    def theme_color(self, token):
        return self._s.theme.get("colors", {}).get(token)

    @property
    def locale(self):
        return self._s.locale

    @property
    def session_dir(self):
        return self._s.session_dir

    @property
    def data_dir(self):
        return self._s.data_dir

    def has_feature(self, name):
        return name in self._s.features

    @property
    def is_open(self):
        return not self._s.closed

    @property
    def dropped_events(self):
        return self._s.dropped

    # ---- sending ----
    def send_line(self, line):
        """Queue one already-encoded line (normal priority). Returns False when closed."""
        return self._s.outbox.send(line)

    def _send(self, t, **fields):
        return self._s.outbox.send(encode_outgoing(t, **fields))

    def set_scene(self, instance, scene):
        """Replace a slot instance's scene (a Node). Validates limits and unique ids first."""
        if not isinstance(scene, Node):
            raise TypeError("scene must be a pigeon_plugin.Node")
        scene.validate()
        return self._send("ui.set", instance=instance, scene=scene.to_dict())

    def patch(self, instance, ops):
        return self._send("ui.patch", instance=instance, ops=list(ops))

    def canvas_draw(self, instance, node, frame, drawlist):
        if not isinstance(drawlist, DrawList):
            raise TypeError("drawlist must be a pigeon_plugin.draw.DrawList")
        drawlist.validate()
        return self._send("canvas.draw", instance=instance, node=node, frame=frame, ops=drawlist.ops)

    def list_rows(self, instance, node, start, rows):
        for row in rows:
            row.validate()
        return self._send("list.rows", instance=instance, node=node, start=start,
                          rows=[r.to_dict() for r in rows])

    def focus(self, instance, node):
        return self._send("ui.focus", instance=instance, node=node)

    def scroll_to(self, instance, node, align="nearest"):
        return self._send("ui.scroll_to", instance=instance, node=node, align=align)

    def beep(self):
        return self._send("ui.beep")

    def contribute(self, contributions):
        return self._send("contribute", contributions=list(contributions))

    def withdraw(self, ids):
        return self._send("withdraw", ids=list(ids))

    def source_changed(self, source, kind, node=None, ids=None):
        fields = {"source": source, "kind": kind}
        if node is not None:
            fields["node"] = node
        if ids is not None:
            fields["ids"] = list(ids)
        return self._send("source.changed", **fields)

    def search_status(self, mode_id, status):
        return self._send("search.status", mode_id=mode_id, status=status)

    def answer(self, request_id, result):
        """Answer a provider request (req) from the host."""
        return self._s.outbox.send(encode_outgoing("res", id=request_id, result=result))

    def answer_error(self, request_id, code, message):
        return self._s.outbox.send(encode_outgoing("res", id=request_id, error={"code": code, "message": message}))

    def spawn(self, fn, *args, **kwargs):
        """Run fn(ctx, *args) on a daemon worker thread. Heavy work belongs here."""
        thread = threading.Thread(target=fn, args=(self,) + args, kwargs=kwargs, daemon=True)
        thread.start()
        return thread

    # ---- host services (section 10) ----
    def _register(self, entry):
        with self._s.lock:
            rid = str(self._s.next_id)
            self._s.next_id += 1
            self._s.pending[rid] = entry
        return rid

    def _send_request(self, rid, method, params):
        line = encode_outgoing("req", id=rid, method=method, params=params or {})
        if not self._s.outbox.send(line):
            raise ServiceError("closed", "connection to the host closed")
        return rid

    def call(self, method, params=None, timeout=DEFAULT_CALL_TIMEOUT):
        """Call a host service and wait for the reply (dict). Raises ServiceError."""
        done = threading.Event()
        box = {}
        rid = self._register((done, box))
        try:
            self._send_request(rid, method, params)
        except ServiceError:
            with self._s.lock:
                self._s.pending.pop(rid, None)
            raise
        if not done.wait(timeout):
            with self._s.lock:
                self._s.pending.pop(rid, None)
            raise ServiceError("timeout", f"host did not answer {method} in time")
        if "error" in box:
            raise box["error"]
        return box.get("result") or {}

    def call_async(self, method, params, callback):
        """Call a host service; callback(ctx, result_or_none, error_or_none) runs on the app thread."""
        rid = self._register(("callback", callback))
        try:
            self._send_request(rid, method, params)
        except ServiceError as exc:
            with self._s.lock:
                self._s.pending.pop(rid, None)
            self._s.inbound.put(("reply", callback, None, exc))

    def log(self, level, message):
        sys.stderr.write(f"[{level}] {message}\n")
        self.call_async("log", {"level": level, "message": message}, lambda ctx, r, e: None)

    def settings_get(self, key):
        return self.call("settings.get", {"key": key}).get("value")

    def settings_set(self, key, value):
        self.call("settings.set", {"key": key, "value": value})

    def settings_delete(self, key):
        self.call("settings.delete", {"key": key})

    def toast(self, level, text):
        self.call("notify.toast", {"level": level, "text": text})

    def progress_start(self, title, cancellable=False):
        return self.call("progress.start", {"title": title, "cancellable": cancellable})["job"]

    def progress_update(self, job, value=None, message=None):
        params = {"job": job, "value": value}
        if message is not None:
            params["message"] = message
        self.call("progress.update", params)

    def progress_end(self, job, ok=True, message=None):
        params = {"job": job, "ok": ok}
        if message is not None:
            params["message"] = message
        self.call("progress.end", params)

    def library_query(self, filter=None, limit=100):
        return self.call("library.query", {"filter": filter or {}, "limit": limit})

    def selection(self):
        return self.call("selection.get", {})

    def open_url(self, url):
        self.call("shell.open_url", {"url": url})

    def open_file_dialog(self, title, multiple=False):
        return self.call("dialog.open_file", {"title": title, "multiple": multiple}).get("paths", [])

    def open_modal(self, contribution, props=None):
        return self.call("window.open_modal", {"contribution": contribution, "props": props or {}})

    @property
    def resources(self):
        return Resources(self)


class ResourceError(Exception):
    pass


class Resources:
    """Publishes images and frames through the session-directory file ring (section 6)."""

    def __init__(self, ctx, ack_wait=DEFAULT_ACK_WAIT):
        self._ctx = ctx
        self._ack_wait = ack_wait

    def put_rgba(self, rid, w, h, pixels, premultiplied=False):
        """RGBA8 straight alpha, row stride w*4. Returns the sequence number."""
        expected = w * h * 4
        if len(pixels) != expected:
            raise ResourceError(f"pixel buffer is {len(pixels)} bytes, expected {expected}")
        return self._publish(rid, "rgba", "rgba", bytes(pixels), w, h, w * 4, premultiplied)

    def put_encoded(self, rid, kind, data, w=0, h=0):
        """kind: png, jpeg, webp or svg."""
        ext = {"png": "png", "jpeg": "jpg", "webp": "webp", "svg": "svg"}.get(kind)
        if ext is None:
            raise ResourceError(f"unsupported encoded kind {kind}")
        return self._publish(rid, kind, ext, bytes(data), w, h, None, False)

    def drop(self, rid):
        with self._ctx._s.lock:
            self._ctx._s.acks.pop(rid, None)
            self._ctx._s.rings.pop(rid, None)
        self._ctx._send("resource.drop", rid=rid)

    def acked(self, rid):
        with self._ctx._s.lock:
            return self._ctx._s.acks.get(rid, 0)

    def _publish(self, rid, kind, ext, data, w, h, stride, premultiplied):
        if not isinstance(rid, str) or not rid or len(rid) > 256:
            raise ResourceError(f"invalid resource id {rid!r}")
        session = self._ctx.session_dir
        if not session:
            raise ResourceError("the host did not provide a session directory")
        shared = self._ctx._s
        with shared.publish_lock:
            seq = shared.rings.get(rid, 0) + 1
            self._wait_for_slot(rid, seq)
            slot = seq % RING_SLOTS
            safe = "".join(c if (c.isascii() and (c.isalnum() or c in "-_")) else "_" for c in rid)
            final = os.path.join(session, f"{safe}.{slot}.{ext}")
            tmp = final + ".tmp"
            with open(tmp, "wb") as f:
                f.write(data)
            os.replace(tmp, final)
            put = {"rid": rid, "kind": kind, "file": final, "w": w, "h": h, "seq": seq,
                   "premultiplied": bool(premultiplied)}
            if stride is not None:
                put["stride"] = stride
            self._ctx._send("resource.put", **put)
            with shared.lock:
                shared.rings[rid] = seq
            return seq

    def _wait_for_slot(self, rid, seq):
        shared = self._ctx._s
        deadline = time.monotonic() + self._ack_wait
        with shared.lock:
            while True:
                if seq <= shared.acks.get(rid, 0) + RING_SLOTS:
                    return
                if shared.closed:
                    raise ServiceError("closed", "connection to the host closed")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ResourceError(f"host has not released an older frame of {rid}")
                shared.acks_cond.wait(remaining)


class Plugin:
    """The running plugin. Create with Plugin.stdio(spec) or Plugin.start(reader, writer, spec)."""

    def __init__(self, shared, hello, reader_stream):
        self._shared = shared
        self.hello = hello
        self._reader_stream = reader_stream
        self._ctx = Ctx(shared)

    @classmethod
    def stdio(cls, spec):
        return cls.start(sys.stdin.buffer, sys.stdout.buffer, spec)

    @classmethod
    def start(cls, reader, writer, spec):
        """reader: binary stream of host lines; writer: binary stream for plugin lines."""
        first = _read_line(reader)
        if first is None:
            raise StartError("host closed stdin before hello")
        try:
            hello = parse_incoming(first)
        except ProtocolError as exc:
            raise StartError(str(exc)) from exc
        if hello.get("t") != "hello":
            raise StartError(f"expected hello, got {hello.get('t')}")
        proto = hello.get("protocol", {})
        if not (proto.get("min", 0) <= PROTOCOL_VERSION <= proto.get("max", 0)):
            raise StartError(f"host speaks protocol {proto.get('min')}-{proto.get('max')}, "
                             f"this plugin speaks {PROTOCOL_VERSION}")
        session = hello.get("session_dir") or tempfile.gettempdir()
        shared = _Shared(spec.plugin_id, spec.version, session)
        shared.data_dir = hello.get("data_dir")
        shared.theme = hello.get("theme", {})
        shared.locale = hello.get("locale", "en")
        shared.scale = hello.get("scale", 1.0)
        shared.features = list(hello.get("features", []))
        shared.host = hello.get("host", {})
        writer_thread = threading.Thread(target=_write_loop, args=(shared.outbox, writer),
                                         name="pigeon-writer", daemon=True)
        writer_thread.start()
        ready = {"plugin": {"id": spec.plugin_id, "version": spec.version}, "protocol": PROTOCOL_VERSION,
                 "contributions": list(spec.contributions)}
        shared.outbox.send(encode_outgoing("ready", **ready))
        reader_thread = threading.Thread(target=_reader_loop, args=(shared, reader),
                                         name="pigeon-reader", daemon=True)
        reader_thread.start()
        plugin = cls(shared, hello, reader)
        plugin._threads = (reader_thread, writer_thread)
        return plugin

    @property
    def ctx(self):
        return self._ctx

    def run(self, handler):
        """Run the app loop until shutdown or end of input. handler(ctx, msg) gets dicts."""
        shared = self._shared
        try:
            while True:
                first = shared.inbound.get()
                batch = [first]
                while len(batch) < 512:
                    try:
                        batch.append(shared.inbound.get_nowait())
                    except queue.Empty:
                        break
                events = sum(1 for item in batch if item[0] == "event")
                with shared.lock:
                    shared.queued = max(0, shared.queued - events)
                for item in _coalesce(batch):
                    if item[0] == "event":
                        msg = item[1]
                        handler(self._ctx, msg)
                        if msg.get("t") == "shutdown":
                            return
                    else:
                        _, callback, result, error = item
                        callback(self._ctx, result, error)
        finally:
            shared.closed = True
            shared.outbox.close()
            # Leave time to flush queued replies before CPython finalizes buffered stdout.
            self._threads[1].join(timeout=1.0)


def _read_line(stream):
    """One line (without newline), bounded to MAX_LINE_BYTES. None at end of input.

    An oversized line is drained up to its newline and reported as an empty line (ignored), so the
    stream stays in sync.
    """
    line = stream.readline(MAX_LINE_BYTES + 2)
    if not line:
        return None
    if not line.endswith(b"\n") and len(line) > MAX_LINE_BYTES:
        while True:
            chunk = stream.readline(65536)
            if not chunk or chunk.endswith(b"\n"):
                break
        return b""
    return line.rstrip(b"\r\n")


def _write_loop(outbox, writer):
    while True:
        line = outbox.next()
        if line is None:
            break
        try:
            writer.write(line.encode("utf-8") + b"\n")
            writer.flush()
        except (OSError, ValueError):
            outbox.close()
            break


def _reader_loop(shared, stream):
    while True:
        raw = _read_line(stream)
        if raw is None:
            break
        if not raw.strip():
            continue
        try:
            msg = parse_incoming(raw)
        except ProtocolError as exc:
            sys.stderr.write(f"pigeon_plugin: ignoring host line: {exc}\n")
            continue
        if msg.get("t") == "shutdown":
            # Do not block again on stdin while the app exits (the host may keep its pipe open).
            break
        _dispatch(shared, msg)
    shared.closed = True
    shared.outbox.close()
    with shared.lock:
        waiting = list(shared.pending.items())
        shared.pending.clear()
        shared.acks_cond.notify_all()
    closed = ServiceError("closed", "connection to the host closed")
    for _, entry in waiting:
        if isinstance(entry, tuple) and entry and entry[0] == "callback":
            shared.inbound.put(("reply", entry[1], None, closed))
        else:
            done, box = entry
            box["error"] = closed
            done.set()
    shared.inbound.put(("event", {"v": PROTOCOL_VERSION, "t": "shutdown"}))


def _dispatch(shared, msg):
    t = msg.get("t")
    if t == "ping":
        line = encode_outgoing("pong", **({"id": msg["id"]} if "id" in msg else {}))
        shared.outbox.send_control(line)
    elif t == "res":
        rid = str(msg.get("id", ""))
        with shared.lock:
            entry = shared.pending.pop(rid, None)
        if entry is None:
            return
        error = msg.get("error")
        result = msg.get("result")
        if isinstance(entry, tuple) and entry and entry[0] == "callback":
            err = ServiceError(error.get("code", "error"), error.get("message", "")) if error else None
            shared.inbound.put(("reply", entry[1], None if err else result, err))
        else:
            done, box = entry
            if error:
                box["error"] = ServiceError(error.get("code", "error"), error.get("message", ""))
            else:
                box["result"] = result
            done.set()
    elif t == "resource.ack":
        with shared.lock:
            rid = msg.get("rid", "")
            seq = int(msg.get("seq", 0))
            if seq > shared.acks.get(rid, 0):
                shared.acks[rid] = seq
            shared.acks_cond.notify_all()
    elif t == "theme.changed":
        with shared.lock:
            shared.theme = msg.get("theme", {})
        _enqueue(shared, msg, droppable=False)
    elif t == "locale.changed":
        with shared.lock:
            shared.locale = msg.get("locale", "en")
        _enqueue(shared, msg, droppable=False)
    elif t == "ui.event":
        _enqueue(shared, msg, droppable=_coalescible(msg))
    elif t == "hello":
        return
    else:
        _enqueue(shared, msg, droppable=False)


def _coalescible(msg):
    kind = msg.get("type")
    if kind in ("wheel", "hover", "list.window", "scroll"):
        return True
    if kind == "pointer":
        return msg.get("phase") in COALESCIBLE_POINTER_PHASES
    return False


def _enqueue(shared, msg, droppable):
    with shared.lock:
        if droppable and shared.queued >= MAX_QUEUED_EVENTS:
            shared.dropped += 1
            return
        shared.queued += 1
    shared.inbound.put(("event", msg))


def _coalesce(items):
    """Merge adjacent pointer moves and wheel ticks for the same node (summing wheel deltas)."""
    out = []
    for item in items:
        if out and item[0] == "event" and out[-1][0] == "event" and _merge(out[-1][1], item[1]):
            continue
        out.append(item)
    return out


def _merge(last, nxt):
    if last.get("t") != "ui.event" or nxt.get("t") != "ui.event":
        return False
    if last.get("instance") != nxt.get("instance") or last.get("node") != nxt.get("node"):
        return False
    if last.get("type") == "pointer" and nxt.get("type") == "pointer":
        if last.get("phase") == "move" and nxt.get("phase") == "move":
            last["x"], last["y"] = nxt["x"], nxt["y"]
            last["buttons"] = nxt.get("buttons", last.get("buttons", 0))
            last["coalesced"] = last.get("coalesced", 1) + nxt.get("coalesced", 1)
            return True
        return False
    if last.get("type") == "wheel" and nxt.get("type") == "wheel":
        last["dx"] = last.get("dx", 0) + nxt.get("dx", 0)
        last["dy"] = last.get("dy", 0) + nxt.get("dy", 0)
        last["x"], last["y"] = nxt.get("x", 0), nxt.get("y", 0)
        last["mods"] = nxt.get("mods", {})
        last["coalesced"] = last.get("coalesced", 1) + nxt.get("coalesced", 1)
        return True
    return False
