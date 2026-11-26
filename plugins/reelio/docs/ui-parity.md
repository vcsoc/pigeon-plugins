# Reelio UI parity (WP5)

Reference: the read-only Reelio desktop checkout, `README.md`, `docs/ui-styling.md`, feature docs,
and the four `apps/desktop/e2e/*.spec.ts` files. This checklist compares the current plugin to the
working desktop preview, not to unimplemented product-spec features. **The plugin is not yet at full
parity**. Mock-host evidence does not imply real Pigeon GUI, native platform, or provider acceptance.

Status meanings: **verified** = the stated behavior has a passing reducer, engine or mock-host test;
**unverified** = implemented but not exercised for the named dependency/platform; **partial** = some
reference behavior is missing or materially different; **missing** = no working UI path;
**host-owned** = outside the plugin under protocol v2.

**44 audited rows:** 23 verified, 14 partial, 4 unverified, 2 missing, 1 host-owned. Rows count
specific acceptance groups, not a weighted percentage of parity.

| Area / behavior | Status | Evidence or remaining gap |
|---|---|---|
| Schema 3, stable IDs and indices, blank MP4 default | verified | `model`, `store`, reducer tests; actual `.reelio` save through the mock host. |
| Duplicate media, default audio roles, attached pictures and still-image classification | verified | Model tests; mock-host import of video, SRT and artwork. |
| Path authorization, per-file import and Add tracks | verified | Mock-host setup, open-file dialog, drop and generated FFmpeg probe. |
| Folder import | unverified | Engine non-recursive, 256-entry chooser path wired; not a WP5 mock scenario. |
| `Open in Reelio` / bulk add from Pigeon asset menu | missing | Static contributions exist; host selection/context is not consumed by these commands. |
| Project open/save/new, undo/redo and dirty guard | partial | Save, edit and undo verified; open/new controls and legacy upgrade wired; replacement prompts and returning to the prior workspace on cancel need more coverage. |
| Job snapshot / legacy upgrade | verified | WP4 engine tests plus WP5 restore review and request tests. Native restore dialog not exercised. |
| Relink missing source | missing | Not exposed in plugin scene. |
| Workspace independent panel toggles and scoped layout settings | verified | Mock checks host persistence, reload fixture and non-dirty project. |
| Left and viewer divider reset/resize; inspector width | partial | Left resize tested over protocol; viewer pointer/keyboard wired. Inspector position/width remains host-owned, and collapse blanks plugin contributions rather than closing host chrome. |
| Shared lane scale, ruler ticks, pointer zoom and capped scroll | verified | Geometry/reducer and responsive width; real-host alignment and scroll rounding not exercised. |
| Clip offset/trim, cancel, one undo step and exact magnetic end | verified | Mock pointer trim/undo; reducer tested quantization, source bounds, snap and Escape. |
| Trim highlight, Home/End and double-click reset | partial | Snap value is tested; full focused-handle/reset/visual behavior still differs. |
| Main video reference and Clip to video | verified | Reducer/geometry tests preserve stable reference, included/excluded behavior and offsets. |
| Clip menu Include/Exclude/Remove and chapter Delete | verified | Reducer menu tests; native focus return and outside-click routing not proven. |
| Chapter insertion, edit, sort, drag and derived ends | verified | Mock-host toolbar insert; reducer clock/drag/duplicate tests; engine warns of MP4/M4V Opening chapter. |
| Preview frame resource and transport steps, jumps, speed ladder | verified | Mock host receives decoded FFmpeg RGBA and tests speed/step/stop/chapters; no native display qualification. |
| Seek coalescing, release and stale reply protection | partial | Scheduler tested; slider submits exact position. Native pointer capture, release delivery and decoder-clock races are not fully proven. |
| Audio preview | partial | ffplay follows media, speed and volume; independently mixed/offset/trimmed audio audition and hardware output were not verified and reference mix parity is incomplete. |
| Source and subtitle audition | verified | Actual preview frames, timed subtitle overlay, source-clock boundary and cue-navigation tests. |
| Media pool/imported image browser and inspector cover | verified | Mock-host media rows, still import, cover resource and drop. |
| Track settings table, dispositions and output controls | partial | Include, default, language, order and selected-lane controls exist; complete raw stream-property and disposition UI not verified. |
| Metadata fields and shortcuts | partial | Editable mapped/custom fields; project-only/container labels, Ctrl/Cmd+F focus and high-contrast selection need completion/host support. |
| Subtitle Studio: open SRT, add/edit/split/merge/shift/replace, undo and save | verified | Mock opens 205 cues, windows and navigates list, edits, undoes and saves a non-overwriting copy. Reducer verifies editing operations. |
| Subtitle recovery and quit | partial | Debounced draft persistence, recovery read and confirm-before-close are wired; normal quit closes the plugin workspace, not the Pigeon application. Host OS-close delivery and full restart recovery not qualified. |
| Embedded text subtitle editable copy | unverified | Authorized zero-source-offset temporary WebVTT converted to an unsaved SRT draft; no mock scenario for embedded codec conversion. |
| OpenSubtitles provider search | partial | Engine-generated focused search opens the system browser. Protocol has no isolated browser, navigation hook, browser download interception or cookie capture. Public HTTPS direct links and local-file fallback work without those. |
| Confirmed downloads and bounded local subtitle import | verified | Policy-checked engine URL, offer ID confirmation/cancel, bounded local UTF-8, validated candidates and explicit add/test. Pending/download routing tested with fixture replies; live provider success not qualified. |
| Ollama model pull/translate, cancellation and review | unverified | Engine-backed local service and shared approval scene, polling/progress, subset and snapshot review; reducer test proves accepted text changes and undo without changing timings. No Ollama service/model was available for a live run. |
| Poster search, orientation, reciprocal ratio and 12/24/48 pagination | verified | Bounded four-batch pages, deduplication/caching and stale-result handling; reducer tests. Live Bing results were not exercised. |
| Poster thumbnails and exact-ratio Apply | partial | Scene references validated JPEG thumbnail resources from three workers. Ratio forwarded and exact crop tested in artwork engine. Successful live thumbnail fetch was not qualified. |
| Cover inspector drop, lane double-click and project undo | verified | Real PNG/JPEG-style cover workflow through mock slot, edited JPEG resource and preset dimensions; multiple/invalid-drop preservation tested in reducer. |
| Crop drag/move/eight handles, presets, locks and image undo | verified | Crop geometry, atomic/cancelled gesture and lane reopening tests; mock confirms preset raster resource. |
| Rotate/flip/resize/tonal preview and preview image save | partial | Engine validates and renders asynchronously; crop drag uses immediate canvas preview, but color preview is not the reference's independent immediate client raster. Engine save formats tested in WP4; native picker not exercised. |
| Cover embedding and export source protection | verified | WP4 generated-media tests and no-overwrite checks; editor only applies through project edits. |
| Dependencies inspection and repair confirmation | unverified | Panel shows capabilities, failures, supported packages and repair phase; unsupported repair is guarded and reducer-tested. No native Linux package manager was invoked. |
| Export-time conversion/render review | verified | Modal reuses shared consent layout, checks plan revision and refuses stale or canceled review. Reducer approval and WP4 engine export/verification tests. |
| Export progress, cancel, durable adjustment history and retry | verified | Current-project history with percentage/phase/details, crop/omission report and guarded restore; reducer tests and WP4 engine durability. End-to-end UI export progress against a real GUI was not run. |
| Floating toast, dismiss and reading pause | verified | Mock host runs past the five-second timeout while hovered; reducer tests independent hover/focus pause and error timeout. Real-host pointer/focus event delivery not qualified. |
| Preferences and About | partial | Engine settings, paths, default folder, image format and check-for-update available; native appearance/OS zoom, complete release download flow and precise desktop build identity are not qualified. |
| Desktop title bar, OS close, host interface zoom | host-owned | Pigeon owns chrome, resize, focus and zoom. No second native title bar is emulated. |
| Behavioural scenario ports | partial | Five actual subprocess cases: transport, subtitle/list, feedback/quit, workspace/trim/cover, layout reload. These cover core flows from the four Playwright suites and selected unit cases, not every assertion in the desktop suites. |
| Protocol, resources and setup | verified | WP2 SDK from `6ef3e3b`, canonical slot IDs, settings/dialog/window services, RGBA/JPEG/list rows, schema-validated mock sessions and clean shutdown. Real host integration is separate. |
| Visual parity | partial | One measured comparison pass below; mock renderer and Tauri do not share fonts, widgets, input, native chrome or rasterization. |

## Verification

From `plugins/reelio`, with `CARGO_TARGET_DIR=/tmp/pigeon-target-reelio-ui`:

- `cargo test -q -p reelio-ui -p reelio-app`: **111 passed** (89 UI unit, 13 UI integration/reducer, 9 app).
- `cargo test -q -p reelio-artwork ratio`: **1 passed**.
- `cargo clippy -p reelio-ui -p reelio-app --all-targets --no-deps`: exit **0**; four lint sites
  reported (one `Result::and_then` that could use `map`, three reverse-iterator opportunities in tests).
- `cargo fmt -p reelio-ui -p reelio-app --check`: exit **0**.
- WP2 mock host: **5 behavioral sessions and 1 populated render session passed**, all with zero
  protocol violations, on the final visual pass (22.36 s). The harness is development-only; it
  uses the sibling `pigeon-plugins-sdk` checkout and a temporary installed artifact layout.

```sh
export CARGO_TARGET_DIR=/tmp/pigeon-target-reelio-ui
cargo build -p reelio-app
export REELIO_TEST_BIN="$CARGO_TARGET_DIR/debug/reelio"
cargo test --manifest-path tests/mock-host/Cargo.toml --test scenarios -- --test-threads=2
export REELIO_REFERENCE_PNG=/path/to/reelio/docs/images/reelio-workspace.png
export REELIO_VISUAL_OUT=/tmp/reelio-workspace.png
cargo test --manifest-path tests/mock-host/Cargo.toml --test scenarios workspace_visual_fixture -- --ignored
```

## Visual comparison (E)

Reference and captures: **1910 × 1046**, RGB channel mean absolute error (0–255), lower is better.
The fixture loads a generated video using the reference's visible still, a synthetic SRT and a cover
cropped from the reference. The WP2 renderer draws actual main and inspector slot snapshots, reading
resource pixels from the mock host. The compositor arranges those slots side by side; it does not
recreate host chrome. Before: `a0eedc3`; after: `acf4a4d`. Artifacts are
`/tmp/pigeon-program/reelio-workspace-{before,after}.png`; `tests/visual_delta.py` measures both.

| Reference region | Before | After |
|---|---:|---:|
| Toolbar | 18.55 | 17.71 |
| Media pool | 14.11 | 7.82 |
| Viewer | 25.63 | 17.62 |
| Video picture | 54.64 | 35.62 |
| Timeline | 22.55 | 7.49 |
| Inspector | 24.50 | 11.69 |

The pass fixes the wide toolbar, letterboxed viewer, fixed left lane headers and full-width lane
canvases, dark panel fills and clip colors. The saturated video-pixel mask `(left, top, right,
bottom)` moved from `(400,357,520,425)` to `(670,83,1241,403)` versus `(668,112,1238,433)` in the
reference: nearly the same size, still 29–30 px higher (floating feedback instead of an old
in-flow banner, plus renderer spacing). Remaining differences include font/glyphs, selected title
field styling, composition of host inspector/chrome, absent screenshot-specific chapter markers
and source size, and native responsive render behavior. These measurements are not a pixel-parity
certificate.
