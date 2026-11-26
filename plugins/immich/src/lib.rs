//! Immich plugin for Pigeon (protocol v2). The crate is a standalone program: `pigeon-immich`.
//!
//! Module map: `app` (requests, sync, transfers, scenes),
//! `api` (Immich HTTP), `db` (SQLite catalogue), `thumbs` (preview cache), `transfer` (verified
//! uploads and downloads), `ui` (scene builders), `config` (settings and keychain wrappers), `calendar`.

pub mod api;
pub mod app;
pub mod calendar;
pub mod config;
pub mod db;
pub mod pool;
pub mod thumbs;
pub mod transfer;
pub mod ui;

use serde_json::{json, Value};

/// Static contributions. `plugin.json` declares the same list (the host reads it before the process
/// starts); a test keeps the two identical.
pub fn contributions() -> Value {
    json!([
        {"slot":"preferences.page","id":"settings","title":"Immich","icon":"cloud","order":60},
        {"slot":"library.source","id":"immich","title":"Immich","icon":"cloud","root":"root",
         "capabilities":["browse","upload","download","metadata_write","tags"]},
        {"slot":"statusbar.item","id":"status","align":"right","order":50},
        {"slot":"modal","id":"transfer","title":"Verified Immich transfers","size":"medium","resizable":true,"modal":true},
        {"slot":"modal","id":"tag-picker","title":"Add Immich tag","size":"small","resizable":false,"modal":true},
        {"slot":"asset.action","id":"send","title":"Send to Immich","icon":"cloud-arrow-up","group":"Immich",
         "applies_to":{"kinds":["image","video","audio","document"],"min":1},"selection":"many"},
        {"slot":"asset.action","id":"download","title":"Download original from Immich...","icon":"cloud-arrow-down","group":"Immich",
         "applies_to":{"kinds":["image"],"min":1},"selection":"many","source":"immich"},
        {"slot":"asset.action","id":"tag","title":"Add Immich tag...","icon":"tag","group":"Immich",
         "applies_to":{"kinds":["image"],"min":1},"selection":"any","source":"immich"},
        {"slot":"command","id":"sync","title":"Sync Immich now"},
        {"slot":"command","id":"sync-collection","title":"Sync Immich and refresh thumbnails"}
    ])
}

/// Services the plugin uses; declared in plugin.json `ui.permissions`.
pub const PERMISSIONS: [&str; 5] = ["library.read", "library.write", "secrets", "dialogs", "windows"];
