#!/usr/bin/env python3
"""Validate installable plugin manifests and generate the hash-verified catalogue."""
import hashlib
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
ID = re.compile(r"^[a-z0-9][a-z0-9-]{1,40}$")
HEX = re.compile(r"^[0-9a-f]{64}$")
KINDS = {"app", "source", "search", "ops"}
LEGACY_KINDS = {"editor-worker", "semantic-index"}
PERMISSIONS = {"selection", "library.read", "library.write", "secrets", "dialogs", "clipboard", "shell", "windows", "drag"}
REQUIRED = ("id", "name", "version", "category", "kind", "description", "author", "licence", "worker", "files")
SLOTS = {
    "sidebar.section": ("title", "icon"), "sidebar.entry": ("title", "icon", "opens"),
    "toolbar.item": ("group",), "workspace.view": ("title",), "inspector.panel": ("title", "icon"),
    "preferences.page": ("title", "icon"), "modal": ("title",), "statusbar.item": (),
    "asset.action": ("title", "icon", "applies_to", "selection"), "library.source": ("title", "icon", "capabilities"),
    "search.provider": ("title", "icon", "mode_id"), "drop.target": ("accepts", "where"), "command": ("title",),
}


def fail(plugin, message):
    sys.exit(f"{plugin}: {message}")


def check_ui(name, m):
    worker, ui = m["worker"], m.get("ui")
    if (not isinstance(ui, dict) or ui.get("protocol") != 2 or worker.get("protocol") != 2
            or ui.get("activation", "visible") not in ("startup", "visible", "manual")):
        fail(name, "worker and ui must declare protocol 2 and a valid UI activation")
    if m["kind"] not in KINDS:
        fail(name, "unknown v2 kind")
    provides = m.get("provides", [])
    permissions = ui.get("permissions", [])
    for key, values, allowed in (("provides", provides, KINDS), ("permissions", permissions, PERMISSIONS)):
        if (not isinstance(values, list) or any(not isinstance(v, str) or v not in allowed for v in values)
                or len(set(values)) != len(values)):
            fail(name, f"invalid {key}")
    sdk = {"python3": "python", "native": "rust"}.get(worker.get("runtime"))
    if sdk is None or m.get("sdk") != sdk:
        fail(name, "v2 runtime must declare its vendored SDK")
    contributions = ui.get("contributions")
    if not isinstance(contributions, list):
        fail(name, "ui.contributions must be a list")
    by_id = {}
    for c in contributions:
        if not isinstance(c, dict):
            fail(name, "invalid contribution")
        slot, ident = c.get("slot"), c.get("id")
        if (not isinstance(slot, str) or slot not in SLOTS or not isinstance(ident, str)
                or not re.fullmatch(r"[^/\s]{1,128}", ident) or ident in by_id):
            fail(name, "unknown slot, invalid or duplicate contribution id")
        for key in SLOTS[slot]:
            expected = {"applies_to": dict, "capabilities": list, "accepts": list}.get(key, str)
            if not isinstance(c.get(key), expected):
                fail(name, f"{ident}: missing or invalid '{key}'")
        if slot == "asset.action" and c["selection"] not in ("one", "many", "any"):
            fail(name, f"{ident}: invalid selection cardinality")
        if (slot == "modal" and c.get("size", "medium") not in ("small", "medium", "large", "fullscreen")
                or slot == "statusbar.item" and c.get("align", "right") not in ("left", "right")):
            fail(name, f"{ident}: invalid modal size or status alignment")
        by_id[ident] = slot
    for c in contributions:
        if c["slot"] == "sidebar.entry" and by_id.get(c["opens"]) != "workspace.view":
            fail(name, f"{c['id']}: opens must point to a workspace.view")
    slots = set(by_id.values())
    for kind, slot in (("source", "library.source"), ("search", "search.provider")):
        if kind in (m["kind"], *provides) and slot not in slots:
            fail(name, f"kind '{kind}' needs a {slot} contribution")


def check_standalone(name, folder, m, listed):
    worker = m["worker"]
    if worker.get("entry") != "run" or not worker.get("standalone") or not listed.get("run", {}).get("executable"):
        fail(name, "standalone worker must bundle an executable 'run' entry")
    if "plugin.json" not in listed:
        fail(name, "standalone worker must bundle plugin.json")
    if worker.get("runtime") == "python3" and "plugin_runtime.py" in listed:
        shared = ["run", "plugin_runtime.py"]
        if "ui_support.py" in listed:
            shared.append("ui_support.py")
        for required in ("worker.py", "requirements.txt"):
            if required not in listed:
                fail(name, f"Python worker must bundle '{required}'")
        for filename in shared:
            if (folder / filename).read_bytes() != (ROOT / "shared" / filename).read_bytes():
                fail(name, f"{filename} differs from shared/{filename}: run scripts/sync_runtime.py")


def sdk_files(folder, lang):
    sdk_dir = folder / ("pigeon_plugin" if lang == "python" else "vendor/pigeon-plugin-sdk")
    stamp = sdk_dir / "SDK_SOURCE.json"
    if not stamp.is_file():
        fail(folder.name, "missing SDK vendor stamp: run sdk/sync_sdk.py")
    manifest = json.loads(stamp.read_text())
    digest = hashlib.sha256()
    for name in manifest["files"]:
        path = sdk_dir / name
        if not safe_path(name) or not path.is_file() or sdk_dir.resolve() not in path.resolve().parents:
            fail(folder.name, f"invalid vendored SDK file '{name}': run sdk/sync_sdk.py")
        digest.update(name.encode() + b"\0")
        digest.update(path.read_bytes() + b"\0")
        yield {"path": path.relative_to(folder).as_posix()}
    if manifest["language"] != lang or manifest["sha256"] != digest.hexdigest():
        fail(folder.name, "vendored SDK content does not match its stamp: run sdk/sync_sdk.py")
    yield {"path": stamp.relative_to(folder).as_posix()}


def safe_path(path):
    return isinstance(path, str) and len(path) <= 128 and all(
        part and not part.startswith('.') and re.fullmatch(r"[a-zA-Z0-9._-]+", part) for part in path.split('/'))


def build():
    plugins = []
    for manifest_path in sorted((ROOT / "plugins").glob("*/plugin.json")):
        folder = manifest_path.parent
        m = json.loads(manifest_path.read_text())
        name = folder.name
        # SDK conformance fixtures without a file inventory are not published by the catalogue.
        if m.get("category") == "Reference" and "files" not in m:
            continue
        for key in REQUIRED:
            if key not in m:
                fail(name, f"missing '{key}'")
        if m["id"] != name or not ID.fullmatch(name):
            fail(name, "id must equal the folder name and be lowercase-hyphenated")
        if not re.fullmatch(r"\d+\.\d+\.\d+", m["version"]):
            fail(name, "version must be x.y.z")
        protocol = m["worker"].get("protocol", 1)
        if protocol == 2:
            check_ui(name, m)
        elif protocol != 1 or m["kind"] not in LEGACY_KINDS | {"ops"} or "ui" in m:
            fail(name, "invalid legacy worker protocol or kind")
        if not isinstance(m["files"], list):
            fail(name, "files must be a list")
        listed = {}
        for f in m["files"]:
            path = f.get("path") if isinstance(f, dict) else None
            if not safe_path(path) or path in listed:
                fail(name, f"invalid or duplicate bundled file: {path!r}")
            listed[path] = f
        if protocol == 2 or m["worker"].get("standalone"):
            check_standalone(name, folder, m, listed)
        if protocol == 2:
            for f in sdk_files(folder, m["sdk"]):
                listed.setdefault(f["path"], f)
        files = []
        for path, f in listed.items():
            p = (folder / path).resolve()
            if not safe_path(path) or folder.resolve() not in p.parents or not p.is_file():
                fail(name, f"bundled file '{path}' is missing or outside the plugin")
            content = p.read_bytes()
            files.append({**f, "bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()})
        m.setdefault("operations", [])  # optional legacy one-shot names; opaque to the UI host
        m.setdefault("network", [])
        m.setdefault("models", [])
        m.setdefault("privacy", "")
        for model in m["models"]:
            if not safe_path(model.get("file")):
                fail(name, f"model {model.get('id')} needs a safe relative file path")
            if "url" in model:
                url = urlsplit(model["url"])
                if (url.scheme != "https" or url.hostname not in m["network"]
                        or not HEX.fullmatch(model.get("sha256", "")) or not model.get("bytes", 0) > 0):
                    fail(name, f"model {model['id']} needs a declared HTTPS host, bytes and sha256")
            elif not re.fullmatch(r"[0-9a-f]{40}", model.get("huggingface", {}).get("revision", "")):
                fail(name, f"model {model['id']} needs a pinned source")
        plugins.append({**m, "files": files, "path": f"plugins/{name}", "approved": True})
    return {"schema": 1, "plugins": plugins}


if __name__ == "__main__":
    catalogue = build()
    serialized = json.dumps(catalogue, indent=2) + "\n"
    target = ROOT / "index.json"
    if "--check" in sys.argv:
        if not target.exists() or target.read_text() != serialized:
            sys.exit("index.json is out of date: run scripts/build_index.py")
        print("index.json is current")
    else:
        target.write_text(serialized)
        print(f"wrote index.json with {len(catalogue['plugins'])} plugins")
