"""Scene nodes (section 5). A Node is {id, kind, children?, **props}; builders return the node."""
import json

from .protocol import MAX_SCENE_BYTES, MAX_SCENE_NODES, ProtocolError

__all__ = [
    "row",
    "column",
    "stack",
    "wrap",
    "grid",
    "split",
    "scroll",
    "tabs",
    "collapsible",
    "panel",
    "spacer",
    "divider",
    "text",
    "icon",
    "image",
    "button",
    "icon_button",
    "checkbox",
    "switch",
    "radio_group",
    "segmented",
    "text_input",
    "number_input",
    "slider",
    "range_slider",
    "select",
    "color_picker",
    "progress",
    "spinner",
    "badge",
    "pill",
    "menu",
    "context_menu",
    "table",
    "tree",
    "list_node",
    "canvas",
    "rich_text",
    "code",
    "Node",
    "Color",
    "Size",
    "MenuItem",
    "Span",
]


class Color:
    """Helpers for color values: `#rrggbb[aa]` strings or theme token names."""

    @staticmethod
    def rgb(r, g, b):
        return f"#{r:02x}{g:02x}{b:02x}"

    @staticmethod
    def rgba(r, g, b, a):
        return f"#{r:02x}{g:02x}{b:02x}{a:02x}"

    @staticmethod
    def token(name):
        return name

    @staticmethod
    def parse(text):
        """Return the color string if it is a hex color or token name, else None."""
        import re
        if re.fullmatch(r"#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?", text or ""):
            return text.lower()
        if re.fullmatch(r"[a-z][a-z0-9_]*", text or ""):
            return text
        return None


class Size:
    """Layout lengths: Size.px(n), Size.fill(w), Size.AUTO."""
    AUTO = "auto"

    @staticmethod
    def px(n):
        return {"px": n}

    @staticmethod
    def fill(weight=1):
        return {"fill": weight}


class Node:
    """A scene node. Children go in `.children`; other props in `.props`."""

    __slots__ = ("id", "kind", "children", "props")

    def __init__(self, id, kind, **props):
        if not isinstance(id, str) or not id:
            raise ProtocolError("node id must be a non-empty string")
        self.id = id
        self.kind = kind
        self.children = []
        self.props = dict(props)

    # ---- generic ----
    def set(self, key, value):
        self.props[key] = value
        return self

    def get(self, key, default=None):
        return self.props.get(key, default)

    def child(self, node):
        self.children.append(node)
        return self

    def children_from(self, nodes):
        self.children.extend(nodes)
        return self

    def width(self, size):
        return self.set("width", size)

    def height(self, size):
        return self.set("height", size)

    def padding(self, px):
        return self.set("padding", px)

    def margin(self, px):
        return self.set("margin", px)

    def gap(self, px):
        return self.set("gap", px)

    def align(self, a):
        return self.set("align", a)

    def justify(self, j):
        return self.set("justify", j)

    def visible(self, on):
        return self.set("visible", on)

    def enabled(self, on):
        return self.set("enabled", on)

    def tooltip(self, text):
        return self.set("tooltip", text)

    def bg(self, c):
        return self.set("bg", c)

    def fg(self, c):
        return self.set("fg", c)

    def border(self, c):
        return self.set("border", c)

    def radius(self, px):
        return self.set("radius", px)

    def accessible(self, label, role):
        return self.set("accessibility", {"label": label, "role": role})

    def on(self, *events):
        return self.set("on", list(events))

    def style(self, s):
        return self.set("style", s)

    def icon(self, name):
        return self.set("icon", name)

    def label(self, text):
        return self.set("label", text)

    def variant(self, v):
        return self.set("variant", v)

    def toggled(self, on):
        return self.set("toggled", on)

    def shortcut(self, text):
        return self.set("shortcut", text)

    def wrap_text(self, on):
        return self.set("wrap", on)

    def selectable(self, on):
        return self.set("selectable", on)

    def placeholder(self, text):
        return self.set("placeholder", text)

    def multiline(self, on):
        return self.set("multiline", on)

    def commit(self, mode):
        return self.set("commit", mode)

    def cursor(self, name):
        return self.set("cursor", name)

    def interactive(self, pointer=True, wheel=True, keys=False, drop=False):
        return self.set("interactive", {"pointer": pointer, "wheel": wheel, "keys": keys,
                                        "drop": drop, "focusable": keys, "coalesce": "move"})

    def draw(self, drawlist, frame):
        """Canvas: attach a full draw list with a frame number."""
        return self.set("draw", drawlist.to_draw_value(frame))

    def walk(self):
        """Yield this node and every descendant (depth first)."""
        stack = [self]
        while stack:
            node = stack.pop()
            yield node
            stack.extend(reversed(node.children))

    def count(self):
        return sum(1 for _ in self.walk())

    def to_dict(self):
        out = {"id": self.id, "kind": self.kind}
        out.update(self.props)
        if self.children:
            out["children"] = [c.to_dict() for c in self.children]
        return out

    def validate(self):
        """Check the documented scene rules (unique non-empty ids, node and byte limits)."""
        seen = set()
        count = 0
        for node in self.walk():
            count += 1
            if not node.id:
                raise ProtocolError("node with an empty id")
            if not node.kind:
                raise ProtocolError(f"node {node.id} has no kind")
            if node.id in seen:
                raise ProtocolError(f"duplicate node id {node.id}")
            seen.add(node.id)
        if count > MAX_SCENE_NODES:
            raise ProtocolError(f"scene has {count} nodes (max {MAX_SCENE_NODES})")
        size = len(json.dumps(self.to_dict(), separators=(",", ":")).encode("utf-8"))
        if size > MAX_SCENE_BYTES:
            raise ProtocolError(f"scene is {size} bytes (max {MAX_SCENE_BYTES})")
        return self

    def __repr__(self):
        return f"Node({self.kind}:{self.id})"


# ---- constructors, one per kind (layout first) ----

def row(id):
    return Node(id, "row")


def column(id):
    return Node(id, "column")


def stack(id):
    return Node(id, "stack")


def wrap(id):
    return Node(id, "wrap")


def grid(id, columns=0, min_cell=None):
    n = Node(id, "grid")
    if columns:
        n.set("columns", columns)
    if min_cell is not None:
        n.set("min_cell", min_cell)
    return n


def split(id, direction, sizes):
    return Node(id, "split", direction=direction, sizes=list(sizes))


def scroll(id, axis="vertical"):
    return Node(id, "scroll", axis=axis, show_bars=True)


def tabs(id, tab_list, active):
    return Node(id, "tabs", tabs=[{"id": i, "label": l} for i, l in tab_list], active=active)


def collapsible(id, title, open=True):
    return Node(id, "collapsible", title=title, open=open)


def panel(id):
    return Node(id, "panel")


def spacer(id):
    return Node(id, "spacer")


def divider(id):
    return Node(id, "divider")


def text(id, value, style=None):
    n = Node(id, "text", text=value)
    if style:
        n.set("style", style)
    return n


def icon(id, name):
    return Node(id, "icon", icon=name)


def image(id, resource, fit="contain"):
    return Node(id, "image", resource=resource, fit=fit)


def button(id, label, icon=None, variant="default"):
    n = Node(id, "button", label=label, variant=variant)
    if icon:
        n.set("icon", icon)
    return n


def icon_button(id, icon_name, tooltip_text=""):
    return Node(id, "icon_button", icon=icon_name, tooltip=tooltip_text)


def checkbox(id, label, value=False):
    return Node(id, "checkbox", label=label, value=bool(value))


def switch(id, label, value=False):
    return Node(id, "switch", label=label, value=bool(value))


def radio_group(id, options, value):
    return Node(id, "radio_group", options=_options(options), value=value)


def segmented(id, options, value):
    return Node(id, "segmented", options=_options(options), value=value)


def text_input(id, value, rev=0):
    return Node(id, "text_input", value=value, rev=rev)


def number_input(id, value, rev=0):
    return Node(id, "number_input", value=value, rev=rev)


def slider(id, value, minimum, maximum, rev=0):
    return Node(id, "slider", value=value, min=minimum, max=maximum, rev=rev)


def range_slider(id, low, high, minimum, maximum, rev=0):
    return Node(id, "range_slider", value=[low, high], min=minimum, max=maximum, rev=rev)


def select(id, options, value):
    return Node(id, "select", options=_options(options), value=value)


def color_picker(id, value):
    return Node(id, "color_picker", value=value)


def progress(id, value=None):
    return Node(id, "progress", value=value)


def spinner(id):
    return Node(id, "spinner")


def badge(id, value):
    return Node(id, "badge", text=value)


def pill(id, value):
    return Node(id, "pill", text=value)


def menu(id, items):
    return Node(id, "menu", items=[m.to_dict() if isinstance(m, MenuItem) else m for m in items])


def context_menu(id, items):
    return Node(id, "context_menu", items=[m.to_dict() if isinstance(m, MenuItem) else m for m in items])


def table(id, columns, count):
    return Node(id, "table", columns=columns, count=count)


def tree(id, nodes):
    return Node(id, "tree", nodes=nodes)


def list_node(id, key, count, row_height=24):
    return Node(id, "list", key=key, count=count, row_height=row_height)


def canvas(id, width=None, height=None):
    n = Node(id, "canvas")
    n.set("width", width or Size.fill(1))
    n.set("height", height or Size.px(120))
    return n


def rich_text(id, spans):
    return Node(id, "rich_text", spans=[s.to_dict() if isinstance(s, Span) else s for s in spans])


def code(id, value, language=""):
    return Node(id, "code", text=value, language=language)


def _options(options):
    return [{"value": v, "label": l} for v, l in options]


class MenuItem:
    """An entry of a menu: MenuItem.item(id, label) or MenuItem.separator()."""

    def __init__(self, id=None, label=None, separator=False):
        self.id = id
        self.label = label
        self.separator = separator
        self.fields = {}
        self.children = []

    @staticmethod
    def item(id, label):
        return MenuItem(id, label)

    @staticmethod
    def separator():
        return MenuItem(separator=True)

    @staticmethod
    def submenu(id, label, children):
        m = MenuItem(id, label)
        m.children = list(children)
        return m

    def icon(self, name):
        self.fields["icon"] = name
        return self

    def shortcut(self, text):
        self.fields["shortcut"] = text
        return self

    def enabled(self, on):
        self.fields["enabled"] = on
        return self

    def checked(self, on):
        self.fields["checked"] = on
        return self

    def to_dict(self):
        if self.separator:
            return {"separator": True}
        out = {"id": self.id, "label": self.label, "enabled": self.fields.get("enabled", True)}
        out.update({k: v for k, v in self.fields.items() if k != "enabled"})
        if self.children:
            out["children"] = [c.to_dict() for c in self.children]
        return out


class Span:
    def __init__(self, text, style=None, color=None):
        self.text = text
        self.style = style
        self.color = color

    def to_dict(self):
        out = {"text": self.text}
        if self.style:
            out["style"] = self.style
        if self.color:
            out["color"] = self.color
        return out

