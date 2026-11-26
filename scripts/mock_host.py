"""Headless stand-in for the Pigeon host (protocol v2) for testing plugin UIs.

Starts a plugin with `sh run --plugin`, performs the handshake, answers the host services a plugin
uses (settings, selection, library, windows, progress, notifications), records every scene and
resource the plugin publishes, validates the messages it sends, and lets a test drive events
(clicks, changes, canvas pointers, commands, visibility). It is deliberately small: the WP2 mock host
(`pigeon_plugin` SDK) supersedes it once that lands; the same checks are kept here until then.

Validation covers the envelope (`v`, `t`), scene shape (ids unique per scene, known node kinds, a
root node) and the host services used. Anything else the plugin sends is recorded in `unknown`.
"""
import json
import os
import queue
import shutil
import subprocess
import tempfile
import threading
import time

NODE_KINDS = {
    'row', 'column', 'stack', 'grid', 'wrap', 'split', 'scroll', 'tabs', 'collapsible', 'panel', 'spacer',
    'divider', 'text', 'icon', 'image', 'button', 'icon_button', 'checkbox', 'switch', 'radio_group',
    'segmented', 'text_input', 'number_input', 'slider', 'range_slider', 'select', 'color_picker', 'progress',
    'spinner', 'badge', 'pill', 'menu', 'context_menu', 'tooltip', 'table', 'tree', 'list', 'canvas',
    'rich_text', 'code',
}
PLUGIN_MESSAGES = {'ready', 'ui.set', 'canvas.draw', 'resource.put', 'resource.drop', 'req', 'pong', 'res',
                   'log', 'ui.focus', 'ui.scroll_to', 'ui.beep', 'contribute', 'withdraw', 'list.rows',
                   'source.changed', 'search.status', 'ui.patch', 'resource.ack'}
HOST_NAME = 'Pigeon'


class MockHostError(AssertionError):
    pass


def walk(node):
    yield node
    for child in node.get('children', []) or []:
        yield from walk(child)


def find(scene, node_id):
    """The node with this id in a scene, or None."""
    if not scene:
        return None
    for item in walk(scene):
        if item.get('id') == node_id:
            return item
    return None


def node_ids(scene):
    return [item.get('id') for item in walk(scene)] if scene else []


class MockHost:
    def __init__(self, plugin_dir, env=None, selection=None, library=None, settings=None, timeout=30):
        self.timeout = timeout
        self.plugin_dir = plugin_dir
        self.work = tempfile.mkdtemp(prefix='mock-host-')
        self.session = os.path.join(self.work, 'session')
        os.makedirs(self.session)
        self.settings = dict(settings or {})
        self.selection = list(selection or [])
        self.library = list(library or [])
        self.scenes = {}
        self.resources = {}
        self.calls = []             # host services received: (method, params)
        self.imports = []
        self.modals = []
        self.toasts = []
        self.progress = []
        self.unknown = []
        self.errors = []
        self.inbox = queue.Queue()
        self.lock = threading.Condition()
        self.ready = None
        self.pongs = set()
        self.replies = {}
        self.exited = False
        self.counter = 0
        environment = {'PATH': '/usr/bin:/bin', 'PIGEON_SESSION_DIR': self.session, **(env or {})}
        self.process = subprocess.Popen(['sh', os.path.join(plugin_dir, 'run'), '--plugin'], cwd=plugin_dir,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding='utf-8', env=environment)
        self.stderr_lines = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()
        self.send('hello', host={'name': HOST_NAME, 'version': '0.4.1'}, protocol={'min': 2, 'max': 2},
                  theme={'mode': 'dark', 'scale': 2.0, 'font': {'ui': 'system', 'mono': 'menlo'},
                         'colors': {'bg': '#1b1d22', 'fg': '#d8dbe2', 'accent': '#4f8cff'}, 'radius': 4,
                         'spacing': 4},
                  locale='en', scale=2.0, data_dir=os.path.join(self.work, 'data'), session_dir=self.session,
                  features=['scene.patch', 'canvas', 'list.window', 'resources.file', 'source', 'search'])

    # ---- transport
    def send(self, t, **fields):
        message = {'v': 2, 't': t, **fields}
        try:
            self.process.stdin.write(json.dumps(message) + '\n')
            self.process.stdin.flush()
        except (BrokenPipeError, OSError, ValueError):
            raise MockHostError('plugin closed its input')

    def _drain_stderr(self):
        for line in self.process.stderr:
            self.stderr_lines.append(line.rstrip())
            del self.stderr_lines[:-200]

    def _read(self):
        for line in self.process.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except ValueError:
                self.errors.append(f'stdout is not JSON: {line[:120]}')
                continue
            if not isinstance(message, dict) or message.get('v') != 2:
                self.errors.append(f'envelope without v=2: {line[:120]}')
                continue
            self._handle(message)
        with self.lock:
            self.exited = True
            self.lock.notify_all()

    def _handle(self, message):
        kind = message.get('t')
        with self.lock:
            if kind == 'ready':
                self.ready = message
            elif kind == 'ui.set':
                problem = validate_scene(message.get('scene'))
                if problem:
                    self.errors.append(f"ui.set {message.get('instance')}: {problem}")
                self.scenes[message.get('instance')] = message.get('scene')
            elif kind == 'resource.put':
                self.resources[message['rid']] = message
                self.send('resource.ack', rid=message['rid'], seq=message['seq'])
            elif kind == 'resource.drop':
                self.resources.pop(message['rid'], None)
            elif kind == 'pong':
                self.pongs.add(str(message.get('id')))
            elif kind == 'res':
                self.replies[str(message.get('id'))] = message
            elif kind == 'req':
                self.lock.release()
                try:
                    reply = self._service(message)
                finally:
                    self.lock.acquire()
                try:
                    self.send('res', id=message.get('id'), **reply)
                except MockHostError:
                    pass                    # the plugin is shutting down
                self.lock.notify_all()
                return
            elif kind in PLUGIN_MESSAGES:
                pass
            else:
                self.unknown.append(kind)
            self.lock.notify_all()

    def _service(self, message):
        method = message.get('method')
        params = message.get('params') or {}
        self.calls.append((method, params))
        if method == 'settings.get':
            return {'result': {'value': self.settings.get(params.get('key'))}}
        if method == 'settings.set':
            self.settings[params.get('key')] = params.get('value')
            return {'result': {}}
        if method == 'selection.get':
            return {'result': {'assets': self.selection}}
        if method == 'selection.subscribe':
            return {'result': {}}
        if method == 'library.query':
            return {'result': {'items': self.library, 'next': None}}
        if method == 'library.import':
            self.counter += 1
            self.imports.append(params)
            return {'result': {'ids': [f'derived-{self.counter}']}}
        if method == 'library.reveal':
            return {'result': {}}
        if method == 'window.open_modal':
            self.modals.append(params)
            self.send('visibility', instance=params.get('instance'), visible=True)
            return {'result': {}}
        if method.startswith('progress.'):
            self.progress.append((method, params))
            return {'result': {'job': f'job-{len(self.progress)}'} if method == 'progress.start' else {}}
        if method == 'notify.toast':
            self.toasts.append(params)
            return {'result': {}}
        return {'error': {'code': 'unsupported', 'message': f'the mock host does not implement {method}'}}

    # ---- waiting and driving
    def wait(self, predicate, timeout=None, what='condition'):
        deadline = time.time() + (timeout or self.timeout)
        with self.lock:
            while not predicate():
                remaining = deadline - time.time()
                if remaining <= 0 or self.exited:
                    raise MockHostError(f'timed out waiting for {what}; stderr tail: {self.stderr_tail()}')
                self.lock.wait(min(remaining, 0.25))
        return True

    def stderr_tail(self):
        return ' | '.join(self.stderr_lines[-6:])[-800:]

    def handshake(self):
        self.wait(lambda: self.ready is not None, what='ready')
        ready = self.ready
        if ready.get('protocol') != 2:
            raise MockHostError(f'ready reported protocol {ready.get("protocol")}')
        return ready

    def scene(self, instance, predicate=None, timeout=None, what=None):
        """Latest scene for an instance, waiting until predicate(scene) holds when one is given."""
        if predicate is None:
            predicate = lambda scene: scene is not None
        self.wait(lambda: predicate(self.scenes.get(instance)), timeout, what=what or f'scene {instance}')
        return self.scenes[instance]

    def visible(self, instance):
        self.send('visibility', instance=instance, visible=True)

    def click(self, instance, node_id):
        self.send('ui.event', instance=instance, node=node_id, type='click')

    def change(self, instance, node_id, value, rev=None):
        message = {'instance': instance, 'node': node_id, 'type': 'change', 'value': value}
        if rev is not None:
            message['rev'] = rev
        self.send('ui.event', **message)

    def pointer(self, instance, node_id, phase, x, y):
        self.send('ui.event', instance=instance, node=node_id, type='pointer', phase=phase, x=x, y=y, button=0,
                  buttons=1 if phase in ('down', 'move') else 0,
                  mods={'shift': False, 'ctrl': False, 'alt': False, 'meta': False}, clicks=1, pointer_id=1)

    def command(self, command_id, assets):
        self.send('command', command=command_id, args={}, context={'assets': assets})

    def ping(self, timeout=2):
        ident = f'ping-{time.time()}'
        self.send('ping', id=ident)
        self.wait(lambda: ident in self.pongs, timeout, what='pong')

    def request(self, method, params=None, ident=None, timeout=None):
        """Ask the plugin a provider request (for example search.query); returns its reply."""
        ident = ident or f'req-{time.time()}'
        self.send('req', id=ident, method=method, params=params or {})
        self.wait(lambda: ident in self.replies, timeout, what=f'reply to {method}')
        return self.replies[ident]

    def resource(self, rid):
        return self.resources.get(rid)

    def shutdown(self):
        try:
            self.send('shutdown')
        except MockHostError:
            pass
        try:
            self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        shutil.rmtree(self.work, ignore_errors=True)
        return self.process.returncode


def validate_scene(scene):
    if not isinstance(scene, dict):
        return 'scene is not an object'
    seen = set()
    for item in walk(scene):
        identifier = item.get('id')
        if not isinstance(identifier, str) or not identifier:
            return 'node without an id'
        if identifier in seen:
            return f'duplicate node id {identifier}'
        seen.add(identifier)
        if item.get('kind') not in NODE_KINDS:
            return f'unknown node kind {item.get("kind")!r} on {identifier}'
    return None


def contributions_match(ready, manifest_path):
    """The ready message must repeat the static contributions in plugin.json (section 11)."""
    with open(manifest_path, encoding='utf-8') as f:
        declared = json.load(f)['ui']['contributions']
    got = ready.get('contributions') or []
    return sorted(c['id'] for c in got) == sorted(c['id'] for c in declared)
