#!/usr/bin/env python3
"""AI Image Enlarger: 3x super-resolution with the bundled ONNX Model Zoo sub-pixel CNN.

Request: {"version": 1, "operation": "enlarge", "input": png, "output": png, "scale": 2 | 3}
The model (model/super-resolution-10.onnx) ships inside the plugin; nothing is downloaded.
The network upscales luminance 3x; colour comes from a bicubic resize and is re-anchored to it.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_runtime as rt  # noqa: E402

MODEL = 'super-resolution-10.onnx'
TILE = 224          # the model works on 224px tiles and returns 672px luminance planes
STEP = 208          # 8px of overlap context on each side


def enlarge(request):
    import numpy as np
    import onnxruntime as ort
    from PIL import Image

    rt.require_paths(request, 'input', 'output')
    scale = request.get('scale', 3)
    if scale not in (2, 3):
        raise rt.PluginError('scale must be 2 or 3')
    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA')
    width, height = image.size
    if width * height * scale * scale > rt.max_pixels(request):
        raise rt.PluginError('the enlarged image would exceed the size limit')

    options = ort.SessionOptions()
    options.intra_op_num_threads = max(1, os.cpu_count() or 1)
    session = ort.InferenceSession(rt.resolve_model(request, MODEL, single=True), sess_options=options,
                                   providers=['CPUExecutionProvider'])
    name = session.get_inputs()[0].name
    size = (width * scale, height * scale)
    baseline = image.resize(size, Image.Resampling.BICUBIC)
    rgb = np.asarray(image.convert('RGB'), dtype=np.float32)
    luminance = (rgb[:, :, 0] * .299 + rgb[:, :, 1] * .587 + rgb[:, :, 2] * .114) / 255
    padded = np.pad(luminance, ((8, TILE), (8, TILE)), mode='edge')
    output = np.array(baseline, dtype=np.uint8)
    positions = [(y, x) for y in range(0, height, STEP) for x in range(0, width, STEP)]
    for index, (y, x) in enumerate(positions):
        rt.progress(request, index / len(positions), f'Enlarging tile {index + 1} of {len(positions)}')
        tile = padded[y:y + TILE, x:x + TILE][None, None].astype(np.float32)
        prediction = session.run(None, {name: tile})[0][0, 0]
        if prediction.shape != (672, 672) or not np.isfinite(prediction).all():
            raise rt.PluginError('unexpected super-resolution output')
        h, w = min(STEP, height - y), min(STEP, width - x)
        ai = Image.fromarray(np.clip(prediction[24:24 + h * 3, 24:24 + w * 3], 0, 1), mode='F').resize(
            (w * scale, h * scale), Image.Resampling.BILINEAR)
        region = output[y * scale:(y + h) * scale, x * scale:(x + w) * scale, :3].astype(np.float32)
        base_y = region[:, :, 0] * .299 + region[:, :, 1] * .587 + region[:, :, 2] * .114
        region += (np.asarray(ai) * 255 - base_y)[:, :, None]
        output[y * scale:(y + h) * scale, x * scale:(x + w) * scale, :3] = np.clip(region, 0, 255).astype(np.uint8)
    Image.fromarray(output, 'RGBA').save(request['output'], format='PNG')
    return {'output_size': list(size)}


def self_test(tmp):
    from PIL import Image
    path = os.path.join(tmp, 'in.png')
    Image.radial_gradient('L').resize((96, 64)).convert('RGB').save(path)
    return {'operation': 'enlarge', 'input': path, 'output': os.path.join(tmp, 'out.png'), 'scale': 3}


if __name__ == '__main__':
    if '--plugin' in sys.argv[1:]:          # protocol v2: the UI process (ui.py) drives this file
        import ui
        sys.exit(ui.main())
    rt.main({'enlarge': enlarge}, self_test=self_test)
