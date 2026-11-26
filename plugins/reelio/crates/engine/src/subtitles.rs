//! Subtitle editor drafts/SRT copies and the local translation model service
//! (reference `apps/desktop/src-tauri/src/subtitles.rs`, Tauri-free).

use crate::context::Ctx;
use crate::host::{ConfirmDialog, ConfirmKind, FileDialog};
use crate::types::*;
use reelio_subtitles::Document;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

fn srt_filter() -> (String, Vec<String>) {
    ("SubRip subtitles".into(), vec!["srt".into()])
}

/// Former `subtitle_editor_open`: an existing authorized `.srt`, or a host picker result.
pub(crate) fn subtitle_editor_open(
    ctx: &Ctx,
    path: Option<PathBuf>,
) -> EngineResult<Option<SubtitleEditorOpened>> {
    let path = if let Some(path) = path {
        ctx.app.require_authorized(&path)?
    } else {
        let Some(files) = ctx.host.pick_files(&FileDialog {
            title: Some("Open SRT for editing (original stays unchanged)".into()),
            directory: Some(ctx.default_folder()?),
            file_name: None,
            filters: vec![srt_filter()],
            multiple: false,
        }) else {
            return Ok(None);
        };
        let Some(file) = files.into_iter().next() else {
            return Ok(None);
        };
        ctx.grant(&file)?
    };
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_none_or(|e| !e.eq_ignore_ascii_case("srt"))
    {
        return Err(
            "The subtitle editor opens .srt files; export embedded subtitles to SRT first".into(),
        );
    }
    let document = Document::read(&path)?;
    Ok(Some(SubtitleEditorOpened { document, path }))
}

/// Former `subtitle_editor_save`: validates, asks for a NEW file, writes a copy and authorizes it.
pub(crate) fn subtitle_editor_save(ctx: &Ctx, document: Document) -> EngineResult<Option<String>> {
    document.validate(true)?;
    let Some(path) = ctx.host.save_file(&FileDialog {
        title: Some("Save a new SRT copy; existing files are protected".into()),
        directory: Some(ctx.default_folder()?),
        file_name: Some("subtitles-edited.srt".into()),
        filters: vec![srt_filter()],
        multiple: false,
    }) else {
        return Ok(None);
    };
    document.save_copy(&path)?;
    let path = ctx.grant(&path)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Former `subtitle_editor_dirty`: only ever raises the flag (cleared by a saved draft/clear).
pub(crate) fn subtitle_editor_dirty(ctx: &Ctx, dirty: bool) {
    if dirty {
        ctx.subtitles.dirty.store(true, Ordering::SeqCst);
    }
}

/// Former `subtitle_editor_draft`: `clear` wins, then `document` saves, otherwise the stored draft.
pub(crate) fn subtitle_editor_draft(
    ctx: &Ctx,
    request: SubtitleEditorDraftRequest,
) -> EngineResult<Option<Document>> {
    if request.clear {
        ctx.subtitles.clear_draft()?;
        Ok(None)
    } else if let Some(document) = request.document {
        ctx.subtitles.save_draft(&document)?;
        Ok(None)
    } else {
        ctx.subtitles.draft()
    }
}

/// Former `subtitle_models`.
pub(crate) fn subtitle_models(ctx: &Ctx) -> EngineResult<Value> {
    ctx.subtitles.models()
}

/// Former `subtitle_model_start`: asks the host for approval (translation or model download).
pub(crate) fn subtitle_model_start(
    ctx: &Ctx,
    request: SubtitleModelStartRequest,
) -> EngineResult<JobInfo> {
    let SubtitleModelStartRequest {
        model,
        document,
        language,
    } = request;
    let message = if document.is_some() {
        "Translation sends cue IDs and text only to the configured loopback Ollama service, never video files or cue timings. Local inference uses additional RAM. Starting a new operation replaces the previous model-result record; save/apply any results you want to keep first. Use a trusted service configured for local inference, not a cloud proxy. Translation quality is not guaranteed; review before applying. Start translation?"
    } else {
        "Download this local model plugin through Ollama? Model weights can require several gigabytes of disk space and internet access. The selected model may be installed or refreshed; other model tags and service settings are not changed by Reelio. Follow the model publisher's license terms. Cancellation stops Reelio's request, but Ollama may retain/finish cached downloads. Ollama must already be installed and running locally."
    };
    let approved = ctx.host.confirm(&ConfirmDialog {
        title: "Local subtitle model".into(),
        message: format!("{model}\n\n{message}"),
        kind: ConfirmKind::Info,
    });
    if !approved {
        return Err("Local model operation was not approved".into());
    }
    ctx.subtitles.start(model, document, language)
}

/// Former `subtitle_model_status`.
pub(crate) fn subtitle_model_status(ctx: &Ctx) -> EngineResult<JobInfo> {
    ctx.subtitles.status()
}

/// Former `subtitle_model_cancel`.
pub(crate) fn subtitle_model_cancel(ctx: &Ctx) {
    ctx.subtitles.cancel();
}

/// Former `subtitle_model_results`.
pub(crate) fn subtitle_model_results(ctx: &Ctx, id: String) -> EngineResult<Value> {
    ctx.subtitles.results(&id)
}
