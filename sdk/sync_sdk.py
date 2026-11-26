#!/usr/bin/env python3
"""Vendor the SDK into every plugin that declares one (protocol v2, amendment 12).

Only a plugin's own folder is installed, so each plugin carries its SDK:

* `"sdk": "python"` in plugin.json: copies sdk/python/pigeon_plugin/*.py to <plugin>/pigeon_plugin/
* `"sdk": "rust"` in plugin.json:   copies sdk/rust/pigeon-plugin-sdk/ (Cargo.toml and src/) to
                                    <plugin>/vendor/pigeon-plugin-sdk/

Every copy gets a SDK_SOURCE.json stamp (content hash of the copied files). Use the path dependency
`pigeon-plugin-sdk = { path = "vendor/pigeon-plugin-sdk" }` in a Rust plugin.

  python3 sdk/sync_sdk.py            copy the current SDK into every declaring plugin
  python3 sdk/sync_sdk.py --check    fail (exit 1) if any copy is missing or differs (CI)

Plugins are discovered in plugins/*/plugin.json and sdk/rust/examples/*/plugin.json.
"""
import hashlib
import json
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
STAMP = "SDK_SOURCE.json"


def declared_plugins():
    for pattern in ("plugins/*/plugin.json", "sdk/rust/examples/*/plugin.json"):
        for manifest in sorted(ROOT.glob(pattern)):
            try:
                data = json.loads(manifest.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as exc:
                raise SystemExit(f"cannot read {manifest}: {exc}")
            lang = data.get("sdk")
            if lang in ("python", "rust"):
                yield manifest.parent, lang
            elif lang is not None:
                raise SystemExit(f"{manifest}: unknown sdk {lang!r} (expected python or rust)")


def source_files(lang):
    """(relative path inside the vendored folder, source path) pairs for one language."""
    if lang == "python":
        base = HERE / "python" / "pigeon_plugin"
        return [(f"pigeon_plugin/{p.name}", p) for p in sorted(base.glob("*.py"))]
    base = HERE / "rust" / "pigeon-plugin-sdk"
    files = [("Cargo.toml", base / "Cargo.toml")]
    files += [(f"src/{p.relative_to(base / 'src').as_posix()}", p) for p in sorted((base / "src").rglob("*.rs"))]
    return files


def target_root(plugin, lang):
    return plugin / ("pigeon_plugin" if lang == "python" else "vendor/pigeon-plugin-sdk")


def digest(files):
    h = hashlib.sha256()
    for rel, src in files:
        h.update(rel.encode() + b"\0")
        h.update(src.read_bytes() + b"\0")
    return h.hexdigest()


def expected_stamp(lang):
    files = source_files(lang)
    return {"language": lang, "sha256": digest(files), "files": [rel for rel, _ in files]}


def sync(check):
    problems = []
    count = 0
    for plugin, lang in declared_plugins():
        count += 1
        dest = target_root(plugin, lang)
        want = expected_stamp(lang)
        stamp_path = dest / STAMP
        if check:
            if not stamp_path.exists():
                problems.append(f"{dest.relative_to(ROOT)}: missing vendored SDK (run sdk/sync_sdk.py)")
                continue
            have = json.loads(stamp_path.read_text(encoding="utf-8"))
            if have != want:
                problems.append(f"{dest.relative_to(ROOT)}: vendored SDK is stale (run sdk/sync_sdk.py)")
            for rel in want["files"]:
                if not (dest / rel).exists():
                    problems.append(f"{dest.relative_to(ROOT)}/{rel}: missing")
            continue
        if dest.exists():
            shutil.rmtree(dest)
        for rel, src in source_files(lang):
            out = dest / rel
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(src, out)
        stamp_path.write_text(json.dumps(want, indent=2) + "\n", encoding="utf-8")
        print(f"synced {lang} SDK -> {dest.relative_to(ROOT)}")
    if check:
        if problems:
            print("\n".join(problems), file=sys.stderr)
            return 1
        print(f"SDK copies are current ({count} plugin(s))")
    elif count == 0:
        print("no plugin declares an sdk")
    return 0


if __name__ == "__main__":
    raise SystemExit(sync(check="--check" in sys.argv))
