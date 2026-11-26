# Reelio engine: test results, differences and limits

Commands (run from `plugins/reelio`, `CARGO_TARGET_DIR=/tmp/pigeon-target-reelio-engine`):

```sh
cargo fmt --all --check                          # passes
cargo clippy --workspace --all-targets -- -D warnings   # passes (exit 0)
cargo test --workspace --no-fail-fast           # 160 passed, 2 failed, 8 ignored
```

Environment: macOS (darwin), Homebrew FFmpeg/FFprobe 9.0.1 (no `libwebp` encoder), no display, no Windows,
no GStreamer/WebKitGTK, no running Ollama, no public-internet provider access.

## Failing tests (environmental, reproduced on the untouched reference)

| Test | Cause | Evidence |
|---|---|---|
| `reelio-ffmpeg/tests/preview.rs::avif_is_a_bounded_image_not_a_movie` | fixture generation needs a WebP encoder; this FFmpeg has none (`[vost] Default encoder for format webp ... probably disabled`) | the same two tests fail identically in `reelio/` with `CARGO_TARGET_DIR=/tmp/pigeon-target-reelio-ref` |
| `reelio-ffmpeg/tests/preview.rs::single_images_use_signature_selected_literal_demuxers` | same | same |

These need an FFmpeg built with `libwebp` (for example the Linux CI package). They were not weakened.

## Fixture fixes made in the port (test-only, assertions unchanged in intent)

On macOS the temp directory is `/var/...`, a symlink to `/private/var/...`. The reference fixtures compare
non-canonical paths against canonical ones, so they failed on macOS in the reference too (the README already lists
relative-project-path fixtures as a Windows/macOS CI failure):

* `reelio-application` `tests::project_roundtrip_relative_paths` now builds its paths from the canonical root
  (authorized paths are canonical in the application).
* `reelio-ffmpeg/tests/media.rs::command_is_structured_and_maps_exact_indices` compares against the canonical
  source path, because `probe()` canonicalizes asset paths.

Both pass after the change. Their assertions are the same as the reference.

## Ignored tests (opt-in; not run)

* `reelio-application`: `live_internal_poster_search_and_selection`, `live_poster_proportions_and_pagination`,
  `live_public_artwork_download_is_validated` (public Bing/Wikimedia access).
* `reelio-subtitles`: one live Ollama test (requires a running local service and a model).
* `reelio-updates`: one live GitHub release test. `reelio-ffmpeg`: one explicit user-image fixture test.
* `reelio-engine` `preview::tests::native_webkit_loopback_playback` and `native_webkit_timeline_audio_sync`:
  need WebKitGTK, a display and the reference's browser harness. Not verifiable here.

## Behaviour fix made in the port

* **Preview range truncation on macOS.** The loopback listener is non-blocking, and BSD/macOS accepted sockets
  inherit `O_NONBLOCK`. Large `write_all` calls then fail with `WouldBlock` and the response is cut off
  (observed: 7,519,788 of 9,436,160 bytes in `open_ended_ranges_do_not_truncate_large_video_at_eight_mib`). The
  reference has the same flaw. The port sets accepted streams back to blocking mode. Three consecutive runs pass.

## Differences from the reference (intentional)

1. **Host dialogs.** `rfd`-style `blocking_*` Tauri dialogs became `EngineHost` calls (`pick_files`,
   `pick_folder`, `save_file`, `confirm`) returning `None` on cancel. `FileDialog.directory` is optional, because
   the reference's update-save dialog set no start directory.
2. **No asset-protocol scope.** `grant()` no longer also calls `asset_protocol_scope().allow_file`. The engine
   authorizes canonical files only; preview uses the loopback capability server, not webview asset URLs.
3. **Trusted-IPC gate is the host's job.** `trusted_ipc_source` (origin check) and its Tauri test are not
   ported. The engine has no IPC surface; the host must allow only its own UI to call privileged methods.
4. **Window and zoom actuation is the host's job.** `window_action` (bounded enum, close and confirm-close
   guards) and `interface_zoom` (50-200%) are engine methods; the actual window operations go through
   `EngineHost::window_control` and `set_interface_zoom`. The reference's `tauri.conf.json` `decorations`
   assertion is host configuration and is not ported.
5. **Search browser.** Creating the isolated webview, its navigation/popup/download hooks and cookie capture are
   host-side. The engine provides the policy (`search_browser_allows`), the offer creation
   (`search_browser_download`, cookie kept in memory, never logged) and the launch check (`search_browser_open`).
6. **Events over a channel.** `std::sync::mpsc` replaces Tauri `emit`; event names are unchanged
   (`export:updated`, `media:dropped`, `app:close-blocked`, `app:close-requested`, `dependencies:changed`,
   `resource:download-requested`, `resource:error`). New: `preview:ready`, `dependencies:repair`, and a
   `notification` variant that the engine does not emit yet (the host or UI decides what to notify).
7. **Synchronous API.** Methods block (disk, FFmpeg, SQLite, dialogs). The host calls them off its UI thread.
   Export still runs on its own thread, as in the reference, and returns a job id immediately.
8. **Version identity.** `app_version()` is `<crate version>-<PACKAGE_REVISION>` with `PACKAGE_REVISION = "6"`.
   The reference derived the revision from `packaging/arch/PKGBUILD.in`, which is not part of this workspace.
9. **Single instance and drop plumbing** (`tauri_plugin_single_instance`, `on_window_event` drop/close) are
   host responsibilities. The engine methods `media_dropped` and `close_requested` keep the reference rules
   (256-file limit, regular files only, active-job blocking).
10. **`default_folder` fallback.** The reference used the Tauri Videos directory, falling back to `~/Videos`.
    `EngineHost::videos_dir` must provide that fallback; the engine creates the folder as before.

Everything else keeps the reference's validation, error strings, JSON shapes, job state machine, no-overwrite
publication, source authorization and schema behaviour (the ported crates are byte-identical).

## Added tests for the new engine layer

* `crates/engine/tests/engine_api.rs` (22): settings whitelist and persistence, zoom bounds, window action wire
  names and guards, close decisions and events, drop authorization, cancelled pickers, project create/validate,
  unauthorized probe/preview refused, subtitle draft round trip, host approval before local-model work, resource
  URL policy, event wire names, reopening the database without reset.
* `crates/engine/tests/engine_media.rs` (2, with generated media): export through `export_start` to a new file
  (job reaches `completed`, source bytes unchanged, durable snapshot restores, existing destination refused);
  project save and reopen through host dialogs with recent-file recording.
* Unit: `preferences` (2, ported), `version` (1, ported), `runtime` interface-zoom (2, ported),
  `resources` navigation policy (1, replaces the Tauri origin test), `preview` (4 ported; 2 native ignored).

## Not verifiable in this environment

* Native file dialogs, the isolated search webview, webview cookies, window chrome, zoom in a real window.
* Windows and macOS installers, packaging, signing; Windows runtime; the native Linux GStreamer/WebKitGTK
  preview and audio-clock tests (ignored).
* The package-manager repair path (`dependencies_repair` runs pkexec/apt/pacman only on a supported Linux host).
* Live provider and service calls: Bing, Wikimedia, GitHub releases, a local Ollama translation model.
* WebP/AVIF cover fixtures that need a libwebp-enabled FFmpeg (the two failing tests above).
