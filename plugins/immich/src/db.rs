//! SQLite catalogue of remote references (one database in the plugin data folder, WAL mode).
//!
//! Only worker threads touch it, and never while a network call is in progress: callers fetch first, then
//! apply the result in one short transaction. A sync result is rejected if a newer generation started
//! (disconnect, account change or another sync) so stale work can never overwrite newer state.

use crate::api::{valid_id, Result};
use crate::calendar::{image_date, month_name};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Mutex};

pub const SCHEMA: i64 = 1;

pub struct Db {
    conn: Mutex<Connection>,
}

/// One remote image as stored locally.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub filename: String,
    pub size: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken: Option<String>,
    pub modified_ms: Option<i64>,
    pub favorite: bool,
    pub tags: Vec<String>,
    pub synced_tags: Vec<String>,
    pub missing: bool,
    pub year: Option<u32>,
    pub month: Option<usize>,
}

/// A node of the collection tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    /// `Immich` (root): every image of the account.
    Root,
    /// `Annual`: every image of the account, grouped by year below.
    Annual,
    Year(u32),
    Month(u32, usize),
}

impl Node {
    pub fn id(&self) -> String {
        match self {
            Node::Root => "root".into(),
            Node::Annual => "annual".into(),
            Node::Year(y) => format!("y:{y}"),
            Node::Month(y, m) => format!("m:{y}:{m:02}"),
        }
    }

    pub fn parse(id: &str) -> Option<Node> {
        match id {
            "root" => return Some(Node::Root),
            "annual" => return Some(Node::Annual),
            _ => {}
        }
        if let Some(year) = id.strip_prefix("y:") {
            let year: u32 = year.parse().ok()?;
            return (year > 0 && year < 10_000).then_some(Node::Year(year));
        }
        let rest = id.strip_prefix("m:")?;
        let (year, month) = rest.split_once(':')?;
        let (year, month): (u32, usize) = (year.parse().ok()?, month.parse().ok()?);
        ((1..=12).contains(&month) && year > 0 && year < 10_000).then_some(Node::Month(year, month))
    }

    pub fn title(&self) -> String {
        match self {
            Node::Root => "Immich".into(),
            Node::Annual => "Annual".into(),
            Node::Year(y) => y.to_string(),
            Node::Month(_, m) => month_name(*m).into(),
        }
    }
}

/// Converts a remote search result into the local row, or `None` for anything that is not a valid image.
pub fn row_from_remote(remote: &Value, synced: &[String]) -> Option<Row> {
    let id = remote["id"].as_str()?;
    valid_id(id).ok()?;
    if remote["type"] != "IMAGE" {
        return None;
    }
    let mut tags: Vec<String> = Vec::new();
    for name in remote["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["value"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !tags.iter().any(|t| t == name) {
            tags.push(name.to_owned());
        }
    }
    let original = remote["originalFileName"].as_str().unwrap_or("Immich image");
    let filename = std::path::Path::new(original)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let modified_ms = chrono::DateTime::parse_from_rfc3339(
        remote["fileModifiedAt"].as_str().or_else(|| remote["fileCreatedAt"].as_str()).unwrap_or(""),
    )
    .ok()
    .map(|t| t.timestamp_millis());
    let date = image_date(remote);
    let taken = remote["localDateTime"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| remote["fileCreatedAt"].as_str())
        .map(str::to_owned);
    Some(Row {
        id: id.to_owned(),
        filename,
        size: remote["exifInfo"]["fileSizeInByte"].as_i64().unwrap_or(0),
        width: remote["width"].as_i64().or_else(|| remote["exifInfo"]["exifImageWidth"].as_i64()),
        height: remote["height"].as_i64().or_else(|| remote["exifInfo"]["exifImageHeight"].as_i64()),
        taken,
        modified_ms,
        favorite: remote["isFavorite"] == true,
        tags,
        synced_tags: synced.to_vec(),
        missing: false,
        year: date.map(|d| d.0),
        month: date.map(|d| d.1),
    })
}

/// Protocol `Item` for a row (`library.source` list/get shape).
pub fn item(row: &Row) -> Value {
    let name = std::path::Path::new(&row.filename)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let ext = std::path::Path::new(&row.filename)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_uppercase();
    let mut tags = row.tags.clone();
    for tag in &row.synced_tags {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
    json!({
        "id": row.id,
        "name": row.filename,
        "stem": name,
        "kind": "image",
        "extension": ext,
        "size": row.size,
        "w": row.width,
        "h": row.height,
        "taken_at": row.taken,
        "modified": row.modified_ms,
        "tags": tags,
        "favorite": row.favorite,
        "meta": {
            "source": "Immich (thumbnail only)",
            "missing": row.missing,
            "originalAvailable": !row.missing,
        }
    })
}

#[derive(Default, Debug, PartialEq)]
pub struct TreeCounts {
    pub total: u64,
    /// Years with at least one valid capture date, newest first, with their counts.
    pub years: Vec<(u32, u64)>,
    /// Counts for every (year, month) pair that has assets.
    pub months: BTreeMap<(u32, usize), u64>,
}

pub struct SyncOutcome {
    pub count: usize,
    pub missing: usize,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "Could not create the Immich database folder".to_owned())?;
        }
        let conn = Connection::open(path).map_err(|_| "Could not open the Immich database".to_owned())?;
        Self::from_connection(conn)
    }

    pub fn in_memory() -> Result<Db> {
        Self::from_connection(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn from_connection(conn: Connection) -> Result<Db> {
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS assets (
                account TEXT NOT NULL, id TEXT NOT NULL, filename TEXT NOT NULL,
                size INTEGER NOT NULL DEFAULT 0, width INTEGER, height INTEGER,
                taken TEXT, modified_ms INTEGER, favorite INTEGER NOT NULL DEFAULT 0,
                tags TEXT NOT NULL DEFAULT '[]', synced_tags TEXT NOT NULL DEFAULT '[]',
                year INTEGER, month INTEGER, generation INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (account, id));
             CREATE INDEX IF NOT EXISTS assets_by_month ON assets (account, year, month);
             CREATE TABLE IF NOT EXISTS tags (account TEXT NOT NULL, id TEXT NOT NULL, value TEXT NOT NULL,
                PRIMARY KEY (account, id));
             CREATE TABLE IF NOT EXISTS sync_state (account TEXT PRIMARY KEY, generation INTEGER NOT NULL,
                synced_ms INTEGER NOT NULL, count INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS receipts (account TEXT NOT NULL, asset_id TEXT NOT NULL,
                path TEXT NOT NULL, direction TEXT NOT NULL, checksum TEXT NOT NULL, sidecar TEXT,
                verified_ms INTEGER NOT NULL, PRIMARY KEY (account, asset_id, path, direction));",
        )
        .map_err(|e| format!("Could not prepare the Immich database: {e}"))?;
        conn.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('schema', ?1)", params![SCHEMA.to_string()])
            .map_err(|e| e.to_string())?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    /// Runs `f` with the connection. Callers must not perform network I/O inside `f`.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut guard = self.conn.lock().map_err(|_| "Immich database unavailable".to_owned())?;
        f(&mut guard)
    }

    /// Current generation for an account (0 when never synced).
    pub fn generation(&self, account: &str) -> Result<i64> {
        self.with(|c| {
            Ok(c.query_row("SELECT generation FROM sync_state WHERE account = ?1", params![account], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?
                .unwrap_or(0))
        })
    }

    /// Applies one complete account snapshot in a single transaction. `still_current` runs inside the
    /// transaction and must confirm that the caller's generation is still the newest; otherwise nothing is
    /// written. Assets not seen in this snapshot are kept but flagged `missing`.
    pub fn apply_snapshot(
        &self,
        account: &str,
        expected_generation: i64,
        remote_assets: &[Value],
        remote_tags: &[Value],
        still_current: &dyn Fn() -> bool,
        synced_ms: i64,
    ) -> Result<Option<SyncOutcome>> {
        self.with(|c| {
            let tx = c.transaction().map_err(|e| e.to_string())?;
            let current: i64 = tx
                .query_row("SELECT generation FROM sync_state WHERE account = ?1", params![account], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?
                .unwrap_or(0);
            if current != expected_generation || !still_current() {
                return Ok(None);
            }
            let next = current + 1;
            let mut count = 0usize;
            for remote in remote_assets {
                let Some(row) = row_from_remote(remote, &[]) else { continue };
                let synced: String = tx
                    .query_row("SELECT synced_tags FROM assets WHERE account = ?1 AND id = ?2", params![account, row.id], |r| r.get(0))
                    .optional()
                    .map_err(|e| e.to_string())?
                    .unwrap_or_else(|| "[]".into());
                let synced_tags: Vec<String> = serde_json::from_str(&synced).unwrap_or_default();
                let row = Row { synced_tags, ..row };
                tx.execute(
                    "INSERT INTO assets (account, id, filename, size, width, height, taken, modified_ms, favorite,
                        tags, synced_tags, year, month, generation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                     ON CONFLICT (account, id) DO UPDATE SET filename = excluded.filename, size = excluded.size,
                        width = excluded.width, height = excluded.height, taken = excluded.taken,
                        modified_ms = excluded.modified_ms, favorite = excluded.favorite, tags = excluded.tags,
                        year = excluded.year, month = excluded.month, generation = excluded.generation",
                    params![
                        account,
                        row.id,
                        row.filename,
                        row.size,
                        row.width,
                        row.height,
                        row.taken,
                        row.modified_ms,
                        row.favorite as i64,
                        serde_json::to_string(&row.tags).unwrap_or_else(|_| "[]".into()),
                        serde_json::to_string(&row.synced_tags).unwrap_or_else(|_| "[]".into()),
                        row.year,
                        row.month.map(|m| m as i64),
                        next
                    ],
                )
                .map_err(|e| e.to_string())?;
                count += 1;
            }
            let missing = tx
                .execute("UPDATE assets SET generation = generation WHERE account = ?1 AND generation <> ?2", params![account, next])
                .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM tags WHERE account = ?1", params![account]).map_err(|e| e.to_string())?;
            for tag in remote_tags {
                let (Some(id), Some(value)) = (tag["id"].as_str(), tag["value"].as_str()) else { continue };
                if valid_id(id).is_err() {
                    continue;
                }
                tx.execute(
                    "INSERT OR REPLACE INTO tags (account, id, value) VALUES (?1, ?2, ?3)",
                    params![account, id, value],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.execute(
                "INSERT INTO sync_state (account, generation, synced_ms, count) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account) DO UPDATE SET generation = excluded.generation,
                    synced_ms = excluded.synced_ms, count = excluded.count",
                params![account, next, synced_ms, count as i64],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(Some(SyncOutcome { count, missing: missing.min(usize::MAX) }))
        })
    }

    /// Counts for the tree. Missing references are kept in the tree (flagged in the list), as before.
    pub fn tree_counts(&self, account: &str) -> Result<TreeCounts> {
        self.with(|c| {
            let mut counts = TreeCounts::default();
            counts.total = c
                .query_row("SELECT COUNT(*) FROM assets WHERE account = ?1", params![account], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())? as u64;
            let mut stmt = c
                .prepare("SELECT year, month, COUNT(*) FROM assets WHERE account = ?1 AND year IS NOT NULL GROUP BY year, month")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![account], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
                .map_err(|e| e.to_string())?;
            let mut years: BTreeMap<u32, u64> = BTreeMap::new();
            for row in rows {
                let (year, month, count) = row.map_err(|e| e.to_string())?;
                *years.entry(year as u32).or_insert(0) += count as u64;
                counts.months.insert((year as u32, month as usize), count as u64);
            }
            counts.years = years.into_iter().rev().collect();
            Ok(counts)
        })
    }

    /// One page of rows for a node, newest capture first (ties by id).
    pub fn list(&self, account: &str, node: Node, offset: u64, limit: u64) -> Result<(Vec<Row>, u64)> {
        let (filter, args): (&str, Vec<i64>) = match node {
            Node::Root | Node::Annual => ("1 = 1", vec![]),
            Node::Year(y) => ("year = ?2", vec![y as i64]),
            Node::Month(y, m) => ("year = ?2 AND month = ?3", vec![y as i64, m as i64]),
        };
        self.with(|c| {
            let total: i64 = {
                let sql = format!("SELECT COUNT(*) FROM assets WHERE account = ?1 AND {filter}");
                let mut bound: Vec<rusqlite::types::Value> = vec![account.to_owned().into()];
                bound.extend(args.iter().map(|v| rusqlite::types::Value::Integer(*v)));
                c.query_row(&sql, rusqlite::params_from_iter(bound), |r| r.get(0)).map_err(|e| e.to_string())?
            };
            let sql = format!(
                "SELECT id, filename, size, width, height, taken, modified_ms, favorite, tags, synced_tags, generation, year, month
                 FROM assets WHERE account = ?1 AND {filter}
                 ORDER BY taken DESC, id DESC LIMIT {limit} OFFSET {offset}"
            );
            let mut stmt = c.prepare(&sql).map_err(|e| e.to_string())?;
            let mut bound: Vec<rusqlite::types::Value> = vec![account.to_owned().into()];
            bound.extend(args.iter().map(|v| rusqlite::types::Value::Integer(*v)));
            let generation_now: i64 = c
                .query_row("SELECT generation FROM sync_state WHERE account = ?1", params![account], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?
                .unwrap_or(0);
            let rows = stmt
                .query_map(rusqlite::params_from_iter(bound), |r| {
                    let tags: String = r.get(8)?;
                    let synced: String = r.get(9)?;
                    Ok(Row {
                        id: r.get(0)?,
                        filename: r.get(1)?,
                        size: r.get(2)?,
                        width: r.get(3)?,
                        height: r.get(4)?,
                        taken: r.get(5)?,
                        modified_ms: r.get(6)?,
                        favorite: r.get::<_, i64>(7)? != 0,
                        tags: serde_json::from_str(&tags).unwrap_or_default(),
                        synced_tags: serde_json::from_str(&synced).unwrap_or_default(),
                        missing: r.get::<_, i64>(10)? != generation_now,
                        year: r.get::<_, Option<i64>>(11)?.map(|y| y as u32),
                        month: r.get::<_, Option<i64>>(12)?.map(|m| m as usize),
                    })
                })
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| e.to_string())?);
            }
            Ok((out, total.max(0) as u64))
        })
    }

    pub fn get(&self, account: &str, id: &str) -> Result<Option<Row>> {
        valid_id(id)?;
        let page = self.list_by_ids(account, &[id.to_owned()])?;
        Ok(page.into_iter().next())
    }

    /// Rows for specific ids, in the order requested. Unknown ids are skipped.
    pub fn list_by_ids(&self, account: &str, ids: &[String]) -> Result<Vec<Row>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            valid_id(id)?;
            let found = self.with(|c| {
                let generation_now: i64 = c
                    .query_row("SELECT generation FROM sync_state WHERE account = ?1", params![account], |r| r.get(0))
                    .optional()
                    .map_err(|e| e.to_string())?
                    .unwrap_or(0);
                c.query_row(
                    "SELECT filename, size, width, height, taken, modified_ms, favorite, tags, synced_tags, generation, year, month
                     FROM assets WHERE account = ?1 AND id = ?2",
                    params![account, id],
                    |r| {
                        let tags: String = r.get(7)?;
                        let synced: String = r.get(8)?;
                        Ok(Row {
                            id: id.clone(),
                            filename: r.get(0)?,
                            size: r.get(1)?,
                            width: r.get(2)?,
                            height: r.get(3)?,
                            taken: r.get(4)?,
                            modified_ms: r.get(5)?,
                            favorite: r.get::<_, i64>(6)? != 0,
                            tags: serde_json::from_str(&tags).unwrap_or_default(),
                            synced_tags: serde_json::from_str(&synced).unwrap_or_default(),
                            missing: r.get::<_, i64>(9)? != generation_now,
                            year: r.get::<_, Option<i64>>(10)?.map(|y| y as u32),
                            month: r.get::<_, Option<i64>>(11)?.map(|m| m as usize),
                        })
                    },
                )
                .optional()
                .map_err(|e| e.to_string())
            })?;
            if let Some(row) = found {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// Account tags (id and name), by name.
    pub fn tags(&self, account: &str) -> Result<Vec<(String, String)>> {
        self.with(|c| {
            let mut stmt = c
                .prepare("SELECT id, value FROM tags WHERE account = ?1 ORDER BY value COLLATE NOCASE")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![account], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.map(|r| r.map_err(|e| e.to_string())).collect()
        })
    }

    /// Every remote asset id of an account (for explicit thumbnail refreshes).
    pub fn ids(&self, account: &str) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt = c
                .prepare("SELECT id FROM assets WHERE account = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map(params![account], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
            rows.map(|r| r.map_err(|e| e.to_string())).collect()
        })
    }

    /// Adds a tag name to the listed assets (after the server confirmed the assignment).
    pub fn add_asset_tag(&self, account: &str, ids: &[String], name: &str) -> Result<usize> {
        let mut changed = 0;
        for id in ids {
            valid_id(id)?;
            let current = self.get(account, id)?;
            let Some(row) = current else { continue };
            if row.tags.iter().any(|t| t == name) {
                continue;
            }
            let mut tags = row.tags.clone();
            tags.push(name.to_owned());
            self.with(|c| {
                c.execute(
                    "UPDATE assets SET tags = ?3 WHERE account = ?1 AND id = ?2",
                    params![account, id, serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into())],
                )
                .map_err(|e| e.to_string())
            })?;
            changed += 1;
        }
        Ok(changed)
    }

    pub fn set_synced_tags(&self, account: &str, id: &str, names: &[String]) -> Result<()> {
        valid_id(id)?;
        self.with(|c| {
            c.execute(
                "UPDATE assets SET synced_tags = ?3 WHERE account = ?1 AND id = ?2",
                params![account, id, serde_json::to_string(names).unwrap_or_else(|_| "[]".into())],
            )
            .map_err(|e| e.to_string())
            .map(|_| ())
        })
    }

    /// Records a verified transfer ("upload" or "download") for the audit trail.
    pub fn record_receipt(&self, account: &str, asset_id: &str, path: &str, direction: &str, checksum: &str, sidecar: Option<&str>, now_ms: i64) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO receipts (account, asset_id, path, direction, checksum, sidecar, verified_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![account, asset_id, path, direction, checksum, sidecar, now_ms],
            )
            .map_err(|e| e.to_string())
            .map(|_| ())
        })
    }

    pub fn receipt_count(&self, account: &str) -> Result<i64> {
        self.with(|c| {
            c.query_row("SELECT COUNT(*) FROM receipts WHERE account = ?1", params![account], |r| r.get(0))
                .map_err(|e| e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "33333333-3333-4333-8333-333333333333";
    const TAG: &str = "22222222-2222-4222-8222-222222222222";

    fn remote(id: &str, local: &str) -> Value {
        json!({"id":id,"type":"IMAGE","originalFileName":"fixture.png","localDateTime":local,"isFavorite":false,
               "tags":[{"id":TAG,"value":"travel"}],"exifInfo":{"fileSizeInByte":123,"exifImageWidth":4,"exifImageHeight":3}})
    }

    #[test]
    fn node_ids_round_trip() {
        for node in [Node::Root, Node::Annual, Node::Year(2025), Node::Month(2025, 3)] {
            assert_eq!(Node::parse(&node.id()), Some(node));
        }
        assert_eq!(Node::parse("m:2025:13"), None);
        assert_eq!(Node::parse("y:0"), None);
        assert_eq!(Node::parse("../etc"), None);
    }

    #[test]
    fn snapshot_builds_tree_and_flags_missing_references() {
        let db = Db::in_memory().unwrap();
        let first = db
            .apply_snapshot("acc", 0, &[remote(A, "2025-03-04T10:00:00"), remote(B, "bad-date")], &[json!({"id":TAG,"value":"travel"})], &|| true, 1)
            .unwrap()
            .unwrap();
        assert_eq!(first.count, 2);
        let counts = db.tree_counts("acc").unwrap();
        assert_eq!(counts.total, 2);
        assert_eq!(counts.years, vec![(2025, 1)]);
        assert_eq!(counts.months.get(&(2025, 3)), Some(&1));
        let (rows, total) = db.list("acc", Node::Root, 0, 10).unwrap();
        assert_eq!(total, 2);
        assert!(rows.iter().all(|r| !r.missing));
        // Second snapshot without B: B stays, flagged missing.
        db.apply_snapshot("acc", 1, &[remote(A, "2025-03-04T10:00:00")], &[], &|| true, 2).unwrap().unwrap();
        let b = db.get("acc", B).unwrap().unwrap();
        assert!(b.missing);
        assert!(!db.get("acc", A).unwrap().unwrap().missing);
    }

    #[test]
    fn stale_generation_writes_nothing() {
        let db = Db::in_memory().unwrap();
        db.apply_snapshot("acc", 0, &[remote(A, "2025-03-04")], &[], &|| true, 1).unwrap().unwrap();
        // A caller that started at generation 0 must be rejected now that generation 1 exists.
        assert!(db.apply_snapshot("acc", 0, &[remote(B, "2025-03-05")], &[], &|| true, 2).unwrap().is_none());
        assert!(db.get("acc", B).unwrap().is_none());
        // A cancelled caller (the check fails inside the transaction) writes nothing either.
        assert!(db.apply_snapshot("acc", 1, &[remote(B, "2025-03-05")], &[], &|| false, 2).unwrap().is_none());
        assert!(db.get("acc", B).unwrap().is_none());
    }

    #[test]
    fn synced_tags_survive_later_snapshots() {
        let db = Db::in_memory().unwrap();
        db.apply_snapshot("acc", 0, &[remote(A, "2025-03-04")], &[], &|| true, 1).unwrap();
        db.set_synced_tags("acc", A, &["pushed".into()]).unwrap();
        db.apply_snapshot("acc", 1, &[remote(A, "2025-03-04")], &[], &|| true, 2).unwrap();
        let row = db.get("acc", A).unwrap().unwrap();
        assert_eq!(row.synced_tags, vec!["pushed".to_owned()]);
        let it = item(&row);
        assert_eq!(it["tags"], json!(["travel", "pushed"]));
    }

    #[test]
    fn invalid_remote_ids_and_non_images_are_ignored() {
        let db = Db::in_memory().unwrap();
        let outcome = db
            .apply_snapshot("acc", 0, &[json!({"id":"../x","type":"IMAGE"}), json!({"id":A,"type":"VIDEO"})], &[], &|| true, 1)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.count, 0);
        assert_eq!(db.tree_counts("acc").unwrap().total, 0);
    }

    #[test]
    fn months_are_paged_and_tags_listed_by_name() {
        let db = Db::in_memory().unwrap();
        db.apply_snapshot("acc", 0, &[remote(A, "2025-03-04"), remote(B, "2025-03-05")], &[json!({"id":TAG,"value":"travel"})], &|| true, 1).unwrap();
        let (first, total) = db.list("acc", Node::Month(2025, 3), 0, 1).unwrap();
        assert_eq!((first.len(), total), (1, 2));
        let (second, _) = db.list("acc", Node::Month(2025, 3), 1, 1).unwrap();
        assert_ne!(first[0].id, second[0].id);
        assert_eq!(db.tags("acc").unwrap(), vec![(TAG.to_owned(), "travel".to_owned())]);
    }
}
