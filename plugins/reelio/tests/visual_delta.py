#!/usr/bin/env python3
"""Measure fixed-reference RGB deltas for WP2-rendered 1910x1046 Reelio snapshots."""
import argparse
import subprocess

WIDTH, HEIGHT = 1910, 1046
REGIONS = {
    "toolbar": (0, 0, 1910, 36),
    "media pool": (0, 69, 374, 552),
    "viewer": (374, 69, 1531, 552),
    "video picture": (668, 112, 1238, 434),
    "timeline": (0, 552, 1531, 1019),
    "inspector": (1531, 69, 1910, 1019),
}


def pixels(path):
    result = subprocess.run(
        ["ffmpeg", "-nostdin", "-v", "error", "-i", path, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"],
        check=True, stdout=subprocess.PIPE, timeout=15,
    ).stdout
    if len(result) != WIDTH * HEIGHT * 3:
        raise ValueError(f"{path}: expected a {WIDTH}x{HEIGHT} RGB image")
    return result


def region_mae(reference, capture, bounds):
    x, y, right, bottom = bounds
    error = 0
    for row in range(y, bottom):
        start, end = (row * WIDTH + x) * 3, (row * WIDTH + right) * 3
        error += sum(abs(a - b) for a, b in zip(reference[start:end], capture[start:end]))
    return error / ((right - x) * (bottom - y) * 3)


def preview_bounds(img):
    # The same saturated green/yellow scene is used as fixture content in both sessions.
    left, top, right, bottom = WIDTH, HEIGHT, 0, 0
    for y in range(44, 460):
        for x in range(390, 1515):
            pos = (y * WIDTH + x) * 3
            r, g, b = img[pos:pos + 3]
            if g > 72 and g > b + 32 and g > r - 8:
                left, top, right, bottom = min(x, left), min(y, top), max(x + 1, right), max(y + 1, bottom)
    return left, top, right, bottom


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    for arg in ("reference", "before", "after"):
        ap.add_argument(arg)
    args = ap.parse_args()
    ref, before, after = map(pixels, (args.reference, args.before, args.after))
    for key, bounds in REGIONS.items():
        print(f"{key:14}: {region_mae(ref, before, bounds):6.2f} -> {region_mae(ref, after, bounds):6.2f} (0–255 RGB MAE)")
    for key, image in (("reference", ref), ("before", before), ("after", after)):
        print(f"{key:14} preview mask bounds: {preview_bounds(image)}")


if __name__ == "__main__":
    main()
