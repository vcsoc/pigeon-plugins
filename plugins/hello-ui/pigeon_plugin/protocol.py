"""Protocol v2 constants, envelope parsing and encoding (see plugin-ui-protocol.md)."""
import json

PROTOCOL_VERSION = 2
MAX_LINE_BYTES = 1024 * 1024
MAX_SCENE_NODES = 50_000
MAX_SCENE_BYTES = 4 * 1024 * 1024
MAX_CANVAS_OPS = 20_000
MAX_CANVAS_BYTES = 4 * 1024 * 1024
HOST_QUEUE_CAPACITY = 512


class ProtocolError(Exception):
    """A malformed message, an oversized line or a documented limit exceeded."""


def parse_incoming(line):
    """Parse one host line into a dict with `t` and the payload fields.

    Unknown `t` values are returned unchanged (forward compatibility). `ui.event` payloads keep
    their flat fields (`type`, `x`, `value`, ...). Raises ProtocolError for non-JSON input.
    """
    if isinstance(line, bytes):
        if len(line) > MAX_LINE_BYTES:
            raise ProtocolError(f"message too large ({len(line)} bytes)")
        line = line.decode("utf-8")
    if len(line) > MAX_LINE_BYTES:
        raise ProtocolError(f"message too large ({len(line)} bytes)")
    try:
        msg = json.loads(line)
    except json.JSONDecodeError as exc:
        raise ProtocolError(f"malformed JSON: {exc}") from exc
    if not isinstance(msg, dict) or not isinstance(msg.get("t"), str):
        raise ProtocolError("message is not an object with a string t")
    return msg


def encode_outgoing(t, **fields):
    """Build one plugin-to-host line (without the newline), enforcing the 1 MiB limit."""
    payload = {"v": PROTOCOL_VERSION, "t": t}
    payload.update(fields)
    line = json.dumps(payload, separators=(",", ":"), ensure_ascii=False)
    if len(line.encode("utf-8")) > MAX_LINE_BYTES:
        raise ProtocolError(f"message too large ({len(line)} bytes)")
    return line
