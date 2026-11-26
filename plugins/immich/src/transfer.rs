//! Verified original transfers (ported from `immich_transfer.rs`).
//!
//! Upload: local file to Immich. The bytes are checksummed (SHA-1, Immich's checksum), uploaded as multipart,
//! the server checksum is compared, the `pigeon.asset/v1` metadata is written and read back and tag
//! assignments are confirmed. An accepted binary whose metadata cannot be verified is an error, not a success.
//!
//! Download: Immich original to a local folder. The original streams into a private temporary file, is
//! checksum-verified, then published without overwriting anything. Identical files already in place are
//! reused without another GET. A content-addressed `.pigeon-immich.<digest>.json` sidecar keeps the complete
//! server metadata. The source file's embedded metadata is never modified.

use crate::api::{valid_id as valid_asset_id, ImmichClient, Result};
use base64::Engine;
use reqwest::{blocking::multipart, Method};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

/// Largest original accepted for either direction.
pub const MAX_ORIGINAL_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// Fields that never leave the machine: paths, runtime URLs, secrets, lock state and the plugin's own
/// bookkeeping. Everything else in the host record travels in the `pigeon` metadata namespace.
const EXCLUDED: &[&str] = &[
    "path", "relativePath", "editedPath", "sourcePath", "mediaUrl", "editorSourceUrl", "proxyUrl", "playbackUrl",
    "fileUrl", "lock", "locked", "apiKey", "password", "accessToken", "locationId", "sourceMissing",
    "sourcePending", "scanCheckpoint", "schema", "encryptedMediaPaths", "encryptedThumbnailPaths", "osTrashed",
];

pub fn portable(asset: &Value) -> Value {
    let mut value = json!({"schema":"pigeon.asset/v1"});
    if let Some(asset) = asset.as_object() {
        for (key, item) in asset {
            let lower = key.to_ascii_lowercase();
            if !EXCLUDED.contains(&key.as_str())
                && !key.starts_with("thumbnail")
                && !key.starts_with("preview")
                && !key.starts_with("immich")
                && !key.starts_with("encrypted")
                && !lower.ends_with("path")
                && !lower.ends_with("paths")
            {
                value[key] = item.clone();
            }
        }
    }
    value
}

fn add_received(received: u64, bytes: usize) -> Result<u64> {
    received
        .checked_add(bytes as u64)
        .filter(|total| *total <= MAX_ORIGINAL_BYTES)
        .ok_or_else(|| "The original exceeds the 50 GiB per-file limit".to_owned())
}

/// Decodes Immich's checksum: 40 hex digits, or base64 of 20 bytes.
pub fn checksum(value: &Value) -> Result<Vec<u8>> {
    let raw = value.as_str().unwrap_or("");
    let bytes = if raw.len() == 40 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        (0..40).step_by(2).map(|i| u8::from_str_radix(&raw[i..i + 2], 16).unwrap_or(0)).collect()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| "Immich did not provide a verifiable checksum".to_owned())?
    };
    if bytes.len() != 20 {
        return Err("Immich did not provide a verifiable checksum".into());
    }
    Ok(bytes)
}

pub fn sha1_file(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path).map_err(|_| "Could not read the transfer file".to_owned())?;
    let mut hash = Sha1::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|_| "Could not read the transfer file".to_owned())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().to_vec())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn millis_to_rfc3339(millis: i64) -> Result<String> {
    Ok(chrono::DateTime::from_timestamp_millis(millis)
        .ok_or_else(|| "Invalid file timestamp".to_owned())?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn system_time_rfc3339(time: std::time::SystemTime) -> Result<String> {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid file timestamp".to_owned())?
        .as_millis() as i64;
    millis_to_rfc3339(millis)
}

/// Uploads one local file. `asset` is the public host record (tags, note, rating, favourite, date, geo).
pub fn upload(client: &ImmichClient, path: &Path, asset: &Value) -> Result<Value> {
    let stat = fs::metadata(path).map_err(|_| "Could not inspect the local file".to_owned())?;
    if !stat.is_file() {
        return Err("Choose a regular file".into());
    }
    if stat.len() > MAX_ORIGINAL_BYTES {
        return Err("The original exceeds the 50 GiB per-file limit".into());
    }
    let digest = sha1_file(path)?;
    let metadata = portable(asset);
    let version = client.json(Method::GET, "/server/version", None)?;
    let major = version["major"].as_u64().ok_or_else(|| "Invalid Immich version".to_owned())?;
    if major < 2 {
        return Err("Verified transfers require Immich 2.5 or newer".into());
    }
    let created = match asset["created"].as_i64().and_then(chrono::DateTime::from_timestamp_millis) {
        Some(date) => date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        None => system_time_rfc3339(
            stat.created().or_else(|_| stat.modified()).map_err(|_| "Could not read file timestamps".to_owned())?,
        )?,
    };
    let modified = system_time_rfc3339(stat.modified().map_err(|_| "Could not read file timestamps".to_owned())?)?;
    let part = multipart::Part::file(path).map_err(|_| "Could not open the upload file".to_owned())?;
    let mut form = multipart::Form::new()
        .part("assetData", part)
        .text("fileCreatedAt", created)
        .text("fileModifiedAt", modified)
        .text("metadata", json!([{"key":"pigeon","value":metadata}]).to_string());
    if major == 2 {
        form = form.text("deviceId", "pigeon").text("deviceAssetId", hex(&digest));
    }
    let response = client
        .http
        .post(format!("{}/api/assets", client.base))
        .timeout(Duration::from_secs(600))
        .multipart(form)
        .send()
        .map_err(|_| "Immich upload connection failed or timed out".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("Immich upload failed (HTTP {})", response.status().as_u16()));
    }
    let mut data = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Could not read the upload receipt".to_owned())?;
    if data.len() > 1024 * 1024 {
        return Err("Immich upload receipt exceeds its safety limit".into());
    }
    let accepted: Value = serde_json::from_slice(&data).map_err(|_| "Immich returned an invalid upload receipt".to_owned())?;
    let id = accepted["id"].as_str().unwrap_or("").to_owned();
    valid_asset_id(&id)?;
    if !matches!(accepted["status"].as_str().unwrap_or(""), "created" | "duplicate") {
        return Err("Immich did not accept the upload".into());
    }
    let after = fs::metadata(path).map_err(|_| "The local upload file disappeared".to_owned())?;
    if after.len() != stat.len() || after.modified().ok() != stat.modified().ok() {
        return Err("The local file changed during upload; completion could not be verified".into());
    }
    let info = client.json(Method::GET, &format!("/assets/{id}"), None)?;
    if checksum(&info["checksum"])? != digest {
        return Err("The server checksum does not match the uploaded original".into());
    }
    client.json(Method::PUT, &format!("/assets/{id}/metadata"), Some(&json!({"items":[{"key":"pigeon","value":metadata}]})))?;
    let mut patch = json!({});
    if metadata["favorite"] == true {
        patch["isFavorite"] = json!(true);
    }
    if let Some(note) = metadata["note"].as_str().filter(|n| !n.is_empty()) {
        patch["description"] = json!(note);
    }
    if let Some(rating) = metadata["rating"].as_i64().filter(|n| (1..=5).contains(n)) {
        patch["rating"] = json!(rating);
    }
    let latitude = metadata["geo"]["latitude"].as_f64().or_else(|| metadata["geo"]["lat"].as_f64());
    let longitude = metadata["geo"]["longitude"].as_f64().or_else(|| metadata["geo"]["lng"].as_f64());
    if let (Some(lat), Some(lon)) = (latitude, longitude) {
        if lat.abs() <= 90. && lon.abs() <= 180. {
            patch["latitude"] = json!(lat);
            patch["longitude"] = json!(lon);
        }
    }
    if patch.as_object().is_some_and(|o| !o.is_empty()) {
        client.json(Method::PUT, &format!("/assets/{id}"), Some(&patch))?;
    }
    let names: Vec<&str> = metadata["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !names.is_empty() {
        let tags = client.json(Method::PUT, "/tags", Some(&json!({"tags":names})))?;
        let tags = tags.as_array().ok_or_else(|| "Immich did not return metadata tags".to_owned())?;
        if names.iter().any(|name| !tags.iter().any(|tag| tag["value"].as_str() == Some(name))) {
            return Err("Immich did not create every metadata tag".into());
        }
        for tag in tags {
            let tag_id = tag["id"].as_str().unwrap_or("");
            valid_asset_id(tag_id)?;
            let result = client.json(Method::PUT, &format!("/tags/{tag_id}/assets"), Some(&json!({"ids":[id]})))?;
            if !result.as_array().is_some_and(|items| items.iter().any(|item| item["id"] == id && item["success"] == true)) {
                return Err("Immich did not confirm the tag assignment".into());
            }
        }
    }
    // An accepted binary without verified metadata is not a success: say so explicitly.
    let stored = client
        .json(Method::GET, &format!("/assets/{id}/metadata/pigeon"), None)
        .map_err(|_| "The original upload exists but its Pigeon metadata could not be read back; retry to finish".to_owned())?;
    if stored["value"] != metadata {
        return Err("The original upload exists but its Pigeon metadata was not fully verified; retry to finish".into());
    }
    Ok(json!({"id":id,"status":accepted["status"],"verified":true,"metadataVerified":true,"checksum":hex(&digest)}))
}

/// Downloads one original into `directory` (an existing folder chosen by the user).
pub fn download(client: &ImmichClient, id: &str, directory: &Path, account: &str) -> Result<Value> {
    valid_asset_id(id)?;
    let root = fs::canonicalize(directory).map_err(|_| "Choose an existing local folder".to_owned())?;
    if !root.is_dir() {
        return Err("Choose an existing local folder".into());
    }
    let info = client.json(Method::GET, &format!("/assets/{id}"), None)?;
    let expected = checksum(&info["checksum"])?;
    let all_metadata = client.json(Method::GET, &format!("/assets/{id}/metadata"), None)?;
    if !all_metadata.is_array() {
        return Err("Immich did not return complete asset metadata".into());
    }
    let raw = info["originalFileName"].as_str().unwrap_or("").replace('\\', "/");
    let name = raw
        .rsplit('/')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect::<String>();
    if name.is_empty() || name == "." || name == ".." || name.len() > 180 {
        return Err("The remote filename is empty or too long; rename it on Immich".into());
    }
    let target = root.join(name);
    let mut reused = false;
    match fs::symlink_metadata(&target) {
        Ok(stat) => {
            if !stat.is_file() || stat.file_type().is_symlink() || sha1_file(&target)? != expected {
                return Err("A different file already exists in this folder; nothing was overwritten".into());
            }
            reused = true;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not inspect the destination file".into()),
    }
    if !reused {
        let mut temporary = tempfile::Builder::new()
            .prefix(".pigeon-download-")
            .suffix(".part")
            .tempfile_in(&root)
            .map_err(|_| "Could not create the download temporary file".to_owned())?;
        let mut response = client
            .http
            .get(format!("{}/api/assets/{id}/original", client.base))
            .timeout(Duration::from_secs(600))
            .send()
            .map_err(|_| "Immich download connection failed or timed out".to_owned())?;
        if !response.status().is_success() {
            return Err(format!("Immich original download failed (HTTP {})", response.status().as_u16()));
        }
        let length = response.content_length();
        let mut received = 0u64;
        let mut digest = Sha1::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = response.read(&mut buffer).map_err(|_| "The original download was interrupted".to_owned())?;
            if n == 0 {
                break;
            }
            received = add_received(received, n)?;
            digest.update(&buffer[..n]);
            temporary.write_all(&buffer[..n]).map_err(|_| "Could not save the original download".to_owned())?;
        }
        if length.is_some_and(|n| n != received) || digest.finalize().to_vec() != expected {
            return Err("The original download failed length or checksum verification".into());
        }
        temporary.as_file().sync_all().map_err(|_| "Could not flush the verified original".to_owned())?;
        match temporary.persist_noclobber(&target) {
            Ok(file) => {
                if let Ok(date) = chrono::DateTime::parse_from_rfc3339(info["fileModifiedAt"].as_str().unwrap_or("")) {
                    if date.timestamp_millis() >= 0 {
                        file.set_modified(UNIX_EPOCH + Duration::from_millis(date.timestamp_millis() as u64))
                            .map_err(|_| "Original saved but its modification date could not be restored".to_owned())?;
                    }
                }
            }
            Err(error) => {
                if error.error.kind() != std::io::ErrorKind::AlreadyExists || sha1_file(&target)? != expected {
                    return Err("The destination changed during download; nothing was overwritten".into());
                }
                reused = true;
            }
        }
    }
    let record = json!({"schema":"pigeon.immich-download/v1","accountKey":account,"assetId":id,"checksum":hex(&expected),"remoteAsset":info,"remoteMetadata":all_metadata});
    let digest = hex(&Sha256::digest(serde_json::to_vec(&record).map_err(|_| "Could not encode the metadata receipt".to_owned())?));
    let sidecar = target.with_file_name(format!(
        "{}.pigeon-immich.{}.json",
        target.file_name().unwrap_or_default().to_string_lossy(),
        &digest[..16]
    ));
    let mut temp = tempfile::Builder::new()
        .prefix(".pigeon-metadata-")
        .tempfile_in(&root)
        .map_err(|_| "Original verified but the metadata temporary file could not be created".to_owned())?;
    temp.write_all(&serde_json::to_vec_pretty(&record).map_err(|_| "Could not encode the original metadata".to_owned())?)
        .map_err(|_| "Original verified but metadata could not be written".to_owned())?;
    temp.as_file().sync_all().map_err(|_| "Could not flush original metadata".to_owned())?;
    if let Err(error) = temp.persist_noclobber(&sidecar) {
        if error.error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err("Original verified but metadata could not be committed".into());
        }
    }
    let stored: Value = serde_json::from_slice(&fs::read(&sidecar).map_err(|_| "Could not verify the metadata sidecar".to_owned())?)
        .map_err(|_| "The metadata sidecar is not valid JSON".to_owned())?;
    if stored != record {
        return Err("Downloaded metadata could not be verified".into());
    }
    let pigeon = all_metadata
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["key"] == "pigeon")
        .map(|item| item["value"].clone())
        .unwrap_or(json!({}));
    Ok(json!({"id":id,"path":target,"sidecar":sidecar,"reused":reused,"verified":true,"metadataVerified":true,"checksum":hex(&expected),"asset":info,"pigeon":pigeon}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::MAX_BYTES;

    #[test]
    fn uploads_keep_annotations_but_exclude_local_paths_and_runtime_secrets() {
        let value = portable(&json!({
            "id":"local","path":"/private/a.jpg","locked":true,"note":"keep","tags":["travel"],
            "thumbnailPath":"/cache/a","accessToken":"secret","immichRemoteId":"x","osTrashed":true,"rating":4
        }));
        assert_eq!(value["schema"], "pigeon.asset/v1");
        assert_eq!(value["note"], "keep");
        assert_eq!(value["tags"], json!(["travel"]));
        assert_eq!(value["rating"], 4);
        let text = value.to_string();
        for forbidden in ["/private", "/cache", "secret", "immichRemoteId", "osTrashed", "locked"] {
            assert!(!text.contains(forbidden), "{forbidden} leaked into upload metadata");
        }
    }

    #[test]
    fn download_counter_accepts_sizes_up_to_the_limit_and_rejects_beyond() {
        assert_eq!(add_received(MAX_ORIGINAL_BYTES - 1, 1).unwrap(), MAX_ORIGINAL_BYTES);
        assert!(add_received(MAX_ORIGINAL_BYTES, 1).is_err());
        assert!(add_received(u64::MAX, 1).is_err());
    }

    #[test]
    fn checksums_accept_hex_and_base64_only_when_twenty_bytes() {
        let hex40 = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(checksum(&json!(hex40)).unwrap().len(), 20);
        let b64 = base64::engine::general_purpose::STANDARD.encode([7u8; 20]);
        assert_eq!(checksum(&json!(b64)).unwrap(), vec![7u8; 20]);
        assert!(checksum(&json!("abcd")).is_err());
        assert!(checksum(&Value::Null).is_err());
    }

    #[test]
    fn size_limits_match_the_documented_contract() {
        assert_eq!(MAX_ORIGINAL_BYTES, 50 * 1024 * 1024 * 1024);
        // Metadata responses are capped separately from originals.
        assert_eq!(MAX_BYTES, 20 * 1024 * 1024);
    }
}
