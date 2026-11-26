//! Verified update checks and no-clobber downloads (reference `apps/desktop/src-tauri/src/updates.rs`).

use crate::context::Ctx;
use crate::host::FileDialog;
use crate::types::*;
use crate::version::app_version;
use reelio_updates::Target;

/// Former `update_check`: fixed HTTPS GitHub endpoints, bounded; never reports "up to date" on failure.
pub(crate) fn update_check(ctx: &Ctx) -> EngineResult<Report> {
    ctx.updates.check(&app_version(), Target::current())
}

/// Former `update_download`: a host save dialog approves the destination; the download is
/// size/SHA-256 verified and published without overwriting.
pub(crate) fn update_download(ctx: &Ctx, offer_id: String) -> EngineResult<Option<String>> {
    let name = ctx.updates.filename(&offer_id)?;
    let Some(path) = ctx.host.save_file(&FileDialog {
        title: Some("Save verified Reelio update".into()),
        directory: None,
        file_name: Some(name),
        filters: vec![],
        multiple: false,
    }) else {
        return Ok(None);
    };
    ctx.updates.download(&offer_id, &path)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Former `update_cancel`.
pub(crate) fn update_cancel(ctx: &Ctx) {
    ctx.updates.cancel();
}
