//! Typed protocol v2 messages (see docs/architecture/plugin-ui-protocol.md).
//!
//! Two directions:
//! * [`Outgoing`] is what a plugin sends to the host (`ready`, `ui.set`, `req`, `res`, `pong`, ...).
//!   It derives serde for both directions so the mock host can parse what plugins send.
//! * [`Incoming`] is what the host sends to a plugin (`hello`, `ui.event`, `req`, `res`, `ping`, ...).
//!   Parsed by hand from a `serde_json::Value` so unknown `t` values become [`Incoming::Unknown`]
//!   (forward compatibility) and `ui.event` can carry the flat, per-type payload.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::canvas::DrawOp;
use crate::contribution::Contribution;
use crate::scene::Node;

/// The protocol version this SDK speaks.
pub const PROTOCOL_VERSION: u32 = 2;
/// Maximum size of one JSON Lines message.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Scene limits (section 9).
pub const MAX_SCENE_NODES: usize = 50_000;
pub const MAX_SCENE_BYTES: usize = 4 * 1024 * 1024;
/// Canvas limits (section 7).
pub const MAX_CANVAS_OPS: usize = 20_000;
pub const MAX_CANVAS_BYTES: usize = 4 * 1024 * 1024;
/// Host to plugin event queue capacity (section 9).
pub const HOST_QUEUE_CAPACITY: usize = 512;

/// Errors produced while parsing or building protocol messages.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolError {
    /// The line is not valid JSON or does not have the expected shape.
    Malformed(String),
    /// A message is larger than [`MAX_LINE_BYTES`].
    TooLarge(usize),
    /// A scene or canvas breaks a documented limit.
    Limit(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Malformed(m) => write!(f, "malformed protocol message: {m}"),
            ProtocolError::TooLarge(n) => write!(f, "protocol message too large ({n} bytes)"),
            ProtocolError::Limit(m) => write!(f, "limit exceeded: {m}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Host identity from `hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: String,
    pub version: String,
}

/// Protocol version range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}

/// Theme tokens (section 12). Colors are token-keyed hex strings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default)]
    pub mode: String,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub font: Fonts,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub spacing: f32,
}

fn one() -> f32 {
    1.0
}

impl Theme {
    /// A color token value (for example `"accent"`), if the host sent it.
    pub fn color(&self, token: &str) -> Option<&str> {
        self.colors.get(token).map(String::as_str)
    }
}

/// Font family names chosen by the host.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Fonts {
    #[serde(default)]
    pub ui: String,
    #[serde(default)]
    pub mono: String,
}

/// `hello`: the first line the host sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub host: HostInfo,
    pub protocol: ProtocolRange,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub locale: String,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub session_dir: String,
    #[serde(default)]
    pub features: Vec<String>,
}

/// Plugin identity sent in `ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginId {
    pub id: String,
    pub version: String,
}

/// Modifier keys carried by pointer, wheel and key events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mods {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub meta: bool,
}

/// Rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// `res` error body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// Kinds of resource payloads (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Rgba,
    Png,
    Jpeg,
    Webp,
    Svg,
}

/// `resource.put` payload. `stride` and `premultiplied` apply to `rgba` only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourcePut {
    pub rid: String,
    pub kind: ResourceKind,
    pub file: String,
    pub w: u32,
    pub h: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stride: Option<u32>,
    pub seq: u64,
    #[serde(default)]
    pub premultiplied: bool,
}

/// `ui.patch` operation (feature `scene.patch`). Paths are node ids joined by `/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum PatchOp {
    Set {
        path: String,
        value: Value,
    },
    Insert {
        path: String,
        index: usize,
        node: Node,
    },
    Remove {
        path: String,
    },
    Move {
        path: String,
        index: usize,
    },
}

/// `canvas.draw` fast path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasDraw {
    pub instance: String,
    pub node: String,
    pub frame: u64,
    pub ops: Vec<DrawOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Rect>,
}

/// Mouse / pen events on a canvas (`type: "pointer"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    Cancel,
    Enter,
    Leave,
}

/// Key phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyPhase {
    Down,
    Up,
}

/// The per-node event payload. Carried flat in `ui.event` (`{"t":"ui.event","instance":..,
/// "node":..,"type":"click"}`), tagged by `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeEvent {
    Click,
    DoubleClick,
    Context,
    /// Controlled input edit: the host's buffer and the `rev` it was based on.
    Change {
        value: Value,
        rev: u64,
    },
    Submit {
        value: Value,
        rev: u64,
    },
    Blur {
        value: Value,
        rev: u64,
    },
    Scroll {
        x: f32,
        y: f32,
    },
    ListWindow {
        first: u64,
        last: u64,
        overscan: u64,
    },
    Pointer {
        phase: PointerPhase,
        x: f32,
        y: f32,
        button: i32,
        buttons: u32,
        mods: Mods,
        clicks: u32,
        pointer_id: u32,
        /// Number of pointer moves merged into this event by the host (1 when not merged).
        coalesced: u32,
    },
    Wheel {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
        mods: Mods,
        coalesced: u32,
    },
    Key {
        phase: KeyPhase,
        key: String,
        code: String,
        text: String,
        mods: Mods,
        repeat: bool,
    },
    Drop {
        paths: Vec<String>,
        assets: Vec<Value>,
        x: f32,
        y: f32,
    },
    Hover {
        x: f32,
        y: f32,
    },
    /// Any event type this SDK does not know yet.
    Other {
        kind: String,
        data: Map<String, Value>,
    },
}

impl NodeEvent {
    /// The wire `type` of this event.
    pub fn kind(&self) -> &str {
        match self {
            NodeEvent::Click => "click",
            NodeEvent::DoubleClick => "double_click",
            NodeEvent::Context => "context",
            NodeEvent::Change { .. } => "change",
            NodeEvent::Submit { .. } => "submit",
            NodeEvent::Blur { .. } => "blur",
            NodeEvent::Scroll { .. } => "scroll",
            NodeEvent::ListWindow { .. } => "list.window",
            NodeEvent::Pointer { .. } => "pointer",
            NodeEvent::Wheel { .. } => "wheel",
            NodeEvent::Key { .. } => "key",
            NodeEvent::Drop { .. } => "drop",
            NodeEvent::Hover { .. } => "hover",
            NodeEvent::Other { kind, .. } => kind,
        }
    }

    /// Serialize the event's fields (without `t`, `instance`, `node`).
    fn to_fields(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("type".into(), Value::from(self.kind()));
        let mut put = |k: &str, v: Value| {
            m.insert(k.into(), v);
        };
        match self {
            NodeEvent::Click | NodeEvent::DoubleClick | NodeEvent::Context => {}
            NodeEvent::Change { value, rev }
            | NodeEvent::Submit { value, rev }
            | NodeEvent::Blur { value, rev } => {
                put("value", value.clone());
                put("rev", Value::from(*rev));
            }
            NodeEvent::Scroll { x, y } | NodeEvent::Hover { x, y } => {
                put("x", Value::from(*x));
                put("y", Value::from(*y));
            }
            NodeEvent::ListWindow {
                first,
                last,
                overscan,
            } => {
                put("first", Value::from(*first));
                put("last", Value::from(*last));
                put("overscan", Value::from(*overscan));
            }
            NodeEvent::Pointer {
                phase,
                x,
                y,
                button,
                buttons,
                mods,
                clicks,
                pointer_id,
                coalesced,
            } => {
                put("phase", to_value(phase));
                put("x", Value::from(*x));
                put("y", Value::from(*y));
                put("button", Value::from(*button));
                put("buttons", Value::from(*buttons));
                put("mods", to_value(mods));
                put("clicks", Value::from(*clicks));
                put("pointer_id", Value::from(*pointer_id));
                put("coalesced", Value::from(*coalesced));
            }
            NodeEvent::Wheel {
                dx,
                dy,
                x,
                y,
                mods,
                coalesced,
            } => {
                put("dx", Value::from(*dx));
                put("dy", Value::from(*dy));
                put("x", Value::from(*x));
                put("y", Value::from(*y));
                put("mods", to_value(mods));
                put("coalesced", Value::from(*coalesced));
            }
            NodeEvent::Key {
                phase,
                key,
                code,
                text,
                mods,
                repeat,
            } => {
                put("phase", to_value(phase));
                put("key", Value::from(key.as_str()));
                put("code", Value::from(code.as_str()));
                put("text", Value::from(text.as_str()));
                put("mods", to_value(mods));
                put("repeat", Value::from(*repeat));
            }
            NodeEvent::Drop {
                paths,
                assets,
                x,
                y,
            } => {
                put("paths", Value::from(paths.clone()));
                put("assets", Value::Array(assets.clone()));
                put("x", Value::from(*x));
                put("y", Value::from(*y));
            }
            NodeEvent::Other { data, .. } => {
                for (k, v) in data {
                    m.insert(k.clone(), v.clone());
                }
            }
        }
        m
    }

    /// Parse the fields of one event (the object that carries `type`). Unknown types become
    /// [`NodeEvent::Other`]; malformed known types also fall back to `Other` (never a panic).
    pub fn from_fields(obj: &Map<String, Value>) -> NodeEvent {
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let f32_of = |k: &str| obj.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let u64_of = |k: &str| obj.get(k).and_then(Value::as_u64).unwrap_or(0);
        let mods = || {
            obj.get("mods")
                .and_then(|v| serde_json::from_value::<Mods>(v.clone()).ok())
                .unwrap_or_default()
        };
        let value = || obj.get("value").cloned().unwrap_or(Value::Null);
        let parsed = match kind.as_str() {
            "click" => Some(NodeEvent::Click),
            "double_click" => Some(NodeEvent::DoubleClick),
            "context" => Some(NodeEvent::Context),
            "change" => Some(NodeEvent::Change {
                value: value(),
                rev: u64_of("rev"),
            }),
            "submit" => Some(NodeEvent::Submit {
                value: value(),
                rev: u64_of("rev"),
            }),
            "blur" => Some(NodeEvent::Blur {
                value: value(),
                rev: u64_of("rev"),
            }),
            "scroll" => Some(NodeEvent::Scroll {
                x: f32_of("x"),
                y: f32_of("y"),
            }),
            "list.window" => Some(NodeEvent::ListWindow {
                first: u64_of("first"),
                last: u64_of("last"),
                overscan: u64_of("overscan"),
            }),
            "pointer" => serde_json::from_value::<PointerPhase>(
                obj.get("phase").cloned().unwrap_or(Value::Null),
            )
            .ok()
            .map(|phase| NodeEvent::Pointer {
                phase,
                x: f32_of("x"),
                y: f32_of("y"),
                button: obj.get("button").and_then(Value::as_i64).unwrap_or(0) as i32,
                buttons: u64_of("buttons") as u32,
                mods: mods(),
                clicks: u64_of("clicks") as u32,
                pointer_id: u64_of("pointer_id") as u32,
                coalesced: obj.get("coalesced").and_then(Value::as_u64).unwrap_or(1) as u32,
            }),
            "wheel" => Some(NodeEvent::Wheel {
                dx: f32_of("dx"),
                dy: f32_of("dy"),
                x: f32_of("x"),
                y: f32_of("y"),
                mods: mods(),
                coalesced: obj.get("coalesced").and_then(Value::as_u64).unwrap_or(1) as u32,
            }),
            "key" => {
                serde_json::from_value::<KeyPhase>(obj.get("phase").cloned().unwrap_or(Value::Null))
                    .ok()
                    .map(|phase| NodeEvent::Key {
                        phase,
                        key: obj.get("key").and_then(Value::as_str).unwrap_or("").into(),
                        code: obj.get("code").and_then(Value::as_str).unwrap_or("").into(),
                        text: obj.get("text").and_then(Value::as_str).unwrap_or("").into(),
                        mods: mods(),
                        repeat: obj.get("repeat").and_then(Value::as_bool).unwrap_or(false),
                    })
            }
            "drop" => Some(NodeEvent::Drop {
                paths: obj
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                assets: obj
                    .get("assets")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                x: f32_of("x"),
                y: f32_of("y"),
            }),
            "hover" => Some(NodeEvent::Hover {
                x: f32_of("x"),
                y: f32_of("y"),
            }),
            _ => None,
        };
        parsed.unwrap_or_else(|| {
            let data = obj
                .iter()
                .filter(|(k, _)| k.as_str() != "type")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            NodeEvent::Other { kind, data }
        })
    }
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// A host-to-plugin message, parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Hello(Hello),
    /// `ping` (answered with `pong` by the protocol thread).
    Ping {
        id: Option<String>,
    },
    /// `ui.event` for one node of one slot instance.
    UiEvent {
        instance: String,
        node: String,
        event: NodeEvent,
    },
    /// `resource.ack`: the host consumed `seq` of `rid`.
    ResourceAck {
        rid: String,
        seq: u64,
    },
    /// `res`: answer to a request this plugin sent.
    Response {
        id: String,
        result: Option<Value>,
        error: Option<ErrorBody>,
    },
    /// `req`: a provider request from the host (`source.*`, `search.query`, ...).
    Request {
        id: String,
        method: String,
        params: Value,
    },
    /// `visibility` (alias `ui.visible`): an instance became visible or hidden.
    Visibility {
        instance: String,
        visible: bool,
    },
    /// `focus`: the plugin's window gained or lost focus.
    Focus {
        has: bool,
    },
    /// `resize`: an instance changed size.
    Resize {
        instance: String,
        w: f32,
        h: f32,
        scale: f32,
    },
    /// `theme.changed`.
    ThemeChanged(Theme),
    /// `locale.changed`.
    LocaleChanged {
        locale: String,
    },
    /// `selection.changed`.
    SelectionChanged {
        ids: Vec<String>,
        kinds: Vec<String>,
        paths: Vec<String>,
    },
    /// `cancel {job}`.
    Cancel {
        job: String,
    },
    /// `command`: a command, asset action, toolbar item or shortcut fired.
    Command {
        command: String,
        args: Value,
        context: Value,
    },
    /// `drop`: files or assets were dropped on a `drop.target`.
    Drop {
        target: String,
        paths: Vec<String>,
        assets: Vec<Value>,
    },
    /// `error`: the host rejected something the plugin sent (for example `canvas_too_large`).
    Error {
        code: String,
        message: String,
        ref_id: Option<String>,
    },
    /// `shutdown`: exit cleanly.
    Shutdown,
    /// Any `t` this SDK does not know (ignored by the SDK, kept for tooling).
    Unknown {
        t: String,
        raw: Value,
    },
}

fn str_field(obj: &Map<String, Value>, k: &str) -> String {
    obj.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn vec_str(obj: &Map<String, Value>, k: &str) -> Vec<String> {
    obj.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

impl Incoming {
    /// Parse one JSON line (or any JSON value) sent by the host.
    pub fn parse(line: &str) -> Result<Incoming, ProtocolError> {
        if line.len() > MAX_LINE_BYTES {
            return Err(ProtocolError::TooLarge(line.len()));
        }
        let value: Value =
            serde_json::from_str(line).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
        Incoming::from_value(value)
    }

    /// Interpret an already-parsed JSON object.
    pub fn from_value(value: Value) -> Result<Incoming, ProtocolError> {
        let obj = match value {
            Value::Object(o) => o,
            _ => return Err(ProtocolError::Malformed("message is not an object".into())),
        };
        let t = obj
            .get("t")
            .and_then(Value::as_str)
            .ok_or_else(|| ProtocolError::Malformed("missing t".into()))?
            .to_string();
        let id = obj.get("id").and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        let bad = |e: serde_json::Error| ProtocolError::Malformed(format!("{t}: {e}"));
        let msg = match t.as_str() {
            "hello" => {
                Incoming::Hello(serde_json::from_value(Value::Object(obj.clone())).map_err(bad)?)
            }
            "ping" => Incoming::Ping { id },
            "ui.event" => Incoming::UiEvent {
                instance: str_field(&obj, "instance"),
                node: str_field(&obj, "node"),
                event: NodeEvent::from_fields(&obj),
            },
            "resource.ack" => Incoming::ResourceAck {
                rid: str_field(&obj, "rid"),
                seq: obj.get("seq").and_then(Value::as_u64).unwrap_or(0),
            },
            "res" => Incoming::Response {
                id: id.clone().unwrap_or_default(),
                result: obj.get("result").cloned(),
                error: obj
                    .get("error")
                    .and_then(|e| serde_json::from_value(e.clone()).ok()),
            },
            "req" => Incoming::Request {
                id: id.clone().unwrap_or_default(),
                method: str_field(&obj, "method"),
                params: obj.get("params").cloned().unwrap_or(Value::Null),
            },
            "visibility" | "ui.visible" => Incoming::Visibility {
                instance: str_field(&obj, "instance"),
                visible: obj.get("visible").and_then(Value::as_bool).unwrap_or(false),
            },
            "focus" => Incoming::Focus {
                has: obj.get("has").and_then(Value::as_bool).unwrap_or(false),
            },
            "resize" => Incoming::Resize {
                instance: str_field(&obj, "instance"),
                w: obj.get("w").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                h: obj.get("h").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                scale: obj.get("scale").and_then(Value::as_f64).unwrap_or(1.0) as f32,
            },
            "theme.changed" => Incoming::ThemeChanged(
                serde_json::from_value(obj.get("theme").cloned().unwrap_or(Value::Null))
                    .map_err(bad)?,
            ),
            "locale.changed" => Incoming::LocaleChanged {
                locale: str_field(&obj, "locale"),
            },
            "selection.changed" => Incoming::SelectionChanged {
                ids: vec_str(&obj, "ids"),
                kinds: vec_str(&obj, "kinds"),
                paths: vec_str(&obj, "paths"),
            },
            "cancel" => Incoming::Cancel {
                job: str_field(&obj, "job"),
            },
            "command" => Incoming::Command {
                command: str_field(&obj, "command"),
                args: obj.get("args").cloned().unwrap_or(Value::Null),
                context: obj.get("context").cloned().unwrap_or(Value::Null),
            },
            "drop" => Incoming::Drop {
                target: str_field(&obj, "target"),
                paths: vec_str(&obj, "paths"),
                assets: obj
                    .get("assets")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            },
            "error" => Incoming::Error {
                code: str_field(&obj, "code"),
                message: str_field(&obj, "message"),
                ref_id: obj.get("ref").and_then(Value::as_str).map(String::from),
            },
            "shutdown" => Incoming::Shutdown,
            _ => Incoming::Unknown {
                t: t.clone(),
                raw: Value::Object(obj),
            },
        };
        Ok(msg)
    }

    /// Serialize to the wire form (used by the mock host and tests).
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("v".into(), Value::from(PROTOCOL_VERSION));
        let mut put = |k: &str, v: Value| {
            m.insert(k.into(), v);
        };
        let t = match self {
            Incoming::Hello(h) => {
                let mut v = to_value(h);
                if let Value::Object(o) = &mut v {
                    o.insert("v".into(), Value::from(PROTOCOL_VERSION));
                    o.insert("t".into(), Value::from("hello"));
                    return v;
                }
                "hello"
            }
            Incoming::Ping { id } => {
                if let Some(id) = id {
                    put("id", Value::from(id.as_str()));
                }
                "ping"
            }
            Incoming::UiEvent {
                instance,
                node,
                event,
            } => {
                put("instance", Value::from(instance.as_str()));
                put("node", Value::from(node.as_str()));
                for (k, v) in event.to_fields() {
                    m.insert(k, v);
                }
                "ui.event"
            }
            Incoming::ResourceAck { rid, seq } => {
                put("rid", Value::from(rid.as_str()));
                put("seq", Value::from(*seq));
                "resource.ack"
            }
            Incoming::Response { id, result, error } => {
                put("id", Value::from(id.as_str()));
                if let Some(r) = result {
                    put("result", r.clone());
                }
                if let Some(e) = error {
                    put("error", to_value(e));
                }
                "res"
            }
            Incoming::Request { id, method, params } => {
                put("id", Value::from(id.as_str()));
                put("method", Value::from(method.as_str()));
                put("params", params.clone());
                "req"
            }
            Incoming::Visibility { instance, visible } => {
                put("instance", Value::from(instance.as_str()));
                put("visible", Value::from(*visible));
                "visibility"
            }
            Incoming::Focus { has } => {
                put("has", Value::from(*has));
                "focus"
            }
            Incoming::Resize {
                instance,
                w,
                h,
                scale,
            } => {
                put("instance", Value::from(instance.as_str()));
                put("w", Value::from(*w));
                put("h", Value::from(*h));
                put("scale", Value::from(*scale));
                "resize"
            }
            Incoming::ThemeChanged(theme) => {
                put("theme", to_value(theme));
                "theme.changed"
            }
            Incoming::LocaleChanged { locale } => {
                put("locale", Value::from(locale.as_str()));
                "locale.changed"
            }
            Incoming::SelectionChanged { ids, kinds, paths } => {
                put("ids", Value::from(ids.clone()));
                put("kinds", Value::from(kinds.clone()));
                put("paths", Value::from(paths.clone()));
                "selection.changed"
            }
            Incoming::Cancel { job } => {
                put("job", Value::from(job.as_str()));
                "cancel"
            }
            Incoming::Command {
                command,
                args,
                context,
            } => {
                put("command", Value::from(command.as_str()));
                put("args", args.clone());
                put("context", context.clone());
                "command"
            }
            Incoming::Drop {
                target,
                paths,
                assets,
            } => {
                put("target", Value::from(target.as_str()));
                put("paths", Value::from(paths.clone()));
                put("assets", Value::Array(assets.clone()));
                "drop"
            }
            Incoming::Error {
                code,
                message,
                ref_id,
            } => {
                put("code", Value::from(code.as_str()));
                put("message", Value::from(message.as_str()));
                if let Some(r) = ref_id {
                    put("ref", Value::from(r.as_str()));
                }
                "error"
            }
            Incoming::Shutdown => "shutdown",
            Incoming::Unknown { t, raw } => {
                if let Value::Object(o) = raw {
                    return Value::Object(o.clone());
                }
                let _ = t;
                "unknown"
            }
        };
        m.insert("t".into(), Value::from(t));
        Value::Object(m)
    }
}

/// A plugin-to-host message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Outgoing {
    #[serde(rename = "ready")]
    Ready {
        plugin: PluginId,
        protocol: u32,
        contributions: Vec<Contribution>,
    },
    #[serde(rename = "contribute")]
    Contribute { contributions: Vec<Contribution> },
    #[serde(rename = "withdraw")]
    Withdraw { ids: Vec<String> },
    #[serde(rename = "ui.set")]
    UiSet { instance: String, scene: Node },
    #[serde(rename = "ui.patch")]
    UiPatch { instance: String, ops: Vec<PatchOp> },
    #[serde(rename = "ui.focus")]
    UiFocus { instance: String, node: String },
    #[serde(rename = "ui.scroll_to")]
    UiScrollTo {
        instance: String,
        node: String,
        align: String,
    },
    #[serde(rename = "ui.beep")]
    UiBeep {},
    #[serde(rename = "canvas.draw")]
    CanvasDraw(CanvasDraw),
    #[serde(rename = "list.rows")]
    ListRows {
        instance: String,
        node: String,
        start: u64,
        rows: Vec<Node>,
    },
    #[serde(rename = "resource.put")]
    ResourcePut(ResourcePut),
    #[serde(rename = "resource.drop")]
    ResourceDrop { rid: String },
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        params: Value,
    },
    #[serde(rename = "res")]
    Response {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ErrorBody>,
    },
    #[serde(rename = "pong")]
    Pong {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "source.changed")]
    SourceChanged {
        source: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ids: Option<Vec<String>>,
    },
    #[serde(rename = "search.status")]
    SearchStatus { mode_id: String, status: Value },
}

impl Outgoing {
    /// The envelope `v` and `t` are added here. Field order is stable.
    pub fn to_value(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            let mut out = Map::new();
            out.insert("v".into(), Value::from(PROTOCOL_VERSION));
            for (k, val) in std::mem::take(o) {
                out.insert(k, val);
            }
            return Value::Object(out);
        }
        v
    }

    /// Serialize as one JSON line (without the trailing newline), enforcing [`MAX_LINE_BYTES`].
    pub fn to_line(&self) -> Result<String, ProtocolError> {
        let line = serde_json::to_string(&self.to_value())
            .map_err(|e| ProtocolError::Malformed(e.to_string()))?;
        if line.len() > MAX_LINE_BYTES {
            return Err(ProtocolError::TooLarge(line.len()));
        }
        Ok(line)
    }

    /// Parse a plugin message (used by the mock host and tests).
    pub fn parse(line: &str) -> Result<Outgoing, ProtocolError> {
        serde_json::from_str(line).map_err(|e| ProtocolError::Malformed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_hello_with_theme_and_unknown_fields() {
        let line = r##"{"v":2,"t":"hello","host":{"name":"Pigeon","version":"0.4.1"},"protocol":{"min":2,"max":2},
            "theme":{"mode":"dark","scale":2.0,"colors":{"accent":"#3b82f6"},"radius":4,"future":true},
            "locale":"en","scale":2.0,"session_dir":"/tmp/s","features":["canvas"],"new_field":1}"##;
        match Incoming::parse(line).unwrap() {
            Incoming::Hello(h) => {
                assert_eq!(h.host.version, "0.4.1");
                assert_eq!(h.protocol.max, 2);
                assert_eq!(h.theme.color("accent"), Some("#3b82f6"));
                assert_eq!(h.session_dir, "/tmp/s");
                assert!(h.data_dir.is_none());
            }
            other => panic!("expected hello, got {other:?}"),
        }
    }

    #[test]
    fn parses_flat_ui_events() {
        let click = Incoming::parse(
            r#"{"v":2,"t":"ui.event","instance":"hello-ui/nav","node":"go","type":"click"}"#,
        )
        .unwrap();
        assert_eq!(
            click,
            Incoming::UiEvent {
                instance: "hello-ui/nav".into(),
                node: "go".into(),
                event: NodeEvent::Click
            }
        );
        let change = Incoming::parse(
            r#"{"t":"ui.event","instance":"i","node":"name","type":"change","value":"ab","rev":3}"#,
        )
        .unwrap();
        assert!(matches!(
            change,
            Incoming::UiEvent {
                event: NodeEvent::Change { rev: 3, .. },
                ..
            }
        ));
        let ptr = Incoming::parse(
            r#"{"t":"ui.event","instance":"i","node":"tl","type":"pointer","phase":"move","x":4.5,"y":2,"button":0,"buttons":1,"mods":{"shift":true},"clicks":0,"pointer_id":1,"coalesced":5}"#,
        )
        .unwrap();
        match ptr {
            Incoming::UiEvent {
                event:
                    NodeEvent::Pointer {
                        phase,
                        x,
                        coalesced,
                        mods,
                        ..
                    },
                ..
            } => {
                assert_eq!(phase, PointerPhase::Move);
                assert_eq!(x, 4.5);
                assert_eq!(coalesced, 5);
                assert!(mods.shift);
            }
            other => panic!("{other:?}"),
        }
        let list = Incoming::parse(r#"{"t":"ui.event","instance":"i","node":"rows","type":"list.window","first":10,"last":30,"overscan":5}"#).unwrap();
        assert!(matches!(
            list,
            Incoming::UiEvent {
                event: NodeEvent::ListWindow {
                    first: 10,
                    last: 30,
                    overscan: 5
                },
                ..
            }
        ));
    }

    #[test]
    fn unknown_event_types_and_messages_are_kept_not_fatal() {
        let ev = Incoming::parse(
            r#"{"t":"ui.event","instance":"i","node":"n","type":"future_gesture","extra":1}"#,
        )
        .unwrap();
        match ev {
            Incoming::UiEvent {
                event: NodeEvent::Other { kind, data },
                ..
            } => {
                assert_eq!(kind, "future_gesture");
                assert_eq!(data["extra"], 1);
            }
            other => panic!("{other:?}"),
        }
        let unknown = Incoming::parse(r#"{"v":2,"t":"telemetry.v9","x":1}"#).unwrap();
        assert!(matches!(unknown, Incoming::Unknown { ref t, .. } if t == "telemetry.v9"));
    }

    #[test]
    fn parses_responses_requests_pings_and_acks() {
        let res = Incoming::parse(r#"{"t":"res","id":"7","result":{"ok":true}}"#).unwrap();
        assert_eq!(
            res,
            Incoming::Response {
                id: "7".into(),
                result: Some(json!({"ok":true})),
                error: None
            }
        );
        let err = Incoming::parse(
            r#"{"t":"res","id":"8","error":{"code":"permission_denied","message":"no"}}"#,
        )
        .unwrap();
        assert!(
            matches!(err, Incoming::Response { error: Some(ErrorBody { ref code, .. }), .. } if code == "permission_denied")
        );
        let req = Incoming::parse(
            r#"{"t":"req","id":"r1","method":"source.list","params":{"source":"s"}}"#,
        )
        .unwrap();
        assert!(matches!(req, Incoming::Request { ref method, .. } if method == "source.list"));
        assert_eq!(
            Incoming::parse(r#"{"t":"ping","id":"p"}"#).unwrap(),
            Incoming::Ping {
                id: Some("p".into())
            }
        );
        assert_eq!(
            Incoming::parse(r#"{"t":"resource.ack","rid":"thumb:1","seq":4}"#).unwrap(),
            Incoming::ResourceAck {
                rid: "thumb:1".into(),
                seq: 4
            }
        );
        assert!(Incoming::parse("not json").is_err());
        assert!(Incoming::parse("[1,2]").is_err());
    }

    #[test]
    fn oversized_lines_are_rejected() {
        let big = format!(r#"{{"t":"ping","pad":"{}"}}"#, "x".repeat(MAX_LINE_BYTES));
        assert!(matches!(
            Incoming::parse(&big),
            Err(ProtocolError::TooLarge(_))
        ));
    }

    #[test]
    fn outgoing_messages_carry_envelope_and_roundtrip() {
        let scene = crate::scene::Node::column("root")
            .child(crate::scene::Node::button("go", "Go").on(&["click"]));
        let msg = Outgoing::UiSet {
            instance: "hello-ui/main".into(),
            scene,
        };
        let v = msg.to_value();
        assert_eq!(v["v"], 2);
        assert_eq!(v["t"], "ui.set");
        assert_eq!(v["scene"]["children"][0]["on"][0], "click");
        let line = msg.to_line().unwrap();
        assert_eq!(Outgoing::parse(&line).unwrap(), msg);
    }

    #[test]
    fn outgoing_resource_put_matches_spec_example() {
        let put = Outgoing::ResourcePut(ResourcePut {
            rid: "thumb:42".into(),
            kind: ResourceKind::Rgba,
            file: "/s/thumb-42-0.rgba".into(),
            w: 640,
            h: 360,
            stride: Some(2560),
            seq: 7,
            premultiplied: false,
        });
        let v = put.to_value();
        assert_eq!(v["t"], "resource.put");
        assert_eq!(v["kind"], "rgba");
        assert_eq!(v["stride"], 2560);
        assert_eq!(v["seq"], 7);
    }

    #[test]
    fn outgoing_pong_and_req_shapes() {
        assert_eq!(
            Outgoing::Pong {
                id: Some("p".into())
            }
            .to_value(),
            json!({"v":2,"t":"pong","id":"p"})
        );
        let req = Outgoing::Request {
            id: "7".into(),
            method: "library.query".into(),
            params: json!({"limit":5}),
        };
        assert_eq!(
            req.to_value(),
            json!({"v":2,"t":"req","id":"7","method":"library.query","params":{"limit":5}})
        );
    }

    #[test]
    fn incoming_serializes_for_the_mock_host() {
        let ev = Incoming::UiEvent {
            instance: "i".into(),
            node: "tl".into(),
            event: NodeEvent::Wheel {
                dx: 0.0,
                dy: -3.0,
                x: 1.0,
                y: 2.0,
                mods: Mods::default(),
                coalesced: 2,
            },
        };
        let v = ev.to_value();
        assert_eq!(v["t"], "ui.event");
        assert_eq!(v["type"], "wheel");
        assert_eq!(Incoming::from_value(v).unwrap(), ev);
    }
}
