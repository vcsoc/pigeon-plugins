#!/usr/bin/env python3
"""Copy shared launchers and runtime helpers into plugins that use them.

Each plugin folder must be self-contained (Pigeon installs one plugin at a time), so the shared
launcher, runtime and protocol-v2 UI adapter are duplicated, not imported. Edit shared/, run this
script, commit the result. `build_index.py --check` fails if any copy differs.
"""
import json
import shutil
import stat
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHARED = ("run", "plugin_runtime.py", "ui_support.py")


def editor_plugins():
    for manifest in sorted((ROOT / "plugins").glob("*/plugin.json")):
        entry = json.loads(manifest.read_text())
        worker = entry.get("worker", {})
        files = {f["path"] for f in entry.get("files", [])}
        if worker.get("runtime") == "python3" and worker.get("standalone") and "plugin_runtime.py" in files:
            names = SHARED if "ui_support.py" in files else SHARED[:2]
            yield manifest.parent, names


def main():
    check = "--check" in sys.argv
    stale = []
    for folder, names in editor_plugins():
        for name in names:
            source, target = ROOT / "shared" / name, folder / name
            if not target.exists() or target.read_bytes() != source.read_bytes():
                stale.append(str(target.relative_to(ROOT)))
                if not check:
                    shutil.copyfile(source, target)
            if name == "run" and not check:
                target.chmod(target.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    if check and stale:
        sys.exit("out of sync with shared/: " + ", ".join(stale) + " (run scripts/sync_runtime.py)")
    print("synced" if not check else "runtime copies are current", *(stale if not check else []))


if __name__ == "__main__":
    main()
