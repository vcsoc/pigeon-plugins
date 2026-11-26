"""Behavioural tests for each plugin's protocol-v2 UI, driven through scripts/mock_host.py.

Each case starts the real plugin (`sh run --plugin`), drives it like Pigeon would (visibility, asset
action, clicks, changes, canvas pointers) and checks the scenes, resources, derivative imports and the
files the operations produced. Heavy operations run for real (models from PIGEON_PLUGIN_SEED or the
plugin's own setup), so these tests need the plugin to be set up in the folder they are given.
"""
import os

from PIL import Image, ImageChops, ImageDraw

import mock_host as mh


def make_images(folder):
    """Synthetic inputs: a photo-like gradient with a yellow block, and a round subject for matting."""
    os.makedirs(folder, exist_ok=True)
    paths = {}
    photo = Image.effect_mandelbrot((200, 150), (-2.2, -1.5, 0.8, 1.5), 60).convert('RGB')
    ImageDraw.Draw(photo).rectangle((100, 100, 130, 130), fill=(250, 250, 0))
    paths['photo'] = os.path.join(folder, 'photo.png')
    photo.save(paths['photo'])
    subject = Image.new('RGB', (256, 256), (230, 230, 230))
    ImageDraw.Draw(subject).ellipse((70, 50, 190, 210), fill=(200, 40, 40))
    paths['subject'] = os.path.join(folder, 'subject.png')
    subject.save(paths['subject'])
    small = Image.effect_mandelbrot((128, 96), (-2.2, -1.5, 0.8, 1.5), 40).convert('RGB')
    paths['small'] = os.path.join(folder, 'small.png')
    small.save(paths['small'])
    return paths


def asset(path, name, width, height, ident='asset-1'):
    return {'id': ident, 'name': name, 'kind': 'image', 'path': path, 'w': width, 'h': height}


def check(condition, message):
    if not condition:
        raise mh.MockHostError(message)


class Session:
    """A started plugin and its mock host, closed on exit."""

    def __init__(self, plugin_dir, seed, settings=None):
        env = {'PIGEON_PLUGIN_SEED': seed} if seed else {}
        self.host = mh.MockHost(plugin_dir, env=env, settings=settings)

    def __enter__(self):
        return self.host

    def __exit__(self, *exc):
        code = self.host.shutdown()
        if not exc or exc[0] is None:
            check(code == 0, f'plugin exit {code}: {self.host.stderr_tail()}')
            check(not self.host.errors, f'protocol errors: {self.host.errors}')
        return False


def open_tool(host, target, run_ready=True):
    """Handshake, show the panel, and open the tool window for `target` (an asset action)."""
    ready = host.handshake()
    check(mh.contributions_match(ready, os.path.join(host.plugin_dir, 'plugin.json')), 'ready contributions differ')
    host.ping()
    host.visible('panel')
    host.scene('panel', lambda s: mh.find(s, 'run') is not None or mh.find(s, 'setup') is not None,
               what='panel content')
    host.wait(lambda: (mh.find(host.scenes.get('panel'), 'run') or {}).get('enabled') is False,
              what='run button disabled without a target')
    host.command('open', [target])
    host.wait(lambda: any(m.get('instance') == ready['plugin']['id'] + '/tool' for m in host.modals),
              what='tool window opened')
    host.scene('tool', lambda s: mh.find(s, 'target') is not None, what='tool shows the target')
    host.wait(lambda: any(rid.startswith('source-') for rid in host.resources), what='source preview published')
    if run_ready:
        host.wait(lambda: (mh.find(host.scenes.get('tool'), 'run') or {}).get('enabled') is True,
                  what='run enabled for the target')


def run_and_review(host, expect_consent=True):
    """Press Run; confirm first-use consent; wait for the review state. Returns the review scene."""
    host.click('tool', 'run')
    if expect_consent:
        host.scene('tool', lambda s: mh.find(s, 'consent-yes') is not None, what='consent card')
        host.click('tool', 'consent-yes')
    host.scene('tool', lambda s: mh.find(s, 'accept') is not None or mh.find(s, 'error') is not None,
               timeout=900, what='review or error')
    scene = host.scenes['tool']
    check(mh.find(scene, 'accept') is not None, f"operation failed: {mh.find(scene, 'error')}")
    return scene


def review_file(host):
    resource = host.resource('after')
    check(resource is not None, 'no after resource was published')
    return resource


def accept_and_check(host, original_path, expected_name=None):
    before = len(host.imports)
    host.click('tool', 'accept')
    host.wait(lambda: len(host.imports) > before, timeout=60, what='library.import derive')
    imported = host.imports[-1]
    check(imported.get('mode') == 'derive', f'import mode {imported.get("mode")}')
    check(imported.get('derive_of') == 'asset-1', f'derive_of {imported.get("derive_of")}')
    check(len(imported.get('paths') or []) == 1, 'one derivative path expected')
    check(os.path.isfile(imported['paths'][0]), 'derivative file missing')
    check(imported['paths'][0] != original_path, 'the original file must not be the derivative')
    return imported


def ui_consent_persisted(host, key):
    check(host.settings.get(key) is True, f'consent not stored under {key}: {host.settings}')


def case_enlarge(plugin_dir, seed, scratch):
    images = make_images(scratch)
    with Session(plugin_dir, seed) as host:
        open_tool(host, asset(images['small'], 'small.png', 128, 96))
        check(mh.find(host.scenes['tool'], 'scale') is not None, 'scale option missing')
        run_and_review(host)
        host.change('tool', 'preview', 'before')
        host.scene('tool', lambda s: (mh.find(s, 'preview-image') or {}).get('resource', '').startswith('source-'),
                   what='before preview')
        host.change('tool', 'preview', 'after')
        host.scene('tool', lambda s: (mh.find(s, 'preview-image') or {}).get('resource') == 'after',
                   what='after preview')
        resource = review_file(host)
        check((resource['w'], resource['h']) == (384, 288), f'3x size wrong: {resource.get("w")}x{resource.get("h")}')
        out = Image.open(resource['file'])
        check(out.size == (384, 288), f'output size {out.size}')
        ui_consent_persisted(host, 'consent.enlarge')
        host.click('tool', 'discard')                  # back to the options, as in the UI
        host.scene('tool', lambda s: mh.find(s, 'run') is not None, what='options after discard')
        host.change('tool', 'scale', '2')
        host.click('tool', 'run')                      # consent already given: straight to running
        host.scene('tool', lambda s: mh.find(s, 'accept') is not None, timeout=900, what='2x review')
        host.wait(lambda: review_file(host)['w'] == 256, what='the 2x result (256 px wide)')
        accept_and_check(host, images['small'])
        host.scene('tool', lambda s: mh.find(s, 'run') is not None, what='back to options')
        host.wait(lambda: bool(host.toasts), what='a success toast')
        check(any(m == 'progress.start' for m, _ in host.progress), 'progress.start expected')
        check(any(m == 'progress.end' for m, _ in host.progress), 'progress.end expected')
    return 'scale 3x/2x, consent, derive import, progress'


def case_background(plugin_dir, seed, scratch):
    images = make_images(scratch)
    with Session(plugin_dir, seed) as host:
        open_tool(host, asset(images['subject'], 'subject.png', 256, 256))
        run_and_review(host)
        resource = review_file(host)
        out = Image.open(resource['file'])
        check(out.mode == 'RGBA', f'expected an RGBA cut-out, got {out.mode}')
        alpha = out.getchannel('A')
        low, high = alpha.getextrema()
        check(low < 64 and high > 192, f'cut-out has no transparent and opaque areas: {alpha.getextrema()}')
        host.click('tool', 'discard')
        host.scene('tool', lambda s: mh.find(s, 'run') is not None, what='discard returns to options')
        check(not host.imports, 'discarding must not import anything')
    return 'consent, cut-out with alpha, discard'


def case_object_removal(plugin_dir, seed, scratch):
    images = make_images(scratch)
    with Session(plugin_dir, seed) as host:
        open_tool(host, asset(images['photo'], 'photo.png', 200, 150), run_ready=False)
        host.scene('tool', lambda s: mh.find(s, 'mask') is not None, what='mask canvas')
        check((mh.find(host.scenes['tool'], 'run') or {}).get('enabled') is False,
              'run needs a marked rectangle first')
        # Mark 100..120 in source pixels (canvas is 1:1 for a 200x150 image).
        host.pointer('tool', 'mask', 'down', 90, 90)
        host.pointer('tool', 'mask', 'move', 110, 110)
        host.pointer('tool', 'mask', 'up', 120, 120)
        host.scene('tool', lambda s: mh.find(s, 'mask-status') and '1 of 256' in mh.find(s, 'mask-status')['text'],
                   what='one rectangle')
        host.wait(lambda: (mh.find(host.scenes['tool'], 'run') or {}).get('enabled') is True, what='run after mark')
        # Erase a part of it: the erase mode takes the middle back.
        host.change('tool', 'mode', 'erase')
        host.pointer('tool', 'mask', 'down', 100, 100)
        host.pointer('tool', 'mask', 'up', 110, 110)
        host.scene('tool', lambda s: mh.find(s, 'mask-status') and '2 of 256' in mh.find(s, 'mask-status')['text'],
                   what='two rectangles')
        run_and_review(host)
        review = review_file(host)
        result = Image.open(review['file']).convert('RGB')
        check(result.size == (200, 150), f'result size {result.size}')
        source = Image.open(images['photo']).convert('RGB')
        diff = ImageChops.difference(result, source).getbbox()
        check(diff is not None, 'the masked area must change')
        check(diff[0] >= 85 and diff[1] >= 85 and diff[2] <= 125 and diff[3] <= 125,
              f'only the masked area may change, changed bbox {diff}')
        check(result.getpixel((10, 10)) == source.getpixel((10, 10)), 'pixels outside the mask must be unchanged')
        # The mask that was sent: marked minus the erased middle.
        job_masks = [f for f in os.listdir(os.path.join(host.session, 'results')) if f.endswith('-mask.png')]
        check(len(job_masks) == 1, f'expected one mask file, found {job_masks}')
        mask = Image.open(os.path.join(host.session, 'results', job_masks[0])).convert('L')
        check(mask.size == (200, 150), f'mask must be source size, got {mask.size}')
        check(mask.getpixel((92, 92)) == 255, 'marked pixel must be in the mask')
        check(mask.getpixel((105, 105)) == 0, 'erased pixel must be out of the mask')
        check(mask.getpixel((10, 10)) == 0, 'unmarked pixel must be out of the mask')
        accept_and_check(host, images['photo'])
        ui_consent_persisted(host, 'consent.remove_object')
    return 'canvas mark/erase, rectangle count, source-size mask, outside pixels unchanged, derive'


def case_expand(plugin_dir, seed, scratch):
    images = make_images(scratch)
    with Session(plugin_dir, seed) as host:
        open_tool(host, asset(images['small'], 'small.png', 128, 96))
        for side in ('left', 'top', 'right', 'bottom'):
            host.change('tool', side, 0)
        host.scene('tool', lambda s: mh.find(s, 'validation') is not None, what='validation for zero margins')
        check('Add space' in mh.find(host.scenes['tool'], 'validation')['text'], 'zero-margin message')
        host.change('tool', 'left', 8001)
        host.scene('tool', lambda s: 'at most 8000' in str(mh.find(s, 'validation') or {}), what='8000 limit message')
        host.change('tool', 'left', 32)
        host.change('tool', 'top', 0)
        host.change('tool', 'right', 32)
        host.change('tool', 'bottom', 64)
        host.scene('tool', lambda s: mh.find(s, 'expand-preview') is not None, what='expand preview canvas')
        host.wait(lambda: (mh.find(host.scenes['tool'], 'run') or {}).get('enabled') is True, what='run enabled')
        run_and_review(host)
        review = review_file(host)
        check((review['w'], review['h']) == (192, 160), f'expanded size {review.get("w")}x{review.get("h")}')
        result = Image.open(review['file']).convert('RGB')
        original = Image.open(images['small']).convert('RGB')
        check(result.crop((32, 0, 32 + 128, 96)) == original, 'the original pixels must be kept exactly')
        accept_and_check(host, images['small'])
    return 'validation messages, live preview, exact original pixels kept, derive'


def case_diffusion(plugin_dir, seed, scratch):
    images = make_images(scratch)
    with Session(plugin_dir, seed) as host:
        open_tool(host, asset(images['small'], 'small.png', 128, 96))
        host.wait(lambda: any('Device:' in str(n.get('text', '')) for n in
                              mh.walk(host.scenes.get('tool') or {})), what='device report')
        host.change('tool', 'left', 0)
        host.change('tool', 'top', 0)
        host.change('tool', 'right', 0)
        host.change('tool', 'bottom', 64)
        host.change('tool', 'time_budget', 0)
        host.change('tool', 'prompt', 'a quiet street at dusk')
        host.change('tool', 'manual', True)
        host.scene('tool', lambda s: mh.find(s, 'steps') is not None, what='manual steps shown')
        host.change('tool', 'steps', 2)
        host.change('tool', 'guidance', 1.0)
        host.wait(lambda: (mh.find(host.scenes['tool'], 'run') or {}).get('enabled') is True, what='run enabled')
        run_and_review(host)
        review = review_file(host)
        check((review['w'], review['h']) == (128, 160), f'diffusion size {review.get("w")}x{review.get("h")}')
        result = Image.open(review['file']).convert('RGB')
        original = Image.open(images['small']).convert('RGB')
        check(result.crop((0, 0, 128, 96)) == original, 'the original pixels must be kept exactly')
        accept_and_check(host, images['small'])
    return 'device report, manual steps, prompt, exact original pixels kept, derive'


def case_semantic(plugin_dir, seed, scratch):
    images = make_images(scratch)
    duplicate = os.path.join(scratch, 'duplicate.png')
    Image.open(images['photo']).save(duplicate)
    first = asset(images['photo'], 'photo.png', 200, 150)
    second = asset(duplicate, 'duplicate.png', 200, 150, 'asset-2')
    env = {'PIGEON_PLUGIN_SEED': seed} if seed else {}
    host = mh.MockHost(plugin_dir, env=env, library=[first, second], settings={'auto': False, 'minimum': 0.3})
    try:
        ready = host.handshake()
        check(mh.contributions_match(ready, os.path.join(plugin_dir, 'plugin.json')), 'ready contributions differ')
        host.visible('index')
        host.visible('prefs')
        host.scene('prefs', lambda s: mh.find(s, 'minimum'), what='preferences')
        host.scene('index', lambda s: (mh.find(s, 'pause') or {}).get('enabled'), what='healthy index controls')
        host.change('prefs', 'workers', 1)
        host.wait(lambda: host.settings.get('workers') == 1, what='saved setting')
        host.click('index', 'pause')
        host.scene('index', lambda s: (mh.find(s, 'pause') or {}).get('label') == 'Resume', what='paused')
        host.click('index', 'pause')
        host.ping()
        host.scene('index', lambda s: '2 files indexed' in (mh.find(s, 'model') or {}).get('text', ''),
                   timeout=120, what='indexed two real images')
        host.change('prefs', 'minimum', 0.3)
        reply = host.request('search.query', {'mode_id': 'semantic', 'query': 'bright yellow square', 'limit': 1},
                             timeout=120)
        check(len(reply.get('result', {}).get('results', [])) == 1, f'expected provider result: {reply}')
        host.command('similar', [first])
        host.scene('similar-results', lambda s: mh.find(s, 'reveal-0') is not None,
                   timeout=120, what='similar duplicate')
        host.click('similar-results', 'reveal-0')
        host.wait(lambda: ('library.reveal', {'id': 'asset-2'}) in host.calls, what='reveal the similar image')
        host.library.clear()
        reply = host.request('search.query', {'mode_id': 'semantic', 'query': 'bright yellow square'}, timeout=120)
        check(reply.get('result', {}).get('results') == [], f'hidden entries must not be returned: {reply}')
        check(not host.errors, f'protocol errors: {host.errors}')
    finally:
        code = host.shutdown()
    check(code == 0, f'plugin exit {code}: {host.stderr_tail()}')
    return 'pause/resume, persisted preferences, model indexing, provider query, find similar and reveal'


CASES = {
    'ai-enlarge': case_enlarge,
    'ai-background-removal': case_background,
    'ai-object-removal': case_object_removal,
    'ai-image-expand': case_expand,
    'ai-image-expand-diffusion': case_diffusion,
    'semantic-search': case_semantic,
}


def run_case(pid, plugin_dir, seed, scratch):
    """Run the UI case for one plugin. Returns a short description of what was checked."""
    return CASES[pid](plugin_dir, seed, scratch)
