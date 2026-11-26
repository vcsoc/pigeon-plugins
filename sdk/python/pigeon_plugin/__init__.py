"""pigeon_plugin: the Python SDK for Pigeon plugins (protocol v2). Standard library only.

Canvas draw ops live in `pigeon_plugin.draw` (`draw.rect`, `draw.text`, `draw.DrawList`).

Mirrors the Rust crate `pigeon-plugin-sdk`: typed-by-convention messages, scene nodes with fluent
builders, canvas draw lists, slot contributions, a protocol thread that answers ping on its own
thread, service calls (blocking and callback), the resource file ring and the event loop.
See docs/architecture/sdk.md.
"""
from .protocol import (
    PROTOCOL_VERSION, MAX_LINE_BYTES, MAX_SCENE_NODES, MAX_SCENE_BYTES, MAX_CANVAS_OPS,
    MAX_CANVAS_BYTES, HOST_QUEUE_CAPACITY, ProtocolError, parse_incoming, encode_outgoing,
)
from . import contribution, draw, protocol, scene
from .scene import *  # noqa: F401,F403  (node classes and one builder per kind)
from .scene import __all__ as _scene_all
from .draw import DrawList
from .contribution import Contribution, AppliesTo, SLOTS

__all__ = list(_scene_all) + [
    "PROTOCOL_VERSION", "MAX_LINE_BYTES", "MAX_SCENE_NODES", "MAX_SCENE_BYTES", "MAX_CANVAS_OPS",
    "MAX_CANVAS_BYTES", "HOST_QUEUE_CAPACITY", "ProtocolError", "parse_incoming", "encode_outgoing",
    "Node", "Size", "Color", "MenuItem", "Span", "DrawList", "draw", "Contribution", "AppliesTo", "SLOTS",
]
