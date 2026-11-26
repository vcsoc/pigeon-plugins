# Conformance cases

From `sdk/rust`, use `cargo run -p pigeon-mock-host --bin pigeon-mock -- conformance <plugin-dir>`.

* `core/` probes any protocol-v2 plugin; optional capabilities may be skipped. The scene probe opens
  declared scene-bearing slots and sends a non-mutating list-window or hover event when possible.
  It does not synthesize clicks that might execute a real plugin action.
* `cases/` checks `../../plugins/hello-ui`: slots, behavioural events, services, providers, resources,
  stalls and crashes, and a golden. This is the default for `hello-ui`; add `--core` for the generic
  probes. Add `--cases DIR` to run your own scripted interactions.
* `--only ID` selects one case and `--json FILE` writes the result. Skipped steps are explicit and do
  not count as failures. The real host runs its own frame-budget check; the mock cannot measure it.

Each `*.json` case is `{"id", "title", "steps": [...]}`; each step is one key with its arguments.
A case starts a fresh plugin process (`run --setup`, `run --plugin`). A failing step stops the case.
The mock checks observed protocol violations and clean shutdown even if not explicitly asserted.

| Step | Arguments | Meaning |
|---|---|---|
| `ready` | `min_contributions` | `ready` arrives within 10 s, matches the manifest identity, has unique contribution ids and at least N contributions |
| `probe_slots` | | mock core: infer scene-bearing contributions and exercise visibility, rendering and safe UI events |
| `probe_resources` | | mock core: verify a resource file was decoded, or skip when none is published |
| `slots` | array of slot ids | each slot has a contribution |
| `show` | instance | `visibility` true for the instance |
| `wait_node` | `instance`, `node` | a node appears in the instance's scene |
| `expect_node` | `instance`, `node`, `props` | waits for the node to have these props (subset match) |
| `event` | `instance`, `node`, `type`, ... | a `ui.event` (fields as in the protocol) |
| `send` | a host message | any host-to-plugin message (commands, drops, selection) |
| `ping` | `within_ms` | a pong arrives |
| `raw` | string | a raw line is sent (robustness) |
| `limits` | `timeout_ms` | mock core: an oversized ping is not answered, and a valid ping is still answered |
| `stall` | `ms` | SIGSTOP for N ms: no pong while stopped, pong after resume |
| `crash` | | the process is killed (exit observed) |
| `expect_toast` | `text_contains` | a `notify.toast` contains the text |
| `expect_progress_end` | | a progress job ended |
| `expect_canvas` | `instance`, `node`, `min_ops`, `contains` | a canvas (fast path or in the scene) has ops, optionally matching a subset |
| `expect_list_rows` | `instance`, `node`, `min` | list rows were sent |
| `expect_resource` | `rid`, `kind`, `min_seq` (optional) | a decoded `resource.put` for the rid, optionally at or beyond the given sequence |
| `provider` | `method`, `params`, `expect` | host request; `expect` checks the `result` |
| `expect_no_violations` | | no schema violation in plugin output |
| `golden` | `instance`, `file`, `size`, `max_fraction` | render the scene and compare with `conformance/goldens/<file>` (written on first run; `--update-goldens` rewrites) |
| `frame_budget` | | host-only check, reported as skipped by the mock |

Timeouts: `timeout_ms` on any wait step (default 5 s).
