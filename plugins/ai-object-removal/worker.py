#!/usr/bin/env python3
"""AI Object Removal: LaMa inpainting of a painted mask.

Request: {"version": 1, "operation": "remove_object", "input": png, "mask": png, "output": png}
`mask` is an 8-bit grayscale PNG the same size as the input; any nonzero pixel is removed.
The model (lama_fp32.onnx, 512px) is downloaded and verified by `run --setup`. Only the masked
pixels change; everything else is copied from the original at full resolution.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_runtime as rt  # noqa: E402

MODEL = 'lama_fp32.onnx'


def remove_object(request):
    import numpy as np
    import onnxruntime as ort
    from PIL import Image

    rt.require_paths(request, 'input', 'output', 'mask')
    if not os.path.isfile(request['mask']):
        raise rt.PluginError('mask image not found')
    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA')
    if image.width * image.height > rt.max_pixels(request):
        raise rt.PluginError('the image exceeds the size limit')
    with Image.open(request['mask']) as opened:
        original_mask = opened.convert('L').point(lambda v: 255 if v else 0)
    if original_mask.size != image.size:
        raise rt.PluginError('mask and image sizes differ')
    if not original_mask.getbbox():
        raise rt.PluginError('the mask is empty')

    options = ort.SessionOptions()
    options.intra_op_num_threads = max(1, os.cpu_count() or 1)
    session = ort.InferenceSession(rt.resolve_model(request, MODEL, single=True), sess_options=options,
                                   providers=['CPUExecutionProvider'])
    mask = original_mask.resize((512, 512), Image.Resampling.NEAREST)
    rgb = image.convert('RGB').resize((512, 512), Image.Resampling.LANCZOS)
    image_tensor = np.asarray(rgb, dtype=np.float32).transpose(2, 0, 1)[None] / 255.0
    mask_tensor = (np.asarray(mask, dtype=np.float32)[None, None] > 0).astype(np.float32)
    inputs = session.get_inputs()
    if len(inputs) != 2 or sum('mask' in i.name.lower() for i in inputs) != 1:
        raise rt.PluginError('unexpected LaMa input contract')
    rt.progress(request, 0.5, 'Inpainting the masked area')
    feed = {i.name: mask_tensor if 'mask' in i.name.lower() else image_tensor for i in inputs}
    prediction = session.run(None, feed)[0][0]
    if not np.isfinite(prediction).all():
        raise rt.PluginError('invalid prediction')
    generated = Image.fromarray(np.clip(prediction.transpose(1, 2, 0), 0, 255).astype(np.uint8)).resize(
        image.size, Image.Resampling.LANCZOS).convert('RGBA')
    generated.putalpha(image.getchannel('A'))
    Image.composite(generated, image, original_mask).save(request['output'], format='PNG')
    return {'output_size': list(image.size)}


def self_test(tmp):
    from PIL import Image, ImageDraw
    path, mask_path = os.path.join(tmp, 'in.png'), os.path.join(tmp, 'mask.png')
    image = Image.new('RGB', (256, 256), (90, 140, 200))
    ImageDraw.Draw(image).rectangle((100, 100, 150, 150), fill=(250, 250, 0))
    image.save(path)
    mask = Image.new('L', (256, 256), 0)
    ImageDraw.Draw(mask).rectangle((95, 95, 155, 155), fill=255)
    mask.save(mask_path)
    return {'operation': 'remove_object', 'input': path, 'mask': mask_path, 'output': os.path.join(tmp, 'out.png')}


if __name__ == '__main__':
    if '--plugin' in sys.argv[1:]:          # protocol v2: the UI process (ui.py) drives this file
        import ui
        sys.exit(ui.main())
    rt.main({'remove_object': remove_object}, self_test=self_test)
