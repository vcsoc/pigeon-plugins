# Standalone plugin protocol

An editor plugin is a **self-contained program**. Pigeon does not embed, import or link any plugin
code: it installs the plugin folder, asks the plugin to set itself up, then sends one request per job
and reads one reply. A bug in a plugin is fixed by publishing a new plugin version here; Pigeon is
not rebuilt or released.

```
plugins/<id>/
  plugin.json          manifest (what it does, files, models + SHA-256, allowed hosts, privacy)
  run                  launcher (POSIX sh). Finds Python 3.10-3.13, builds <folder>/.data/venv
  plugin_runtime.py    setup, model download + verification, health, self-test, error handling
  worker.py            the plugin's own operation(s)
  requirements.txt     Python dependencies (requirements-cuda.txt is used when nvidia-smi exists)
  .data/               created by the plugin: venv/, models/, last-error.log   (keep across updates)
```

`run` and `plugin_runtime.py` are identical in every plugin (`shared/`, synced by
`scripts/sync_runtime.py`); they are duplicated on purpose so a plugin never depends on another.
The state directory can be moved with `PIGEON_PLUGIN_DATA`.

## Commands (all reply with one JSON object on stdout; diagnostics go to stderr)

| Command | When Pigeon calls it | Reply |
|---|---|---|
| `sh run --setup` | once, when the user enables the plugin (can take minutes; progress on stderr) | `{"version":1,"ok":true,"ready":true,"models":{...}}` |
| `sh run --health [--verify]` | to show status | `{"ok":true,"ready":true,"dependencies":{...},"models":{...},"device":"cpu"}`; exit 1 + `ready:false` if not usable |
| `sh run --self-test` | diagnostics | runs a tiny synthetic job end to end |
| `sh run --version` | optional | `{"ok":true,"id":...,"plugin_version":...}` |
| `sh run` + JSON on stdin | every job | see below |

`--setup` is idempotent and resumable: it creates the private venv, installs requirements, and
downloads each model from the pinned URL, discarding any file whose size or SHA-256 differs. Only
hosts listed in `plugin.json` `network` (including redirects) are contacted.
Run `run` with `sh` (or after setting the executable bit from `"executable": true`). The launcher
adds `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin` to `PATH` itself, so a cleared environment works.

## Job request

One JSON object on stdin. Paths are absolute files in a private temp folder Pigeon owns:

```json
{"version": 1, "operation": "<op>", "input": "in.png", "output": "out.png", ...}
```

| Operation | Extra fields | Result |
|---|---|---|
| `enlarge` | `scale` 2 or 3 | image enlarged |
| `remove_background` | — | input with transparent background |
| `remove_object` | `mask` (grayscale PNG, nonzero = remove) | object painted out |
| `expand` | `expand: {left,top,right,bottom}` px **or** `scale` 1-4; diffusion plugin also `prompt`, `steps`, `guidance`, `seed`, `time_budget` | canvas enlarged, new border generated |

Common optional field: `max_pixels` (default 40,000,000) - the plugin refuses, with a clear error,
any job whose result would be larger. Pass the host's own limit here. `model` is accepted for
compatibility with protocol v1 hosts (a staged model file, or a folder), but is not needed.
`{"version":1,"operation":"health"}` (no image) is the v1 dependency probe and returns plain `ok`.

## Job reply

```
success  {"version":1,"ok":true, "output_size":[w,h], "seconds":1.2, ...}   exit 0, output PNG written
failure  {"version":1,"ok":false,"error":"short human-readable cause"}     exit 1, no output
```

The reply is a superset of protocol v1 (`version` and `ok` first), so a v1 reader keeps working.
`error` is safe to show to the user. Unexpected crashes say `internal error (...)` and leave a
traceback in `.data/last-error.log`.

## What Pigeon needs to do (differences from the current host)

The current host (`editor_ai_runtime.rs`) runs its **own embedded** Python worker and passes one
staged model. To use these plugins as intended it must:

1. Install the plugin folder from `index.json` (verify each file's SHA-256; set `executable`).
2. Run `sh <folder>/run --setup` on enable; show stderr progress; treat exit 0 as installed.
3. Per job: write the input PNG (and mask) to a temp folder, spawn `sh <folder>/run` with the JSON
   request on stdin, enforce its timeout/cancellation, read `output`, and show `error` on failure.
4. Add the `expand` operation (margins) to its request builder; no model staging or hashing is needed.
5. Do not overwrite `<folder>/.data` when updating a plugin.

Notes: the launcher is POSIX sh (Pigeon's worker runner is Unix-only today). On a GPU machine the
launcher installs `onnxruntime-gpu` (needs CUDA 12 + cuDNN 9 on the machine); `--health` reports the
device actually in use.
