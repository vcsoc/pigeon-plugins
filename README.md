# Pigeon plugins

Approved plugins for [Pigeon](https://github.com/vcsoc/pigeon-rust). Pigeon reads
[`index.json`](index.json) to list what can be installed.

| Plugin | What it does | Model | Download |
|---|---|---|---|
| `ai-enlarge` | 3x image enlargement | ONNX Super Resolution CNN | none (bundled, 235 KB) |
| `ai-background-removal` | Remove a background, preview before saving | U2-Net-P | 4.5 MB |
| `ai-object-removal` | Paint over an object to inpaint it | Simple LaMa fp32 | 198 MB |
| `ai-image-expand` | Enlarge the canvas; the model paints in the new border | MI-GAN 512 | 28 MB |
| `ai-image-expand-diffusion` | Expand the canvas by any amount; Stable Diffusion paints the new border at full resolution (slow) | SD 1.5 Inpainting fp16 | ~2.1 GB |
| `semantic-search` | Private multimodal similarity search (reference engine) | EmbeddingGemma 2 | ~1.5 GB |

## Trust model

- Every bundled file is listed in `index.json` with its size and SHA-256.
- Every downloaded model is pinned by URL (to a commit or release) plus size and SHA-256. A file that
  does not match is discarded.
- Each plugin declares the only hosts it may download from, and its privacy statement.
- Editor workers speak Pigeon's worker protocol v1 (see `editor_ai_runtime.rs` in pigeon-rust): one JSON
  request on stdin, paths inside a private temp folder, one JSON reply. A worker is trusted code that runs
  with your user's permissions, which is why only reviewed plugins are published here.
- Nothing runs, and nothing downloads, until you enable it.

## Adding a plugin

1. Create `plugins/<id>/plugin.json` (see an existing one) and its files.
2. Pin models by hash; Hugging Face models by full commit.
3. `python3 scripts/build_index.py` and commit the regenerated `index.json`. CI fails if it is stale.
4. Open a pull request. Plugins are published only after review.

## Status

`semantic-search` is the engine from the original Pigeon app and is published as a reference
(`"status": "reference"`); the Rust app does not load it yet. The three image-editing plugins use the
worker protocol the Rust app already implements. Background and object removal have not yet been
verified against the real models.
