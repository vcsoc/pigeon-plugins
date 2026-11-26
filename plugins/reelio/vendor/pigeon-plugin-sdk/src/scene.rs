//! Scene tree (section 5): a [`Node`] is `{id, kind, ...props, children?}`.
//!
//! Nodes are built with fluent helpers. Every helper sets a documented prop; anything not covered
//! can be set with [`Node::set`], so newer host features never need an SDK release:
//!
//! ```
//! use pigeon_plugin_sdk::scene::{Node, Size};
//! let scene = Node::column("root").gap(8.0).padding(12.0).child(
//!     Node::text("title", "Reelio").style("heading"),
//! ).child(
//!     Node::button("go", "Open media").icon("folder-open").variant("primary").on(&["click"]),
//! );
//! assert_eq!(scene.kind, "column");
//! ```

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::{ProtocolError, MAX_SCENE_BYTES, MAX_SCENE_NODES};

/// A color: `#rrggbb` / `#rrggbbaa`, or a theme token name such as `"fg_weak"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Color(pub String);

impl Color {
    /// `#rrggbb`.
    pub fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(format!("#{r:02x}{g:02x}{b:02x}"))
    }
    /// `#rrggbbaa`.
    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color(format!("#{r:02x}{g:02x}{b:02x}{a:02x}"))
    }
    /// A theme token, resolved by the host (`"accent"`, `"bg_panel"`, ...).
    pub fn token(name: &str) -> Color {
        Color(name.to_string())
    }
    /// Parse `#rrggbb[aa]` or a token name. Returns `None` for anything else.
    pub fn parse(text: &str) -> Option<Color> {
        if let Some(hex) = text.strip_prefix('#') {
            let ok =
                (hex.len() == 6 || hex.len() == 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
            return ok.then(|| Color(text.to_ascii_lowercase()));
        }
        let ok = !text.is_empty()
            && text.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        ok.then(|| Color(text.to_string()))
    }
}

impl From<&str> for Color {
    fn from(s: &str) -> Color {
        Color(s.to_string())
    }
}

impl From<String> for Color {
    fn from(s: String) -> Color {
        Color(s)
    }
}

/// A layout length: `{"px":n}`, `{"fill":weight}` or `"auto"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Px(f32),
    Fill(f32),
    Auto,
}

impl Size {
    pub fn px(n: f32) -> Size {
        Size::Px(n)
    }
    pub fn fill(weight: f32) -> Size {
        Size::Fill(weight)
    }
    pub fn to_value(self) -> Value {
        match self {
            Size::Px(n) => serde_json::json!({ "px": n }),
            Size::Fill(w) => serde_json::json!({ "fill": w }),
            Size::Auto => Value::from("auto"),
        }
    }
}

impl Serialize for Size {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Size, D::Error> {
        let v = Value::deserialize(d)?;
        match &v {
            Value::String(s) if s == "auto" => Ok(Size::Auto),
            Value::Object(o) => {
                if let Some(n) = o.get("px").and_then(Value::as_f64) {
                    Ok(Size::Px(n as f32))
                } else if let Some(n) = o.get("fill").and_then(Value::as_f64) {
                    Ok(Size::Fill(n as f32))
                } else {
                    Err(serde::de::Error::custom("size object needs px or fill"))
                }
            }
            _ => Err(serde::de::Error::custom(
                "size must be {px}, {fill} or \"auto\"",
            )),
        }
    }
}

/// A scene node. `children` is the only typed child list; every other prop lives in `props`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    #[serde(flatten)]
    pub props: Map<String, Value>,
}

impl Node {
    /// A bare node of any kind. Prefer the typed constructors below for known kinds.
    pub fn new(id: impl Into<String>, kind: impl Into<String>) -> Node {
        Node {
            id: id.into(),
            kind: kind.into(),
            children: Vec::new(),
            props: Map::new(),
        }
    }

    /// Set any prop (escape hatch; also how a plugin uses newer host props).
    pub fn set(mut self, key: &str, value: impl Into<Value>) -> Node {
        self.props.insert(key.to_string(), value.into());
        self
    }

    /// Read a prop.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.props.get(key)
    }

    /// Append one child.
    pub fn child(mut self, child: Node) -> Node {
        self.children.push(child);
        self
    }

    /// Append several children.
    pub fn children_from<I: IntoIterator<Item = Node>>(mut self, children: I) -> Node {
        self.children.extend(children);
        self
    }

    /// Prop setters shared by every kind.
    pub fn width(self, s: Size) -> Node {
        self.set("width", s.to_value())
    }
    pub fn height(self, s: Size) -> Node {
        self.set("height", s.to_value())
    }
    pub fn min_width(self, px: f32) -> Node {
        self.set("min_width", px)
    }
    pub fn min_height(self, px: f32) -> Node {
        self.set("min_height", px)
    }
    pub fn max_width(self, px: f32) -> Node {
        self.set("max_width", px)
    }
    pub fn max_height(self, px: f32) -> Node {
        self.set("max_height", px)
    }
    pub fn padding(self, px: f32) -> Node {
        self.set("padding", px)
    }
    pub fn margin(self, px: f32) -> Node {
        self.set("margin", px)
    }
    pub fn gap(self, px: f32) -> Node {
        self.set("gap", px)
    }
    pub fn align(self, a: &str) -> Node {
        self.set("align", a)
    }
    pub fn justify(self, j: &str) -> Node {
        self.set("justify", j)
    }
    pub fn visible(self, v: bool) -> Node {
        self.set("visible", v)
    }
    pub fn enabled(self, v: bool) -> Node {
        self.set("enabled", v)
    }
    pub fn tooltip(self, text: &str) -> Node {
        self.set("tooltip", text)
    }
    pub fn bg(self, c: impl Into<Color>) -> Node {
        self.set("bg", c.into().0)
    }
    pub fn fg(self, c: impl Into<Color>) -> Node {
        self.set("fg", c.into().0)
    }
    pub fn border(self, c: impl Into<Color>) -> Node {
        self.set("border", c.into().0)
    }
    pub fn radius(self, px: f32) -> Node {
        self.set("radius", px)
    }
    /// Screen-reader label and role (`accessibility`).
    pub fn accessible(self, label: &str, role: &str) -> Node {
        self.set(
            "accessibility",
            serde_json::json!({ "label": label, "role": role }),
        )
    }
    /// Stable hint used by the host for state restoration.
    pub fn id_hint(self, hint: &str) -> Node {
        self.set("id_hint", hint)
    }

    // ---- layout kinds ----

    pub fn row(id: impl Into<String>) -> Node {
        Node::new(id, "row")
    }
    pub fn column(id: impl Into<String>) -> Node {
        Node::new(id, "column")
    }
    pub fn stack(id: impl Into<String>) -> Node {
        Node::new(id, "stack")
    }
    pub fn wrap(id: impl Into<String>) -> Node {
        Node::new(id, "wrap")
    }
    /// A grid with a fixed number of `columns`, or `min_cell` px cells when `columns` is 0.
    pub fn grid(id: impl Into<String>, columns: u32) -> Node {
        let mut n = Node::new(id, "grid");
        if columns > 0 {
            n = n.set("columns", columns);
        }
        n
    }
    pub fn grid_min_cell(id: impl Into<String>, min_cell: f32) -> Node {
        Node::new(id, "grid").set("min_cell", min_cell)
    }
    /// A split with two or more children. `sizes` are fractions (sum to 1).
    pub fn split(id: impl Into<String>, direction: &str, sizes: &[f32]) -> Node {
        Node::new(id, "split")
            .set("direction", direction)
            .set("sizes", sizes.to_vec())
    }
    pub fn scroll(id: impl Into<String>, axis: &str) -> Node {
        Node::new(id, "scroll")
            .set("axis", axis)
            .set("show_bars", true)
    }
    /// Tabs; children are selected by tab id (child id == tab id).
    pub fn tabs(id: impl Into<String>, tabs: &[(&str, &str)], active: &str) -> Node {
        let tabs: Vec<Value> = tabs
            .iter()
            .map(|(id, label)| serde_json::json!({ "id": id, "label": label }))
            .collect();
        Node::new(id, "tabs")
            .set("tabs", tabs)
            .set("active", active)
    }
    pub fn collapsible(id: impl Into<String>, title: &str, open: bool) -> Node {
        Node::new(id, "collapsible")
            .set("title", title)
            .set("open", open)
    }
    pub fn panel(id: impl Into<String>) -> Node {
        Node::new(id, "panel")
    }
    pub fn spacer(id: impl Into<String>) -> Node {
        Node::new(id, "spacer")
    }
    pub fn divider(id: impl Into<String>) -> Node {
        Node::new(id, "divider")
    }

    // ---- content and controls ----

    /// Text. Use [`Node::style`] with `heading`, `title`, `body`, `caption`, `code`, `mono`.
    pub fn text(id: impl Into<String>, text: &str) -> Node {
        Node::new(id, "text").set("text", text)
    }
    pub fn style(self, style: &str) -> Node {
        self.set("style", style)
    }
    pub fn wrap_text(self, on: bool) -> Node {
        self.set("wrap", on)
    }
    pub fn selectable(self, on: bool) -> Node {
        self.set("selectable", on)
    }
    pub fn icon(self, name: &str) -> Node {
        self.set("icon", name)
    }
    pub fn label(self, text: &str) -> Node {
        self.set("label", text)
    }
    pub fn variant(self, v: &str) -> Node {
        self.set("variant", v)
    }
    pub fn toggled(self, on: bool) -> Node {
        self.set("toggled", on)
    }
    pub fn shortcut(self, text: &str) -> Node {
        self.set("shortcut", text)
    }
    /// Events the plugin wants: any of `click`, `double_click`, `context`, `change`, `submit`,
    /// `blur`, `scroll`, `list.window`, `pointer`, `wheel`, `key`, `drop`, `hover`.
    pub fn on(self, events: &[&str]) -> Node {
        self.set(
            "on",
            events.iter().map(|e| Value::from(*e)).collect::<Vec<_>>(),
        )
    }

    pub fn button(id: impl Into<String>, label: &str) -> Node {
        Node::new(id, "button").set("label", label)
    }
    pub fn icon_button(id: impl Into<String>, icon: &str, tooltip: &str) -> Node {
        Node::new(id, "icon_button")
            .set("icon", icon)
            .set("tooltip", tooltip)
    }
    /// An image from a resource id. `fit`: contain, cover, fill or none.
    pub fn image(id: impl Into<String>, resource: &str, fit: &str) -> Node {
        Node::new(id, "image")
            .set("resource", resource)
            .set("fit", fit)
    }
    pub fn checkbox(id: impl Into<String>, label: &str, checked: bool) -> Node {
        Node::new(id, "checkbox")
            .set("label", label)
            .set("value", checked)
    }
    pub fn switch(id: impl Into<String>, label: &str, on: bool) -> Node {
        Node::new(id, "switch").set("label", label).set("value", on)
    }
    pub fn radio_group(id: impl Into<String>, options: &[(&str, &str)], value: &str) -> Node {
        Node::new(id, "radio_group")
            .set("options", options_value(options))
            .set("value", value)
    }
    pub fn segmented(id: impl Into<String>, options: &[(&str, &str)], value: &str) -> Node {
        Node::new(id, "segmented")
            .set("options", options_value(options))
            .set("value", value)
    }
    /// A controlled text input. `rev` must increase whenever the plugin changes `value` itself.
    pub fn text_input(id: impl Into<String>, value: &str, rev: u64) -> Node {
        Node::new(id, "text_input")
            .set("value", value)
            .set("rev", rev)
    }
    pub fn number_input(id: impl Into<String>, value: f64, rev: u64) -> Node {
        Node::new(id, "number_input")
            .set("value", value)
            .set("rev", rev)
    }
    pub fn slider(id: impl Into<String>, value: f64, min: f64, max: f64, rev: u64) -> Node {
        Node::new(id, "slider")
            .set("value", value)
            .set("min", min)
            .set("max", max)
            .set("rev", rev)
    }
    pub fn range_slider(
        id: impl Into<String>,
        low: f64,
        high: f64,
        min: f64,
        max: f64,
        rev: u64,
    ) -> Node {
        Node::new(id, "range_slider")
            .set("value", vec![low, high])
            .set("min", min)
            .set("max", max)
            .set("rev", rev)
    }
    pub fn select(id: impl Into<String>, options: &[(&str, &str)], value: &str) -> Node {
        Node::new(id, "select")
            .set("options", options_value(options))
            .set("value", value)
    }
    pub fn color_picker(id: impl Into<String>, color: impl Into<Color>) -> Node {
        Node::new(id, "color_picker").set("value", color.into().0)
    }
    /// Determinate progress when `value` is `Some(0.0..=1.0)`, indeterminate when `None`.
    pub fn progress(id: impl Into<String>, value: Option<f64>) -> Node {
        Node::new(id, "progress").set("value", value.map(Value::from).unwrap_or(Value::Null))
    }
    pub fn spinner(id: impl Into<String>) -> Node {
        Node::new(id, "spinner")
    }
    pub fn badge(id: impl Into<String>, text: &str) -> Node {
        Node::new(id, "badge").set("text", text)
    }
    pub fn pill(id: impl Into<String>, text: &str) -> Node {
        Node::new(id, "pill").set("text", text)
    }
    /// An anchored popup menu. `items` are built with [`MenuItem`].
    pub fn menu(id: impl Into<String>, items: Vec<MenuItem>) -> Node {
        Node::new(id, "menu").set(
            "items",
            items
                .into_iter()
                .map(MenuItem::into_value)
                .collect::<Vec<_>>(),
        )
    }
    /// Attach a context menu to any node: it opens on the `context` event.
    pub fn context_menu(id: impl Into<String>, items: Vec<MenuItem>) -> Node {
        Node::new(id, "context_menu").set(
            "items",
            items
                .into_iter()
                .map(MenuItem::into_value)
                .collect::<Vec<_>>(),
        )
    }
    pub fn tooltip_node(id: impl Into<String>, text: &str) -> Node {
        Node::new(id, "tooltip").set("text", text)
    }
    /// A virtualized table: `columns` are `{id,title,width}` values; rows are supplied in a window.
    pub fn table(id: impl Into<String>, columns: Value, count: u64) -> Node {
        Node::new(id, "table")
            .set("columns", columns)
            .set("count", count)
    }
    pub fn tree(id: impl Into<String>, nodes: Value) -> Node {
        Node::new(id, "tree").set("nodes", nodes)
    }
    /// A virtualized list of `count` rows. Rows are sent with `list.rows` after a `list.window`
    /// event; `row_height` is a px number or `"variable"`.
    pub fn list(id: impl Into<String>, key: &str, count: u64, row_height: Value) -> Node {
        Node::new(id, "list")
            .set("key", key)
            .set("count", count)
            .set("row_height", row_height)
    }
    /// A custom-drawn canvas. Attach the draw list with [`Node::draw`].
    pub fn canvas(id: impl Into<String>, width: Size, height: Size) -> Node {
        Node::new(id, "canvas").width(width).height(height)
    }
    pub fn rich_text(id: impl Into<String>, spans: Vec<Span>) -> Node {
        Node::new(id, "rich_text").set(
            "spans",
            spans.into_iter().map(Span::into_value).collect::<Vec<_>>(),
        )
    }
    pub fn code(id: impl Into<String>, text: &str, language: &str) -> Node {
        Node::new(id, "code")
            .set("text", text)
            .set("language", language)
    }

    /// Canvas: attach a full draw list (`{frame, clear, ops}`).
    pub fn draw(self, list: &crate::canvas::DrawList, frame: u64) -> Node {
        self.set("draw", list.to_draw_value(frame))
    }

    /// Canvas: which events the host should send (`pointer`, `wheel`, `keys`, `drop`, `focusable`).
    pub fn interactive(self, pointer: bool, wheel: bool, keys: bool) -> Node {
        self.set(
            "interactive",
            serde_json::json!({ "pointer": pointer, "wheel": wheel, "keys": keys, "coalesce": "move" }),
        )
    }

    /// Canvas: pointer cursor name (`default`, `pointer`, `grab`, `ew-resize`, ...).
    pub fn cursor(self, name: &str) -> Node {
        self.set("cursor", name)
    }

    /// Text input: `multiline`, `password`, `monospace`, `commit` (change|submit|blur), placeholder.
    pub fn placeholder(self, text: &str) -> Node {
        self.set("placeholder", text)
    }
    pub fn multiline(self, on: bool) -> Node {
        self.set("multiline", on)
    }
    pub fn commit(self, mode: &str) -> Node {
        self.set("commit", mode)
    }

    /// Visit this node and every descendant.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }

    /// Count nodes in this subtree.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    /// Check the documented scene rules: unique ids, the node limit, the byte limit, and sane
    /// ids. Returns the first problem found. Hosts reject a scene with the same rules, so a
    /// plugin should call this before `ui.set` (the SDK does it for you).
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let mut seen = HashSet::new();
        let mut count = 0usize;
        let mut problem: Option<String> = None;
        self.walk(&mut |n| {
            count += 1;
            if problem.is_some() {
                return;
            }
            if n.id.is_empty() {
                problem = Some("node with an empty id".into());
            } else if n.kind.is_empty() {
                problem = Some(format!("node {} has no kind", n.id));
            } else if !seen.insert(n.id.as_str()) {
                problem = Some(format!("duplicate node id {}", n.id));
            }
        });
        if let Some(p) = problem {
            return Err(ProtocolError::Limit(p));
        }
        if count > MAX_SCENE_NODES {
            return Err(ProtocolError::Limit(format!(
                "scene has {count} nodes (max {MAX_SCENE_NODES})"
            )));
        }
        let bytes = serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if bytes > MAX_SCENE_BYTES {
            return Err(ProtocolError::Limit(format!(
                "scene is {bytes} bytes (max {MAX_SCENE_BYTES})"
            )));
        }
        Ok(())
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

fn options_value(options: &[(&str, &str)]) -> Vec<Value> {
    options
        .iter()
        .map(|(v, l)| serde_json::json!({ "value": v, "label": l }))
        .collect()
}

/// One entry of a `menu` / `context_menu`.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItem {
    Item {
        id: String,
        label: String,
        icon: Option<String>,
        shortcut: Option<String>,
        enabled: bool,
        checked: Option<bool>,
        children: Vec<MenuItem>,
    },
    Separator,
}

impl MenuItem {
    /// A normal item.
    pub fn item(id: &str, label: &str) -> MenuItem {
        MenuItem::Item {
            id: id.into(),
            label: label.into(),
            icon: None,
            shortcut: None,
            enabled: true,
            checked: None,
            children: Vec::new(),
        }
    }
    pub fn separator() -> MenuItem {
        MenuItem::Separator
    }
    /// A submenu.
    pub fn submenu(id: &str, label: &str, children: Vec<MenuItem>) -> MenuItem {
        match MenuItem::item(id, label) {
            MenuItem::Item {
                id,
                label,
                icon,
                shortcut,
                enabled,
                checked,
                ..
            } => MenuItem::Item {
                id,
                label,
                icon,
                shortcut,
                enabled,
                checked,
                children,
            },
            other => other,
        }
    }
    pub fn icon(self, name: &str) -> MenuItem {
        self.map_item(|i| i.icon = Some(name.into()))
    }
    pub fn shortcut(self, text: &str) -> MenuItem {
        self.map_item(|i| i.shortcut = Some(text.into()))
    }
    pub fn enabled(self, on: bool) -> MenuItem {
        self.map_item(|i| i.enabled = on)
    }
    pub fn checked(self, on: bool) -> MenuItem {
        self.map_item(|i| i.checked = Some(on))
    }
    fn map_item(self, f: impl FnOnce(&mut MenuItemFields)) -> MenuItem {
        match self {
            MenuItem::Item {
                id,
                label,
                icon,
                shortcut,
                enabled,
                checked,
                children,
            } => {
                let mut fields = MenuItemFields {
                    icon,
                    shortcut,
                    enabled,
                    checked,
                };
                f(&mut fields);
                MenuItem::Item {
                    id,
                    label,
                    icon: fields.icon,
                    shortcut: fields.shortcut,
                    enabled: fields.enabled,
                    checked: fields.checked,
                    children,
                }
            }
            MenuItem::Separator => MenuItem::Separator,
        }
    }
    fn into_value(self) -> Value {
        match self {
            MenuItem::Separator => serde_json::json!({ "separator": true }),
            MenuItem::Item {
                id,
                label,
                icon,
                shortcut,
                enabled,
                checked,
                children,
            } => {
                let mut m = Map::new();
                m.insert("id".into(), Value::from(id));
                m.insert("label".into(), Value::from(label));
                if let Some(i) = icon {
                    m.insert("icon".into(), Value::from(i));
                }
                if let Some(s) = shortcut {
                    m.insert("shortcut".into(), Value::from(s));
                }
                m.insert("enabled".into(), Value::from(enabled));
                if let Some(c) = checked {
                    m.insert("checked".into(), Value::from(c));
                }
                if !children.is_empty() {
                    m.insert(
                        "children".into(),
                        Value::Array(children.into_iter().map(MenuItem::into_value).collect()),
                    );
                }
                Value::Object(m)
            }
        }
    }
}

struct MenuItemFields {
    icon: Option<String>,
    shortcut: Option<String>,
    enabled: bool,
    checked: Option<bool>,
}

/// One rich-text span: `{text, style?, color?}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Option<String>,
    pub color: Option<Color>,
}

impl Span {
    pub fn plain(text: &str) -> Span {
        Span {
            text: text.into(),
            style: None,
            color: None,
        }
    }
    pub fn styled(text: &str, style: &str) -> Span {
        Span {
            text: text.into(),
            style: Some(style.into()),
            color: None,
        }
    }
    fn into_value(self) -> Value {
        let mut m = Map::new();
        m.insert("text".into(), Value::from(self.text));
        if let Some(s) = self.style {
            m.insert("style".into(), Value::from(s));
        }
        if let Some(c) = self.color {
            m.insert("color".into(), Value::from(c.0));
        }
        Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_produce_protocol_shapes() {
        let n = Node::column("root")
            .gap(8.0)
            .padding(12.0)
            .child(Node::text("title", "Reelio").style("heading"));
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["id"], "root");
        assert_eq!(v["kind"], "column");
        assert_eq!(v["gap"], 8.0);
        assert_eq!(v["children"][0]["style"], "heading");
    }

    #[test]
    fn size_serializes_as_documented() {
        assert_eq!(Size::Px(10.0).to_value(), serde_json::json!({"px": 10.0}));
        assert_eq!(Size::Fill(1.0).to_value(), serde_json::json!({"fill": 1.0}));
        assert_eq!(Size::Auto.to_value(), serde_json::json!("auto"));
        let back: Size = serde_json::from_value(serde_json::json!({"fill": 2.0})).unwrap();
        assert_eq!(back, Size::Fill(2.0));
        assert!(serde_json::from_value::<Size>(serde_json::json!({"bogus": 1})).is_err());
    }

    #[test]
    fn color_parse_accepts_tokens_and_hex_only() {
        assert_eq!(Color::parse("#1B1d22").unwrap().0, "#1b1d22");
        assert!(Color::parse("#12345").is_none());
        assert_eq!(Color::parse("fg_weak").unwrap().0, "fg_weak");
        assert!(Color::parse("Accent").is_none());
        assert_eq!(Color::rgba(1, 2, 3, 255).0, "#010203ff");
    }

    #[test]
    fn validate_rejects_duplicate_ids_and_accepts_unique_tree() {
        let ok = Node::column("root")
            .child(Node::button("a", "A"))
            .child(Node::button("b", "B"));
        assert!(ok.validate().is_ok());
        let dup = Node::column("root")
            .child(Node::button("a", "A"))
            .child(Node::button("a", "B"));
        let err = dup.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate node id a"));
    }

    #[test]
    fn menu_items_nest() {
        let m = Node::menu(
            "m",
            vec![
                MenuItem::item("open", "Open")
                    .shortcut("Cmd+O")
                    .icon("folder-open"),
                MenuItem::separator(),
                MenuItem::submenu("more", "More", vec![MenuItem::item("x", "X").checked(true)]),
            ],
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["items"][0]["shortcut"], "Cmd+O");
        assert_eq!(v["items"][1]["separator"], true);
        assert_eq!(v["items"][2]["children"][0]["checked"], true);
    }

    #[test]
    fn unknown_props_survive_roundtrip() {
        let raw = serde_json::json!({"id":"x","kind":"text","text":"hi","future_prop":{"a":1}});
        let n: Node = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(n.get("future_prop").unwrap()["a"], 1);
        assert_eq!(serde_json::to_value(&n).unwrap(), raw);
    }
}
