#!/usr/bin/env python3
"""AI Image Expand (fast): enlarge the canvas and fill the new border with MI-GAN (28 MB).

Request: {"version": 1, "operation": "expand", "input": png, "output": png,
          "expand": {"left": px, "top": px, "right": px, "bottom": px}  or  "scale": 1.0-4.0,
          "max_pixels": optional output limit (default 40,000,000)}
Original pixels are copied back untouched; only the new border is generated.

Generation is native resolution (never rescaled): the new area is filled outward from the original
edge in bands of up to 256px, using 512px tiles that include the pixels next to the band as context;
overlapping tiles are blended and each band becomes context for the next.
Model: migan_pipeline_v2.onnx (uint8 image/mask, mask 255 = keep), downloaded by `run --setup`.
(A LaMa 512 model passed as request["model"] also works; it is detected from its input types.)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_runtime as rt  # noqa: E402

WORK = 512
MAX_SIDE = 16384
MODEL = 'migan_pipeline_v2.onnx'
STEP = 8
STRIP = 256
SEAM = 8


def margins(request, width, height):
    limit = rt.max_pixels(request)
    if isinstance(request.get('expand'), dict):
        e = request['expand']
        m = [int(e.get(k, 0)) for k in ('left', 'top', 'right', 'bottom')]
    elif 'scale' in request:
        scale = float(request['scale'])
        if not 1.0 < scale <= 4.0:
            raise rt.PluginError('scale must be between 1 and 4')
        dx, dy = round(width * (scale - 1) / 2), round(height * (scale - 1) / 2)
        m = [dx, dy, dx, dy]
    else:
        raise rt.PluginError('expand or scale required')
    if min(m) < 0 or sum(m) == 0:
        raise rt.PluginError('margins must be non-negative and not all zero')
    cw, ch = width + m[0] + m[2], height + m[1] + m[3]
    if max(cw, ch) > MAX_SIDE or cw * ch > limit:
        raise rt.PluginError(f'the expanded image ({cw}x{ch}) would exceed the size limit')
    return m


def expand(request):
    import numpy as np
    import onnxruntime as ort
    from PIL import Image

    rt.require_paths(request, 'input', 'output')
    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA')
    width, height = image.size
    left, top, right, bottom = margins(request, width, height)
    full = (width + left + right, height + top + bottom)

    rgb = image.convert('RGB')
    canvas = np.pad(np.asarray(rgb), ((top, bottom), (left, right), (0, 0)), mode='edge').copy()
    known = np.zeros(canvas.shape[:2], dtype=bool)
    known[top:top + height, left:left + width] = True

    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    session = ort.InferenceSession(rt.resolve_model(request, MODEL, single=True), sess_options=options,
                                   providers=['CPUExecutionProvider'])
    names = [i.name for i in session.get_inputs()]
    migan = all(i.type == 'tensor(uint8)' for i in session.get_inputs())
    if not migan and not (len(names) == 2 and sum('mask' in n.lower() for n in names) == 1):
        raise rt.PluginError('unexpected model input contract')

    def infer(tile, tile_known):
        if migan:
            out = session.run(None, {
                'image': np.ascontiguousarray(tile.transpose(2, 0, 1)[None]),
                'mask': (tile_known.astype(np.uint8) * 255)[None, None],
            })[0][0].transpose(1, 2, 0)
        else:  # LaMa: fixed 512x512, float image 0-1, mask 1 = hole
            if tile.shape[:2] != (512, 512):
                raise rt.PluginError('this model needs 512px windows')
            image_t = tile.transpose(2, 0, 1)[None].astype(np.float32) / 255.0
            mask_t = (~tile_known)[None, None].astype(np.float32)
            out = session.run(None, {n: mask_t if 'mask' in n.lower() else image_t for n in names})[0][0].transpose(1, 2, 0)
        if out.shape != tile.shape:
            raise rt.PluginError('unexpected model output shape')
        return out

    # Generated at native resolution (never rescaled), working outward from the original edge
    # in bands of at most STRIP px. Each band sees the pixels next to it as context, and
    # becomes context for the next band. Views let one routine serve all four sides.
    views = lambda a: {'top': a, 'bottom': a[::-1], 'left': a.transpose(1, 0, 2) if a.ndim == 3 else a.T,
                       'right': (a.transpose(1, 0, 2) if a.ndim == 3 else a.T)[::-1]}
    pix, kn = views(canvas), views(known)
    # Left/right first over the original's rows only, then top/bottom across the full width.
    jobs = [('left', left, top, top + height), ('right', right, top, top + height),
            ('top', top, 0, full[0]), ('bottom', bottom, 0, full[0])]
    for side, depth, a0, b0 in jobs:
        arr, kmask = pix[side], kn[side]
        rows = arr.shape[0]
        cur = depth
        while cur > 0:
            d = min(STRIP, cur)
            y0 = cur - d
            redo = SEAM if cur < depth else 0  # re-generate the previous band's last rows
            kmask[cur:cur + redo, a0:b0] = False
            wh = min(WORK, (rows - y0) // STEP * STEP)
            d = min(d, wh // 2) if wh // 2 else d
            y0 = cur - d
            tw = min(WORK, (b0 - a0) // STEP * STEP)
            if wh < STEP or tw < STEP:
                raise rt.PluginError('the image is too small to expand this far')
            stride = max(STEP, tw * 3 // 4)
            xs = list(range(a0, b0 - tw + 1, stride))
            if xs[-1] != b0 - tw:
                xs.append(b0 - tw)
            overlap = max(1, tw - stride)
            ramp = np.minimum(1.0, np.minimum(np.arange(1, tw + 1), np.arange(tw, 0, -1)) / overlap)
            acc = np.zeros((d + redo, b0 - a0, 3), np.float32)
            weight = np.zeros((1, b0 - a0, 1), np.float32)
            for i, x in enumerate(xs):
                tile = arr[y0:y0 + wh, x:x + tw]
                gen = infer(tile, kmask[y0:y0 + wh, x:x + tw])[:d + redo].astype(np.float32)
                w = ramp.copy()
                if i == 0:
                    w[:overlap] = 1
                if i == len(xs) - 1:
                    w[-overlap:] = 1
                acc[:, x - a0:x - a0 + tw] += gen * w[None, :, None]
                weight[:, x - a0:x - a0 + tw] += w[None, :, None]
            arr[y0:cur + redo, a0:b0] = np.clip(acc / weight + 0.5, 0, 255).astype(np.uint8)
            kmask[y0:cur + redo, a0:b0] = True
            cur = y0

    generated = Image.fromarray(canvas)
    # Original pixels win inside the original rectangle; the generated border is opaque.
    result = generated.convert('RGBA')
    result.paste(rgb, (left, top))
    # Preserve the original's own transparency inside its rectangle.
    alpha = Image.new('L', full, 255)
    alpha.paste(image.getchannel('A'), (left, top))
    result.putalpha(alpha)
    result.save(request['output'], format='PNG')
    return {'output_size': list(full)}


def self_test(tmp):
    from PIL import Image
    path = os.path.join(tmp, 'in.png')
    Image.effect_mandelbrot((256, 192), (-2.2, -1.5, 0.8, 1.5), 40).convert('RGB').save(path)
    return {'operation': 'expand', 'input': path, 'output': os.path.join(tmp, 'out.png'),
            'expand': {'left': 32, 'top': 0, 'right': 32, 'bottom': 64}}


if __name__ == '__main__':
    rt.main({'expand': expand}, self_test=self_test)
