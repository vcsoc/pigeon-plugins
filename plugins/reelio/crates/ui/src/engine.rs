//! Engine-neutral contract between the UI and the Reelio engine (WP4 `reelio-engine`).
//!
//! One variant per former Tauri command (see `generate_handler!` and `services/desktop.ts` in the
//! reference). Payloads are the same JSON the reference IPC carried, so the app crate maps each
//! variant one-to-one onto `reelio_engine::Engine` methods. Replies are JSON values that the UI
//! decodes with the typed helpers below. Nothing here depends on Tauri, egui or the plugin SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    BackendStatus,
    DependenciesStatus,
    DependenciesRepair,
    SettingsGet,
    SettingsUpdate {
        key: String,
        value: Value,
    },
    SettingsChooseFolder,
    ChooseMedia {
        folder: bool,
    },
    /// Former `subtitle_editor_open`: a path, or None for the host open dialog.
    SubtitleEditorOpen {
        path: Option<String>,
    },
    /// Former `subtitle_editor_save`: the document goes to a host save dialog (never overwrites).
    SubtitleEditorSave {
        document: Value,
    },
    /// Former `subtitle_editor_dirty`: unsaved subtitle work protects the window from closing.
    SubtitleEditorDirty {
        dirty: bool,
    },
    /// Former `choose_subtitles`: host open dialog for standalone subtitle files.
    ChooseSubtitles,
    /// Former `poster_search`: one provider batch at `offset` for the proportion filter.
    PosterSearch {
        query: String,
        proportion: String,
        offset: u32,
    },
    /// Former `poster_image`: download, validate and normalize one result into a cover.
    PosterImage {
        url: String,
        proportion: Option<String>,
    },
    /// Former `artwork_edit`: crop/rotate/flip/tonal edits, returning a new cover JPEG.
    ArtworkEdit {
        artwork: Value,
        edits: Value,
    },
    /// Authorizes dropped files (engine `media_dropped`); returns the authorized canonical paths.
    MediaDropped {
        paths: Vec<String>,
    },
    MediaProbe {
        path: String,
    },
    ProjectValidate {
        project: Value,
    },
    ProjectOpen,
    ProjectSave {
        project: Value,
    },
    ExportPlan {
        project: Value,
    },
    ExportStart {
        project: Value,
        approve_conversion: bool,
    },
    ExportCancel {
        id: String,
    },
    JobsList,
    JobProject {
        id: String,
    },
    ArtworkImport {
        path: String,
    },
    SubtitlePreview {
        project: Value,
        track_id: String,
    },
    PreviewSource {
        path: String,
    },
    ResourceDownload {
        id: String,
    },
    ResourceCancel {
        id: String,
    },
    WindowAction {
        action: String,
    },
}

/// Engine reply: `Ok` carries the command's JSON result, `Err` the user-facing message.
pub type Reply = Result<Value, String>;

/// Decode a reply payload into a typed value; a shape mismatch is a user-visible error.
pub fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("Unexpected engine reply: {e}"))
}

/// Mirrors `Backend` in `types/index.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backend {
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    #[serde(default)]
    pub muxers: Vec<String>,
    #[serde(default)]
    pub subtitle_encoders: Vec<String>,
}

/// Mirrors `RuntimeDependencies`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RuntimeDependencies {
    pub preview_available: bool,
    pub network_available: bool,
    pub backend_available: bool,
    pub missing_components: Vec<String>,
    pub repair_supported: bool,
    pub repair_hint: String,
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Compatibility {
    pub action: String,
    pub reason: String,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub target_codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTrack {
    pub track_id: String,
    pub output_index: u32,
    pub source_index: i64,
    pub asset_id: String,
    pub compatibility: Compatibility,
    #[serde(default)]
    pub default: bool,
}

/// Mirrors `Plan`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Plan {
    pub tracks: Vec<PlanTrack>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub duration_ms: Option<i64>,
    pub estimated_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Progress {
    pub phase: String,
    pub processed_ms: i64,
    pub percent: Option<f64>,
    pub bytes: u64,
    pub elapsed_ms: i64,
    #[serde(default)]
    pub notices: Vec<String>,
}

/// Mirrors `Job`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    pub sequence: u64,
    pub status: String,
    pub destination: String,
    #[serde(default)]
    pub progress: Option<Progress>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: String,
}

pub fn job_is_active(status: &str) -> bool {
    matches!(status, "queued" | "processing" | "verifying")
}

pub fn job_is_finished(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub default_folder: Option<String>,
    #[serde(default)]
    pub image_format: Option<String>,
    #[serde(default)]
    pub backend: Option<BackendPaths>,
    #[serde(default)]
    pub recents: Vec<Recent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BackendPaths {
    #[serde(default)]
    pub ffmpeg: Option<String>,
    #[serde(default)]
    pub ffprobe: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub path: String,
    pub kind: String,
}

/// Payload of a pending download request from a provider (`resource:download-requested`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingResource {
    pub id: String,
    pub kind: String,
    pub host: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_serialize_as_tagged_commands() {
        let r = Request::ExportCancel { id: "j1".into() };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"command": "export_cancel", "id": "j1"})
        );
        let back: Request = serde_json::from_value(json!({"command": "backend_status"})).unwrap();
        assert_eq!(back, Request::BackendStatus);
    }

    #[test]
    fn job_status_classification_matches_reference() {
        assert!(job_is_active("verifying"));
        assert!(!job_is_active("interrupted"));
        assert!(job_is_finished("cancelled"));
    }

    #[test]
    fn plan_decodes_reference_shape() {
        let plan: Plan = decode(json!({
            "tracks": [{
                "track_id": "t", "output_index": 0, "source_index": 0, "asset_id": "a",
                "compatibility": {"action": "copy", "reason": "ok", "warning": null, "target_codec": null},
                "default": true
            }],
            "errors": [], "warnings": [], "duration_ms": 10000, "estimated_bytes": 5
        }))
        .unwrap();
        assert_eq!(plan.tracks[0].compatibility.action, "copy");
    }
}
