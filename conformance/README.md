# Conformance cases

Run against any plugin with the mock host:

    cd sdk/rust && cargo run -p pigeon-mock-host --bin pigeon-mock -- conformance ../../plugins/hello-ui

Each `*.json` case is `{"id", "title", "steps": [...]}`; each step is one key with its arguments.
A case starts a fresh plugin process (`run --setup`, `run --plugin`). A failing step stops the case.

| Step | Arguments | Meaning |
|---|---|---|
| `ready` | `min_contributions` | `ready` arrives within 10 s with at least N contributions |
| `slots` | array of slot ids | each slot has a contribution |
| `show` | instance | `visibility` true for the instance |
| `wait_node` | `instance`, `node` | a node appears in the instance's scene |
| `expect_node` | `instance`, `node`, `props` | waits for the node to have these props (subset match) |
| `event` | `instance`, `node`, `type`, ... | a `ui.event` (fields as in the protocol) |
| `send` | a host message | any host-to-plugin message (commands, drops, selection) |
| `ping` | `within_ms` | a pong arrives |
| `raw` | string | a raw line is sent (robustness) |
| `stall` | `ms` | SIGSTOP for N ms: no pong while stopped, pong after resume |
| `crash` | | the process is killed (exit observed) |
| `expect_toast` | `text_contains` | a `notify.toast` contains the text |
| `expect_progress_end` | | a progress job ended |
| `expect_canvas` | `instance`, `node`, `min_ops`, `contains` | a canvas (fast path or in the scene) has ops, optionally matching a subset |
| `expect_list_rows` | `instance`, `node`, `min` | list rows were sent |
| `expect_resource` | `rid`, `kind` | a `resource.put` for the rid |
| `provider` | `method`, `params`, `expect` | host request; `expect` checks the `result` |
| `expect_no_violations` | | no schema violation in plugin output |
| `golden` | `instance`, `file`, `size`, `max_fraction` | render the scene and compare with `conformance/goldens/<file>` (written on first run; `--update-goldens` rewrites) |
| `frame_budget` | | host-only check, reported as skipped by the mock |

Timeouts: `timeout_ms` on any wait step (default 5 s).
