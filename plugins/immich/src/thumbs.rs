//! Thumbnail cache and preview pipeline (ported from `immich_cache.rs` and `immich.rs::thumbnail`).
//!
//! Policy (unchanged from the built-in implementation):
//! * Account-scoped folders, private to the OS user (0700 on Unix). Only `<64 hex account>/<uuid>.jpg`
//!   files are ever pruned or refreshed; anything else in the folder is left alone.
//! * Bounded to about 256 MiB; the oldest files go first. Pruning runs every 32 writes and never blocks a
//!   request (it is skipped when another prune is running).
//! * Concurrent requests for the same asset share one fetch (single flight).
//! * At most two previews decode at the same time. Previews larger than 40 megapixels are refused before
//!   decoding and every preview is reduced to a 1024-pixel JPEG.
//! * Only the preview endpoint is used. Originals are never downloaded for browsing.

use crate::api::Result;
use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
};

pub const CACHE_LIMIT: u64 = 256 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;
pub const DECODE_SLOTS: usize = 2;
const PRUNE_EVERY: usize = 32;

pub struct Thumbs {
    root: PathBuf,
    locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    slots: (Mutex<usize>, Condvar),
    writes: AtomicUsize,
    prune_guard: Mutex<()>,
}

impl Thumbs {
    pub fn new(root: PathBuf) -> Thumbs {
        Thumbs {
            root,
            locks: Mutex::new(HashMap::new()),
            slots: (Mutex::new(0), Condvar::new()),
            writes: AtomicUsize::new(0),
            prune_guard: Mutex::new(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Account folder (validated: 64 lowercase hex digits).
    pub fn account_dir(&self, account: &str) -> Result<PathBuf> {
        if account.len() != 64 || !account.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid Immich account identifier.".into());
        }
        Ok(self.root.join(account))
    }

    pub fn path_for(&self, account: &str, asset_id: &str) -> Result<PathBuf> {
        crate::api::valid_id(asset_id)?;
        Ok(self.account_dir(account)?.join(format!("{asset_id}.jpg")))
    }

    fn lock(&self, key: &str) -> Result<Arc<Mutex<()>>> {
        let mut locks = self.locks.lock().map_err(|_| "Immich preview locks unavailable".to_owned())?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }

    /// Returns the cached preview for an asset, fetching and decoding it first when needed.
    /// `current` is checked before the fetch and before the write: a request whose session has ended
    /// never repopulates the cache.
    pub fn ensure(
        &self,
        account: &str,
        asset_id: &str,
        fetch: &dyn Fn() -> Result<Vec<u8>>,
        current: &dyn Fn() -> bool,
    ) -> Result<PathBuf> {
        let path = self.path_for(account, asset_id)?;
        let shared = self.lock(&path.to_string_lossy())?;
        let _same_asset = shared.lock().map_err(|_| "Immich preview unavailable".to_owned())?;
        if path.is_file() {
            // Touch so pruning keeps recently viewed previews.
            if let Ok(file) = fs::File::open(&path) {
                let _ = file.set_modified(std::time::SystemTime::now());
            }
            return Ok(path);
        }
        if !current() {
            return Err("Immich session changed before loading the preview".into());
        }
        let raw = fetch()?;
        let jpeg = {
            let _slot = self.decode_slot()?;
            decode_preview(&raw)?
        };
        if !current() {
            return Err("Immich session changed.".into());
        }
        let dir = self.account_dir(account)?;
        private_directory(&self.root)?;
        private_directory(&dir)?;
        let temporary = dir.join(format!(".{asset_id}.{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary, &jpeg)
            .and_then(|_| fs::rename(&temporary, &path))
            .map_err(|_| {
                let _ = fs::remove_file(&temporary);
                "Could not cache the Immich preview.".to_owned()
            })?;
        if self.writes.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == 0 {
            if let Ok(_guard) = self.prune_guard.try_lock() {
                let _ = prune(&self.root, CACHE_LIMIT);
            }
        }
        Ok(path)
    }

    fn decode_slot(&self) -> Result<DecodeSlot<'_>> {
        let (count, ready) = &self.slots;
        let mut active = count.lock().map_err(|_| "Immich preview queue unavailable.".to_owned())?;
        while *active >= DECODE_SLOTS {
            active = ready.wait(active).map_err(|_| "Immich preview queue unavailable.".to_owned())?;
        }
        *active += 1;
        Ok(DecodeSlot { thumbs: self })
    }

    /// Removes cached previews for the given assets of one account (explicit refresh).
    /// Only regular files named like cache entries are removed; symlinks and other files are refused.
    pub fn invalidate(&self, account: &str, ids: &[String]) -> Result<usize> {
        let dir = self.account_dir(account)?;
        let metadata = match fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(_) => return Err("Immich cache directory is unavailable".into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("Immich cache directory is not a private directory".into());
        }
        let mut removed = 0;
        for id in ids {
            let path = self.path_for(account, id)?;
            let shared = self.lock(&path.to_string_lossy())?;
            let _guard = shared.lock().map_err(|_| "Immich preview unavailable".to_owned())?;
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                    fs::remove_file(&path).map_err(|_| "Could not refresh an Immich thumbnail".to_owned())?;
                    removed += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err("Unexpected file in the Immich thumbnail cache".into()),
            }
        }
        Ok(removed)
    }

    /// Deletes everything this plugin cached (used by `--uninstall`).
    pub fn remove_all(&self) -> Result<()> {
        match fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Could not delete the Immich thumbnail cache".into()),
        }
    }
}

struct DecodeSlot<'a> {
    thumbs: &'a Thumbs,
}

impl Drop for DecodeSlot<'_> {
    fn drop(&mut self) {
        let (count, ready) = &self.thumbs.slots;
        if let Ok(mut active) = count.lock() {
            *active = active.saturating_sub(1);
            ready.notify_one();
        }
    }
}

/// Validates the image header, applies the pixel limit, then decodes and writes a 1024-pixel RGB JPEG.
pub fn decode_preview(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() || bytes.len() as u64 > crate::api::MAX_BYTES {
        return Err("Invalid Immich preview image.".into());
    }
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Invalid Immich preview image.".to_owned())?;
    let (width, height) = reader.into_dimensions().map_err(|_| "Invalid Immich preview image.".to_owned())?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("Immich preview dimensions exceed the safety limit.".into());
    }
    let image = image::load_from_memory(bytes)
        .map_err(|_| "Unsupported Immich preview image.".to_owned())?
        .thumbnail(1024, 1024)
        .to_rgb8();
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut output, image::ImageFormat::Jpeg)
        .map_err(|_| "Could not decode the Immich preview.".to_owned())?;
    Ok(output.into_inner())
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|_| "Could not create the private Immich thumbnail cache".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect the Immich thumbnail cache".to_owned())?;
    }
    Ok(())
}

/// Evicts the least recently used cache entries until the total is at most `max` bytes.
pub fn prune(root: &Path, max: u64) -> Result<()> {
    let mut files = Vec::new();
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(root)
        .min_depth(2)
        .max_depth(2)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|e| e != "jpg") {
            continue;
        }
        let account = entry.path().parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or("");
        let asset = entry.path().file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if account.len() != 64 || !account.bytes().all(|b| b.is_ascii_hexdigit()) || uuid::Uuid::parse_str(asset).is_err() {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        bytes = bytes.saturating_add(meta.len());
        files.push((meta.modified().ok(), meta.len(), entry.into_path()));
    }
    files.sort_by_key(|f| f.0);
    for (_, size, file) in files {
        if bytes <= max {
            break;
        }
        if fs::remove_file(file).is_ok() {
            bytes = bytes.saturating_sub(size);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const ASSET: &str = "11111111-1111-4111-8111-111111111111";
    const SECOND: &str = "33333333-3333-4333-8333-333333333333";
    const ACCOUNT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbImage::from_pixel(width, height, image::Rgb([9, 90, 200]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn accounts_are_bounded_and_unrelated_files_are_untouched() {
        let root = tempfile::tempdir().unwrap();
        let account = root.path().join(ACCOUNT);
        fs::create_dir_all(&account).unwrap();
        fs::write(account.join(format!("{ASSET}.jpg")), vec![0; 60]).unwrap();
        fs::write(account.join(format!("{SECOND}.jpg")), vec![0; 60]).unwrap();
        fs::write(account.join("user-note.txt"), b"keep").unwrap();
        prune(root.path(), 100).unwrap();
        assert_eq!(fs::read(account.join("user-note.txt")).unwrap(), b"keep");
        let remaining = fs::read_dir(&account).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "jpg")).count();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn same_asset_is_fetched_once_for_simultaneous_requests() {
        let dir = tempfile::tempdir().unwrap();
        let thumbs = Arc::new(Thumbs::new(dir.path().to_path_buf()));
        let fetches = Arc::new(AtomicUsize::new(0));
        let bytes = png(8, 8);
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let thumbs = thumbs.clone();
                let fetches = fetches.clone();
                let bytes = bytes.clone();
                std::thread::spawn(move || {
                    thumbs
                        .ensure(ACCOUNT, ASSET, &|| {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            Ok(bytes.clone())
                        }, &|| true)
                        .unwrap()
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn preview_is_refused_when_session_ended_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let thumbs = Thumbs::new(dir.path().to_path_buf());
        let fetched = AtomicUsize::new(0);
        let result = thumbs.ensure(ACCOUNT, ASSET, &|| {
            fetched.fetch_add(1, Ordering::SeqCst);
            Ok(png(4, 4))
        }, &|| false);
        assert!(result.is_err());
        assert_eq!(fetched.load(Ordering::SeqCst), 0);
        assert!(!thumbs.path_for(ACCOUNT, ASSET).unwrap().exists());
    }

    #[test]
    fn oversized_pixel_counts_are_refused_before_decoding() {
        // 6400 x 6251 is about 40.0 megapixels, just above the limit: refused from the header alone.
        let big = png(6400, 6251);
        let refused = decode_preview(&big);
        assert!(refused.is_err());
        assert!(refused.unwrap_err().contains("safety limit"));
    }

    #[test]
    fn previews_are_reduced_to_a_1024_pixel_jpeg() {
        let jpeg = decode_preview(&png(2000, 1000)).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert!(decoded.width() <= 1024 && decoded.height() <= 1024);
        assert_eq!(decoded.width(), 1024);
    }

    #[test]
    fn invalidation_removes_only_validated_entries() {
        let dir = tempfile::tempdir().unwrap();
        let thumbs = Thumbs::new(dir.path().to_path_buf());
        let account = thumbs.account_dir(ACCOUNT).unwrap();
        fs::create_dir_all(&account).unwrap();
        fs::write(account.join(format!("{ASSET}.jpg")), b"old").unwrap();
        fs::write(account.join("notes.txt"), b"keep").unwrap();
        assert!(thumbs.invalidate("bad", &[ASSET.to_owned()]).is_err());
        assert_eq!(thumbs.invalidate(ACCOUNT, &[ASSET.to_owned()]).unwrap(), 1);
        assert!(!account.join(format!("{ASSET}.jpg")).exists());
        assert_eq!(fs::read(account.join("notes.txt")).unwrap(), b"keep");
        assert!(thumbs.invalidate(ACCOUNT, &["../escape".to_owned()]).is_err());
    }
}
