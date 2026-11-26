//! Default folder preference (reference `apps/desktop/src-tauri/src/preferences.rs`, Tauri-free).

use crate::context::Ctx;
use crate::types::EngineResult;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Validate a `default_folder` setting value. `null` means "reset to Videos".
pub fn validate_folder(value: &Value) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    let folder = value
        .as_str()
        .ok_or("Default folder must be a folder path")?;
    if !Path::new(folder).is_absolute() || !Path::new(folder).is_dir() {
        return Err("Choose an existing absolute folder path".into());
    }
    Ok(())
}

fn resolve_folder(custom: Option<&str>, videos: &Path) -> Result<PathBuf, String> {
    if let Some(path) = custom
        .map(Path::new)
        .filter(|path| path.is_absolute() && path.is_dir())
    {
        return Ok(path.into());
    }
    if !videos.is_absolute() {
        return Err("The operating system did not provide an absolute Videos folder".into());
    }
    std::fs::create_dir_all(videos).map_err(|e| format!("Could not use the Videos folder: {e}"))?;
    Ok(videos.into())
}

/// The folder used to seed native dialogs. Seeding a dialog does not grant access to any file.
pub fn default_folder(ctx: &Ctx) -> Result<PathBuf, String> {
    let custom = ctx
        .app
        .storage
        .lock()
        .map_err(|_| "Storage lock failed")?
        .setting("default_folder")?;
    let videos = ctx
        .host
        .videos_dir()
        .ok_or("The operating system did not provide a Videos folder")?;
    resolve_folder(custom.as_ref().and_then(Value::as_str), &videos)
}

/// Resolve the folder for an explicit settings-page picker result (reference
/// `settings_choose_folder`'s dialog seeding uses [`default_folder`]).
pub fn choose_folder(ctx: &Ctx) -> EngineResult<Option<String>> {
    let start = default_folder(ctx)?;
    let Some(path) = ctx.host.pick_folder(&crate::host::FileDialog {
        title: Some("Choose Reelio's default folder".into()),
        directory: Some(start),
        file_name: None,
        filters: vec![],
        multiple: false,
    }) else {
        return Ok(None);
    };
    let value = path
        .to_str()
        .ok_or("Folder path must be valid Unicode")?
        .to_string();
    validate_folder(&Value::String(value.clone()))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_videos_and_honors_existing_custom_folder() {
        let root = tempfile::tempdir().unwrap();
        let videos = root.path().join("Videos");
        assert_eq!(resolve_folder(None, &videos).unwrap(), videos);
        assert!(videos.is_dir());
        let custom = root.path().join("custom");
        std::fs::create_dir(&custom).unwrap();
        assert_eq!(resolve_folder(custom.to_str(), &videos).unwrap(), custom);
    }

    #[test]
    fn invalid_settings_are_rejected_and_deleted_folder_falls_back() {
        let root = tempfile::tempdir().unwrap();
        let videos = root.path().join("Videos");
        for value in [
            serde_json::json!("relative"),
            serde_json::json!(root.path().join("missing")),
            serde_json::json!(12),
        ] {
            assert!(validate_folder(&value).is_err());
        }
        let file = root.path().join("file");
        std::fs::write(&file, b"preserved").unwrap();
        assert!(validate_folder(&serde_json::json!(file)).is_err());
        assert!(validate_folder(&Value::Null).is_ok());
        assert_eq!(resolve_folder(file.to_str(), &videos).unwrap(), videos);
        assert_eq!(std::fs::read(file).unwrap(), b"preserved");
    }
}
