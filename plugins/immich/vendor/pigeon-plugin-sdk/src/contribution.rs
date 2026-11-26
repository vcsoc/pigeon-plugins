//! Slot contributions (section 4): a placeholder a plugin declares, in `ready`, `contribute` or
//! the manifest `ui.contributions`. The host shows it before the process starts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The slot ids of protocol section 4.
pub const SLOTS: &[&str] = &[
    "sidebar.section",
    "sidebar.entry",
    "toolbar.item",
    "workspace.view",
    "inspector.panel",
    "preferences.page",
    "modal",
    "statusbar.item",
    "asset.action",
    "library.source",
    "search.provider",
    "drop.target",
    "command",
];

/// `applies_to`: which assets an action or panel is for. Evaluated by the host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppliesTo {
    /// Any of `image`, `video`, `audio`, `document`.
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default = "one")]
    pub min: u32,
    #[serde(default)]
    pub max: Option<u32>,
    #[serde(default)]
    pub ext: Vec<String>,
}

fn one() -> u32 {
    1
}

impl AppliesTo {
    /// Applies to any number (at least one) of these media kinds.
    pub fn kinds(kinds: &[&str]) -> AppliesTo {
        AppliesTo { kinds: kinds.iter().map(|k| k.to_string()).collect(), min: 1, max: None, ext: vec![] }
    }
    /// Restrict to file extensions (without dots).
    pub fn with_ext(mut self, ext: &[&str]) -> AppliesTo {
        self.ext = ext.iter().map(|e| e.to_string()).collect();
        self
    }
    /// Restrict the selection size.
    pub fn count(mut self, min: u32, max: Option<u32>) -> AppliesTo {
        self.min = min;
        self.max = max;
        self
    }
}

/// One slot contribution. `props` holds the slot-specific properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contribution {
    pub slot: String,
    pub id: String,
    #[serde(flatten)]
    pub props: Map<String, Value>,
}

impl Contribution {
    /// A contribution of any slot; add props with [`Contribution::prop`].
    pub fn new(slot: &str, id: &str) -> Contribution {
        Contribution { slot: slot.into(), id: id.into(), props: Map::new() }
    }

    /// Set any prop.
    pub fn prop(mut self, key: &str, value: impl Into<Value>) -> Contribution {
        self.props.insert(key.into(), value.into());
        self
    }

    pub fn title(self, title: &str) -> Contribution {
        self.prop("title", title)
    }
    pub fn icon(self, icon: &str) -> Contribution {
        self.prop("icon", icon)
    }
    pub fn order(self, order: i64) -> Contribution {
        self.prop("order", order)
    }
    pub fn badge(self, badge: &str) -> Contribution {
        self.prop("badge", badge)
    }
    pub fn applies_to(self, a: AppliesTo) -> Contribution {
        self.prop("applies_to", serde_json::to_value(a).unwrap_or(Value::Null))
    }

    /// `sidebar.section`: a collapsible section in the left navigation.
    pub fn sidebar_section(id: &str, title: &str, icon: &str, order: i64) -> Contribution {
        Contribution::new("sidebar.section", id).title(title).icon(icon).order(order)
    }
    /// `sidebar.entry`: a navigation row that opens a `workspace.view`.
    pub fn sidebar_entry(id: &str, title: &str, icon: &str, opens: &str) -> Contribution {
        Contribution::new("sidebar.entry", id).title(title).icon(icon).prop("opens", opens).order(0)
    }
    /// `toolbar.item`: a button, menu or group in the top toolbar.
    pub fn toolbar_item(id: &str, group: &str, order: i64) -> Contribution {
        Contribution::new("toolbar.item", id).prop("group", group).order(order)
    }
    /// `workspace.view`: the central area while active.
    pub fn workspace_view(id: &str, title: &str, icon: &str) -> Contribution {
        Contribution::new("workspace.view", id).title(title).icon(icon)
    }
    /// `inspector.panel`: a tab in the right inspector.
    pub fn inspector_panel(id: &str, title: &str, icon: &str, order: i64, applies_to: AppliesTo) -> Contribution {
        Contribution::new("inspector.panel", id).title(title).icon(icon).order(order).applies_to(applies_to)
    }
    /// `preferences.page`: a page in Preferences.
    pub fn preferences_page(id: &str, title: &str, icon: &str, order: i64) -> Contribution {
        Contribution::new("preferences.page", id).title(title).icon(icon).order(order)
    }
    /// `modal`: a dialog the plugin opens with `window.open_modal`.
    pub fn modal(id: &str, title: &str, size: &str, modal: bool) -> Contribution {
        Contribution::new("modal", id).title(title).prop("size", size).prop("modal", modal).prop("resizable", true)
    }
    /// `statusbar.item`: `align` is `left` or `right`.
    pub fn statusbar_item(id: &str, align: &str, order: i64) -> Contribution {
        Contribution::new("statusbar.item", id).prop("align", align).order(order)
    }
    /// `asset.action`: an entry in asset menus. `selection` is `one`, `many` or `any`.
    pub fn asset_action(id: &str, title: &str, icon: &str, applies_to: AppliesTo, selection: &str) -> Contribution {
        Contribution::new("asset.action", id)
            .title(title)
            .icon(icon)
            .applies_to(applies_to)
            .prop("selection", selection)
    }
    /// `command`: usable from menus, shortcuts and the command palette.
    pub fn command(id: &str, title: &str, shortcut: Option<&str>) -> Contribution {
        let c = Contribution::new("command", id).title(title);
        match shortcut {
            Some(s) => c.prop("shortcut", s),
            None => c,
        }
    }
    /// `drop.target`: accepts files or assets dropped on a named region.
    pub fn drop_target(id: &str, accepts: &[&str], region: &str) -> Contribution {
        Contribution::new("drop.target", id).prop("accepts", accepts).prop("where", region)
    }
    /// `library.source`: assets shown in the navigation tree and browse grid.
    pub fn library_source(id: &str, title: &str, icon: &str, capabilities: &[&str]) -> Contribution {
        Contribution::new("library.source", id).title(title).icon(icon).prop("capabilities", capabilities)
    }
    /// `search.provider`: an extra search mode in the search bar.
    pub fn search_provider(id: &str, title: &str, icon: &str, mode_id: &str) -> Contribution {
        Contribution::new("search.provider", id).title(title).icon(icon).prop("mode_id", mode_id)
    }

    /// Whether `slot` is one of the section 4 slot ids.
    pub fn is_known_slot(slot: &str) -> bool {
        SLOTS.contains(&slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_entry_matches_manifest_shape() {
        let c = Contribution::sidebar_entry("nav", "Reelio", "film", "main");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v, serde_json::json!({"slot":"sidebar.entry","id":"nav","title":"Reelio","icon":"film","opens":"main","order":0}));
    }

    #[test]
    fn asset_action_applies_to_serializes_null_max() {
        let c = Contribution::asset_action("enh", "Enlarge", "arrows-out", AppliesTo::kinds(&["image"]), "one");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["applies_to"]["max"], Value::Null);
        assert_eq!(v["applies_to"]["kinds"][0], "image");
        assert_eq!(v["selection"], "one");
    }

    #[test]
    fn all_helpers_use_known_slots() {
        let list = vec![
            Contribution::sidebar_section("s", "S", "folder", 1),
            Contribution::sidebar_entry("e", "E", "film", "v"),
            Contribution::toolbar_item("t", "ai", 2),
            Contribution::workspace_view("v", "V", "film"),
            Contribution::inspector_panel("i", "I", "info", 1, AppliesTo::kinds(&["video"])),
            Contribution::preferences_page("p", "P", "gear", 3),
            Contribution::modal("m", "M", "medium", true),
            Contribution::statusbar_item("b", "right", 1),
            Contribution::asset_action("a", "A", "x", AppliesTo::kinds(&["image"]), "many"),
            Contribution::command("c", "C", Some("Cmd+K")),
            Contribution::drop_target("d", &["files"], "workspace"),
            Contribution::library_source("l", "L", "cloud", &["browse"]),
            Contribution::search_provider("q", "Q", "magnifier", "semantic"),
        ];
        for c in list {
            assert!(Contribution::is_known_slot(&c.slot), "{}", c.slot);
        }
        assert_eq!(SLOTS.len(), 13);
    }
}
