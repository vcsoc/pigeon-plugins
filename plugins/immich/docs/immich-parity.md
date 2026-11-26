# Immich parity catalogue

Audit of the built-in Immich support in Pigeon 0.3.181 (`pigeon-rust` main, `src-tauri/src/immich*.rs`,
`docs/IMMICH.md`, egui touchpoints) against the isolated plugin `pigeon-immich` 1.0.0.

Status legend:
- **verified**: exercised by a test in this repository that was run (command and result in the report).
- **unit**: pure logic covered by a unit test that was run.
- **implemented**: code path present, no test run here.
- **host**: depends on a host service or host UI that is not integrated yet (see "Open items").
- **not live**: needs a real Immich server; the fake server in `tests/support/fake_immich.rs` stands in.
- **intentional**: a deliberate difference, listed so it can be audited.

Test evidence: `cargo test` in `plugins/immich` (35 unit, 34 integration, including a targeted re-run of the 401 test) and `pigeon-mock run .`
(official WP2 mock host: 0 schema violations, preferences scene validated).

## A. Accounts and connection

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| A1 | Preferences > Immich: server URL, email, password, enable toggle, "Remember credentials" toggle, Connect / save, Sync now, Disconnect | `preferences.page` `settings` (`ui.rs::prefs`) with the same controls, plus **Test connection** (new: logs in and reads tags, saves nothing) | verified (`preferences_remember_*`, `test_connection_*`) |
| A2 | Remembered password stored in the system keyring; config holds only a lookup id | `secrets.set/get/delete` through the host; `immich.json` holds `secretId`, never the password | verified on disk and in mock keychain (`preferences_remember_*`, `session_only_*`); real keychain **host** |
| A3 | Session-only login: password kept in memory, not persisted, no auto-reconnect at startup | in-memory only; `start()` reconnects only when a secret id exists | verified (nothing written) ; no-reconnect path implemented |
| A4 | Unticking "remember" disables the previous remembered connection without deleting its keyring entry (so a restart does not reconnect with an older password) | `configure()` saves previous config with `enabled:false`, keeps `secret_history` | implemented (not tested) |
| A5 | Login rejected message (HTTP 401) with guidance; no password in errors | same text; error bodies are never shown (status only) | verified (`wrong_password_*`) |
| A6 | HTTP 401 during use: re-login with remembered password, once; session-only refuses with the same explanation | `with_auth` / `reauthenticate` | verified (`expired_sessions_retry_once_only_when_credentials_are_remembered`) |
| A7 | Account identity = sha256(normalised URL, lower-cased email), so identities survive migration | `api::account_key`, `config::secret_key` | unit (`account_identity_is_case_insensitive_on_email`) |
| A8 | Server URL: http/https only, no credentials, query or fragment; trailing `/api` removed | `api::normalize_server_url` | unit (`urls_are_normalised_and_unsafe_forms_rejected`) |
| A9 | Redirects rejected (credentials never forwarded); 10 s timeouts; 20 MiB response cap | reqwest `Policy::none()`, same timeouts and cap | verified for redirect rejection and advertised length (`authenticated_requests_reject_redirects`, `responses_over_the_size_limit_are_rejected`); timeouts not separately tested |
| A10 | Keychain identifiers never reach the UI | `config::public` has no lookup id | unit (`settings_file_never_holds_a_password...`) |
| A11 | No personal `.immich` configuration is read or copied | no such code path | verified by inspection |

## B. Collection tree and references

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| B1 | Tree `Immich / Annual / year / twelve months` (root, annual, year, month) | `source.children`: root -> annual -> years (newest first) -> 12 months with counts | verified (`collection_tree_lists_root_annual_years_and_twelve_months`) |
| B2 | Capture-local date decides grouping; invalid or missing dates stay at the root | `calendar::image_date` (`localDateTime`, fallback `fileCreatedAt`, leap-year checks) | unit + verified (`list_shows_invalid_dates_at_root...`) |
| B3 | Root and Annual contain every image of the account | `list` for `root`/`annual` uses all rows | verified |
| B4 | Only IMAGE assets are indexed; videos are ignored | `db::row_from_remote` | unit (`invalid_remote_ids_and_non_images_are_ignored`) |
| B5 | Remote references are not local originals; they are listed as "Immich (thumbnail only)" | `db::item` meta `source`, `originalAvailable:false` for missing rows | verified |
| B6 | Items removed on the server are kept and flagged as missing (`sourceMissing`) | generation check; `meta.missing` | unit (`snapshot_builds_tree_and_flags_missing_references`) |
| B7 | Favourite flag and tag names from the server | `favorite`, `tags` | unit + verified via item JSON |
| B8 | Listing is by capture date, newest first (built-in: server search order) | `ORDER BY taken DESC, id DESC` | intentional (order source differs) |
| B9 | Browsing works from the cached catalogue while disconnected (built-in required a session for its image list) | `account()` falls back to the configured account | intentional; thumbnails still need a session |

## C. Thumbnails and cache

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| C1 | Thumbnails use the preview endpoint only; originals are never fetched for browsing | `api::thumbnail` (`size=preview`) | verified (`originals_are_never_opened_for_editing_only_previews`, `thumbnails_come_from_the_preview...`) |
| C2 | Cache is account-scoped, private to the user (0700) | `thumbs.rs::private_directory` | verified (0700 check) |
| C3 | Bounded to 256 MiB; oldest evicted; pruning every 32 writes, never blocking a request | `prune`, `PRUNE_EVERY`, `try_lock` | unit (`accounts_are_bounded_and_unrelated_files_are_untouched`) |
| C4 | Concurrent requests for one thumbnail share one fetch | per-path lock with weak map | verified (`same_asset_is_fetched_once...`) |
| C5 | At most two previews decode at once | `DECODE_SLOTS = 2` | implemented (not tested) |
| C6 | Previews over 40 megapixels refused before decoding; output reduced to 1024 px JPEG | `decode_preview` | unit (`oversized_pixel_counts...`, `previews_are_reduced...`) |
| C7 | Cached previews survive refreshes and reconnects; only "Sync and refresh thumbnails" evicts this account's files | `Thumbs::invalidate` only on `sync-collection` | implemented (not tested) |
| C8 | Disconnect revokes access to previews (cache kept on disk) | `preview_file` requires the live session id | implemented (not tested) |
| C9 | Stalled or slow preview server never blocks the host | own worker pool (4) with bounded queue; saturated pool answers `busy` | verified (`a_stalled_thumbnail...`, `a_saturated_worker_pool...`) |
| C10 | Single-decode preview refusal for corrupt/unsupported images | `decode_preview` errors | implemented (not tested) |

## D. Explicit transfers

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| D1 | Local files sent to Immich as originals; local source files never moved or removed | `transfer::upload` streams the file; no delete | verified (`upload_sends_local_files...`: bytes unchanged) |
| D2 | Upload metadata in the `pigeon.asset/v1` namespace, without paths, locks, secrets or runtime URLs | `transfer::portable` | unit (`uploads_keep_annotations_but_exclude...`) + verified (no local path in metadata) |
| D3 | Immich 2.5 or newer required; v2 sends device fields | `major < 2` refused; `major == 2` adds device fields | verified for v2 fixture; v3 path implemented, not tested |
| D4 | Server SHA-1 checksum compared with the uploaded bytes | `transfer::upload` | verified (success path) |
| D5 | Uploaded metadata read back and compared | `GET .../metadata/pigeon` equality | verified (`upload_is_not_a_success_when_metadata_cannot_be_read_back`) |
| D6 | Tag assignments must be confirmed by the server | `add_existing_tag`/confirmation check | implemented (not tested) |
| D7 | Source file changed during upload is refused | size and mtime comparison | implemented (not tested) |
| D8 | Originals limited to 50 GiB per file; batches to 200; explicit messages | `MAX_ORIGINAL_BYTES`, `BATCH_LIMIT` | unit (`size_limits_match...`, `download_counter...`); batch limit not tested |
| D9 | Downloads streamed to a private temp file, verified, then published without overwrite | `transfer::download` (`persist_noclobber`) | verified (`corrupt_download_is_never_published` leaves no temp files) |
| D10 | Identical file already in place is reused without another original GET | `reused` path | verified (`identical_destination_is_reused...`) |
| D11 | Different file in place: nothing overwritten, item reported | `download` error "nothing was overwritten" | verified (`conflicting_file_is_left_unchanged...`) |
| D12 | Length or checksum mismatch: discarded | error message | verified (`corrupt_download_is_never_published`) |
| D13 | Filenames sanitised (control and reserved characters), 180-character limit | `download` name rules | implemented (not tested) |
| D14 | Complete server metadata kept in an immutable `.pigeon-immich.<digest>.json` sidecar, re-read and compared | `download` sidecar | verified (`download_verifies_the_checksum...`: schema, one sidecar) |
| D15 | Original embedded metadata stays byte-identical | exact bytes written | verified (bytes compared) |
| D16 | Verified download is indexed locally with tags, note, rating, favourite and capture date | `index_download` -> host `library.import`, `library.set_tags`, `library.set_meta` | verified against the mock host (note and rating patch); **host** (service shapes assumed) |
| D17 | Stop finishes the current file; unstarted files listed as not started; retry reuses verified originals | `stop_batch`, `finish_batch`, `retry_batch` | verified (`stop_finishes_the_current_file_and_retry_completes_the_rest`) |
| D18 | Dialog: per-file state, "n of m verified", stop/retry, final success line | `ui::transfer` | verified (text in tests) |
| D19 | Accepted binary with failed metadata verification is not a successful file or batch | `verified && metadataVerified` | verified (test D5) |
| D20 | One batch at a time ("Finish the current transfer batch first") | `start_batch` | implemented (not tested) |
| D21 | Upload of a reference that already lives on Immich is refused per item | `send_local` | implemented (not tested); unknown ids verified ("Not found in the library") |
| D22 | Mixed local and Immich selections are split so each transfer is verified | the host splits selections; plugin handles only its own ids | **host** |
| D23 | Locked (password-protected) folders are never written to | not implemented in the plugin | **host** (gap: the host must refuse the folder before the plugin writes) |
| D24 | Virtual collections are not download destinations | destination is a folder chosen in the OS dialog | intentional (host dialog) |

## E. Tags and metadata

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| E1 | Tags added in Pigeon are created on Immich and assigned; errors reported, never silent | `source.write_meta` -> `push_tags`; toast on failure | verified (`tag_write_back_failure_is_reported_never_silent`, `tag_write_back_success_is_recorded_locally`) |
| E2 | Pushed tags recorded locally (`immichSyncedTags`) | `set_synced_tags` | unit (`synced_tags_survive_later_snapshots`) |
| E3 | Tag picker: assign an existing Immich tag to up to 1000 selected references | `command tag` -> modal `tag-picker` -> `assign_tag` | verified (`tag_picker_*`) |
| E4 | Tags listed by name | `db::tags` | verified (picker options) |
| E5 | A tag the image already has is not counted again | `add_asset_tag` returns 0 -> "already has that tag" | verified |
| E6 | Tag picker refuses references outside the connected account | `open_tag_picker` checks ids | verified (`tag_picker_refuses_references_outside...`) |

## F. Sync and background work

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| F1 | Full catalogue sync (paged search, 250 per page, repeated page refused) | `api::list_images`, `visit_image_pages` | unit (`pages_are_followed_until_null`, `repeated_pages_are_rejected`) |
| F2 | Sync and per-collection refresh run in the background with a footer job | dedicated threads, `progress.start/update/end` | verified (progress calls in tests) |
| F3 | Sync cancellable; stale or cancelled results never change the catalogue | cancel flag, generation and session checks inside the SQLite transaction | verified (`sync_can_be_cancelled_by_the_host...`, unit `stale_generation_writes_nothing`) |
| F4 | Startup: remembered connection reconnects and syncs automatically | `start()` | verified (`startup_reconnects_with_the_remembered_password...`) |
| F5 | Status messages ("Immich synchronized: N images; originals stay on the server", cancellation text) | `finish_busy` | verified (text) |
| F6 | Sync waits for portfolio loading | not applicable (no portfolio) | intentional |

## G. Status, footer, commands

| # | Built-in behaviour | Plugin | Status |
|---|---|---|---|
| G1 | Connection status in the footer / status bar | `statusbar.item` `status`: "Immich: off" / "Immich: N images" | verified (`status_bar_shows_the_connection_state`) |
| G2 | Footer job labels for sync and transfer | `progress.start` titles | implemented (titles not asserted) |
| G3 | Toasts for batch summaries and tag errors | `notify.toast` | verified (tag error toast) |
| G4 | Sync Immich command; refresh-thumbnails command on the root | commands `sync`, `sync-collection` | implemented (command route verified for `sync`) |
| G5 | Asset actions: Send to Immich; Download original from Immich; Add Immich tag | `asset.action` `send`, `download`, `tag` | verified via `command` pushes (send, download, tag) |

## H. Intentional differences and omissions

| # | Item | Reason |
|---|---|---|
| H1 | No drag-out of Immich references | the built-in code excluded them; the plugin has no `drag` permission |
| H2 | Remote items cannot be edited; "download the original before editing" | enforced by `source.open` (edit refused, verified) and by the host inspector |
| H3 | Magnifier, hover preview, similar images and video editing disabled for references | host responsibility (these read Pigeon records); **host** |
| H4 | Hidden API-key browser | only the headless constructor `ImmichClient::with_api_key` remains (not reachable from the UI or any command) |
| H5 | Electron-era API surfaces | not ported |
| H6 | Per-plugin preview decode pool instead of a process-wide one | the plugin is its own process |

## Open items and assumptions (must be confirmed with the host)

1. `library.get {ids}` answers `{records:[...]}` (also accepted: `items` or a bare array); records carry `id`, `name`, `path`, `tags`, `note`, `rating`, `favorite`, `created`.
2. `library.import {paths, mode:"reference"}` answers `{ids:[...]}`.
3. `library.set_tags {id, tags, mode:"add"}` and `library.set_meta {id, patch}`.
4. `dialog.pick_folder {title}` answers `{path}`; called with a 10-minute timeout.
5. `command` pushes carry the selection as `context.ids` or `context.selection`.
6. `asset.action` contributions carry `source:"immich"` to show "Download" and "Tag" only on Immich references (host filter).
7. `source.write_meta {ids, tags}` and `source.upload {node, files}` / `source.download {ids, dest}` parameter shapes follow the manifest's capability list; the protocol text does not fix them.
8. `progress.start` answers `{job}` (SDK behaviour).
9. Uninstall (`run --uninstall`) requires the host to send `hello` and answer `secrets.delete`; with no answer each key is given 10 seconds, then local data is removed and the exit code is 1.
10. No live Immich server was available: every server interaction was tested against the loopback fake only.
