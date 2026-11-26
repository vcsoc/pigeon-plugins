//! JSON Schema validation for protocol v2, using the schema files in `schema/ui-v2` embedded at
//! build time (so the mock host always validates against the schemas of the same commit).

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use jsonschema::{Resource, Validator};
use serde_json::Value;

const SCHEMA_FILES: &[(&str, &str)] = &[
    ("common.schema.json", include_str!("../../../../schema/ui-v2/common.schema.json")),
    ("scene.schema.json", include_str!("../../../../schema/ui-v2/scene.schema.json")),
    ("draw.schema.json", include_str!("../../../../schema/ui-v2/draw.schema.json")),
    ("contribution.schema.json", include_str!("../../../../schema/ui-v2/contribution.schema.json")),
    ("to_host.schema.json", include_str!("../../../../schema/ui-v2/to_host.schema.json")),
    ("to_plugin.schema.json", include_str!("../../../../schema/ui-v2/to_plugin.schema.json")),
    ("methods.schema.json", include_str!("../../../../schema/ui-v2/methods.schema.json")),
    ("manifest-ui.schema.json", include_str!("../../../../schema/ui-v2/manifest-ui.schema.json")),
];

/// The icon names the host ships (schema/ui-v2/icons.json).
pub const ICONS_JSON: &str = include_str!("../../../../schema/ui-v2/icons.json");

const BASE: &str = "https://pigeon.app/schema/ui-v2/";

/// One validation failure: where in the document, and what.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", if self.path.is_empty() { "/" } else { &self.path }, self.message)
    }
}

/// Compiled validators for every message direction and every method.
pub struct Schemas {
    to_host: Validator,
    to_plugin: Validator,
    manifest_ui: Validator,
    methods_doc: Value,
    method_validators: Mutex<HashMap<String, Result<Validator, String>>>,
    pub icons: Vec<String>,
}

fn parse_doc(text: &str) -> Value {
    serde_json::from_str(text).expect("embedded schema is valid JSON (checked by the SDK tests)")
}

fn build(root: &Value, docs: &[(String, Value)]) -> Result<Validator, String> {
    let mut opts = jsonschema::options();
    let resources: Vec<(String, Resource)> = docs
        .iter()
        .map(|(uri, doc)| Resource::from_contents(doc.clone()).map(|r| (uri.clone(), r)).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    opts = opts.with_resources(resources.into_iter());
    opts.build(root).map_err(|e| e.to_string())
}

impl Schemas {
    /// Compile every schema. Panics only if the embedded schemas are broken (a repository bug).
    pub fn load() -> Schemas {
        let docs: Vec<(String, Value)> = SCHEMA_FILES
            .iter()
            .map(|(name, text)| (format!("{BASE}{name}"), parse_doc(text)))
            .collect();
        let get = |name: &str| docs.iter().find(|(u, _)| u == &format!("{BASE}{name}")).map(|(_, d)| d.clone()).expect("schema present");
        let to_host = build(&get("to_host.schema.json"), &docs).expect("to_host compiles");
        let to_plugin = build(&get("to_plugin.schema.json"), &docs).expect("to_plugin compiles");
        let manifest_ui = build(&get("manifest-ui.schema.json"), &docs).expect("manifest-ui compiles");
        let icons_doc: Value = parse_doc(ICONS_JSON);
        let icons = icons_doc["names"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        Schemas {
            to_host,
            to_plugin,
            manifest_ui,
            methods_doc: get("methods.schema.json"),
            method_validators: Mutex::new(HashMap::new()),
            icons,
        }
    }

    /// Validate a plugin-to-host message (the root oneOf of to_host.schema.json).
    pub fn check_to_host(&self, msg: &Value) -> Vec<Violation> {
        collect(&self.to_host, msg)
    }

    /// Validate a host-to-plugin message (the root oneOf of to_plugin.schema.json).
    pub fn check_to_plugin(&self, msg: &Value) -> Vec<Violation> {
        collect(&self.to_plugin, msg)
    }

    /// Validate a plugin.json `ui` block.
    pub fn check_manifest_ui(&self, ui: &Value) -> Vec<Violation> {
        collect(&self.manifest_ui, ui)
    }

    /// Validate `params` for a method (`<method>.params`) or a result (`<method>.result`).
    pub fn check_method(&self, key: &str, value: &Value) -> Vec<Violation> {
        let mut cache = match self.method_validators.lock() {
            Ok(c) => c,
            Err(p) => p.into_inner(),
        };
        let entry = cache.entry(key.to_string()).or_insert_with(|| {
            if self.methods_doc["$defs"].get(key).is_none() {
                return Err(format!("no schema for {key}"));
            }
            let root = serde_json::json!({ "$ref": format!("{BASE}methods.schema.json#/$defs/{key}") });
            let docs = vec![(format!("{BASE}methods.schema.json"), self.methods_doc.clone())];
            let mut all: Vec<(String, Value)> = docs;
            // The method schema references common.schema.json for theme/icon definitions.
            if let Some((_, text)) = SCHEMA_FILES.iter().find(|(n, _)| *n == "common.schema.json") {
                all.push((format!("{BASE}common.schema.json"), parse_doc(text)));
            }
            build(&root, &all)
        });
        match entry {
            Ok(v) => collect(v, value),
            Err(e) => vec![Violation { path: String::new(), message: e.clone() }],
        }
    }
}

fn collect(v: &Validator, instance: &Value) -> Vec<Violation> {
    v.iter_errors(instance)
        .map(|e| Violation { path: e.instance_path.to_string(), message: e.to_string() })
        .collect()
}

/// Known method names with their required permission (protocol section 10). `None` means open.
pub fn method_permission(method: &str) -> Option<&'static str> {
    Some(match method {
        "selection.get" | "selection.subscribe" => "selection",
        "library.query" | "library.get" | "library.reveal" => "library.read",
        "library.import" | "library.set_tags" | "library.set_meta" => "library.write",
        "secrets.get" | "secrets.set" | "secrets.delete" => "secrets",
        "dialog.open_file" | "dialog.save_file" | "dialog.pick_folder" => "dialogs",
        "clipboard.read" | "clipboard.write" => "clipboard",
        "shell.open_url" | "shell.open_path" | "shell.reveal" => "shell",
        "window.open_modal" | "window.close_modal" | "window.set_title" => "windows",
        "drag.begin" => "drag",
        _ => return None,
    })
}
