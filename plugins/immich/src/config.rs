//! Non-secret connection settings (`immich.json` in the plugin data folder) and the credential
//! wrappers over the host's `secrets.*` service.
//!
//! Plaintext credential persistence is forbidden: the file never contains a password, and the password
//! for a remembered account lives only in the host keychain under an opaque key derived from the account
//! identity. Session-only logins keep the password in memory for the session and nowhere else.

use crate::api::{account_key, normalize_server_url, Result};
use pigeon_plugin_sdk::runtime::Ctx;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, io::Write, path::{Path, PathBuf}};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub email: String,
    /// Keychain lookup identifier of a remembered password. Never shown to the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_id: Option<String>,
    #[serde(default)]
    pub session_only: bool,
    /// Every keychain key this plugin has ever stored (so `--uninstall` can remove them all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_history: Vec<String>,
}

pub fn file(data_dir: &Path) -> PathBuf {
    data_dir.join("immich.json")
}

pub fn load(data_dir: &Path) -> Config {
    fs::read(file(data_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok())
        .unwrap_or_default()
}

/// Atomic write: a temporary file in the same folder, flushed, then renamed over the target.
pub fn save(data_dir: &Path, config: &Config) -> Result<()> {
    fs::create_dir_all(data_dir).map_err(|_| "Could not create the Immich settings directory".to_owned())?;
    let mut temp = tempfile::NamedTempFile::new_in(data_dir).map_err(|_| "Could not create private Immich settings".to_owned())?;
    temp.write_all(&serde_json::to_vec_pretty(config).map_err(|_| "Could not encode Immich settings".to_owned())?)
        .map_err(|_| "Could not write Immich settings".to_owned())?;
    temp.as_file().sync_all().map_err(|_| "Could not flush Immich settings".to_owned())?;
    temp.persist(file(data_dir)).map_err(|_| "Could not commit Immich settings".to_owned())?;
    Ok(())
}

/// Keychain key for an account. Opaque: it is a hash of the server URL and email.
pub fn secret_key(server_url: &str, email: &str) -> Result<String> {
    let base = normalize_server_url(server_url)?;
    Ok(format!("immich-account/{}", account_key(&base, email)))
}

pub fn store_password(host: &Ctx, key: &str, password: &str) -> Result<()> {
    host.call("secrets.set", json!({"key":key,"value":password}))
        .map(|_| ())
        .map_err(|e| format!("The system keyring rejected the Immich credential; no plaintext password was saved ({e})"))
}

pub fn read_password(host: &Ctx, key: &str) -> Result<String> {
    let result = host
        .call("secrets.get", json!({"key":key}))
        .map_err(|_| "The saved Immich password is unavailable; unlock the system keyring or enter it again".to_owned())?;
    result["value"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "The saved Immich password is unavailable; unlock the system keyring or enter it again".to_owned())
}

pub fn delete_password(host: &Ctx, key: &str) -> Result<()> {
    host.call("secrets.delete", json!({"key":key})).map(|_| ()).map_err(|e| e.to_string())
}

/// Public view for the preferences page and status. Contains no lookup identifiers.
pub fn public(config: &Config, connected: bool) -> Value {
    json!({
        "enabled": config.enabled || connected,
        "serverUrl": config.server_url,
        "email": config.email,
        "hasPassword": config.secret_id.is_some(),
        "sessionOnly": config.session_only,
        "connected": connected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_file_never_holds_a_password_or_lookup_identifier_in_public_views() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            enabled: true,
            server_url: "https://example.test".into(),
            email: "fixture@example.test".into(),
            secret_id: Some("immich-account/private-lookup-identifier".into()),
            session_only: false,
            secret_history: vec!["immich-account/private-lookup-identifier".into()],
        };
        save(dir.path(), &config).unwrap();
        let on_disk = fs::read_to_string(file(dir.path())).unwrap();
        assert!(on_disk.contains("secretId"));
        assert!(!on_disk.to_lowercase().contains("password"));
        let view = public(&config, false).to_string();
        assert!(!view.contains("private-lookup-identifier"));
        assert!(view.contains("\"hasPassword\":true"));
        assert_eq!(load(dir.path()), config);
    }

    #[test]
    fn missing_or_corrupt_settings_fall_back_to_disabled() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Config::default());
        fs::write(file(dir.path()), b"not json").unwrap();
        assert_eq!(load(dir.path()), Config::default());
    }

    #[test]
    fn secret_key_is_stable_for_case_variants_of_the_email() {
        assert_eq!(
            secret_key("https://example.test", "A@EXAMPLE.TEST").unwrap(),
            secret_key("https://example.test/api/", "a@example.test").unwrap()
        );
        assert!(!secret_key("https://example.test", "a@example.test").unwrap().contains("example.test"));
    }
}
