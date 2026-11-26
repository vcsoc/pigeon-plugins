//! Services the engine needs from the Pigeon host. The engine never calls Tauri or the plugin SDK
//! directly; the plugin binary implements this trait. Every method is called from a worker
//! thread, never from the UI thread, and may block until the user answers a dialog.

use crate::types::WindowAction;
use std::path::PathBuf;

/// One file/folder/save dialog request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDialog {
    /// Dialog title. `None` lets the host use its default.
    pub title: Option<String>,
    /// Start directory (already resolved by the engine through the default-folder setting).
    pub directory: PathBuf,
    /// Suggested file name for save dialogs.
    pub file_name: Option<String>,
    /// Extension filters as `(label, extensions without dot)`.
    pub filters: Vec<(String, Vec<String>)>,
}

/// Severity of a yes/no confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Info,
    Warning,
}

/// A yes/no confirmation. `true` means the user chose Yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub kind: ConfirmKind,
}

/// Host services. Dialogs return `None` when the user cancels.
pub trait EngineHost: Send + Sync + 'static {
    /// Private, per-plugin data directory (database, subtitle drafts, downloads, caches).
    fn data_dir(&self) -> PathBuf;

    /// The operating system's Videos folder, if known.
    fn videos_dir(&self) -> Option<PathBuf>;

    /// Open dialog for one or more existing files.
    fn pick_files(&self, request: &FileDialog) -> Option<Vec<PathBuf>>;

    /// Open dialog for one existing folder.
    fn pick_folder(&self, request: &FileDialog) -> Option<PathBuf>;

    /// Save dialog for one new or chosen path. The engine re-checks existence itself.
    fn save_file(&self, request: &FileDialog) -> Option<PathBuf>;

    /// Yes/no confirmation shown before a privileged or expensive operation.
    fn confirm(&self, request: &ConfirmDialog) -> bool;

    /// Open an allow-listed fixed URL in the system browser (the engine passes only fixed URLs).
    fn open_url(&self, url: &str) -> Result<(), String>;

    /// Open the isolated search browser window at `url` (no Reelio capabilities granted).
    fn open_search_browser(&self, url: &str) -> Result<(), String>;

    /// Perform a window-chrome action. Returns whether the window is maximized afterwards.
    fn window_control(&self, action: WindowAction) -> Result<bool, String>;

    /// Apply a validated interface zoom factor (0.5..=2.0).
    fn set_interface_zoom(&self, factor: f64) -> Result<(), String>;
}
