# Reelio engine: command parity checklist

Reference: `reelio/apps/desktop/src-tauri/src/lib.rs` `generate_handler!` (47 commands) and the
window/drop/close handlers. Every command has one `Engine` method (`crates/engine/src/api.rs`) implemented
in `crates/engine/src/runtime.rs` (or its command module). "Ported" means the behaviour and
validation of the reference command are reproduced without Tauri. "Verification" states what was
actually run:

* **E2E** - exercised through `ReelioEngine` with real FFmpeg output (`tests/engine_media.rs`).
* **API** - exercised through `ReelioEngine` with a fake host (`tests/engine_api.rs`).
* **Unit** - a ported reference unit test or a new unit test of the same function.
* **Guard** - only the refusal/validation path is exercised here; the success path needs a live
  external service or a native host and is not verifiable in this environment.
* **Backend** - the underlying reelio-* crate behaviour is covered by its ported test suite
  (see "Crate tests" below); the engine method is a thin, tested wrapper.

Host-only pieces (not engine commands): native dialog rendering, webview window creation,
webview cookie capture, the trusted-IPC origin gate, the asset-protocol scope and the single-instance
plugin. These are the `EngineHost` trait or the host's responsibility (see `docs/test-results.md`, "Differences from the reference").

## Commands

| # | Tauri command (reference) | Engine method | Status | Verification |
|---|---|---|---|---|
| 1 | `backend_status` | `backend_status` | ported | API (`jobs_and_backend_queries_work_on_an_empty_database` calls dependency status; capabilities via backend crate) |
| 2 | `dependencies_status` | `dependencies_status` | ported | API |
| 3 | `dependencies_repair` | `dependencies_repair` | ported (host confirm, repair progress events) | Guard: plan/confirm via `reelio-application` dependency tests; the package manager is not run here |
| 4 | `choose_media(folder)` | `choose_media` | ported (non-recursive, 256-entry limit, authorizes each file) | API (cancel returns empty); E2E (authorize then probe) |
| 5 | `media_probe` | `media_probe` | ported (requires an authorized source) | E2E; API (unauthorized and missing refused) |
| 6 | `project_create` | `project_create` | ported | API; E2E |
| 7 | `interface_zoom` | `interface_zoom` | ported (50..=200, through host) | Unit (`runtime::tests`); API |
| 8 | `app_version` | `app_version` | ported (`<version>-6`) | Unit (`version::tests`) |
| 9 | `window_action` | `window_action` | ported (bounded enum; confirm-close guards) | API (wire names; delegation; idle confirm-close) |
| 10 | `window_platform` | `window_platform` | ported | API |
| 11 | `open_project_homepage` | `open_project_homepage` | ported (fixed URL only) | API |
| 12 | `update_check` | `update_check` | ported | Backend (reelio-updates: 18 tests, 1 live test ignored) |
| 13 | `update_download` | `update_download` | ported (host save dialog, verified no-clobber) | API (unknown offer refused); Backend (download verification tests) |
| 14 | `update_cancel` | `update_cancel` | ported | API (`update_cancel` idle, via reelio-updates) |
| 15 | `subtitle_editor_open` | `subtitle_editor_open` | ported (`.srt` only, authorized path or host picker) | Backend (reelio-subtitles: 15 tests) |
| 16 | `subtitle_editor_save` | `subtitle_editor_save` | ported (validate, new SRT copy, authorize) | Backend (`save_copy` tests) |
| 17 | `subtitle_editor_dirty` | `subtitle_editor_dirty` | ported | API (idle) |
| 18 | `subtitle_editor_draft` | `subtitle_editor_draft` | ported (clear > save > read) | API (`subtitle_draft_roundtrip_and_clear_do_not_touch_original_files`) |
| 19 | `subtitle_models` | `subtitle_models` | ported (loopback Ollama only) | API (error names the local service when absent) |
| 20 | `subtitle_model_start` | `subtitle_model_start` | ported (host approval first) | API (`local_model_operations_need_host_approval_first`); live Ollama not verifiable here |
| 21 | `subtitle_model_status` | `subtitle_model_status` | ported | API (idle) |
| 22 | `subtitle_model_cancel` | `subtitle_model_cancel` | ported | API (idle) |
| 23 | `subtitle_model_results` | `subtitle_model_results` | ported | Backend (reelio-subtitles `validate_translations` and transport tests); engine call for unknown id is a passthrough |
| 24 | `project_validate` | `project_validate` | ported (pure planning) | API; E2E |
| 25 | `project_open` | `project_open` | ported (validated load, grants referenced files, records recent) | E2E (`project_save_and_open_round_trip_through_the_host_dialogs`); Backend (23 application/domain tests) |
| 26 | `project_save` | `project_save` | ported (atomic, never replaces a source) | E2E; Backend (`project_roundtrip_relative_paths`) |
| 27 | `export_plan` | `export_plan` | ported (validates sources first) | E2E |
| 28 | `export_start` | `export_start` | ported (plan errors, conversion approval, host save dialog, no-overwrite refusal, background job) | E2E (`export_through_the_engine_publishes_a_new_file_and_never_touches_the_source`); Backend (FFmpeg suites: timeline, trimming, video window, media) |
| 29 | `export_cancel` | `export_cancel` | ported | Guard (unknown id); cancellation semantics in reelio-application/ffmpeg tests |
| 30 | `jobs_list` | `jobs_list` | ported (metadata-only summaries) | E2E (lists the completed job); API (empty) |
| 31 | `job_project` | `job_project` | ported (schema upgrade, validation, artwork check, host approval, source grant) | E2E (restores a completed export's project); API (missing job refused) |
| 32 | `settings_get` | `settings_get` | ported (same JSON shape) | API (defaults and keys) |
| 33 | `settings_choose_folder` | `settings_choose_folder` | ported | API (cancel returns none) |
| 34 | `settings_update` | `settings_update` | ported (whitelisted keys, validated values) | API (`settings_update_accepts_only_whitelisted_values_and_persists_them`); Unit (`preferences::tests`) |
| 35 | `choose_artwork` | `choose_artwork` | ported | API (cancel returns none) |
| 36 | `artwork_import` | `artwork_import` | ported (authorized, bounded, probes still images only) | Backend (reelio-artwork: 8 tests); Guard |
| 37 | `artwork_edit` | `artwork_edit` | ported | Backend (artwork edit tests) |
| 38 | `artwork_save_copy` | `artwork_save_copy` | ported (new file in the default format) | Backend (`save_copy` tests); Guard |
| 39 | `poster_search` | `poster_search` | ported | Backend (reelio-resources: 10 tests; live Bing test ignored, requires public access) |
| 40 | `poster_image` | `poster_image` | ported (proportion check, crop) | Backend (reelio-resources); live test ignored |
| 41 | `choose_subtitles` | `choose_subtitles` | ported (max 32 authorized files) | API (cancel returns empty) |
| 42 | `subtitle_preview` | `subtitle_preview` | ported (zero-offset tracks only) | Backend (reelio-ffmpeg subtitle preview); Guard |
| 43 | `preview_source` | `preview_source` | ported (loopback capability server; emits `preview:ready`) | Unit (preview server: 4 tests, 2 native WebKit ignored); API (unauthorized refused); Backend (`reelio-ffmpeg` preview tests) |
| 44 | `resource_queue` | `resource_queue` | ported (public HTTPS only, expiring offer) | API (non-HTTPS and loopback refused) |
| 45 | `resource_cancel` | `resource_cancel` | ported | API (unknown id) |
| 46 | `resource_download` | `resource_download` | ported (validated artwork or subtitles, authorized outputs) | Backend (reelio-resources download/unpack tests); Guard |
| 47 | `search_browser_open` | `search_browser_open` | ported (dependency check, host opens isolated browser) | Guard (component check event); browser itself is host-only |

### Additional engine methods (replace webview/window handlers)

| Reference mechanism | Engine method | Verification |
|---|---|---|
| `on_window_event` drag-drop (`media:dropped`) | `media_dropped` | API (regular files only, 256 limit, event payload) |
| `on_window_event` close (`app:close-blocked` / `app:close-requested`) | `close_requested` | API (idle path emits `CloseRequested`) |
| Browser navigation policy (`on_navigation`, `on_new_window`) | `search_browser_allows` | API (non-HTTPS, `tauri://` refused) and Unit (`resources::tests`) |
| Browser download hook (`on_download` + cookie capture) | `search_browser_download` | Unit (URL validation shared with `resource_queue`); the webview cookie capture is host-only |

## Crate tests (ported verbatim, identical sources)

`crates/*` in `plugins/reelio` are byte-identical to the reference copies (checked with `diff -rq`).
Current results (`cargo test --workspace --no-fail-fast`, this machine: macOS, FFmpeg 9.0.1 without libwebp):

* reelio-domain 20 passed; reelio-artwork 8; reelio-resources 10; reelio-storage 5 (transactional schema and upgrade tests);
  reelio-subtitles 15 (1 ignored); reelio-updates 18 (1 ignored); reelio-ffmpeg lib 7;
  FFmpeg integration: timeline 5, trimming 4, video_window 4, media 12 (after the fixture fix below), preview 2 of 4 (1 ignored).
* reelio-application 16 (3 live network tests ignored).
* reelio-engine: unit 10 (2 native WebKit tests ignored), `engine_api` 22, `engine_media` 2.

See `docs/test-results.md` for the exact failing and ignored cases and their causes.
