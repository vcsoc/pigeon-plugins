#!/usr/bin/env python3
"""Prove each editor plugin runs on its own, the way Pigeon will drive it.

For every plugin (or the ones named on the command line) this copies the plugin folder to a clean
temporary directory and, with a cleared environment (PATH=/usr/bin:/bin only) and an unrelated
working directory, runs:  --setup, --health, the host's `health` probe, --self-test, a real request
on --image, a deliberately bad request, and a real v2 UI session driven by a mock host.

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
    "semantic-search": lambda img, out: {"operation": "embed", "text": "a bird flying"},   # no image output
}


def serve_test(plugin, env, scratch):
    """Drive the long-running engine the way Pigeon does: sync, index, search, check the ranking."""
    from PIL import Image, ImageDraw
    files = {}
    red = Image.new("RGB", (256, 256), (240, 240, 240))
    ImageDraw.Draw(red).ellipse((60, 60, 196, 196), fill=(220, 20, 20))
    blue = Image.new("RGB", (256, 256), (240, 240, 240))
    ImageDraw.Draw(blue).rectangle((60, 60, 196, 196), fill=(20, 40, 220))
    for name, image in (("red-circle.png", red), ("blue-square.png", blue)):
        image.save(scratch / name)
        files[name] = ("image", scratch / name)
    (scratch / "recipe.txt").write_text("Preheat the oven. Whisk eggs and flour into a smooth batter for pancakes.")
    files["recipe.txt"] = ("document", scratch / "recipe.txt")
    records, assets = [], {}
    for name, (kind, path) in files.items():
        stat = path.stat()
        fingerprint = f"{path}|{stat.st_size}|{stat.st_mtime_ns}"
        asset = {"id": name, "path": str(path), "kind": kind, "name": name, "tags": [], "note": "",
                 "modified": stat.st_mtime * 1000, "size": stat.st_size}
        records.append({"asset": asset, "fingerprint": fingerprint, "contentFingerprint": fingerprint})
        assets[name] = fingerprint
    full_env = {"PATH": "/usr/bin:/bin", **env}
    child = subprocess.Popen(["sh", str(plugin / "run"), "--serve", str(scratch / "index.db")], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=full_env,
                             cwd=tempfile.gettempdir())
    counter = [0]

    def call(action, **fields):
        counter[0] += 1
        child.stdin.write(json.dumps({"id": counter[0], "action": action, **fields}) + "\n")
        child.stdin.flush()
        while True:
            reply = json.loads(child.stdout.readline())
            if reply.get("id") == counter[0] and "progress" not in reply:
                if "error" in reply:
                    raise RuntimeError(reply["error"])
                return reply["result"]

    try:
        if not json.loads(child.stdout.readline()).get("ready"):
            raise RuntimeError("engine did not report ready")
        call("sync-assets", records=records)
        for _ in range(6):                                  # parts are indexed in rounds
            pending = call("plan")["pending"]
            if not pending:
                break
            call("index_batch", jobs=[{"asset": r["asset"], "fingerprint": r["fingerprint"]} for r in records
                                     if r["asset"]["id"] in pending], workers=1, steps=2)
        indexed = call("info")["indexed"]
        ranking = {}
        for query in ("a red circle", "a blue square", "pancake batter recipe"):
            hits = call("search", query=query, assets=assets, minimum=-1, limit=5)["results"]
            ranking[query] = hits[0]["id"] if hits else None
    finally:
        child.kill()
        child.wait()
    expected = {"a red circle": "red-circle.png", "a blue square": "blue-square.png", "pancake batter recipe": "recipe.txt"}
    return indexed == 3 and ranking == expected, {"indexed": indexed, "top_hit": ranking}


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


def bounded_command(command, cwd, env=None, timeout=420):
    p = subprocess.Popen(command, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True)
    try:
        out, err = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        import signal
        os.killpg(p.pid, signal.SIGKILL)
        p.communicate(timeout=10)
        return False, "", f"timed out after {timeout}s"
    return p.returncode == 0, out, err


def bounded_case(plugin, *args):
    command = [sys.executable, str(Path(__file__).resolve()), *map(str, args)]
    good, out, err = bounded_command(command, plugin)
    return good, json.loads(out) if good else {"error": err[-1600:]}


def sdk_cases(binary, plugin, pid, out_dir):
    """The SDK starts one fresh data directory per case; reuse setup from this copied plugin."""
    launcher = plugin / "run"
    real = plugin / "run.actual"
    launcher.rename(real)
    launcher.write_text('''#!/bin/sh
set -eu
HERE=$(cd "$(dirname "$0")" && pwd -P)
if [ "$#" -gt 0 ] && [ "$1" = --setup ] && [ "${PIGEON_PLUGIN_DATA:-}" != "$HERE/.data" ]; then
  : "${PIGEON_PLUGIN_DATA:?missing mock data dir}"
  for component in venv models; do
    if [ -e "$HERE/.data/$component" ]; then
      ln -s "$HERE/.data/$component" "$PIGEON_PLUGIN_DATA/$component"
    fi
  done
  exec sh "$HERE/run.actual" --health
fi
exec sh "$HERE/run.actual" "$@"
''')
    launcher.chmod(0o755)
    try:
        png = out_dir / f"{pid}.png"
        instance = f"{pid}/" + ("index" if pid == "semantic-search" else "panel")
        for name, args in (("SDK conformance", ("conformance", plugin)),
                           ("SDK render", ("render", plugin, "--instance", instance, "--size", "420x680", "-o", png))):
            started = time.monotonic()
            good, out, err = bounded_command([str(binary), *map(str, args)], plugin, timeout=90)
            if name == "SDK render":
                good = good and png.is_file() and png.stat().st_size > 0
            yield name, good, time.monotonic() - started, {"output": (out + err)[-1600:].strip()}
    finally:
        real.replace(launcher)


def main():
    if len(sys.argv) == 6 and sys.argv[1] == "--internal-case":
        _, _, case, pid, seed, scratch = sys.argv
        if case == "engine":
            good, info = serve_test(Path.cwd(), {"PIGEON_PLUGIN_SEED": seed} if seed else {}, Path(scratch))
            if not good:
                sys.exit(json.dumps(info))
        else:
            from ui_cases import run_case
            info = {"checks": run_case(pid, str(Path.cwd()), seed, scratch)}
        print(json.dumps(info))
        return
    ap = argparse.ArgumentParser()
    ap.add_argument("plugins", nargs="*", metavar="plugin-id")
    ap.add_argument("--image")
    ap.add_argument("--seed")
    ap.add_argument("--pigeon-mock", type=Path, default=shutil.which("pigeon-mock"),
                    help="also run SDK conformance and render using this binary (default: find it on PATH)")
    ap.add_argument("--renders", type=Path, help="retain SDK scene PNGs here")
    ap.add_argument("--prepared", type=Path,
                    help="reuse models and venv from each DIR/<plugin-id>/.data; skip download/setup")
    args = ap.parse_args()
    if unknown := set(args.plugins) - REQUESTS.keys():
        ap.error(f"unknown plugin(s): {', '.join(sorted(unknown))}")
    mock = args.pigeon_mock.resolve() if args.pigeon_mock else None
    if mock and not mock.is_file():
        ap.error(f"missing pigeon-mock binary: {mock}")
    renders = args.renders.resolve() if args.renders else None
    if renders:
        if not mock:
            ap.error("--renders requires pigeon-mock")
        renders.mkdir(parents=True, exist_ok=True)
        renders = Path(tempfile.mkdtemp(prefix="run-", dir=renders))
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

        def step(name, args_, stdin=None, expect_ok=True, timeout=3600):
            code, reply, seconds, err = run(args_, plugin, stdin, env, timeout=timeout)
            good = (code == 0 and reply.get("ok") is True) if expect_ok else (code != 0 and reply.get("ok") is False and "error" in reply)
            steps.append((name, good, seconds, reply if not good else {k: v for k, v in reply.items() if k in ("output_size", "device", "working_scale", "error")}))
            if not good and err:
                print(f"--- {pid} {name} stderr (tail)\n{err[-800:]}", file=sys.stderr)
            return good, reply

        step("v2 launch before setup", ["--plugin"], expect_ok=False, timeout=5)
        if args.prepared:
            data = plugin / ".data"
            data.mkdir()
            source = args.prepared.resolve() / pid / ".data"
            for name in ("venv", "models"):
                if (source / name).exists():
                    (data / name).symlink_to(source / name, target_is_directory=True)
            step("health", ["--health"])
        else:
            step("setup", ["--setup"])
            step("health", ["--health", "--verify"])
        step("host health probe", [], json.dumps({"version": 1, "operation": "health"}))
        step("self-test", ["--self-test"])
        output = scratch / "output.png"
        request = {"version": 1, "input": str(image), "output": str(output), **REQUESTS[pid](image, scratch)}
        good, _ = step("real request", [], json.dumps(request))
        if good and pid != "semantic-search":
            produced = Image.open(output)
            steps.append(("output is a valid PNG", True, 0, {"size": list(produced.size)}))
        if pid == "semantic-search":
            started = time.time()
            try:
                good, info = bounded_case(plugin, "--internal-case", "engine", pid, args.seed or "", scratch)
            except Exception as error:      # report, keep going
                good, info = False, {"error": str(error)[:300]}
            steps.append(("engine: index + search", good, time.time() - started, info))
        step("bad request fails cleanly", [], json.dumps({"version": 1, "operation": "nonsense"}), expect_ok=False)
        started = time.monotonic()
        good, info = bounded_case(plugin, "--internal-case", "ui", pid, args.seed or "", scratch / "ui")
        steps.append(("v2 mock host", good, time.monotonic() - started, info))
        step("uninstall hook", ["--uninstall"])
        if mock:
            steps.extend(sdk_cases(mock, plugin, pid, renders or scratch))
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
