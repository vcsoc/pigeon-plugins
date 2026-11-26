"""Standalone runtime shared by every Pigeon editor plugin (identical copy in each plugin folder;
edit shared/plugin_runtime.py and run scripts/sync_runtime.py).

Standard library only, so it works before any dependency is installed. A plugin's worker.py
registers its operations with `main(...)`; this module provides everything else a plugin needs to
run without Pigeon: manifest access, model provisioning and verification, a request/reply protocol
that is a superset of Pigeon's worker protocol v1, health checks, a self-test and error reporting.

Reply contract (one JSON object on stdout, nothing else):
  success  {"version": 1, "ok": true, ...optional info...}                    exit 0
  failure  {"version": 1, "ok": false, "error": "short human-readable cause"}  exit 1
Diagnostics go to stderr and to <data>/last-error.log.
"""
import hashlib
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import time
import traceback
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.environ.get('PIGEON_PLUGIN_DATA') or os.path.join(HERE, '.data')
MODELS = os.path.join(DATA, 'models')
BUNDLED = os.path.join(HERE, 'model')
DEFAULT_MAX_PIXELS = 40_000_000     # Pigeon rejects larger results; a request may override it
IMPORT_NAMES = {'Pillow': 'PIL', 'opencv-python': 'cv2', 'sentence-transformers': 'sentence_transformers'}


class PluginError(Exception):
    """An expected, user-presentable failure (bad request, missing model, limit exceeded)."""


def log(message):
    print(message, file=sys.stderr, flush=True)


def manifest():
    with open(os.path.join(HERE, 'plugin.json'), encoding='utf-8') as f:
        return json.load(f)


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


# --------------------------------------------------------------------------- models

def _model_entry(name):
    for entry in manifest().get('models', []):
        if entry.get('file') == name:
            return entry
    return None


def resolve_model(request, name, single=False):
    """Find a model file. Order: the request's `model` (a directory containing `name`, or, when
    `single`, the file itself), the plugin's bundled model/ folder, then the downloaded models."""
    override = request.get('model')
    if override:
        if os.path.isdir(override) and os.path.isfile(os.path.join(override, name)):
            return os.path.join(override, name)
        if single and os.path.isfile(override):
            return override
    for base in (BUNDLED, MODELS):
        path = os.path.join(base, name)
        if os.path.isfile(path):
            entry = _model_entry(name)
            if entry and entry.get('bytes') and os.path.getsize(path) != entry['bytes']:
                raise PluginError(f'model {name} is incomplete or corrupt; run the plugin with --setup')
            return path
    raise PluginError(f'model {name} is not installed; run the plugin with --setup')


def _allowed(url, hosts):
    parts = urllib.parse.urlparse(url)
    return parts.scheme == 'https' and any(
        parts.hostname == h or (parts.hostname or '').endswith('.' + h) for h in hosts)


class _Redirects(urllib.request.HTTPRedirectHandler):
    def __init__(self, hosts):
        self.hosts = hosts

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not _allowed(newurl, self.hosts):
            raise PluginError('download redirected to a host the plugin does not declare')
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _download(entry, hosts):
    dest = os.path.join(MODELS, entry['file'])
    os.makedirs(os.path.dirname(dest), exist_ok=True)   # model files may live in subfolders
    part = dest + '.part'
    if not _allowed(entry['url'], hosts):
        raise PluginError(f"{entry['file']}: host is not declared in the plugin's network list")
    have = os.path.getsize(part) if os.path.exists(part) else 0
    if have >= entry['bytes']:
        os.remove(part)
        have = 0
    headers = {'User-Agent': 'pigeon-plugin/1'}
    if have:
        headers['Range'] = f'bytes={have}-'
    opener = urllib.request.build_opener(_Redirects(hosts))
    with opener.open(urllib.request.Request(entry['url'], headers=headers), timeout=60) as response:
        if have and response.status != 206:
            have = 0                                   # server ignored Range: start over
        total, last = have, -1
        with open(part, 'ab' if have else 'wb') as out:
            while True:
                chunk = response.read(1 << 20)
                if not chunk:
                    break
                out.write(chunk)
                total += len(chunk)
                if total > entry['bytes']:
                    break
                pct = total * 100 // entry['bytes']
                if pct // 5 != last // 5:
                    last = pct
                    log(f"downloading {entry['file']}: {pct}%")
    if os.path.getsize(part) != entry['bytes'] or sha256_file(part) != entry['sha256']:
        os.remove(part)
        raise PluginError(f"{entry['file']} failed verification (size or SHA-256 mismatch); discarded")
    os.replace(part, dest)


def model_status(entry, verify=False):
    path = os.path.join(MODELS, entry['file'])
    if not os.path.isfile(path):
        return 'missing'
    if os.path.getsize(path) != entry['bytes']:
        return 'size mismatch'
    if verify and sha256_file(path) != entry['sha256']:
        return 'hash mismatch'
    return 'ok'


def setup(verify=False):
    """Download and verify every model listed in plugin.json. Idempotent and resumable.
    PIGEON_PLUGIN_SEED may name a folder holding already-downloaded copies (used offline / in tests)."""
    m = manifest()
    os.makedirs(MODELS, exist_ok=True)
    result = {}
    for entry in m.get('models', []):
        if model_status(entry, verify=True) == 'ok':
            result[entry['file']] = 'ok'
            continue
        seed = os.path.join(os.environ.get('PIGEON_PLUGIN_SEED', ''), entry['file'])
        if os.environ.get('PIGEON_PLUGIN_SEED') and os.path.isfile(seed) and sha256_file(seed) == entry['sha256']:
            os.makedirs(os.path.dirname(os.path.join(MODELS, entry['file'])), exist_ok=True)
            shutil.copyfile(seed, os.path.join(MODELS, entry['file']))
            log(f"{entry['file']}: copied from seed folder")
        else:
            _download(entry, m.get('network', []))
        result[entry['file']] = model_status(entry, verify=True)
    return result


# --------------------------------------------------------------------------- health

def health(verify=False, device=None):
    m = manifest()
    deps = {}
    for requirement in m['worker'].get('requires', []):
        deps[requirement] = importlib.util.find_spec(IMPORT_NAMES.get(requirement, requirement)) is not None
    models = {}
    for entry in m.get('models', []):
        models[entry['file']] = model_status(entry, verify=verify)
    for item in m.get('files', []):
        if item.get('role') == 'model':
            models[item['path']] = 'ok' if os.path.isfile(os.path.join(HERE, item['path'])) else 'missing'
    ready = all(deps.values()) and all(v == 'ok' for v in models.values())
    info = {'id': m['id'], 'version': m['version'], 'ready': ready, 'python': sys.version.split()[0],
            'dependencies': deps, 'models': models}
    if device and all(deps.values()):
        try:
            info['device'] = device()
        except Exception as error:      # a broken GPU stack must not make health itself fail
            info['device'] = f'unavailable ({type(error).__name__})'
    return info


# --------------------------------------------------------------------------- requests

def max_pixels(request):
    try:
        return int(request.get('max_pixels', DEFAULT_MAX_PIXELS))
    except (TypeError, ValueError):
        raise PluginError('invalid max_pixels')


def require_paths(request, *keys):
    for key in keys:
        value = request.get(key)
        if not isinstance(value, str) or not value:
            raise PluginError(f'request field "{key}" is required')
    if not os.path.isfile(request['input']):
        raise PluginError('input image not found')


def _reply(payload, code=0):
    sys.stdout.write(json.dumps(payload, separators=(',', ':')) + '\n')
    sys.stdout.flush()
    sys.exit(code)


def _fail(message):
    _reply({'version': 1, 'ok': False, 'error': str(message)[:500]}, 1)


def main(operations, self_test=None, device=None):
    """Entry point for worker.py.

    operations: {"enlarge": fn(request) -> dict | None, ...}
    self_test:  fn(tmpdir) -> request dict for a tiny synthetic job (optional)
    device:     fn() -> str describing the compute device (optional, shown by --health)
    """
    argv = sys.argv[1:]
    started = time.time()
    try:
        if '--version' in argv:
            m = manifest()
            _reply({'version': 1, 'ok': True, 'id': m['id'], 'plugin_version': m['version']})
        if '--setup' in argv:
            models = setup()
            _reply({'version': 1, 'ok': True, 'ready': True, 'models': models})
        if '--health' in argv:
            info = health(verify='--verify' in argv, device=device)
            _reply({'version': 1, 'ok': info['ready'], **info}, 0 if info['ready'] else 1)
        if '--self-test' in argv:
            if not self_test:
                raise PluginError('this plugin has no self-test')
            with tempfile.TemporaryDirectory(prefix='pigeon-selftest-') as tmp:
                request = {'version': 1, **self_test(tmp)}
                info = _dispatch(operations, request) or {}
                size = None
                if request.get('output'):               # image plugins: the result must be a real PNG
                    from PIL import Image
                    with Image.open(request['output']) as produced:
                        size = list(produced.size)
            _reply({'version': 1, 'ok': True, 'self_test': 'passed', **({'output_size': size} if size else {}),
                    'seconds': round(time.time() - started, 2), **info})

        try:
            request = json.load(sys.stdin)
        except ValueError:
            raise PluginError('request is not valid JSON')
        if not isinstance(request, dict) or request.get('version') != 1:
            raise PluginError('unsupported protocol version')
        if request.get('operation') == 'health':        # Pigeon's dependency probe: no image, no model
            info = health(device=device)
            if not all(info['dependencies'].values()):
                raise PluginError('missing Python packages: ' + ', '.join(k for k, v in info['dependencies'].items() if not v))
            _reply({'version': 1, 'ok': True})
        info = _dispatch(operations, request) or {}
        _reply({'version': 1, 'ok': True, **info, 'seconds': round(time.time() - started, 2)})
    except SystemExit:
        raise
    except (PluginError, ValueError, KeyError) as error:
        _fail(error if isinstance(error, PluginError) else f'invalid request or input ({error})')
    except MemoryError:
        _fail('out of memory: the image is too large for this computer')
    except BaseException as error:                      # keep a trace so the plugin can be fixed fast
        text = traceback.format_exc()
        log(text)
        try:
            os.makedirs(DATA, exist_ok=True)
            with open(os.path.join(DATA, 'last-error.log'), 'w', encoding='utf-8') as f:
                f.write(text)
        except OSError:
            pass
        _fail(f'internal error ({type(error).__name__}); see last-error.log in the plugin folder')


def progress(request, fraction, text=''):
    """Report progress for a long operation. Written to stderr as `::progress {json}`, which the
    plugin's UI reads; ordinary diagnostics on stderr are shown only in the plugin log."""
    try:
        value = None if fraction is None else max(0.0, min(1.0, float(fraction)))
    except (TypeError, ValueError):
        value = None
    print('::progress ' + json.dumps({'fraction': value, 'text': str(text)[:200]}), file=sys.stderr, flush=True)


def image_info(request):
    """Generic operation `info`: the size of an image (reads the header only)."""
    from PIL import Image
    require_paths(request, 'input')
    with Image.open(request['input']) as opened:
        width, height = opened.size
    return {'width': width, 'height': height}


def image_preview(request):
    """Generic operation `preview`: a downscaled PNG copy of the input for on-screen display only."""
    from PIL import Image
    require_paths(request, 'input', 'output')
    try:
        side = int(request.get('max_side', 960))
    except (TypeError, ValueError):
        raise PluginError('invalid max_side')
    side = max(64, min(side, 2048))
    with Image.open(request['input']) as opened:
        image = opened.convert('RGBA' if 'A' in opened.getbands() or opened.mode in ('P', 'LA') else 'RGB')
    image.thumbnail((side, side), Image.Resampling.LANCZOS)
    image.save(request['output'], format='PNG')
    return {'output_size': list(image.size)}


BUILTIN_OPERATIONS = {'info': image_info, 'preview': image_preview}


def _dispatch(operations, request):
    operation = request.get('operation')
    if operation in BUILTIN_OPERATIONS and operation not in operations:
        return BUILTIN_OPERATIONS[operation](request)
    if operation not in operations:
        raise PluginError(f'unsupported operation: {operation}')
    return operations[operation](request)
