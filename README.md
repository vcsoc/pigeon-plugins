# Pigeon plugins

Approved plugins for [Pigeon](https://github.com/vcsoc/pigeon-rust). Pigeon reads
[`index.json`](index.json) to list what can be installed.

Since Pigeon 0.4 Pigeon is a canvas of placeholders: every plugin is its own program that contains
its own UI, code, workers and storage, and draws itself into Pigeon's slots (sidebar, toolbar,
workspace, inspector, preferences, dialogs, status bar, asset actions, library sources, search
providers) by sending scenes over a small protocol. A stalled or crashed plugin can never freeze
Pigeon. Contract: [`docs/architecture/plugin-ui-protocol.md`](docs/architecture/plugin-ui-protocol.md);
SDKs (Rust and Python), the mock host, renderer and conformance suite: [`sdk/`](sdk/) and
[`docs/architecture/sdk.md`](docs/architecture/sdk.md).

| Plugin | Version | What it does | Needs |
|---|---|---|---|
| `immich` | 1.0.0 | Immich photo server as a library source: browse, thumbnails, upload/download with verification (isolated, own threads) | an Immich server |
| `reelio` | 2.0.0 | Reelio media container editor: timelines, chapters, subtitles, cover art, FFmpeg export | FFmpeg/FFprobe |
| `ai-enlarge` | 2.0.0 | 3x image enlargement | bundled 235 KB model |
| `ai-background-removal` | 2.0.0 | Remove a background, preview before saving | U2-Net-P, 4.5 MB |
| `ai-object-removal` | 2.0.0 | Paint over an object to inpaint it | LaMa fp32, 198 MB |
| `ai-image-expand` | 2.0.0 | Enlarge the canvas; the model paints in the new border | MI-GAN 512, 28 MB |
| `ai-image-expand-diffusion` | 2.0.0 | Expand the canvas by any amount with Stable Diffusion (slow) | SD 1.5 Inpainting fp16, ~2.1 GB |
| `semantic-search` | 2.0.0 | Private multimodal similarity search, indexing and "find similar" | EmbeddingGemma 2, ~1.5 GB |

Native (Rust) plugins (`immich`, `reelio`) install as three files: `plugin.json`, `run` and a
deterministic `source.tar.gz` (rebuild with `python3 scripts/pack_source.py`); `run --setup`
unpacks and builds it, or installs a verified binary artifact when one is listed.

## Standalone plugins

Every editor plugin is a self-contained program, independent of Pigeon (see
[`schema/worker-protocol.md`](schema/worker-protocol.md)). Pigeon installs the folder, runs
`sh run --setup` once (private Python environment + pinned, verified models), then sends one JSON
request per job (image in, image out, one JSON reply). A plugin can be fixed and republished without
touching Pigeon, and can be tested alone:

```sh
python3 scripts/test_standalone.py            # installs each plugin in a clean folder and runs it
sh plugins/ai-enlarge/run --setup && sh plugins/ai-enlarge/run --self-test
```

## Trust model

- Every bundled file is listed in `index.json` with its size and SHA-256.
- Every downloaded model is pinned by URL (to a commit or release) plus size and SHA-256. A file that
  does not match is discarded.
- Each plugin declares the only hosts it may download from, and its privacy statement.
- Editor workers speak a superset of Pigeon's worker protocol v1: one JSON request on stdin, paths
  inside a private temp folder, one JSON reply (`schema/worker-protocol.md`). A worker is trusted code that runs
  with your user's permissions, which is why only reviewed plugins are published here.
- Nothing runs, and nothing downloads, until you enable it.

## Adding a plugin

1. Copy an existing editor plugin, change `plugin.json` and `worker.py`; run `python3 scripts/sync_runtime.py`.
2. Pin models by hash; Hugging Face models by full commit.
3. `python3 scripts/build_index.py` and commit the regenerated `index.json`. CI fails if it is stale.
4. Open a pull request. Plugins are published only after review.

## Status

All eight plugins use the v2 protocol and are checked with the SDK mock host and conformance suite
(`scripts/test_standalone.py`, `pigeon-mock conformance`). Known limits: the Reelio plugin is a
substantial but **not complete** port of Reelio (see `plugins/reelio/docs/ui-parity.md` for the item-by-item
audit: 23 of 44 verified, 14 partial, 4 unverified, 2 missing); the Immich plugin has been tested only
against a fake Immich server; the NVIDIA path of `ai-image-expand-diffusion` has not been run on an NVIDIA
machine.
