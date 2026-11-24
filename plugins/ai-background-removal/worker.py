#!/usr/bin/env python3
"""AI Background Removal: U2-Net-P salient-object matting.

Request: {"version": 1, "operation": "remove_background", "input": png, "output": png}
The output is the input with the background made transparent (alpha channel from the mask).
The model (u2netp.onnx) is downloaded and verified by `run --setup`.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_runtime as rt  # noqa: E402

MODEL = 'u2netp.onnx'


def remove_background(request):
    import numpy as np
    import onnxruntime as ort
    from PIL import Image, ImageChops

    rt.require_paths(request, 'input', 'output')
    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA')
    if image.width * image.height > rt.max_pixels(request):
        raise rt.PluginError('the image exceeds the size limit')
    options = ort.SessionOptions()
    options.intra_op_num_threads = max(1, os.cpu_count() or 1)
    session = ort.InferenceSession(rt.resolve_model(request, MODEL, single=True), sess_options=options,
                                   providers=['CPUExecutionProvider'])
    array = np.asarray(image.convert('RGB').resize((320, 320), Image.Resampling.LANCZOS), dtype=np.float32)
    array /= max(float(array.max()), 1.0)
    array = (array - np.array([.485, .456, .406], dtype=np.float32)) / np.array([.229, .224, .225], dtype=np.float32)
    prediction = session.run(None, {session.get_inputs()[0].name: array.transpose(2, 0, 1)[None]})[0][0, 0]
    low, high = float(prediction.min()), float(prediction.max())
    if not np.isfinite(prediction).all() or high - low < 1e-6:
        raise rt.PluginError('no clear foreground found')
    mask = Image.fromarray(np.clip((prediction - low) / (high - low) * 255, 0, 255).astype(np.uint8)).resize(
        image.size, Image.Resampling.LANCZOS)
    image.putalpha(ImageChops.multiply(image.getchannel('A'), mask))
    image.save(request['output'], format='PNG')
    return {'output_size': list(image.size)}


def self_test(tmp):
    from PIL import Image, ImageDraw
    path = os.path.join(tmp, 'in.png')
    image = Image.new('RGB', (256, 256), (230, 230, 230))
    ImageDraw.Draw(image).ellipse((70, 50, 190, 210), fill=(200, 40, 40))
    image.save(path)
    return {'operation': 'remove_background', 'input': path, 'output': os.path.join(tmp, 'out.png')}


if __name__ == '__main__':
    rt.main({'remove_background': remove_background}, self_test=self_test)
