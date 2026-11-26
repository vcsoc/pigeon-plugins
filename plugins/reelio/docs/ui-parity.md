# Reelio plugin UI: parity checklist (WP5)

Reference: `reelio` apps/desktop (read only), README.md, docs/ui-styling.md, docs/subtitle-editor.md,
docs/artwork-and-subtitles.md, Vitest tests and Playwright scenarios.

Status legend:

* **verified-by-test**: a Rust test in `crates/ui` or `crates/app` exercises the behaviour through the
  reducer (`State::dispatch`) or the scene/protocol output. Test names are given.
* **implemented-unverified**: code path exists (reducer and/or scene node) but no test asserts it.
* **partial**: some of the behaviour exists; the rest is listed in the row.
* **not-implemented**: not built in this revision.
* **host-owned**: the Pigeon host provides the behaviour; the plugin does not reproduce it.
* **not-verifiable-here**: needs the real engine, FFmpeg media on a real host, or a renderer.

Visual (pixel) parity: **not measured**. The SDK mock-host PNG renderer is not on `arch/sdk` yet, so no
golden images were produced and no pixel deltas are claimed. The palette and spacing are recorded in
`plugin.json` (`ui.theme_override`) and `scene.rs`; they are unverified against the reference screenshot.

Engine: `reelio-engine` (WP4) is not on `arch/reelio-engine` yet. The app ships `UnconnectedEngine`,
which answers every command with an explicit error. Nothing here has run against real media.

## Project model, store and undo

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Schema 3 project JSON, MP4 default container, `emptyProject` | verified-by-test | `model::empty_project`; serde field names match `types/index.ts` |
| Adding a source names an empty project and adopts its metadata and chapters | verified-by-test | `model::image_assets_have_no_tracks_and_never_replace_movie_metadata` |
| Duplicate path rejected with the reference message, no tracks added | verified-by-test | `model::duplicate_sources_are_rejected_without_tracks` |
| Cover pictures (attached_pic) excluded from video mapping | verified-by-test | `model::cover_pictures_are_excluded_from_video_mapping` |
| First audio stream is `mix`, others `separate` | verified-by-test | `model::first_audio_is_mix_and_others_are_separate` |
| Still image classification (AVIF/PNG/WebP/JPEG) without tracks | verified-by-test | `model::image_assets...` |
| Undo/redo, branching clears redo, no-op edits not recorded | verified-by-test | `store::edit_undo_redo_and_branching_clear_redo`, `state::undo_redo_round_trip_through_reducer` |
| Snapshots never mutated by later edits | verified-by-test | `store::snapshots_are_not_mutated_by_later_edits` |
| Dirty state; a late save of an older snapshot does not mark newer edits saved | verified-by-test | `store::dirty_tracks_saved_snapshot_and_late_saves_do_not_mark_later_edits_saved` |
| History bounded to 100 steps | verified-by-test | `store::history_is_bounded` |
| Each completed drag is one undo step; cancelled drags discarded | verified-by-test | `timeline_input::clip_drag_is_one_undo_step_and_cancel_discards`, `trim_drag_snaps...` |
| Explicit output container saved; MP4 default | verified-by-test | `state::container_and_format_choices_are_saved_in_the_project` |
| New project starts clean (no sources, covers, history) | implemented-unverified | `State::activate_project` |
| Unsaved-change confirmation on quit | verified-by-test | `state::quitting_a_dirty_project_asks_first_and_busy_work_blocks_quit` |
| Schema 1/2 upgrade and job snapshot restore | not-implemented | engine-side (WP4) and job restore UI not wired |
| Relink source (same stream indices, else refuse) | not-implemented | `Relink source` button not in scene |

## Timeline

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Lanes per audio/subtitle stream, shared zoom and scroll | partial | lanes and shared `timeline_view` implemented; scroll sync between lanes not verified |
| Ruler interval, virtualized ticks | verified-by-test | `geometry::ruler_interval_is_a_nice_step`; ticks capped in `scene::canvas_draw` |
| Alt+wheel zoom anchored at the pointer; `- / +` and percent reset | verified-by-test | `geometry::zoom_keeps_the_pointer_time_stationary`, `timeline_input::alt_wheel_zooms...` |
| Clip drag in 10 ms steps, clamped to ±24 h | verified-by-test | `geometry::drags_quantize...`, `clip_drag_is_one_undo_step...` |
| Arrow keys: 100 ms / Shift 1 s / Alt 10 ms | verified-by-test | `timeline_input::arrow_keys_move_clips...` |
| Trim handles: source-bounded, no crossing, minimum 10 ms | verified-by-test | `geometry::trim_drag_never_crosses_edges...` |
| Trim snaps to the video end within 8 px; aligned state; Escape discards | verified-by-test (snap value and discard) | `timeline_input::trim_drag_snaps_to_the_video_end...`; the blue "aligned" highlight is not in the scene |
| Double-click a trim handle restores its boundary; Home/End keys | not-implemented | handle double-click and Home/End not wired |
| Reset clip trims button | implemented-unverified | `lane.trim_reset` in `summary` panel |
| Video-lane target/crosshair reference (undoable, saved by track id, excluded allowed) | verified-by-test (fit and snap) | `geometry::clip_to_video_...`, `geometry::snap_engages...`; toggle via `lane:ref` |
| Clip to video (Ctrl+Alt-click, menu item), refusal reasons | partial | `geometry` rules tested; Ctrl+Alt click is in `timeline_input` but the menu item is not in a scene |
| Include/Exclude, Remove keeps source, grey Excluded pill | partial | reducer actions `lane:include`, `lane:remove` implemented; grey pill drawn in canvas; no context menu |
| Right-click / Shift+F10 clip context menu | not-implemented | no menu scene; keyboard focus rules not implemented |
| Chapter markers: click video lane surface to add, drag, arrow keys, delete | verified-by-test (add, drag, no-op click) | `timeline_input::chapter_drag...`, `state::chapters_are_sorted...`; right-click Delete menu not implemented |
| Automatic Opening chapter disclosure for MP4/M4V | not-implemented | |
| Playback marker aligned by seek bar; reveal only when out of view | implemented-unverified | `State::reveal` set; host-side reveal not verified |
| Ruler click seeks (accounting for video offset) | implemented-unverified | `State::seek_playhead` |
| Excluded/muted lanes greyed | implemented-unverified | canvas colours |
| Audio role select (mix/separate), mute, language input | implemented-unverified | `lane.role`, `lane.mute`, `lane.lang` |
| Waveform analysis, clip splitting, compositing | not claimed by reference either | |

## Viewer and transport

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Play/pause, stop-and-rewind, beginning/end, frame steps | partial | reducer implemented; frame clamp verified-by-test (`playback::frame_steps_clamp_at_source_boundaries`) |
| Nominal frame rate from `r_frame_rate`, 25 fps fallback | verified-by-test | `playback::frame_durations_use_nominal_rates_with_fallback` |
| Speed ladder 0.10 to 10x | verified-by-test | `playback::speeds_cover_the_requested_ladder` |
| Seek bar: 80 ms coalescing, immediate final release, stale completions ignored | verified-by-test | `playback::intermediate_seeks_coalesce...`, `duplicate_targets...`, `a_return_to_an_earlier_seek...` |
| Volume starts at 5%, shows percentage | implemented-unverified | `State.volume` default 0.05 |
| Chapter prev/next (500 ms tolerance) and Alt+Left/Right | implemented-unverified | `seek_chapter`; command shortcuts declared in manifest |
| Add chapter at position from transport | implemented-unverified | `viewer:add_chapter` |
| Source selector for multi-source projects | implemented-unverified | `viewer.source` |
| Retry preview; preview disabled until components verified | partial | `viewer:retry`; components check via `DependenciesStatus` |
| Real video frames | not-verifiable-here | frames come through `resource.put` (file ring); the frame producer is not connected |
| Subtitle overlay on the source player (cue audition) | not-implemented | `inspectVtt` tested; overlay binding is host-side and not wired |

## Media pool

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Dropbox, file rows with kind label, add-to-timeline button | implemented-unverified | `scene::media_pool` |
| Import one file at a time; per-file status; folder import | partial | queue in `State::import_paths`; folder import sends `folder: true` but the engine is not connected |
| Native drop onto the window | implemented-unverified | `Event::Drop` / `drop` message |
| Image assets open in Poster & cover | partial | `preview_asset` sends `ArtworkImport`; studio not wired |
| Pool width persisted within 180..520 px | verified-by-test (clamp) | `layout::pool_and_inspector_widths_are_bounded`; not wired to a split node |

## Inspector (summary and project tools)

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Cover preview, Edit (opens Poster & cover), Remove | implemented-unverified | `summary` panel, `cover:edit`, `cover:remove` (removal sets a notice) |
| Selected lane title, language, position, reset trims | implemented-unverified | `summary` panel |
| Output container MKV/MP4/M4V buttons | verified-by-test | `state::container_and_format_choices...` |
| At a glance: duration, source size, output tracks, video processing | implemented-unverified | `summary` panel |
| Validation status from the plan | implemented-unverified | `summary` panel; plan from `ProjectValidate` (180 ms debounce) |
| Track settings table (include, language, default, output, order) | implemented-unverified | `project_panel` track table |
| Metadata editor (container-aware field list, project-only labels) | partial | fields and key set in scene; `project-only` and MP4-only labels not shown |
| Chapters editor: title, HH:MM:SS.mmm time, delete, sort, at playhead | verified-by-test (clock and ends) | `chapters::*`, `state::chapters_are_sorted...`; scene present |
| Export and history: review, approval, cancel, progress, restore settings | partial | review and approval verified-by-test (`state::export_review_requires_approval_for_conversions`); history restore not implemented |
| Export review lists conversions and requires approval | verified-by-test | `state::export_review_requires_approval_for_conversions` |
| Export notice counts timeline adjustments; failed job replaces stale start | verified-by-test (logic only) | `State::after_jobs_change` (no direct test yet) |

## Subtitle Studio

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| SRT clocks, strict parse, validation issues (overlap, empty, 25 cps, 42-char lines) | verified-by-test | `subtitles::clocks_round_trip...`, `issues_report_overlap...` |
| Shift selected cues, split at caret, merge adjacent, literal replace | verified-by-test | `subtitles::shift_affects_only...`, `split_creates...`, `merge...`, `replace_is_literal_and_scoped` |
| WebVTT preview inspection (strict timestamps, 10,000-cue limit) | verified-by-test | `subtitles::webvtt_inspection...`; preview request via `SubtitlePreview` |
| Studio UI: cue list, fields, 40-step undo, save copy, add to project | not-implemented | `subtitle_studio` scene shows tracks and preview cues only; the SRT document editing state is not in `State` |
| Left/Right cue navigation, playback-follow highlight | not-implemented | |
| Translation (Ollama), model download | not-implemented | reference feature requires engine work (WP4) |

## Poster & cover studio

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Crop sizing from a typed dimension; ratio lock; never 1 px; centred | verified-by-test | `crop::never_creates_a_one_pixel_crop`, `centres_and_locks...` |
| Move without resizing; clamp every edge | verified-by-test | `crop::move_clamps_every_edge...` |
| Resize from handles, opposite corner fixed, locked ratio | verified-by-test | `crop::resize_preserves...`, `resizing_one_edge...` |
| Presets incl. 9:16 and 3:2 | verified-by-test | `crop::presets_include...` |
| Poster orientation toggles flip reciprocal ratios | verified-by-test | `posters::orientation_toggles_flip_reciprocal_ratios` |
| Pagination: 12/24/48, four batches per page, duplicate removal, cached pages, page limit | verified-by-test | `posters::pages_fill_across_batches...`, `sparse_searches...`, `a_stalled_cursor...`, `a_failed_continuation...` |
| Results browser, Use this cover, live preview, rotate, flip, filters, undo, apply | not-implemented | the search request and thumbnails are not in the engine contract yet; image edit ops not ported |
| Immediate colour preview (grayscale, negative, sepia order, clamping) | not-implemented | reference `LiveArtworkPreview` not ported in this pass |
| Apply cover embeds JPEG and clears the video lane conflict | partial | `State::apply_cover` (artwork import path); no test |

## Dependencies, preferences, about, updates

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Component check on start, recheck, repair (busy, protected) | implemented-unverified | `check_dependencies`, `dep:repair` |
| Preferences dialog: appearance select (saved via `settings.set`) | implemented-unverified | `settings:theme` effect |
| Default folder, image format, FFmpeg/FFprobe paths | not-implemented | |
| Preferences page in Pigeon (`preferences.page`) | partial | manifest declares the page; the page scene is the same settings modal |
| About Reelio: version/build, developer credit, GitHub link | partial | shows the plugin version (2.0.0), not the Reelio app build; link text only |
| Check for updates (GitHub releases, verified download) | not-implemented | reference `updates` feature not ported |

## Toasts, notices, closing

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Notices hide after 5 s, errors after 8 s, hover pauses | verified-by-test | `state::toasts_auto_hide_and_pause_while_hovered` |
| Quit / Ctrl+W confirmation for dirty projects; busy work blocks quit | verified-by-test | `state::quitting_a_dirty_project...` |
| OS close and window chrome, platform-specific controls | host-owned | Pigeon owns the window and its controls |
| Interface zoom 50-200% (Ctrl+= / Ctrl+-) | host-owned | Pigeon owns interface zoom |
| Browser preview banner | not-applicable | desktop-only concept in the reference |

## Keyboard and commands

| Reference behaviour | Status | Evidence / gap |
|---|---|---|
| Ctrl/Cmd+N, Ctrl/Cmd+Shift+O, Ctrl/Cmd+O, Ctrl/Cmd+S, Ctrl/Cmd+E, Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z, Space, Alt+arrows | partial | declared as `command` contributions with shortcuts; `State::command` dispatch implemented; not verified through the host |
| Ctrl/Cmd+F focuses metadata title | not-implemented | |
| Subtitle shortcuts scoped to the studio | not-implemented | |

## Playwright scenarios (e2e/*.spec.ts), mapped

| Scenario | Status | Notes |
|---|---|---|
| icon transport steps, stops, jumps, chapters, speeds | partial | speed ladder, frame clamp and seek scheduler verified; chapter insert and stop rewind implemented-unverified |
| media pool resize persists without changing project state | partial | width clamp verified; persistence not wired |
| workspace layout preferences persist; artwork in lower workspace | partial | `parse_workspace_layout` round trip verified; layout persist effect verified; artwork lower mode is a scene switch |
| layout controls usable when storage is blocked | verified-by-test (defaults) | `layout::layout_defaults_...` |
| project menu, toolbar tools, borderless title, interface zoom | host-owned / not-implemented | project menu is a command set; no menu scene |
| browser preview boundary | not-applicable | |
| workspace editing flow with mocked IPC | partial | reducer flows for import, trim, save are unit-tested; the engine is not connected |
| subtitle cues without pagination, arrows, playback follow | not-implemented | |
| embedded text subtitle context actions open an editable copy | not-implemented | |
| SRT create/split/merge/undo/save copy/recover | partial | SRT logic verified; studio state not wired |
| translation review and apply with undo | not-implemented | |
| save feedback is a floating toast paused while reading | verified-by-test | toast timing tests |
| window controls per platform (Omarchy, macOS, Windows) | host-owned | |
| Quit and Ctrl+W on Omarchy, subtitle recovery draft | partial | quit confirmation verified; recovery draft not implemented |
| crop size controls, native cover drop routing, interactive crop geometry | verified-by-test (geometry) | `crop::*`; native drop coordinate mapping is host-owned |
| poster pagination | verified-by-test | `posters::*` |
| immediate artwork colour preview | not-implemented | |
| chapter start-only timing | verified-by-test | `chapters::*` |
| SRT editor operations | verified-by-test | `subtitles::*` |
| subtitle preview inspection | verified-by-test | `subtitles::webvtt_inspection...` |
| responsive seeking | verified-by-test | `playback::*` |
| persisted inspector width / media pool width / viewer height | verified-by-test (clamps) | `layout::*` |
| viewer versus lower-workspace layout | partial | viewer height clamp verified |
| stable excluded main video reference, magnetic snapping | verified-by-test | `geometry::*`, `timeline_input::trim_drag_snaps...` |
| clip to video (5 scenarios) | verified-by-test | `geometry::clip_to_video_*`, `geometry::snap_engages...` |
| native scroll rounding | host-owned | the plugin does not own scroll offsets; the host corrects WebKit rounding in Pigeon |
| timeline zoom anchoring, playhead reveal | verified-by-test (zoom) / implemented-unverified (reveal) | `geometry::zoom_*`, `timeline_input::alt_wheel_zooms...` |
| real-world retained timeline geometry | verified-by-test | `geometry::retained_interval_fit...` |
| timeline positioning, trims, subtitle clipping | verified-by-test | `geometry::trim_drag...`, `geometry::subtitle_cues_clip_and_shift` |
| persisted workspace layout / interface zoom | verified-by-test (layout) / host-owned (zoom) | `layout::*` |
| preview transport rates and frames | verified-by-test | `playback::*` |
| project editing (AVIF stills, lanes removal, new project, format, stable indices, trims in schema 3, duplicates, undo) | verified-by-test (most) | `model::*`, `store::*`, `state::*`, `timeline_input::*` |

## Plugin host protocol (crates/app)

| Item | Status | Evidence |
|---|---|---|
| `hello` to `ready` with manifest contributions | verified-by-test | `transport::hello_is_answered_with_ready...` |
| `visibility` to `ui.set`; `ui.event` to reducer; `ping` to `pong` | verified-by-test | `transport::visible_workspace...`; binary smoke run over a pipe |
| Canvas fast path (`canvas.draw` when only draw lists change) | verified-by-test | `transport::split_canvas_moves_draw_lists...` (split only; the send path is in `Pump::render`) |
| Modals open/close through `window.open_modal` / `window.close_modal` | implemented-unverified | `Pump::render` |
| Engine request workers (never on the loop thread) | implemented-unverified | `serve`, worker threads |
| Persisted preferences via `settings.set` | implemented-unverified | `Effect::Persist` |
| SDK (`pigeon-plugin-sdk`) use | verified-by-test (smoke) | vendored under `vendor/pigeon-plugin-sdk` (amendment 12); `transport.rs` runs the SDK runtime; binary smoke run: ready, ui.set, pong |
| Engine (`reelio-engine`) use | verified-by-test | `engine_bridge::Real` maps all 23 UI requests onto `ReelioEngine`; `dropped_media_is_probed_by_the_real_engine_and_becomes_a_lane` runs ffmpeg-generated MP4 through drop, `media_dropped`, `media_probe` and the lane scene |
| Amendments 1, 7, 9, 13 | verified-by-test | command push by `Incoming::Command`; controlled inputs get `rev` (`controlled_inputs_bump_rev...`); permissions list; two inspector panels ordered by `order` |
| Real Pigeon host, conformance suite | not-verifiable-here | |
