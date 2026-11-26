# Migrated Python plugins (2.0)

The five AI image plugins each own an inspector panel and an image context action.
Their options, first-use confirmation, progress, and before/after review all belong
to the plugin process; **Accept** imports a managed derivative and **Discard** never
changes the original. Object removal applies up to 256 mark/erase rectangles in
source-image coordinates, in drawing order. Canvas expansion previews the new
margins; diffusion expansion also exposes prompt, steps, guidance and time budget.

Semantic Search exposes **Meaning** in the search bar, an indexing panel with Pause /
Resume, a preferences page, and **Find similar** on local assets. The model and SQLite
index run within the plugin's own process tree. The index lives in the plugin data
folder and a provider never returns an asset that is absent from its last library scan.

## Launch and validate

- `sh run --setup` prepares the isolated environment and local model files.
- `sh run --plugin` speaks the v2 UI protocol after setup; the original JSON-on-stdin
  one-shot `sh run` interface is still supported. `sh run --uninstall` is the optional
  cleanup hook before Pigeon deletes plugin files and data.
- `plugin.json` declares `kind`, optional `provides`, `ui` (activation, permissions,
  and static contributions) and `sdk`. Build the catalogue with
  `python3 scripts/build_index.py`; it includes hashes for the SDK files automatically.
- After editing shared code: `python3 scripts/sync_runtime.py` and
  `python3 sdk/sync_sdk.py`. CI runs both with `--check`.
- Run `python3 scripts/test_standalone.py [plugin-id ...]` with Pillow and jsonschema
  installed in the test interpreter. It runs v1 checks plus a schema-validated v2 mock
  host against the actual plugin. `--seed DIR` reuses model files by hash;
  `--prepared DIR` reuses already-prepared models and Python environments while keeping
  each test's plugin data and results isolated. Pass `--pigeon-mock path/to/pigeon-mock`
  to add the SDK's core conformance and live scene renders; `--renders DIR` saves the PNGs.
