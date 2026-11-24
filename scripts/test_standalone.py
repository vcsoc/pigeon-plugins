#!/usr/bin/env python3
"""Prove each editor plugin runs on its own, the way Pigeon will drive it.

For every plugin (or the ones named on the command line) this copies the plugin folder to a clean
temporary directory and, with a cleared environment (PATH=/usr/bin:/bin only) and an unrelated
working directory, runs:  --setup, --health, the host's `health` probe, --self-test, a real request
on --image, and a deliberately bad request (which must fail cleanly with a JSON error).

  python3 scripts/test_standalone.py [--image photo.jpg] [--seed DIR] [plugin-id ...]

--seed DIR points PIGEON_PLUGIN_SEED at a folder of already-downloaded model files (offline runs).
Needs network (or --seed) for pip and the model downloads. Slow the first time, quick afterwards.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REQUESTS = {
    "ai-enlarge": lambda img, out: {"operation": "enlarge", "scale": 3},
    "ai-background-removal": lambda img, out: {"operation": "remove_background"},
    "ai-object-removal": lambda img, out: {"operation": "remove_object", "mask": str(out / "mask.png")},
    "ai-image-expand": lambda img, out: {"operation": "expand", "expand": {"top": 128, "bottom": 128}},
    "ai-image-expand-diffusion": lambda img, out: {"operation": "expand", "expand": {"top": 128, "bottom": 128},
                                                   "steps": 6, "guidance": 1, "time_budget": 0, "seed": 1},
}


def run(args, plugin, stdin=None, env_extra=None, timeout=3600):
    env = {"PATH": "/usr/bin:/bin"}
    env.update(env_extra or {})
    started = time.time()
    p = subprocess.run(["sh", str(plugin / "run"), *args], input=stdin, text=True, capture_output=True,
                       env=env, cwd=tempfile.gettempdir(), timeout=timeout)
    try:
        reply = json.loads(p.stdout.strip().splitlines()[-1]) if p.stdout.strip() else {}
    except ValueError:
        reply = {"unparsable_stdout": p.stdout[-200:]}
    return p.returncode, reply, time.time() - started, p.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("plugins", nargs="*")
    ap.add_argument("--image")
    ap.add_argument("--seed")
    args = ap.parse_args()
    from PIL import Image, ImageDraw

    ids = args.plugins or sorted(REQUESTS)
    results, failed = [], False
    for pid in ids:
        work = Path(tempfile.mkdtemp(prefix=f"standalone-{pid}-"))
        plugin = work / "installed" / pid
        shutil.copytree(ROOT / "plugins" / pid, plugin, ignore=shutil.ignore_patterns("__pycache__", ".data"))
        scratch = work / "scratch"
        scratch.mkdir()
        image = scratch / "input.png"
        if args.image:
            Image.open(args.image).convert("RGB").save(image)
        else:
            Image.effect_mandelbrot((384, 256), (-2.2, -1.5, 0.8, 1.5), 60).convert("RGB").save(image)
        size = Image.open(image).size
        mask = Image.new("L", size, 0)
        ImageDraw.Draw(mask).rectangle((size[0] // 3, size[1] // 3, size[0] // 2, size[1] // 2), fill=255)
        mask.save(scratch / "mask.png")
        env = {"PIGEON_PLUGIN_SEED": args.seed} if args.seed else {}
        steps = []

        def step(name, args_, stdin=None, expect_ok=True):
            code, reply, seconds, err = run(args_, plugin, stdin, env)
            good = (code == 0 and reply.get("ok") is True) if expect_ok else (code != 0 and reply.get("ok") is False and "error" in reply)
            steps.append((name, good, seconds, reply if not good else {k: v for k, v in reply.items() if k in ("output_size", "device", "working_scale", "error")}))
            if not good and err:
                print(f"--- {pid} {name} stderr (tail)\n{err[-800:]}", file=sys.stderr)
            return good, reply

        step("setup", ["--setup"])
        step("health", ["--health", "--verify"])
        step("host health probe", [], json.dumps({"version": 1, "operation": "health"}))
        step("self-test", ["--self-test"])
        output = scratch / "output.png"
        request = {"version": 1, "input": str(image), "output": str(output), **REQUESTS[pid](image, scratch)}
        good, _ = step("real request", [], json.dumps(request))
        if good:
            produced = Image.open(output)
            steps.append(("output is a valid PNG", True, 0, {"size": list(produced.size)}))
        step("bad request fails cleanly", [], json.dumps({"version": 1, "operation": "nonsense"}), expect_ok=False)
        results.append((pid, steps))
        failed |= not all(s[1] for s in steps)
        shutil.rmtree(work, ignore_errors=True)

    for pid, steps in results:
        print(f"\n{pid}")
        for name, good, seconds, info in steps:
            print(f"  {'PASS' if good else 'FAIL'}  {name:<28} {seconds:7.1f}s  {json.dumps(info) if info else ''}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
