//! `pigeon-immich`: the Immich plugin's process.
//!
//!   pigeon-immich --plugin       protocol v2 session (started by the host, JSON lines on stdin/stdout)
//!   pigeon-immich --uninstall    removes keychain entries and every local file (cache, catalogue, settings)
//!   pigeon-immich --self-test    checks the local machinery without a server (no network)
//!   pigeon-immich --health       readiness as JSON
//!   pigeon-immich --version

use pigeon_immich::{app::App, config, db::Db, thumbs::Thumbs, ui};
use pigeon_plugin_sdk::{runtime::{Plugin, PluginSpec}, Contribution};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

fn data_dir() -> PathBuf {
    std::env::var_os("PIGEON_PLUGIN_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".data"))
}

fn session_dir(data: &std::path::Path) -> PathBuf {
    std::env::var_os("PIGEON_SESSION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("session"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--plugin") => run_plugin(),
        Some("--uninstall") => uninstall(),
        Some("--self-test") => self_test(),
        Some("--health") => {
            println!("{}", json!({"version":1,"ok":true,"ready":true,"protocol":pigeon_plugin_sdk::PROTOCOL_VERSION}));
            0
        }
        Some("--version") => {
            println!("{}", json!({"version":1,"plugin":"immich","ok":true,"plugin_version":env!("CARGO_PKG_VERSION")}));
            0
        }
        _ => {
            eprintln!("usage: pigeon-immich --plugin | --uninstall | --self-test | --health | --version");
            2
        }
    };
    std::process::exit(code);
}

/// The plugin spec: identity and the static contributions (the same list plugin.json declares).
fn spec() -> PluginSpec {
    let mut spec = PluginSpec::new("immich", env!("CARGO_PKG_VERSION"));
    spec.contributions = pigeon_immich::contributions()
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| serde_json::from_value::<Contribution>(c.clone()).ok())
        .collect();
    spec
}

/// The long-running session. The SDK owns the protocol threads (hello/ready, pings, coalescing); the app
/// only parses and forwards, so no real work runs on the protocol side.
fn run_plugin() -> i32 {
    let plugin = match Plugin::stdio(spec()) {
        Ok(plugin) => plugin,
        Err(error) => {
            eprintln!("pigeon-immich: {error}");
            return 1;
        }
    };
    let data = data_dir();
    let session = session_dir(&data);
    let app = match App::new(data, session, plugin.ctx()) {
        Ok(app) => app,
        Err(message) => {
            eprintln!("pigeon-immich: {message}");
            return 1;
        }
    };
    app.start();
    plugin.run(move |_ctx, message| app.handle(message));
    0
}

/// Removes everything the plugin stored outside the host: keychain entries (through the host's
/// `secrets.delete`, bounded to 10 s each) and the local cache, catalogue and settings. Library data is never
/// touched. Exits 0 when everything was removed, 1 otherwise. Human-readable notes go to stderr; stdout is
/// reserved for protocol messages.
fn uninstall() -> i32 {
    let data = data_dir();
    let plugin = match Plugin::stdio(spec()) {
        Ok(plugin) => plugin,
        Err(error) => {
            eprintln!("pigeon-immich uninstall: no host handshake: {error}");
            return 1;
        }
    };
    let host = plugin.ctx();
    let config = config::load(&data);
    let mut keys: Vec<String> = config.secret_history.clone();
    if let Some(current) = config.secret_id.clone() {
        if !keys.contains(&current) {
            keys.push(current);
        }
    }
    let mut failures = 0;
    for key in &keys {
        if host
            .call_timeout("secrets.delete", json!({"key":key}), Duration::from_secs(10))
            .is_err()
        {
            failures += 1;
        }
    }
    let mut local_ok = true;
    for name in ["immich.sqlite3", "immich.sqlite3-wal", "immich.sqlite3-shm", "immich.json"] {
        let path = data.join(name);
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                local_ok = false;
            }
        }
    }
    if Thumbs::new(data.join("thumbs")).remove_all().is_err() {
        local_ok = false;
    }
    let _ = std::fs::remove_dir_all(session_dir(&data));
    eprintln!("pigeon-immich uninstall: keychain entries removed {}, could not remove {failures}; local data removed: {local_ok}", keys.len().saturating_sub(failures));
    if failures == 0 && local_ok {
        0
    } else {
        1
    }
}

/// Local checks only: catalogue round trip, thumbnail decode and bounds, settings privacy and the
/// scene builders. Prints one JSON object; exit 0 when every check passed.
fn self_test() -> i32 {
    let mut checks: Vec<Value> = Vec::new();
    let mut record = |name: &str, ok: bool| checks.push(json!({"name":name,"ok":ok}));
    let dir = std::env::temp_dir().join(format!("pigeon-immich-self-test-{}", uuid::Uuid::new_v4()));
    let ok_db = (|| -> Result<bool, String> {
        let db = Db::open(&dir.join("self.sqlite3"))?;
        db.apply_snapshot(
            &"a".repeat(64),
            0,
            &[json!({"id":"11111111-1111-4111-8111-111111111111","type":"IMAGE","localDateTime":"2025-03-04T10:00:00"})],
            &[],
            &|| true,
            0,
        )?;
        Ok(db.tree_counts(&"a".repeat(64))?.total == 1)
    })()
    .unwrap_or(false);
    record("catalogue_round_trip", ok_db);
    let mut png = Vec::new();
    let image = image::RgbImage::from_pixel(64, 48, image::Rgb([10, 20, 30]));
    let encoded = image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_ok();
    record("preview_encode_fixture", encoded);
    record("preview_decode", pigeon_immich::thumbs::decode_preview(&png).is_ok());
    record("scene_builder", ui::status_item(false, 0).is_object());
    record("account_path_validation", Thumbs::new(dir.clone()).account_dir(&"a".repeat(64)).is_ok());
    let stored = config::Config { enabled: true, server_url: "https://x.test".into(), email: "a@x.test".into(), secret_id: Some("k".into()), session_only: false, secret_history: vec![] };
    record("settings_have_no_password", !serde_json::to_string(&stored).unwrap_or_default().to_lowercase().contains("password"));
    let _ = std::fs::remove_dir_all(&dir);
    let passed = checks.iter().all(|c| c["ok"] == true);
    println!("{}", json!({"version":1,"ok":passed,"checks":checks}));
    if passed {
        0
    } else {
        1
    }
}
