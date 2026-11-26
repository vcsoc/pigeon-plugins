#!/usr/bin/env python3
"""Pack the source tree of native plugins into one deterministic archive.

A native plugin (Rust) installs from the catalogue as three files: plugin.json, run and
source.tar.gz. `run --setup` unpacks the archive into the plugin's data folder and builds it
(or downloads a verified binary artifact). The archive is committed so it is hashed in index.json;
build_index.py --check fails when it is stale. Declared in plugin.json:

  "sourceArchive": {"file": "source.tar.gz", "include": ["Cargo.toml", "Cargo.lock", "src", "vendor"]}

Usage: pack_source.py [--check]
"""
import gzip
import io
import json
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {"target", ".data", "__pycache__", ".git", "node_modules"}


def members(folder, include):
    for entry in include:
        path = folder / entry
        if path.is_file():
            yield path
        elif path.is_dir():
            for p in sorted(path.rglob("*")):
                if p.is_file() and not (set(p.relative_to(folder).parts) & SKIP_DIRS):
                    yield p
        else:
            sys.exit(f"{folder.name}: sourceArchive include '{entry}' does not exist")


def build(folder, include):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for p in sorted(set(members(folder, include)), key=lambda q: q.relative_to(folder).as_posix()):
            info = tarfile.TarInfo(p.relative_to(folder).as_posix())
            data = p.read_bytes()
            info.size, info.mtime, info.uid, info.gid, info.uname, info.gname = len(data), 0, 0, 0, "", ""
            info.mode = 0o755 if p.stat().st_mode & 0o111 else 0o644
            tar.addfile(info, io.BytesIO(data))
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0, compresslevel=9) as gz:
        gz.write(raw.getvalue())
    return out.getvalue()


def main():
    check = "--check" in sys.argv
    stale = []
    for manifest in sorted((ROOT / "plugins").glob("*/plugin.json")):
        spec = json.loads(manifest.read_text()).get("sourceArchive")
        if not spec:
            continue
        folder = manifest.parent
        target = folder / spec["file"]
        data = build(folder, spec["include"])
        if not target.exists() or target.read_bytes() != data:
            stale.append(str(target.relative_to(ROOT)))
            if not check:
                target.write_bytes(data)
                print(f"packed {target.relative_to(ROOT)} ({len(data) / 1e6:.2f} MB)")
    if check and stale:
        sys.exit("source archive out of date: " + ", ".join(stale) + " (run scripts/pack_source.py)")
    if check:
        print("source archives are current")


if __name__ == "__main__":
    main()
