"""AI Image Expand (Diffusion): enlarge the canvas and outpaint it with Stable Diffusion 1.5 Inpainting.

Request: {"version": 1, "operation": "expand", "input": png, "output": png,
          "expand": {"left": px, "top": px, "right": px, "bottom": px}  or  "scale": 1.0-4.0,
          "max_pixels":  optional output limit (default 40,000,000),
          "time_budget": optional seconds (default 45; 0 = no limit, always full resolution),
          "prompt":      optional scene description,
          "steps":       optional 1-50 (default 20 on a GPU, 12 on CPU),
          "guidance":    optional 1-12 (default 3.0 on a GPU, 1.0 on CPU; 1 = no classifier-free guidance),
          "seed":        optional integer}
Original pixels are copied back untouched; only the new border is generated.

Runs fp16 ONNX models with its own CLIP tokenizer and DDIM scheduler (numpy, Pillow, onnxruntime only).
Models (text_encoder/unet/vae_encoder/vae_decoder .onnx, vocab.json, merges.txt) are downloaded and
verified by `run --setup`; request["model"] may name a folder containing them instead.

Hardware: an NVIDIA GPU (onnxruntime-gpu, CUDAExecutionProvider; `run --setup` installs it when
nvidia-smi is present) or a DirectML GPU is used when available, otherwise the CPU. CoreML is
deliberately not used: onnxruntime's CoreML provider fails on this model. On start the plugin times
the device, estimates the cost of generating at full resolution and, if that exceeds time_budget,
generates at the largest reduced resolution that fits and upscales the new border (softer, but done
in time). Fast GPUs usually stay at full size.

Full-resolution generation fills outward from the original edge in bands of up to 320px using 512px
tiles. Each tile sees the pixels already known beside and below it (original or previously generated)
as context, so tiles join consistently. Progress and the chosen plan are written to stderr.
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_runtime as rt  # noqa: E402

WORK = 512            # tile size; the UNet wants multiples of 64
UNIT = 64
STRIP = 320           # maximum band depth
STRIDE = 384          # new columns per tile (the rest is context from the previous tile)
MAX_SIDE = 16384
DEFAULT_PROMPT = 'a high quality photograph, natural, sharp, seamless continuation of the scene'
NEGATIVE_PROMPT = 'blurry, low quality, text, watermark, frame, border, seam, distorted'
FILES = ('text_encoder.onnx', 'unet.onnx', 'vae_encoder.onnx', 'vae_decoder.onnx', 'vocab.json', 'merges.txt')


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


class ClipTokenizer:
    """Minimal CLIP BPE tokenizer (ASCII-oriented approximation of the reference regex)."""
    PATTERN = re.compile(r"<\|startoftext\|>|<\|endoftext\|>|'s|'t|'re|'ve|'m|'ll|'d|[^\W\d_]+|\d|[^\s\w]+")

    def __init__(self, vocab_path, merges_path):
        with open(vocab_path, encoding='utf-8') as f:
            self.vocab = json.load(f)
        with open(merges_path, encoding='utf-8') as f:
            lines = f.read().split('\n')[1:49152 - 256 - 2 + 1]
        self.ranks = {tuple(line.split()): i for i, line in enumerate(lines)}
        bs = list(range(33, 127)) + list(range(161, 173)) + list(range(174, 256))
        cs, n = bs[:], 0
        for b in range(256):
            if b not in bs:
                bs.append(b)
                cs.append(256 + n)
                n += 1
        self.byte_map = dict(zip(bs, map(chr, cs)))
        self.cache = {}

    def bpe(self, token):
        if token in self.cache:
            return self.cache[token]
        word = tuple(token[:-1]) + (token[-1] + '</w>',)
        while len(word) > 1:
            pairs = {(a, b) for a, b in zip(word, word[1:])}
            best = min(pairs, key=lambda p: self.ranks.get(p, float('inf')))
            if best not in self.ranks:
                break
            merged, i = [], 0
            while i < len(word):
                if i < len(word) - 1 and (word[i], word[i + 1]) == best:
                    merged.append(word[i] + word[i + 1])
                    i += 2
                else:
                    merged.append(word[i])
                    i += 1
            word = tuple(merged)
        self.cache[token] = word
        return word

    def encode(self, text, length=77):
        ids = [49406]
        for piece in self.PATTERN.findall(' '.join(text.lower().split())):
            mapped = ''.join(self.byte_map[b] for b in piece.encode('utf-8'))
            ids.extend(self.vocab[t] for t in self.bpe(mapped) if t in self.vocab)
        ids = ids[:length - 1] + [49407]
        return ids + [49407] * (length - len(ids))


def plan(full, m):
    """Yield every tile as (side, y0, cur, d, wh, a0, b0, x, tw) in generation order."""
    left, top, right, bottom = m
    fw, fh = full
    jobs = [('left', left, top, fh - bottom), ('right', right, top, fh - bottom),
            ('top', top, 0, fw), ('bottom', bottom, 0, fw)]
    for side, depth, a0, b0 in jobs:
        if depth == 0:
            continue
        rows = fw if side in ('left', 'right') else fh
        bands = -(-depth // STRIP)
        cur = depth
        while cur > 0:
            d = min(cur, -(-depth // bands))
            y0 = cur - d
            wh = min(WORK, (rows - y0) // UNIT * UNIT)
            tw = min(WORK, (b0 - a0) // UNIT * UNIT)
            if wh < UNIT or tw < UNIT or wh <= d:
                raise rt.PluginError('the image is too small to expand this far')
            stride = min(STRIDE, tw)
            xs = list(range(a0, b0 - tw + 1, stride))
            if xs[-1] != b0 - tw:
                xs.append(b0 - tw)
            for x in xs:
                yield side, y0, cur, d, wh, a0, b0, x, tw
            cur = y0


def providers_for(ort):
    """Pick the execution provider for this machine; returns (providers, device name)."""
    available = ort.get_available_providers()
    if 'CUDAExecutionProvider' in available:
        return [('CUDAExecutionProvider', {'device_id': 0}), 'CPUExecutionProvider'], 'cuda'
    if 'DmlExecutionProvider' in available:
        return ['DmlExecutionProvider', 'CPUExecutionProvider'], 'directml'
    return ['CPUExecutionProvider'], 'cpu'


def ddim_schedule(steps):
    import numpy as np
    betas = np.linspace(0.00085 ** 0.5, 0.012 ** 0.5, 1000, dtype=np.float64) ** 2
    acp = np.cumprod(1.0 - betas)
    ratio = 1000 // steps
    timesteps = (np.arange(steps) * ratio).round()[::-1].astype(np.int64) + 1
    return acp, timesteps, ratio


def expand(request):
    import time
    import numpy as np
    import onnxruntime as ort
    from PIL import Image

    started = time.time()
    rt.require_paths(request, 'input', 'output')
    paths = {name: rt.resolve_model(request, name) for name in FILES}

    providers, device = providers_for(ort)
    gpu = device != 'cpu'
    steps = int(request.get('steps', 20 if gpu else 12))
    guidance = float(request.get('guidance', 3.0 if gpu else 1.0))
    budget = float(request.get('time_budget', 45))
    if not 1 <= steps <= 50 or not 1.0 <= guidance <= 12.0 or budget < 0:
        raise rt.PluginError('invalid steps, guidance or time_budget')
    prompt = str(request.get('prompt') or DEFAULT_PROMPT)[:500]
    rng = np.random.default_rng(int(request['seed']) if 'seed' in request else None)

    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA')
    width, height = image.size
    m = margins(request, width, height)
    full = (width + m[0] + m[2], height + m[1] + m[3])
    rgb = image.convert('RGB')

    options = ort.SessionOptions()
    options.intra_op_num_threads = max(1, os.cpu_count() or 1)
    options.inter_op_num_threads = 1

    def load(name):
        try:
            return ort.InferenceSession(paths[name], sess_options=options, providers=providers)
        except Exception:
            if not gpu:
                raise
            return ort.InferenceSession(paths[name], sess_options=options, providers=['CPUExecutionProvider'])

    tokenizer = ClipTokenizer(paths['vocab.json'], paths['merges.txt'])
    encoder = load('text_encoder.onnx')
    embed = lambda text: encoder.run(None, {'input_ids': np.array([tokenizer.encode(text)], dtype=np.int32)})[0]
    cond, uncond = embed(prompt), embed(NEGATIVE_PROMPT)
    del encoder
    use_cfg = guidance > 1.0
    copies = 2 if use_cfg else 1
    context = np.concatenate([uncond, cond]) if use_cfg else cond
    unet, vae_enc, vae_dec = load('unet.onnx'), load('vae_encoder.onnx'), load('vae_decoder.onnx')
    if gpu and 'CPUExecutionProvider' == unet.get_providers()[0]:
        device = 'cpu (GPU unavailable)'

    def infer(tile, tile_known, nsteps):
        """Outpaint the unknown pixels of one tile (H, W multiples of 64); returns uint8 HxWx3."""
        h, w = tile.shape[:2]
        acp, timesteps, ratio = ddim_schedule(nsteps)
        hole = ~tile_known
        pixels = tile.astype(np.float32) / 127.5 - 1.0
        pixels[hole] = 0.0
        masked = vae_enc.run(None, {'sample': pixels.transpose(2, 0, 1)[None].astype(np.float16)})[0]
        masked = masked.astype(np.float32) * 0.18215
        latent_mask = hole.reshape(h // 8, 8, w // 8, 8).any(axis=(1, 3)).astype(np.float32)[None, None]
        latents = rng.standard_normal((1, 4, h // 8, w // 8)).astype(np.float32)
        fixed = np.concatenate([np.repeat(latent_mask, copies, 0), np.repeat(masked, copies, 0)], axis=1)
        for t in timesteps:
            model_in = np.concatenate([np.repeat(latents, copies, 0), fixed], axis=1).astype(np.float16)
            eps = unet.run(None, {'sample': model_in, 'timestep': np.full(copies, t, np.float16),
                                  'encoder_hidden_states': context})[0].astype(np.float32)
            if use_cfg:
                eps = eps[0:1] + guidance * (eps[1:2] - eps[0:1])
            a_t = acp[t]
            a_prev = acp[t - ratio] if t - ratio >= 0 else acp[0]
            x0 = (latents - np.sqrt(1 - a_t) * eps) / np.sqrt(a_t)
            latents = (np.sqrt(a_prev) * x0 + np.sqrt(1 - a_prev) * eps).astype(np.float32)
        decoded = vae_dec.run(None, {'latent_sample': (latents / 0.18215).astype(np.float16)})[0][0]
        out = np.clip((decoded.astype(np.float32).transpose(1, 2, 0) + 1.0) * 127.5 + 0.5, 0, 255)
        if out.shape[:2] != (h, w) or not np.isfinite(out).all():
            raise rt.PluginError('unexpected decoder output')
        return out.astype(np.uint8)

    # ---- choose a working scale that fits the time budget
    scale = 1.0
    if budget > 0:
        probe = np.zeros((WORK, WORK, 3), np.uint8)
        probe_known = np.zeros((WORK, WORK), bool)
        probe_known[WORK // 2:] = True
        infer(probe, probe_known, 1)                       # warm-up (graph load, GPU init)
        t0 = time.time()
        infer(probe, probe_known, 1)
        t_one = time.time() - t0                           # encode + 1 step + decode
        sample = np.zeros((copies, 9, WORK // 8, WORK // 8), np.float16)
        t0 = time.time()
        unet.run(None, {'sample': sample, 'timestep': np.full(copies, 500, np.float16), 'encoder_hidden_states': context})
        t_step = time.time() - t0
        remaining = budget - (time.time() - started)
        options_found = []   # (scale, steps, tiles, seconds), best quality first
        for candidate in (1.0, 0.75, 0.5, 0.35, 0.25, 0.18, 0.125, 0.09, 0.065, 0.045):
            sm = [max(8, round(v * candidate)) if v else 0 for v in m]
            sfull = (max(1, round(width * candidate)) + sm[0] + sm[2], max(1, round(height * candidate)) + sm[1] + sm[3])
            try:
                tiles_list = list(plan(sfull, sm))
            except rt.PluginError:
                continue
            # Below full size the model must still see full 512px windows or it produces garbage.
            if candidate < 1.0 and any(t[4] != WORK or t[8] != WORK for t in tiles_list):
                continue
            for n in dict.fromkeys((steps, min(steps, 8), min(steps, 5))):
                options_found.append((candidate, n, len(tiles_list), len(tiles_list) * (t_one + (n - 1) * t_step)))
        if not options_found:
            raise rt.PluginError('the image is too small to expand this far')
        fitting = [o for o in options_found if o[3] <= remaining]
        scale, steps, ntiles, eta = fitting[0] if fitting else min(options_found, key=lambda o: o[3])
        print(f'device={device} step={t_step:.2f}s -> scale={scale} steps={steps} tiles={ntiles} '
              f'eta={eta:.0f}s' + ('' if fitting else ' (over budget: smallest valid size)'), file=sys.stderr, flush=True)
    else:
        print(f'device={device} scale=1 (no time limit)', file=sys.stderr, flush=True)

    # ---- generate (on a reduced canvas when scale < 1)
    if scale < 1.0:
        sm = [max(8, round(v * scale)) if v else 0 for v in m]
        work_rgb = rgb.resize((max(1, round(width * scale)), max(1, round(height * scale))), Image.Resampling.LANCZOS)
    else:
        sm, work_rgb = list(m), rgb
    ww, wh_ = work_rgb.size
    wfull = (ww + sm[0] + sm[2], wh_ + sm[1] + sm[3])
    canvas = np.pad(np.asarray(work_rgb), ((sm[1], sm[3]), (sm[0], sm[2]), (0, 0)), mode='edge').copy()
    known = np.zeros(canvas.shape[:2], dtype=bool)
    known[sm[1]:sm[1] + wh_, sm[0]:sm[0] + ww] = True

    views = lambda a: {'top': a, 'bottom': a[::-1], 'left': a.transpose(1, 0, 2) if a.ndim == 3 else a.T,
                       'right': (a.transpose(1, 0, 2) if a.ndim == 3 else a.T)[::-1]}
    pix, kn = views(canvas), views(known)
    count = 0
    tiles = list(plan(wfull, sm))
    for side, y0, cur, d, wh, a0, b0, x, tw in tiles:
        rt.progress(request, count / len(tiles), f'Tile {count + 1} of {len(tiles)} ({side} edge)')
        arr, kmask = pix[side], kn[side]
        tile = arr[y0:y0 + wh, x:x + tw]
        tile_known = kmask[y0:y0 + wh, x:x + tw]
        fresh = ~kmask[y0:cur, x:x + tw]      # only unknown pixels are written back
        if fresh.any():
            generated = infer(tile, tile_known, steps)[:d]
            band = arr[y0:cur, x:x + tw]
            band[fresh] = generated[fresh]
            kmask[y0:cur, x:x + tw] = True
        count += 1
        print(f'tile {count} ({side}) {time.time() - started:.0f}s', file=sys.stderr, flush=True)

    generated_full = Image.fromarray(canvas)
    if scale < 1.0:
        generated_full = generated_full.resize(full, Image.Resampling.LANCZOS)
    result = generated_full.convert('RGBA')
    result.paste(rgb, (m[0], m[1]))
    alpha = Image.new('L', full, 255)
    alpha.paste(image.getchannel('A'), (m[0], m[1]))
    result.putalpha(alpha)
    result.save(request['output'], format='PNG')
    return {'device': device, 'working_scale': scale, 'steps': steps, 'output_size': list(full)}


def describe_device():
    import onnxruntime as ort
    return providers_for(ort)[1]


def self_test(tmp):
    from PIL import Image
    path = os.path.join(tmp, 'in.png')
    Image.effect_mandelbrot((512, 256), (-2.2, -1.5, 0.8, 1.5), 40).convert('RGB').save(path)
    return {'operation': 'expand', 'input': path, 'output': os.path.join(tmp, 'out.png'),
            'expand': {'left': 0, 'top': 0, 'right': 0, 'bottom': 128}, 'steps': 4, 'guidance': 1, 'time_budget': 0, 'seed': 1}


if __name__ == '__main__':
    if '--plugin' in sys.argv[1:]:          # protocol v2: the UI process (ui.py) drives this file
        import ui
        sys.exit(ui.main())
    rt.main({'expand': expand}, self_test=self_test, device=describe_device)
