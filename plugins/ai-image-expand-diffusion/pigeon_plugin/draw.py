"""Canvas draw lists (section 7). Ops are dicts with an `op` tag, built by the helpers below."""
import json

from .protocol import MAX_CANVAS_BYTES, MAX_CANVAS_OPS, ProtocolError


def _clean(d):
    return {k: v for k, v in d.items() if v is not None}


def op(name, **fields):
    """Build a raw op dict: op("rect", x=0, y=0, w=10, h=10, fill="accent")."""
    return _clean({"op": name, **fields})


def rect(x, y, w, h, fill=None, stroke=None, stroke_w=None, radius=None):
    return op("rect", x=x, y=y, w=w, h=h, fill=fill, stroke=stroke, stroke_w=stroke_w, radius=radius)


def line(x1, y1, x2, y2, stroke, stroke_w=1.0):
    return op("line", x1=x1, y1=y1, x2=x2, y2=y2, stroke=stroke, stroke_w=stroke_w)


def polyline(pts, stroke, stroke_w=1.0):
    return op("polyline", pts=[list(p) for p in pts], stroke=stroke, stroke_w=stroke_w)


def polygon(pts, fill=None, stroke=None, stroke_w=None):
    return op("polygon", pts=[list(p) for p in pts], fill=fill, stroke=stroke, stroke_w=stroke_w)


def path(cmds, fill=None, stroke=None, stroke_w=None):
    """cmds: lists like ["M", x, y], ["L", x, y], ["Q", cx, cy, x, y], ["C", ...6], ["Z"]."""
    return op("path", cmds=[list(c) for c in cmds], fill=fill, stroke=stroke, stroke_w=stroke_w)


def circle(cx, cy, r, fill=None, stroke=None, stroke_w=None):
    return op("circle", cx=cx, cy=cy, r=r, fill=fill, stroke=stroke, stroke_w=stroke_w)


def ellipse(cx, cy, rx, ry, fill=None, stroke=None, stroke_w=None):
    return op("ellipse", cx=cx, cy=cy, rx=rx, ry=ry, fill=fill, stroke=stroke, stroke_w=stroke_w)


def arc(cx, cy, r, start, end, fill=None, stroke=None, stroke_w=None):
    return op("arc", cx=cx, cy=cy, r=r, start=start, end=end, fill=fill, stroke=stroke, stroke_w=stroke_w)


def text(x, y, value, size=13, weight=None, color=None, align=None, baseline=None, max_w=None, font=None):
    return op("text", x=x, y=y, text=value, size=size, weight=weight, color=color, align=align,
              baseline=baseline, max_w=max_w, font=font)


def image(rid, dst, src=None, tint=None, opacity=None):
    """dst/src: {"x","y","w","h"} dicts."""
    return op("image", rid=rid, dst=dst, src=src, tint=tint, opacity=opacity)


def clip(x, y, w, h):
    return op("clip", x=x, y=y, w=w, h=h)


def unclip():
    return {"op": "unclip"}


def translate(x, y):
    return op("translate", x=x, y=y)


def scale(x, y):
    return op("scale", x=x, y=y)


def save():
    return {"op": "save"}


def restore():
    return {"op": "restore"}


def shadow(dx, dy, blur, color):
    return op("shadow", dx=dx, dy=dy, blur=blur, color=color)


def gradient(x0, y0, x1, y1, stops):
    """stops: [[offset, color], ...]."""
    return op("gradient", x0=x0, y0=y0, x1=x1, y1=y1, stops=[list(s) for s in stops])


class DrawList:
    """An ordered list of ops plus the clear color. `add` returns self for chaining."""

    def __init__(self, clear=None):
        self.clear = clear
        self.ops = []

    def add(self, *ops):
        self.ops.extend(ops)
        return self

    def __len__(self):
        return len(self.ops)

    def to_draw_value(self, frame):
        out = {"frame": frame, "ops": self.ops}
        if self.clear is not None:
            out["clear"] = self.clear
        return out

    def validate(self):
        if len(self.ops) > MAX_CANVAS_OPS:
            raise ProtocolError(f"canvas_too_large: {len(self.ops)} ops (max {MAX_CANVAS_OPS})")
        size = len(json.dumps(self.ops, separators=(",", ":")).encode("utf-8"))
        if size > MAX_CANVAS_BYTES:
            raise ProtocolError(f"canvas_too_large: {size} bytes (max {MAX_CANVAS_BYTES})")
        return self
