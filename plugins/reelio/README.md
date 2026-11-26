# Reelio (Pigeon plugin)

Reelio rebuilt as a Pigeon plugin (protocol v2). The editor is a set of scenes and canvases drawn by
the Pigeon host; the media work runs in the Reelio engine (`reelio-engine`, WP4).

* `crates/ui`: pure UI state, reducers with undo/redo, scene and canvas builders (no I/O).
* `crates/app`: the plugin binary (`reelio`): protocol v2 transport, scene pump and the engine bridge.
* `plugin.json`: manifest with `ui` contributions, permissions and the Reelio theme override.
* `run`: launcher (`run --plugin`, `--version`, `--uninstall`); builds `crates/app` when no `bin/reelio`.
* `docs/ui-parity.md`: parity checklist against the reference application.

## Verify

```sh
export CARGO_TARGET_DIR=/tmp/pigeon-target-reelio-ui
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Protocol smoke run (no host needed):

```sh
printf '%s\n' '{"v":2,"t":"hello"}' '{"v":2,"t":"visibility","instance":"main","visible":true}' '{"v":2,"t":"shutdown"}' | "$CARGO_TARGET_DIR/release/reelio" --plugin
```

The engine is not linked yet: `UnconnectedEngine` answers every media command with an explicit error.
