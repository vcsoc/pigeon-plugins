//! Persisted workspace preferences (pool/inspector widths, viewer height, panel visibility).
//! Ports of `workspaceLayout.ts`, `PoolDivider.tsx`, `InspectorDivider.tsx`, `WorkspaceDivider.tsx`.
//! Preferences never dirty a project. Malformed or oversized values fall back to defaults.

use serde_json::Value;

pub const WORKSPACE_LAYOUT_KEY: &str = "reelio.workspace-layout.v1";
pub const POOL_WIDTH_KEY: &str = "reelio.media-pool-width.v1";
pub const INSPECTOR_WIDTH_KEY: &str = "reelio.inspector-width.v1";
pub const VIEWER_HEIGHT_KEY: &str = "reelio.viewer-height.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkspaceLayout {
    pub full_height_left: bool,
    pub left_collapsed: bool,
    pub right_collapsed: bool,
}

/// Only exact `true` values count; unrelated fields are dropped; >512 bytes is ignored.
pub fn parse_workspace_layout(value: Option<&str>) -> WorkspaceLayout {
    let Some(text) = value.filter(|v| v.len() <= 512) else {
        return WorkspaceLayout::default();
    };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => WorkspaceLayout {
            full_height_left: map.get("fullHeightLeft") == Some(&Value::Bool(true)),
            left_collapsed: map.get("leftCollapsed") == Some(&Value::Bool(true)),
            right_collapsed: map.get("rightCollapsed") == Some(&Value::Bool(true)),
        },
        _ => WorkspaceLayout::default(),
    }
}

pub fn workspace_layout_json(l: WorkspaceLayout) -> String {
    serde_json::json!({
        "fullHeightLeft": l.full_height_left,
        "leftCollapsed": l.left_collapsed,
        "rightCollapsed": l.right_collapsed,
    })
    .to_string()
}

/// Media pool width: 180..520 px, default 280, and never wider than the window allows.
pub fn clamp_pool_width(value: Option<f64>, window_width: Option<f64>) -> f64 {
    let v = match value.filter(|v| v.is_finite()) {
        Some(v) => v.clamp(180.0, 520.0),
        None => 280.0,
    };
    match window_width {
        Some(w) => v.min((w - 620.0).max(180.0)).max(180.0),
        None => v,
    }
}

/// Inspector width: 300..900 px, default 440.
pub fn clamp_inspector_width(value: Option<f64>) -> f64 {
    value
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(300.0, 900.0))
        .unwrap_or(440.0)
}

/// Viewer height in the artwork/subtitle workspace: 220..1600 px, `None` when unset.
pub fn clamp_viewer_height(value: Option<f64>, available: Option<f64>) -> Option<f64> {
    let v = value.filter(|v| v.is_finite())?.clamp(220.0, 1600.0);
    Some(match available {
        Some(a) => v.min((a - 260.0).max(220.0)).max(220.0),
        None => v,
    })
}

/// Keyboard resize step for a focused divider (Shift: larger step; Home resets to default).
pub fn divider_step(shift: bool) -> f64 {
    if shift {
        40.0
    } else {
        10.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_defaults_missing_malformed_and_oversized_values() {
        assert_eq!(parse_workspace_layout(None), WorkspaceLayout::default());
        assert_eq!(
            parse_workspace_layout(Some("{bad")),
            WorkspaceLayout::default()
        );
        assert_eq!(
            parse_workspace_layout(Some("[true]")),
            WorkspaceLayout::default()
        );
        let huge = format!("{{\"leftCollapsed\":true,\"x\":\"{}\"}}", "a".repeat(600));
        assert_eq!(
            parse_workspace_layout(Some(&huge)),
            WorkspaceLayout::default()
        );
    }

    #[test]
    fn each_boolean_is_retained_independently_and_coercion_is_refused() {
        let parsed = parse_workspace_layout(Some(
            r#"{"fullHeightLeft":true,"leftCollapsed":"true","rightCollapsed":true}"#,
        ));
        assert!(parsed.full_height_left);
        assert!(
            !parsed.left_collapsed,
            "strings are not coerced to booleans"
        );
        assert!(parsed.right_collapsed);
        let round = parse_workspace_layout(Some(&workspace_layout_json(parsed)));
        assert_eq!(round, parsed);
    }

    #[test]
    fn pool_and_inspector_widths_are_bounded() {
        assert_eq!(clamp_pool_width(None, None), 280.0);
        assert_eq!(clamp_pool_width(Some(f64::NAN), None), 280.0);
        assert_eq!(clamp_pool_width(Some(9000.0), None), 520.0);
        assert_eq!(clamp_pool_width(Some(400.0), Some(800.0)), 180.0);
        assert_eq!(clamp_inspector_width(Some(10.0)), 300.0);
        assert_eq!(clamp_inspector_width(None), 440.0);
    }

    #[test]
    fn viewer_height_reserves_space_and_defaults_when_unset() {
        assert_eq!(clamp_viewer_height(None, None), None);
        assert_eq!(clamp_viewer_height(Some(5000.0), None), Some(1600.0));
        assert_eq!(clamp_viewer_height(Some(900.0), Some(800.0)), Some(540.0));
    }
}
