# Pigeon plugin SDK (protocol v2)

Status: WP2 deliverable. Contract: `plugin-ui-protocol.md` (with amendments 1-13). Schemas are the
normative detail: `schema/ui-v2/`.

## What is here

| Path | What |
|---|---|
| `schema/ui-v2/*.schema.json` | JSON Schemas: scene nodes, draw ops, contributions, plugin-to-host and host-to-plugin messages, host-service methods, manifest `ui` block. `icons.json` is the icon list (`sdk/icons.md`). |
| `schema/ui-v2/examples/` | 45 valid and 18 invalid fixtures (`generate.py` regenerates them with the Python builders). |
| `sdk/rust/pigeon-plugin-sdk/` | Rust crate: typed messages, scene and draw builders, process runtime (protocol threads, app loop, service calls, resource ring). serde and serde_json only. |
| `sdk/python/pigeon_plugin/` | Python package with the same vocabulary. Standard library only. |
| `sdk/rust/pigeon-mock-host/` | Mock host: library and `pigeon-mock` binary (session driver, schema validation, service fakes, software renderer, conformance runner). |
| `sdk/sync_sdk.py` | Vendors the SDK into each plugin that declares `"sdk"` (see Vendoring). |
| `conformance/` | Conformance cases (data) and golden images. |
| `plugins/hello-ui/` | Reference plugin (Python): every slot, every node kind, canvas, list, resources, providers, services. |
| `sdk/rust/examples/minimal-plugin/` | Reference native Rust plugin that vendors the crate. |

## Writing a Python plugin

```python
import pigeon_plugin as p

spec = p.PluginSpec("my-plugin", "1.0.0", [p.Contribution.workspace_view("main", "My plugin", "sparkle")])
plugin = p.Plugin.stdio(spec)

def handle(ctx, msg):                       # msg is the parsed message dict
    if msg["t"] == "visibility" and msg["visible"]:
        ctx.set_scene(msg["instance"], p.column("root").gap(8).child(
            p.button("go", "Click", variant="primary").on("click")))
    elif msg["t"] == "ui.event" and msg["type"] == "click":
        ctx.toast("info", "clicked")

plugin.run(handle)
```

* Scenes: `p.column/row/stack/grid/split/tabs/scroll/panel/...` and one builder per node kind; any
  other prop with `node.set(key, value)`. `ctx.set_scene` validates unique ids and the limits first.
* Canvas: `p.draw.DrawList().add(p.draw.rect(...), p.draw.text(...))`, sent with `ctx.canvas_draw`
  (fast path) or attached with `node.draw(list, frame)`.
* Services: `ctx.call(method, params)` (blocking, 30 s default), `ctx.call_async(method, params, cb)`
  (callback on the app thread), and helpers such as `ctx.toast`, `ctx.progress_start`,
  `ctx.settings_get/set`, `ctx.open_file_dialog`.
* Heavy work: `ctx.spawn(fn)` on a worker thread. Never block the handler for long.
* Resources: `ctx.resources.put_rgba(rid, w, h, pixels)` / `put_encoded(...)`. The file ring waits
  for `resource.ack` before reusing a slot.

## Writing a Rust plugin

```rust
use pigeon_plugin_sdk::{runtime::PluginSpec, Contribution, Incoming, Node, NodeEvent, Plugin};

let plugin = Plugin::stdio(PluginSpec::new("my-plugin", "1.0.0")
    .contribution(Contribution::workspace_view("main", "My plugin", "sparkle")))?;
plugin.run(|ctx, msg| match msg {
    Incoming::Visibility { instance, visible: true } =>
        { let _ = ctx.set_scene(instance, Node::button("go", "Click").on(&["click"])); }
    _ => {}
});
```

Builders: `Node::column(id).gap(8.0).child(...)`, `Node::text`, `Node::button`, `Node::canvas(id, Size::fill(1.0), Size::px(120.0))`, `Node::list`, ...
Canvas: `DrawList::new().clear(c).with(DrawOp::rect(x, y, w, h).fill(c).radius(4.0))`.
Replies: `ctx.answer(id, value)` for provider requests; `ctx.call` for host services.

Threads (protocol section 9): the reader thread answers `ping` itself and resolves replies, the
writer drains control lines (pong, responses) first, and the app loop coalesces pointer moves and
wheel ticks (the count is kept in `coalesced`). Presses, releases, keys and edits are never merged.

## Vendoring (amendment 12)

Only a plugin's folder is installed, so each plugin carries its SDK. In `plugin.json` add
`"sdk": "python"` or `"sdk": "rust"`, then:

    python3 sdk/sync_sdk.py          # copies into the plugin folder
    python3 sdk/sync_sdk.py --check  # CI: fails on missing or stale copies

* Python: `<plugin>/pigeon_plugin/` (the runtime; `schemas.py`, the repository test helper, is not shipped).
* Rust: `<plugin>/vendor/pigeon-plugin-sdk/` (Cargo.toml and src/, self-contained). Depend on it with
  `pigeon-plugin-sdk = { path = "vendor/pigeon-plugin-sdk" }` and make the plugin its own workspace
  (`[workspace]`). `minimal-plugin` builds this way from its own folder.
* Each copy has `SDK_SOURCE.json` with a content hash. Commit the copies.

## Launch rules (amendment 11)

The host runs `run --setup` (idempotent) before the first `run --plugin`. `--plugin` before setup
fails with a JSON error. Python plugins read the UI protocol from `worker.py --plugin` (see
`plugins/hello-ui/worker.py`).

## Mock host

    cd sdk/rust
    cargo run -p pigeon-mock-host --bin pigeon-mock -- run <plugin-dir> [--show <instance>]
    cargo run -p pigeon-mock-host --bin pigeon-mock -- conformance <plugin-dir> [--only ID] [--update-goldens] [--json report.json]
    cargo run -p pigeon-mock-host --bin pigeon-mock -- render-scene scene.json --out scene.png [--size WxH] [--theme dark|light]
    cargo run -p pigeon-mock-host --bin pigeon-mock -- validate-fixtures ../../schema/ui-v2/examples

What it does for a plugin:

* Launches it exactly as Pigeon does (`run --setup`, then `run --plugin`, cwd = plugin folder,
  `PIGEON_PLUGIN_DATA`, `PIGEON_SESSION_DIR`), then sends `hello`.
* Validates every line in both directions against the schemas. The mock refuses to send an invalid
  host message; invalid plugin output is recorded as a violation.
* Answers host services with fakes (`services.rs`): settings, secrets, toasts, progress jobs,
  modals, URLs, clipboard, library and selection fixtures. It enforces the manifest permissions
  (`permission_denied`) and validates params and results against `methods.schema.json`.
* Acknowledges `resource.put` immediately, as the host does after copying the file.
* Simulates crashes (kill) and stalls (SIGSTOP/SIGCONT), and sends provider requests.
* Renders scenes and canvases to PNG (`render/`): rectangles, rounded corners, lines, polygons,
  paths, arcs, ellipses, clip, transforms, linear gradients, shadows (offset), images (nearest),
  text in the bundled Ubuntu-Light and Hack fonts (the egui defaults), and a layout for the node
  kinds. The renderer is deterministic (no anti-aliasing), so golden images are stable. It is a
  test renderer: it checks well-formedness and regressions, not pixel parity with Pigeon.

## Conformance

`conformance/cases/*.json` are data: each case launches a fresh plugin and runs steps in order.
Format: `conformance/README.md`. The same files are the input of `pigeon --conformance` on the real
host; the host must implement the same step interpreter. Steps that only a host can check (the
20 fps frame budget under a 10 s stall) report `skipped`, never `pass`.

Current result against `plugins/hello-ui` (`pigeon-mock conformance plugins/hello-ui`):
19 cases, 0 failed; 1 golden written on first run and compared on the second run; the frame-budget
case is skipped (host-only).

## Limits and what is not done

* Renderer: no anti-aliasing, no text shaping or kerning, no font weights, scroll bars and wrapping
  are approximated. Golden images are advisory (amendment: section 13).
* `pigeon --conformance` (the real-host side) is not in this repository. The case format is ready for it.
* No Python plugin runs the mock's stall test under the real launcher on all platforms; the stall
  and crash steps use `kill`, so they are POSIX-only.
* Patch/focus/scroll/beep messages are validated and recorded by the mock; the mock has no UI to
  apply them.
* Not verified here: the real Pigeon host, Windows.
