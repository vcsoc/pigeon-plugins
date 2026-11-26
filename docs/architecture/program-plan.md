# Program plan: Pigeon 0.4 "canvas of placeholders" architecture

Goal: Pigeon becomes a host whose every feature beyond the core library browser is a separately
installed, separately fixable plugin that draws itself into host placeholders (slots). Contract:
`docs/architecture/plugin-ui-protocol.md` (read it fully first). Lead: the main Claude session
(Pigeon 0.4.1 release is the lead's job: only the lead bumps versions, writes release notes, merges).

## Repositories and branches (all under /Users/chris/projects/vcsoc)

| Repo | Role | Rule |
|---|---|---|
| `pigeon-plugins` | plugins, SDK, protocol, catalogue. Base branch for this program: `arch/plugin-ui` (contains this plan + the spec). | create your branch from it: `git worktree add ../pigeon-plugins-<wp> -b arch/<wp> arch/plugin-ui` |
| `pigeon-rust` | the host (egui). Base: `main`. Other sessions keep committing to `main` and may have uncommitted work in the main checkout: **never edit or build in `/Users/chris/projects/vcsoc/pigeon-rust` itself.** | `git -C /Users/chris/projects/vcsoc/pigeon-rust worktree add /Users/chris/projects/vcsoc/pigeon-rust-<wp> -b arch/<wp> main` |
| `reelio` | **read-only reference** for the Reelio plugin (source of truth for parity). Never modify it. | read only |

Hard rules for every agent:
1. Work only in your own worktree(s) and branch(es) named `arch/<wp>`. Commit early and often with
   clear messages. **Do not merge to main, do not push, do not bump Pigeon's version, do not edit
   other work packages' files** (ownership below). If you need a change in someone else's area or
   in the contract, write it in `/tmp/pigeon-program/questions/<wp>.md` (what, why, your proposed
   wording), continue with your best assumption, and mention it in your report.
2. Use a private cargo target dir: `export CARGO_TARGET_DIR=/tmp/pigeon-target-<wp>`. Do not run
   `cargo` with another package's target dir.
3. **Status file**: append a dated line to `/tmp/pigeon-program/status/<wp>.md` at every milestone
   (started, each deliverable done, blocked, tests green, finished). The lead reads these.
4. Commits authored as `vcsoc <vcsoc@outlook.com>` via `git -c user.name=vcsoc -c user.email=vcsoc@outlook.com commit`.
5. "Done" means: every deliverable of your WP implemented, tests written and **actually run**
   (paste real command output summaries in your final report), no stubs or TODO-shaped gaps
   presented as complete, and an explicit list of anything not done or not verifiable here.
   Never claim a result you did not run. Do not weaken or delete existing tests to make them pass
   (tests that assert removed built-in features are removed together with the feature and listed).
6. Quality bar: no `unwrap()` on external input, bounded resources, no blocking on the host UI
   thread, errors surfaced to the user, security as in the existing Pigeon/Reelio rules
   (never shell strings, validate untrusted input, never overwrite user files).
7. Existing code conventions: read `CLAUDE.md`/`AGENTS.md` in each repo you touch.

## Work packages

### WP1 `host` : Pigeon host runtime (pigeon-rust, branch `arch/host`)
Build the slot/scene host described by the protocol. Owns: new `plugin_host` modules (suggested:
`src-tauri/src/plugin_host/` for supervisor, transport, scene model/validation, services,
providers; `src/app/plugin_ui/` for egui rendering, canvas, resources/textures, slot integration),
`src-tauri/src/plugin_store.rs`, `src/app/plugins.rs`, and the host's top-level layout wiring
(`src/app.rs`, `src/app/browser.rs` panel code only where slots plug in).
Deliver: (a) process supervisor with the exact lifecycle/watchdog/crash/backoff rules; (b) reader/
writer threads and bounded queues; (c) scene store + diff by node id, all node kinds and the canvas
ops of section 5-7 rendered with egui, incl. virtualized lists, controlled inputs, menus, split/tabs,
resources/textures (video frame replace-in-place), cursor, focus, accessibility labels;
(d) every slot of section 4 wired into the real layout (sidebar section/entry, toolbar, workspace
view, inspector panel, preferences page, modal, status bar, asset actions/context menu, commands,
shortcuts, drop targets); (e) all host services of section 10 incl. progress jobs in the footer;
(f) provider glue for `library.source` (items appear in nav tree + grid with async placeholders and
thumbnail cache) and `search.provider`; (g) enable/disable/uninstall UX with `run --uninstall`;
(h) manifest `ui` parsing, static contributions shown before the process starts; (i) `pigeon
--conformance` and `--render-scene` test modes; (j) the non-blocking proof tests (stalled/crashed/
flooding plugin keeps >= 20 fps and input working). It must work with **zero** plugins installed
and must not contain plugin-specific code. Do NOT delete Immich/AI-editing code yet (WP3/WP6 do
that after your runtime lands) but expose everything they need. Milestone M1 (early): supervisor +
transport + a working `workspace.view` + `sidebar.entry` rendering text/button/row/column from a
fixture plugin; report it in the status file as soon as it works so others can integrate.

### WP2 `sdk` : SDK, schemas, mock host, conformance (pigeon-plugins, branch `arch/sdk`)
Owns `sdk/**`, `schema/ui-v2/**`, `conformance/**`, `docs/architecture/sdk.md`, and
`plugins/hello-ui/` (reference plugin).
Deliver: JSON Schemas for every message, scene node and draw op; **Rust crate** `pigeon-plugin-sdk`
(typed messages with serde, a dedicated protocol I/O thread honouring section 9, scene builder
helpers, canvas draw-list builder, resource writer with the file ring, service client with futures-
free blocking + callback APIs, event dispatch, ping/pong, lifecycle) and **Python module**
`pigeon_plugin` with the same capabilities; a **mock host** binary/library (headless; validates all
messages against the schemas, drives events, records scenes, simulates stalls/crashes, renders
scenes + canvases to PNG with a software renderer for golden tests, themeable); the conformance
suite; the icon list; the `hello-ui` plugin exercising every slot and node kind; docs. Milestone
M1 (as fast as possible, within the first hour of work, other WPs depend on it): the Rust crate's
message/scene/canvas types + Python equivalents + schemas committed and noted in the status file.
Keep the Rust crate dependency-light (serde, serde_json, std threads; no async runtime).

### WP3 `immich` : Immich as an isolated plugin (pigeon-plugins `plugins/immich/` + pigeon-rust removal; branches `arch/immich` in both)
Extract ALL Immich functionality from Pigeon (`immich*.rs`, `docs/IMMICH.md`, UI in
browser/inspector/dialogs/preferences/footer/startup_picker/similar/metadata sync/transfer/drag-out,
jobs/commands, model fields) into `plugins/immich/`, a plugin with `library.source`, a
`preferences.page`, transfer dialogs (`modal`), status/progress and its own threads/process.
It must have **full behavioural parity** with the current built-in Immich support (read the
current code and `docs/IMMICH.md`; port its tests and keep their intent), run entirely in its own
process with its own worker pool/background sync/thumbnail fetchers/transfer queue, **never block
the host UI** (all network and SQLite in the plugin), keep credentials in `secrets.*`, and be
fully removable: after the host removal, a Pigeon without the plugin has no Immich code, UI, fields
or settings; disabling hides it; uninstalling deletes plugin + cache (`--uninstall`). Native Rust
plugin (reuse the existing code) using the WP2 Rust SDK; artifact/`cargo` setup per the manifest
rules. Phase A (parallel with others): build and test the plugin against the WP2 mock host and a
fake Immich server (the existing loopback tests are a good base). Phase B (after the lead tells you
WP1 landed `library.source` in the host, or you see M-provider in `/tmp/pigeon-program/status/host.md`):
integrate with the real host and **then** remove Immich from the host in your `pigeon-rust` branch,
keeping existing non-Immich tests green.

### WP4 `reelio-engine` : Reelio engine (pigeon-plugins `plugins/reelio/crates/engine`, branch `arch/reelio-engine`)
Port the complete headless Reelio backend: reelio-domain, -ffmpeg, -subtitles, -artwork,
-resources, -storage, -application, -updates (as plugin self-update/dependency checks) and the
Tauri-free logic from `apps/desktop/src-tauri/src` (commands, preview server/loopback streaming,
jobs supervisor, settings, project IO, dependencies check/repair, window-agnostic parts) into a
workspace under `plugins/reelio/`. Provide `reelio-engine`: a Tauri-free API
`Engine` with one method per Tauri command (see `generate_handler!` in the reference lib.rs and
`apps/desktop/src/services/desktop.ts`) using plain serde request/response types, plus an event
stream (job progress, preview ready, dependency repair, notifications) delivered over a channel.
Port **all** Reelio Rust tests and the generated-media integration tests; keep behaviour identical
(every rule in Reelio `AGENTS.md`, `README.md`, `docs/*`). Publish the Engine trait/types early
(milestone M1: crate compiles with the command signatures and serde types; note it in the status
file) because WP5 consumes it; then fill in implementations.

### WP5 `reelio-ui` : Reelio UI (pigeon-plugins `plugins/reelio/crates/ui` + `plugins/reelio/crates/app`, branch `arch/reelio-ui`)
Recreate the **entire** Reelio front end as scenes + canvases on the WP2 SDK, in a `workspace.view`
(plus `modal`s, toasts via `notify.toast`, `preferences.page`, `asset.action` "Open in Reelio" for
video/audio files, `command`s/shortcuts) with **visual, functional and behavioural parity**: read
every file in `reelio/apps/desktop/src`, `reelio/apps/desktop/e2e`, `reelio/docs/ui-styling.md`,
README and the screenshots in `reelio/docs/images` and reproduce: workspace layout (media pool,
viewer + full transport/seek/volume/speed, stacked timelines with canvas lanes, zoom, trim handles,
snapping, context menus, markers, inspector tabs, chapters editor, metadata, track settings,
export & history, subtitle editor/studio and provider search, poster & cover studio with browser/
crop/editor, dependencies/repair, preferences, about/updates, project menu, shortcuts, undo/redo
store, notifications, quit/unsaved flows adapted to the plugin context). The Reelio look (its
Pigeon-derived charcoal/blue palette, spacing, typography) is delivered through the theme override.
Use the WP4 engine crate in-process (binary `plugins/reelio/crates/app` wires SDK + UI + engine).
Port the Vitest store/logic tests and the Playwright e2e scenarios as mock-host behavioural tests;
produce golden PNG comparisons of the main screens against the reference screenshots and report
the measured differences. Until WP4's engine is ready, develop against its published trait with a
fake engine.

### WP6 `migrate` : migrate existing plugins and retire built-in UIs (pigeon-plugins + pigeon-rust, branches `arch/migrate`)
Give each existing plugin its own UI and protocol v2 entry (`run --plugin`): `ai-enlarge`,
`ai-background-removal`, `ai-object-removal`, `ai-image-expand`, `ai-image-expand-diffusion`
(each: an `inspector.panel` + `asset.action`s with options, run, progress, before/after preview
and Accept/Discard writing a derivative via `library.import mode:"derive"`; object-removal's mask
painting on a `canvas`; expand's margin controls; the diffusion plugin's prompt/steps/guidance/time
budget) and `semantic-search` (a `search.provider`, an indexing status panel with pause/resume,
"find similar" `asset.action`, a `preferences.page`; the `--serve` engine stays internal to the
plugin process). Keep each plugin's headless v1 one-shot path working. Update
`scripts/build_index.py`, validators, `sync_runtime.py`/`plugin_runtime.py` for the v2 manifest
(`ui` block, permissions, `kind`/`provides`), the catalogue, docs, tests and the standalone test
harness (including a mock-host run for every plugin); bump each migrated plugin to 2.0.0 with a
tag. Host side (pigeon-rust branch `arch/migrate`, after WP1's slots exist in `arch/host`; rebase on
it): delete the hard-coded Local AI editing UI and capability/operation machinery (`editor_ai*.rs`,
the AI parts of `editor.rs`, `jobs.rs`, plugin capability tables and consent UI), the semantic-
search stub, and their tests; everything they did is now provided by plugins.

## Interfaces between packages
* WP2 -> everyone: SDK crate/module + schemas + mock host (M1 early). Until it lands, use the spec.
* WP1 <-> WP2: the schemas are the single source of truth; WP1 validates messages with them (or a
  generated equivalent) in its conformance mode.
* WP4 -> WP5: `reelio-engine` API (M1 early); WP5 owns `crates/app` and the plugin manifest for reelio.
* WP1 -> WP3, WP6: slots + providers in `arch/host`; they rebase their host-side removals on it.
* File ownership prevents conflicts; shared files (`index.json`, `README.md`, `scripts/build_index.py`
  validators) are edited only by WP6 (build_index/validators) and the lead (index/README).

## Integration and release (lead)
After all WPs report: the lead merges the branches in order (sdk, host, engine, ui, immich,
migrate), runs the full suites in every repo, runs the conformance suite against the real host,
fixes integration gaps, bumps Pigeon to **0.4.1** with release notes, regenerates the catalogue and
publishes. Anything not finished or not verified is called out explicitly, never hidden.
