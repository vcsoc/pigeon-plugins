#!/usr/bin/env python3
"""Validate every plugins/*/plugin.json and (re)generate index.json.

index.json is what Pigeon downloads. It lists each approved plugin, and for every bundled file its
size and SHA-256, so the app can verify what it fetched. Run with --check in CI to make sure the
committed index is current.
"""
import hashlib, json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ID = re.compile(r"^[a-z0-9][a-z0-9-]{1,40}$")
HEX = re.compile(r"^[0-9a-f]{64}$")
KINDS = {"editor-worker", "semantic-index"}
OPERATIONS = {"enlarge", "expand", "remove_background", "remove_object", "embed"}
REQUIRED = ["id", "name", "version", "category", "kind", "description", "author", "licence", "operations", "worker", "files", "models", "network", "privacy"]


def fail(plugin, message):
    sys.exit(f"{plugin}: {message}")


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build():
    plugins = []
    for manifest_path in sorted((ROOT / "plugins").glob("*/plugin.json")):
        folder = manifest_path.parent
        m = json.loads(manifest_path.read_text())
        name = folder.name
        for key in REQUIRED:
            if key not in m:
                fail(name, f"missing '{key}'")
        if m["id"] != name or not ID.match(m["id"]):
            fail(name, "id must equal the folder name and be lowercase-hyphenated")
        if m["kind"] not in KINDS or not set(m["operations"]) <= OPERATIONS:
            fail(name, "unknown kind or operation")
        if not re.match(r"^\d+\.\d+\.\d+$", m["version"]):
            fail(name, "version must be x.y.z")
        files = []
        for f in m["files"]:
            p = (folder / f["path"]).resolve()
            if folder.resolve() not in p.parents or not p.is_file():
                fail(name, f"bundled file '{f['path']}' is missing or outside the plugin")
            files.append({**f, "bytes": p.stat().st_size, "sha256": sha256(p)})
        for model in m["models"]:
            if "url" in model:
                if not model["url"].startswith("https://") or not HEX.match(model.get("sha256", "")) or not model.get("bytes"):
                    fail(name, f"model {model['id']} needs an https url, bytes and sha256")
            elif "huggingface" in model:
                if not re.match(r"^[0-9a-f]{40}$", model["huggingface"]["revision"]):
                    fail(name, "Hugging Face models must pin a full commit hash")
            else:
                fail(name, f"model {model['id']} has no source")
        plugins.append({**m, "files": files, "path": f"plugins/{name}", "approved": True})
    return {"schema": 1, "plugins": plugins}


if __name__ == "__main__":
    text = json.dumps(build(), indent=2) + "\n"
    target = ROOT / "index.json"
    if "--check" in sys.argv:
        if not target.exists() or target.read_text() != text:
            sys.exit("index.json is out of date: run scripts/build_index.py")
        print("index.json is current")
    else:
        target.write_text(text)
        print(f"wrote index.json with {len(json.loads(text)['plugins'])} plugins")
