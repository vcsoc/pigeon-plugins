"""Shared UI code for the Pigeon plugins in this repository (protocol v2).

Identical copy in every plugin folder: edit shared/ui_support.py, then run scripts/sync_runtime.py.
Built on the vendored SDK (`pigeon_plugin/`, copied by sdk/sync_sdk.py): transport, handshake, ping,
coalescing, service calls and resources all come from there. This module adds:

  * scene helpers: thin wrappers that keep the call shape used by the plugins' ui.py and return
    `pigeon_plugin` nodes (so every scene is validated by the SDK before it is sent);
  * child runs: the plugin's protocol-v1 `run` launcher runs heavy work (models, packages) as a child
    process. This process never imports those packages, so a broken model cannot take the UI down,
    and a child can be killed on cancel. `::progress {json}` lines on stderr report progress;
  * App: a small base for the threads and state every plugin UI needs (lock, spawn, render);
  * ImageTool and ExpandTool: the shared flow for image operations (options, consent, run, progress,
    before/after preview, Accept/Discard as a derivative through library.import mode "derive").

Plugins never import this module's private helpers from another plugin; each folder is standalone.
"""
import json
import os
import struct
import subprocess
import sys
import tempfile
import threading
import traceback
import zlib

from pigeon_plugin import DrawList, Node, PluginSpec, Plugin, ServiceError, Size, draw
from pigeon_plugin import scene as S

MAX_OUTPUT_PIXELS = 40_000_000     # Pigeon's limit for generated images
MAX_INPUT_PIXELS = 16_000_000      # Pigeon's limit for the editing tools
HERE = os.path.dirname(os.path.abspath(__file__))


def log(*parts):
    print('plugin:', *parts, file=sys.stderr, flush=True)


# --------------------------------------------------------------------------- scene helpers

def _node(n, props):
    for key, value in props.items():
        if value is not None:
            n.set(key, value)
    return n


def column(id, children=(), **props):
    return _node(S.column(id).children_from(list(children)), props)


def row(id, children=(), **props):
    return _node(S.row(id).children_from(list(children)), props)


def text(id, value, style='body', **props):
    return _node(S.text(id, value, style), {'wrap': True, **props})


def button(id, label, variant='default', enabled=True, **props):
    n = S.button(id, label, variant=variant).enabled(enabled).on('click')
    return _node(n, props)


def progress_bar(id, value=None):
    return S.progress(id, value)


def number_input(id, value, rev, minimum, maximum, step=1, unit=None, precision=0):
    n = S.number_input(id, value, rev)
    return _node(n, {'min': minimum, 'max': maximum, 'step': step, 'unit': unit, 'precision': precision})


def slider(id, value, rev, minimum, maximum, step):
    return _node(S.slider(id, value, minimum, maximum, rev), {'step': step})


def text_input(id, value, rev, placeholder=None, max_len=None, multiline=False):
    n = S.text_input(id, value, rev).commit('change')
    return _node(n, {'placeholder': placeholder, 'max_len': max_len, 'multiline': multiline or None})


def checkbox(id, label, checked):
    return S.checkbox(id, label, bool(checked))


def segmented(id, options, value):
    return S.segmented(id, list(options), value)


def canvas(id, width, height, ops, frame=0, interactive=None):
    n = S.canvas(id, Size.px(width), Size.px(height))
    drawing = DrawList(clear='#1b1d22').add(*ops)
    n.draw(drawing, frame)
    if interactive:
        n.set('interactive', interactive)
    return n


def image(id, resource, width, height, fit='contain'):
    return S.image(id, resource, fit).width(Size.px(width)).height(Size.px(height))


def draw_rect(x, y, w, h, fill=None, stroke=None, stroke_w=1.0):
    return draw.rect(x, y, w, h, fill=fill, stroke=stroke, stroke_w=stroke_w)


def draw_image(rid, x, y, w, h):
    return draw.image(rid, {'x': x, 'y': y, 'w': w, 'h': h})


def draw_text(x, y, value, size=12, color='#d8dbe2'):
    return draw.text(x, y, value, size=size, color=color)


def fit_box(width, height, max_w, max_h):
    scale = min(max_w / max(width, 1), max_h / max(height, 1), 1.0)
    return max(1, round(width * scale)), max(1, round(height * scale))


# --------------------------------------------------------------------------- files

def write_mask_png(path, width, height, rects):
    """8-bit grayscale PNG, nonzero = remove. Rects are applied in order: mark 255, erase 0."""
    stride = width + 1
    raw = bytearray(stride * height)
    for rect in rects:
        x0, y0 = rect['min']
        x1, y1 = rect['max']
        value = 0 if rect.get('erase') else 255
        for y in range(y0, y1):
            start = y * stride + 1 + x0
            raw[start:start + (x1 - x0)] = bytes([value]) * (x1 - x0)

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xFFFFFFFF)

    header = struct.pack('>IIBBBBB', width, height, 8, 0, 0, 0, 0)
    with open(path, 'wb') as f:
        f.write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(bytes(raw), 6))
                + chunk(b'IEND', b''))


# --------------------------------------------------------------------------- child runs

class OperationError(Exception):
    """A child run failed; the message is safe to show to the user."""


def launcher(folder):
    return os.path.join(folder, 'run')


def run_child(folder, request, on_progress=None, on_process=None, timeout=None):
    """Run one protocol-v1 request in the plugin's own launcher and return its reply dict."""
    process = subprocess.Popen(['sh', launcher(folder)], cwd=folder, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8')
    if on_process:
        on_process(process)
    tail = []

    def drain():
        for line in process.stderr:
            if line.startswith('::progress '):
                if on_progress:
                    try:
                        data = json.loads(line[len('::progress '):])
                        on_progress(data.get('fraction'), str(data.get('text', ''))[:200])
                    except (ValueError, TypeError):
                        pass
            else:
                tail.append(line.rstrip())
                del tail[:-30]

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    # The child returns one small JSON reply. Time out before reading stdout so a
    # stalled child cannot hold the worker forever.
    try:
        with process.stdin:
            process.stdin.write(json.dumps(request) + '\n')
    except BrokenPipeError:
        pass
    try:
        process.wait(timeout=timeout if timeout is not None else 900)
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.wait(timeout=5)
        raise OperationError('The operation timed out.') from error
    finally:
        reader.join(timeout=1)
    with process.stdout:
        out = process.stdout.read()
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        reply = json.loads(lines[-1]) if lines else {}
    except ValueError:
        reply = {}
    if not isinstance(reply, dict):
        reply = {}
    if process.returncode != 0 or reply.get('ok') is not True:
        message = reply.get('error') or ('the plugin stopped unexpectedly' if not reply else 'the plugin failed')
        raise OperationError(str(message))
    return reply


def child_health(folder):
    """`run --health` as JSON (exit 1 when not ready is still a valid answer)."""
    process = subprocess.run(['sh', launcher(folder), '--health'], cwd=folder, capture_output=True, text=True,
                             encoding='utf-8', timeout=120)
    lines = [line for line in process.stdout.splitlines() if line.strip()]
    try:
        info = json.loads(lines[-1]) if lines else {}
    except ValueError:
        info = {}
    return info if isinstance(info, dict) else {}


def run_setup(folder, on_text):
    """`run --setup` (venv, packages, models). Streams stderr lines to on_text. Raises OperationError."""
    process = subprocess.Popen(['sh', launcher(folder), '--setup'], cwd=folder, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, encoding='utf-8')
    for line in process.stderr:
        line = line.strip()
        if line:
            on_text(line[:200])
    out = process.stdout.read()
    process.wait()
    if process.returncode != 0:
        try:
            message = json.loads(out.strip().splitlines()[-1]).get('error', 'setup failed')
        except (ValueError, IndexError):
            message = 'setup failed'
        raise OperationError(str(message))


# --------------------------------------------------------------------------- app base

class App:
    """Base for a plugin UI: the SDK calls handle(ctx, msg) on one thread; state is guarded by
    self.lock. Anything that waits on the host or a child runs on a spawned thread, never in handle()."""

    def __init__(self, folder):
        self.folder = folder
        with open(os.path.join(folder, 'plugin.json'), encoding='utf-8') as f:
            self.manifest = json.load(f)
        self.lock = threading.RLock()
        self.ctx = None
        self.opened = set()
        self.session = os.environ.get('PIGEON_SESSION_DIR') or tempfile.mkdtemp(prefix='pigeon-session-')
        os.makedirs(os.path.join(self.session, 'results'), exist_ok=True)

    def handle(self, ctx, msg):
        self.ctx = ctx
        with self.lock:
            try:
                self.on_message(msg)
            except Exception:
                log(traceback.format_exc())

    def on_message(self, msg):
        kind = msg.get('t')
        if kind in ('visibility', 'ui.visible'):
            instance = msg.get('instance')
            if not isinstance(instance, str):
                return
            if msg.get('visible', True) is not False:
                self.opened.add(instance)
                self.on_visible(instance)
                self.render(instance)
            else:
                self.opened.discard(instance)

    def slot_id(self, instance):
        prefix = self.manifest['id'] + '/'
        return instance.removeprefix(prefix).split('/', 1)[0]

    def on_visible(self, instance):
        pass

    def close(self):
        pass

    def spawn(self, fn, *args):
        threading.Thread(target=self._guard, args=(fn, args), daemon=True).start()

    @staticmethod
    def _guard(fn, args):
        try:
            fn(*args)
        except ServiceError as error:
            log('host service error:', error)
        except Exception:
            log(traceback.format_exc())

    def call(self, method, params=None):
        return self.ctx.call(method, params or {})

    def setting(self, key, default=None):
        try:
            value = self.ctx.settings_get(key)
        except ServiceError:
            return default
        return default if value is None else value

    def put_setting(self, key, value):
        self.ctx.settings_set(key, value)

    def toast(self, level, text_value):
        self.spawn(self._toast, level, text_value)

    def _toast(self, level, text_value):
        try:
            self.ctx.toast(level, text_value)
        except ServiceError:
            pass

    def render(self, instance):
        pass

    def render_all(self):
        for instance in list(self.opened):
            self.render(instance)

    def publish(self, instance, node):
        self.ctx.set_scene(instance, node)


def run_app(app_class, folder):
    """Entry point for `worker.py --plugin`: handshake, then the SDK loop until shutdown."""
    with open(os.path.join(folder, 'plugin.json'), encoding='utf-8') as f:
        manifest = json.load(f)
    plugin = Plugin.stdio(PluginSpec(manifest['id'], manifest['version'], manifest['ui']['contributions']))
    app = app_class(folder)
    app.ctx = plugin.ctx
    try:
        plugin.run(app.handle)
    finally:
        app.close()
    return 0


# --------------------------------------------------------------------------- image tool

class ImageTool(App):
    """Inspector panel and modal for one image operation.

    Subclasses set OPERATION (protocol-v1 operation), ACTION_ID (the asset.action that opens the tool),
    OPTIONS/DEFAULTS and override options_nodes(), extra_nodes(), validate(), build_request().
    """
    OPERATION = None
    ACTION_ID = 'open'
    PANEL = 'panel'
    MODAL = 'tool'
    TITLE = 'Image tool'
    RUN_LABEL = 'Run'
    CONSENT_KEY = None
    CONSENT_TITLE = ''
    CONSENT_LINES = ()
    OPTIONS = ()
    DEFAULTS = {}
    EXTRA_ACTIONS = {}          # node id -> method name, for buttons specific to a tool
    CANVAS_IDS = ()
    PREVIEW_SIDE = 960
    KINDS = ('image',)

    def __init__(self, folder):
        super().__init__(folder)
        self.opts = dict(self.DEFAULTS)
        self.revs = {}
        self.target = None
        self.loaded = False
        self.ready = None
        self.device = None
        self.health_error = ''
        self.consented = None
        self.setup_text = ''
        self.setup_running = False
        self.phase = 'idle'         # idle | consent | running | saving | review | failed
        self.preview_side = 'after'
        self.status = ''
        self.progress_value = None
        self.error = ''
        self.job = None
        self.host_job = None
        self.process = None
        self.cancelled = False
        self.result = None
        self.counter = 0

    # ---- host events (lock held)
    def on_message(self, msg):
        kind = msg.get('t')
        if kind in ('visibility', 'ui.visible'):
            super().on_message(msg)
            if msg.get('visible', True) is not False:
                self.ensure_loaded()
        elif kind == 'ui.event':
            self.on_ui_event(msg)
        elif kind == 'command' and msg.get('command') == self.ACTION_ID:
            self.spawn(self.open_for_command, msg.get('context') or {})
        elif kind == 'selection.changed':
            self.spawn(self.load_selection)
        elif kind == 'cancel' and msg.get('job') in (self.job, self.host_job):
            self.cancel_job()
        elif kind == 'shutdown':
            self.cancel_job()

    def ensure_loaded(self):
        if not self.loaded:
            self.loaded = True
            self.spawn(self.refresh_health)
            self.spawn(self.load_consent)
            self.spawn(self.subscribe_selection)

    def subscribe_selection(self):
        try:
            self.call('selection.subscribe')
        except ServiceError:
            pass
        self.load_selection()

    def close(self):
        self.cancel_job()

    # ---- host interaction (worker threads)
    def load_consent(self):
        value = bool(self.setting(self.CONSENT_KEY, False)) if self.CONSENT_KEY else True
        with self.lock:
            self.consented = value
            self.render_all()

    def load_selection(self):
        with self.lock:
            previous = self.target
        assets = self.selection_assets({})
        with self.lock:
            if self.target is previous and self.phase in ('idle', 'failed') and not any(
                    self.slot_id(instance) == self.MODAL for instance in self.opened):
                self.set_target(assets[0] if len(assets) == 1 else None)

    def selection_assets(self, context):
        items = [item for item in (context.get('assets') or context.get('selection') or [])
                 if isinstance(item, dict) and item.get('id') and item.get('path')]
        if not items:
            try:
                reply = self.call('selection.get')
            except ServiceError:
                return []
            items = reply.get('assets') or reply.get('items') or []
            if not items and reply.get('ids'):
                kinds = reply.get('kinds') or []
                paths = reply.get('paths') or []
                items = [{'id': ident, 'kind': kinds[i] if i < len(kinds) else 'image',
                          'path': paths[i] if i < len(paths) else None} for i, ident in enumerate(reply['ids'])]
        return [item for item in items if isinstance(item, dict) and item.get('id') and item.get('path')
                and item.get('kind', 'image') in self.KINDS]

    def open_for_command(self, context):
        assets = self.selection_assets(context)
        with self.lock:
            if not assets or self.phase in ('running', 'saving', 'review', 'consent'):
                return
            self.set_target(assets[0])
        try:
            self.call('window.open_modal', {'contribution': self.MODAL, 'instance': self.MODAL, 'props': {}})
        except ServiceError as error:
            with self.lock:
                self.error = f'Could not open the tool window: {error.message}'
                self.render_all()

    def refresh_health(self):
        try:
            info = child_health(self.folder)
        except (OSError, subprocess.TimeoutExpired) as error:
            info = {'ready': False, 'error': str(error)[:200]}
        with self.lock:
            self.ready = bool(info.get('ready'))
            self.device = info.get('device')
            self.health_error = '' if self.ready else str(info.get('error') or 'Not set up yet.')
            if self.ready and self.target and 'source_size' not in self.target:
                self.spawn(self.prepare_target, self.target)
            self.render_all()

    def prepare_target(self, target):
        """Read the image size and publish a downscaled 'source' preview."""
        try:
            info = run_child(self.folder, {'version': 1, 'operation': 'info', 'input': target['path']}, timeout=60)
            if info['width'] * info['height'] > MAX_INPUT_PIXELS:
                raise OperationError('Images above 16 megapixels are not supported.')
            fd, preview = tempfile.mkstemp(suffix='.png', dir=os.path.join(self.session, 'results'))
            os.close(fd)
            try:
                dimensions = run_child(self.folder, {'version': 1, 'operation': 'preview', 'input': target['path'],
                                                     'output': preview, 'max_side': self.PREVIEW_SIDE}, timeout=60)
                with open(preview, 'rb') as f:
                    data = f.read()
            finally:
                os.unlink(preview)
        except (OperationError, OSError) as error:
            with self.lock:
                if self.target is target:
                    self.error = str(error)
                    self.render_all()
            return
        source_rid = f"source-{target['generation']}"
        self.ctx.resources.put_encoded(source_rid, 'png', data, *dimensions['output_size'])
        with self.lock:
            if self.target is not target:
                self.ctx.resources.drop(source_rid)
                return
            self.target.update(w=info['width'], h=info['height'], source_size=dimensions['output_size'])
            self.render_all()

    def start_setup(self):
        with self.lock:
            if self.setup_running:
                return
            self.setup_running = True
            self.setup_text = 'Starting setup...'
            self.render_all()
        self.spawn(self._setup)

    def _setup(self):
        def on_text(line):
            with self.lock:
                self.setup_text = line
                self.render_all()
        try:
            run_setup(self.folder, on_text)
            with self.lock:
                self.setup_text = 'Setup finished.'
        except OperationError as error:
            with self.lock:
                self.setup_text = f'Setup failed: {error}'
        finally:
            with self.lock:
                self.setup_running = False
            self.refresh_health()

    # ---- state transitions (lock held)
    def set_target(self, asset):
        if self.target and asset and self.target.get('id') == str(asset['id']) and self.target['path'] == asset['path']:
            return
        if self.target and 'source_size' in self.target:
            self.ctx.resources.drop(self.source_rid())
        if asset is None:
            self.target = None
            self.phase = 'idle'
            self.result = None
            self.error = ''
            self.render_all()
            return
        self.counter += 1
        self.target = {'id': str(asset['id']), 'name': str(asset.get('name') or asset['id']),
                       'path': asset['path'], 'kind': asset.get('kind', 'image'), 'generation': self.counter}
        self.phase = 'idle'
        self.result = None
        self.error = ''
        self.rects_reset()
        if self.ready:
            self.spawn(self.prepare_target, self.target)
        self.render_all()

    def source_rid(self):
        return f"source-{self.target['generation']}"

    def rects_reset(self):
        pass

    def set_option(self, name, value, rev):
        stored = self.revs.get(name, 0)
        if rev is not None and rev < stored:
            return                              # stale edit
        self.opts[name] = value
        self.revs[name] = stored + 1 if rev is None else rev

    def on_ui_event(self, msg):
        name, kind = msg.get('node'), msg.get('type')
        if msg.get('instance') not in self.opened:
            return
        if self.phase in ('running', 'saving') and name != 'cancel':
            return
        if name == 'preview' and kind == 'change' and msg.get('value') in ('before', 'after'):
            self.preview_side = msg['value']
        elif name in self.OPTIONS and kind == 'change':
            self.set_option(name, msg.get('value'), msg.get('rev'))
        elif name in self.CANVAS_IDS:
            self.on_canvas(msg)
        elif kind in ('click', 'change'):
            action = {**ACTIONS, **self.EXTRA_ACTIONS}.get(name)
            if action:
                getattr(self, action)()
        self.render_all()

    def on_canvas(self, msg):
        pass

    def validate(self):
        if not self.target:
            return 'Choose one image in the library.'
        if self.ready is not True:
            return 'Set up this plugin first.'
        if not self.target.get('w') or not self.target.get('h'):
            return 'Reading the image size...'
        if self.target['w'] * self.target['h'] > MAX_INPUT_PIXELS:
            return 'Images above 16 megapixels are not supported.'
        return None

    def request_run(self):
        problem = self.validate()
        if problem:
            self.error = problem
            return
        if self.phase not in ('idle', 'failed') or self.consented is None:
            return
        self.error = ''
        if self.consented:
            self.launch()
        else:
            self.phase = 'consent'

    def consent_yes(self):
        if self.phase != 'consent':
            return
        self.consented = True
        self.spawn(self._store_consent)
        self.launch()

    def _store_consent(self):
        try:
            self.put_setting(self.CONSENT_KEY, True)
        except ServiceError:
            pass

    def consent_no(self):
        self.phase = 'idle'

    def launch(self):
        self.counter += 1
        self.job = f'{self.OPERATION}-{self.counter}'
        self.phase = 'running'
        self.progress_value = None
        self.status = 'Starting...'
        self.error = ''
        self.cancelled = False
        self.result = None
        self.spawn(self._job, dict(self.target), dict(self.opts), self.snapshot_rects(), self.job)

    def snapshot_rects(self):
        return None

    def build_request(self, target, opts, rects, job):
        return {'version': 1, 'operation': self.OPERATION, 'max_pixels': MAX_OUTPUT_PIXELS}

    def _job(self, target, opts, rects, job):
        output = os.path.join(self.session, 'results', f'{job}.png')
        try:
            host_job = self.ctx.progress_start(self.TITLE, cancellable=True)
        except (ServiceError, KeyError):
            host_job = None
        with self.lock:
            self.host_job = host_job

        def on_progress(fraction, text_value):
            with self.lock:
                if self.job != job:
                    return
                self.progress_value = None if fraction is None else max(0.0, min(1.0, float(fraction)))
                self.status = text_value or self.status
                self.render_all()
            if host_job is not None and fraction is not None:
                try:
                    self.ctx.progress_update(host_job, self.progress_value, text_value or None)
                except ServiceError:
                    pass

        outcome = None
        try:
            request = self.build_request(target, opts, rects, job)
            request['input'] = target['path']
            request['output'] = output
            reply = run_child(self.folder, request, on_progress=on_progress,
                              on_process=lambda process: self._set_process(job, process))
            if self.cancelled:
                raise OperationError('Cancelled.')
            preview = os.path.join(self.session, 'results', f'{job}-preview.png')
            dimensions = run_child(self.folder, {'version': 1, 'operation': 'preview', 'input': output,
                                                 'output': preview, 'max_side': self.PREVIEW_SIDE}, timeout=60,
                                   on_process=lambda process: self._set_process(job, process))
            with open(preview, 'rb') as f:
                data = f.read()
            os.unlink(preview)
            size = reply.get('output_size') or dimensions['output_size']
            self.ctx.resources.put_encoded('after', 'png', data, *dimensions['output_size'])
            with self.lock:
                if self.cancelled:
                    raise OperationError('Cancelled.')
                self.result = {'job': job, 'path': output, 'target': target, 'size': size,
                               'device': reply.get('device')}
                self.phase = 'review'
                self.preview_side = 'after'
                self.status = ''
                self.progress_value = None
                outcome = 'review'
                self.render_all()
        except OperationError as error:
            outcome = str(error)
        except Exception:
            log(traceback.format_exc())
            outcome = 'Internal error: see the plugin log in the plugin folder.'
        finally:
            with self.lock:
                if self.job == job:
                    self.process = None
                    self.job = None
                    if outcome not in (None, 'review'):
                        self.phase = 'failed'
                        self.error = 'Cancelled.' if self.cancelled else outcome
                        self.render_all()
            if host_job is not None:
                try:
                    self.ctx.progress_end(host_job, ok=outcome == 'review')
                except ServiceError:
                    pass

    def _set_process(self, job, process):
        with self.lock:
            if self.job == job:
                self.process = process
            if self.cancelled:
                process.kill()

    def cancel_job(self):
        with self.lock:
            if self.job is None:
                return
            self.cancelled = True
            self.status = 'Cancelling...'
            if self.process is not None:
                try:
                    self.process.kill()
                except OSError:
                    pass
            self.render_all()

    def accept(self):
        if not self.result or self.phase != 'review':
            return
        self.phase = 'saving'
        self.status = 'Saving a derivative...'
        self.spawn(self._accept_job, dict(self.result))

    def _accept_job(self, result):
        target = result['target']
        try:
            self.call('library.import', {'paths': [result['path']], 'mode': 'derive', 'derive_of': target['id']})
        except ServiceError as error:
            with self.lock:
                self.phase = 'review'
                self.error = f'Could not save the result: {error.message}'
                self.render_all()
            return
        with self.lock:
            self.phase = 'idle'
            self.result = None
            self.status = f"Saved as a derivative of {target['name']}."
            self.render_all()
        self.toast('success', f"Saved a new derivative of {target['name']}.")

    def discard(self):
        if self.result:
            try:
                os.remove(self.result['path'])
            except OSError:
                pass
        self.result = None
        self.phase = 'idle'
        self.error = ''
        self.render_all()

    def dismiss(self):
        self.phase = 'idle'
        self.error = ''
        self.render_all()

    # ---- rendering (lock held)
    def render(self, instance):
        if self.slot_id(instance) in (self.PANEL, self.MODAL):
            self.publish(instance, self.scene())

    def scene(self):
        children = [text('title', self.TITLE, style='heading')]
        if self.target:
            size = f"  {self.target['w']} x {self.target['h']}" if self.target.get('w') else ''
            children.append(text('target', self.target['name'] + size, style='caption'))
        else:
            children.append(text('hint', 'Select one image in the library to use this tool.', style='caption'))
        if self.ready is None:
            children.append(text('checking', 'Checking this plugin...', style='caption'))
        elif self.ready is False:
            children.append(self.setup_card())
        elif self.phase == 'consent':
            children.append(self.consent_card())
        elif self.phase == 'running':
            children.append(self.running_card())
        elif self.phase in ('review', 'saving'):
            children.append(self.review_card())
        else:
            children.extend(self.form_nodes())
        if self.error:
            children.append(text('error', self.error, style='body', color='danger'))
        if self.status and self.phase != 'running':
            children.append(text('status', self.status, style='caption'))
        if self.device and self.ready:
            children.append(text('device', f'Device: {self.device}', style='caption'))
        return column('root', children, gap=10, padding=12)

    def setup_card(self):
        lines = [text('setup-text', 'This tool needs its Python packages and model files, downloaded once '
                                    'from their publishers. Images are processed on this computer.', style='body')]
        if self.health_error and not self.setup_running:
            lines.append(text('setup-reason', self.health_error[:300], style='caption'))
        if self.setup_text:
            lines.append(text('setup-progress', self.setup_text, style='caption'))
        lines.append(button('setup', 'Set up', variant='primary', enabled=not self.setup_running))
        return column('setup-card', lines, gap=8)

    def consent_card(self):
        lines = [text('consent-title', self.CONSENT_TITLE, style='title')]
        lines.extend(text(f'consent-{index}', line, style='body') for index, line in enumerate(self.CONSENT_LINES))
        lines.append(row('consent-actions', [button('consent-yes', 'Allow and run', variant='primary'),
                                             button('consent-no', 'Not now')], gap=8))
        return column('consent', lines, gap=8, padding=10)

    def running_card(self):
        return column('running', [
            progress_bar('progress', self.progress_value),
            text('status', self.status or 'Working...', style='caption'),
            button('cancel', 'Cancel', variant='danger'),
        ], gap=8)

    def review_card(self):
        if self.phase == 'saving' or not self.result:
            return column('saving-card', [text('saving', self.status or 'Saving...', style='caption')], gap=8)
        size = self.result.get('size') or [None, None]
        rows = [
            segmented('preview', [('before', 'Before'), ('after', 'After')], self.preview_side),
            image('preview-image', self.source_rid() if self.preview_side == 'before' else 'after', 300, 240),
            text('result-size', f'Result: {size[0]} x {size[1]} pixels', style='caption'),
            row('review-actions', [button('accept', 'Accept and save', variant='primary'),
                                   button('discard', 'Discard')], gap=8),
        ]
        return column('review', rows, gap=8)

    def form_nodes(self):
        nodes = list(self.options_nodes())
        nodes.extend(self.extra_nodes())
        problem = self.validate()
        if problem and self.target and self.ready is True and not self.error:
            nodes.append(text('validation', problem, style='caption', color='warning'))
        enabled = self.target is not None and self.ready is True and self.consented is not None and problem is None
        nodes.append(button('run', self.RUN_LABEL, variant='primary', enabled=enabled))
        return nodes

    def options_nodes(self):
        return []

    def extra_nodes(self):
        return []


class ExpandTool(ImageTool):
    """Canvas expansion: four margins in pixels; the operation paints the new border.

    Limits follow Pigeon's editor: each side at most 8000 px, the result at most 16384 px on a side and
    40 megapixels. The live preview shows the enlarged canvas with the original placed on it."""
    OPTIONS = ('left', 'top', 'right', 'bottom')
    DEFAULTS = {'left': 0, 'top': 256, 'right': 0, 'bottom': 256}
    PREVIEW_BOX = (360, 240)
    MAX_SIDE_PX = 8000
    MAX_RESULT_SIDE = 16384

    def margins(self):
        try:
            return [int(self.opts[key]) for key in ('left', 'top', 'right', 'bottom')]
        except (TypeError, ValueError, KeyError):
            return None

    def result_size(self):
        margins = self.margins()
        if not self.target or not self.target.get('w') or margins is None:
            return None
        left, top, right, bottom = margins
        return self.target['w'] + left + right, self.target['h'] + top + bottom

    def options_nodes(self):
        def field(key):
            return number_input(key, self.opts[key], self.revs.get(key, 0), 0, self.MAX_SIDE_PX, 1, 'px')
        return [column('margins', [
            text('margins-title', 'Add space around the image (pixels)', style='caption'),
            row('margins-horizontal', [field('left'), field('right')], gap=8),
            row('margins-vertical', [field('top'), field('bottom')], gap=8),
        ], gap=6)]

    def validate(self):
        problem = super().validate()
        if problem:
            return problem
        margins = self.margins()
        if margins is None:
            return 'Enter whole pixel amounts.'
        if min(margins) < 0:
            return 'Use 0 or more pixels per side.'
        if max(margins) > self.MAX_SIDE_PX:
            return f'Expand each side by at most {self.MAX_SIDE_PX} pixels.'
        if sum(margins) == 0:
            return 'Add space on at least one side.'
        width, height = self.result_size()
        if max(width, height) > self.MAX_RESULT_SIDE or width * height > MAX_OUTPUT_PIXELS:
            return 'The expanded image would exceed 40 megapixels.'
        return None

    def extra_nodes(self):
        size = self.result_size()
        if size is None:
            return []
        width, height = size
        if width <= 0 or height <= 0:
            return []
        cw, ch = fit_box(width, height, *self.PREVIEW_BOX)
        scale = cw / width
        left, top, _, _ = self.margins()
        ops = [draw_rect(0, 0, cw, ch, '#2a2d35', '#4a4f5c'),
               draw_image(self.source_rid(), left * scale, top * scale, self.target['w'] * scale, self.target['h'] * scale)]
        return [canvas('expand-preview', cw, ch, ops),
                text('expand-size', f'Result: {width} x {height} pixels. The new border is generated.',
                     style='caption')]

    def build_request(self, target, opts, rects, job):
        left, top, right, bottom = (int(opts[key]) for key in ('left', 'top', 'right', 'bottom'))
        return {'version': 1, 'operation': self.OPERATION,
                'expand': {'left': left, 'top': top, 'right': right, 'bottom': bottom},
                'max_pixels': MAX_OUTPUT_PIXELS}


ACTIONS = {
    'run': 'request_run',
    'consent-yes': 'consent_yes',
    'consent-no': 'consent_no',
    'accept': 'accept',
    'discard': 'discard',
    'cancel': 'cancel_job',
    'setup': 'start_setup',
    'dismiss': 'dismiss',
}
