#!/usr/bin/env python3
"""Pigeon editor protocol v1 CPU worker. Requires numpy, Pillow, onnxruntime.

Models are provisioned and pinned by the host, never downloaded by this worker.
Enlarge expects ONNX Model Zoo super-resolution-10 (3x luminance); background
expects U2Net-P; object removal expects the 512px LaMa ONNX model used by the
old Pigeon plugin. The host must choose the correct model for each capability.
"""
import json
import sys


def main():
    import numpy as np
    import onnxruntime as ort
    from PIL import Image, ImageChops

    request = json.load(sys.stdin)
    if request['version'] != 1:
        raise ValueError('unsupported protocol')
    if request['operation'] == 'health':
        print(json.dumps({'version': 1, 'ok': True}), flush=True)
        return
    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    session = ort.InferenceSession(request['model'], sess_options=options,
                                   providers=['CPUExecutionProvider'])
    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA')
    operation = request['operation']
    if operation == 'remove_background':
        array = np.asarray(image.convert('RGB').resize((320, 320), Image.Resampling.LANCZOS), dtype=np.float32)
        array /= max(float(array.max()), 1.0)
        array = (array - np.array([.485, .456, .406], dtype=np.float32)) / np.array([.229, .224, .225], dtype=np.float32)
        prediction = session.run(None, {session.get_inputs()[0].name: array.transpose(2, 0, 1)[None]})[0][0, 0]
        low, high = float(prediction.min()), float(prediction.max())
        if not np.isfinite(prediction).all() or high - low < 1e-6:
            raise ValueError('no clear foreground')
        mask = Image.fromarray(np.clip((prediction - low) / (high - low) * 255, 0, 255).astype(np.uint8)).resize(image.size, Image.Resampling.LANCZOS)
        image.putalpha(ImageChops.multiply(image.getchannel('A'), mask))
        result = image
    elif operation == 'remove_object':
        with Image.open(request['mask']) as opened:
            mask = opened.convert('L').point(lambda v: 255 if v else 0)
        original_mask = mask
        mask = mask.resize((512, 512), Image.Resampling.NEAREST)
        rgb = image.convert('RGB').resize((512, 512), Image.Resampling.LANCZOS)
        image_tensor = np.asarray(rgb, dtype=np.float32).transpose(2, 0, 1)[None] / 255.0
        mask_tensor = (np.asarray(mask, dtype=np.float32)[None, None] > 0).astype(np.float32)
        inputs = session.get_inputs()
        if len(inputs) != 2 or sum('mask' in i.name.lower() for i in inputs) != 1:
            raise ValueError('unexpected LaMa input contract')
        feed = {i.name: mask_tensor if 'mask' in i.name.lower() else image_tensor for i in inputs}
        prediction = session.run(None, feed)[0][0]
        if not np.isfinite(prediction).all():
            raise ValueError('invalid prediction')
        generated = Image.fromarray(np.clip(prediction.transpose(1, 2, 0), 0, 255).astype(np.uint8)).resize(image.size, Image.Resampling.LANCZOS).convert('RGBA')
        generated.putalpha(image.getchannel('A'))
        result = Image.composite(generated, image, original_mask)
    elif operation == 'enlarge':
        scale = request['scale']
        if scale not in (2, 3):
            raise ValueError('invalid scale')
        width, height = image.size
        size = (width * scale, height * scale)
        baseline = image.resize(size, Image.Resampling.BICUBIC)
        rgb = np.asarray(image.convert('RGB'), dtype=np.float32)
        luminance = (rgb[:, :, 0] * .299 + rgb[:, :, 1] * .587 + rgb[:, :, 2] * .114) / 255
        # The model uses 224px tiles and returns a 3x luminance plane.
        padded = np.pad(luminance, ((8, 224), (8, 224)), mode='edge')
        output = np.array(baseline, dtype=np.uint8)
        for y in range(0, height, 208):
            for x in range(0, width, 208):
                tile = padded[y:y + 224, x:x + 224][None, None].astype(np.float32)
                prediction = session.run(None, {session.get_inputs()[0].name: tile})[0][0, 0]
                if prediction.shape != (672, 672) or not np.isfinite(prediction).all():
                    raise ValueError('unexpected super-resolution output')
                h, w = min(208, height - y), min(208, width - x)
                ai = Image.fromarray(np.clip(prediction[24:24 + h * 3, 24:24 + w * 3], 0, 1), mode='F').resize((w * scale, h * scale), Image.Resampling.BILINEAR)
                region = output[y * scale:(y + h) * scale, x * scale:(x + w) * scale, :3].astype(np.float32)
                base_y = region[:, :, 0] * .299 + region[:, :, 1] * .587 + region[:, :, 2] * .114
                region += (np.asarray(ai) * 255 - base_y)[:, :, None]
                output[y * scale:(y + h) * scale, x * scale:(x + w) * scale, :3] = np.clip(region, 0, 255).astype(np.uint8)
        result = Image.fromarray(output, 'RGBA')
    else:
        raise ValueError('unsupported operation')
    result.save(request['output'], format='PNG')
    print(json.dumps({'version': 1, 'ok': True}), flush=True)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        sys.exit(1)
