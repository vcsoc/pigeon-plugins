"""Local Semantic Search: protocol v2 UI, search provider and indexing service.

Threads in this process (standard library only):
  * the SDK dispatcher only updates state and publishes scenes; it never waits;
  * the scheduler thread scans the library and indexes in small batches through the engine;
  * search and "find similar" requests run on their own threads (host requests are answered there);
  * the engine (`run --serve <db>`, semantic-engine.py in a child process) does the model work. It
    handles one request at a time, so index batches are short (at most four files) and searches
    wait for at most one batch.

Pause stops the scheduler between batches. Removing the index stops the engine first.
"""
import json
import os
import select
import time
import subprocess
import sys
import threading
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ui_support as ui  # noqa: E402

HostError = ui.ServiceError

KINDS = ('image', 'video', 'audio', 'document')
LIBRARY_PAGE = 500
MAX_ASSETS = 200_000
BATCH = 4
DEFAULTS = {'workers': 2, 'minimum': 0.68, 'auto': True}


class EngineError(Exception):
    pass


class Engine:
    """The semantic engine as a child process: one JSON request at a time, replies matched by id."""

    def __init__(self, folder, database):
        self.folder = folder
        self.database = database
        self.process = None
        self.lock = threading.Lock()
        self.counter = 0
        self.stopping = False
        self.buffer = b''

    def alive(self):
        return self.process is not None and self.process.poll() is None

    def start(self):
        self.buffer = b''
        self.process = subprocess.Popen(['sh', os.path.join(self.folder, 'run'), '--serve', self.database],
                                        cwd=self.folder, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        ready = self._read(time.monotonic() + 60)
        if not ready.get('ready'):
            self.close()
            raise EngineError(str(ready.get('error') or 'the semantic engine did not start'))

    def _read(self, deadline):
        fd = self.process.stdout.fileno()
        while b'\n' not in self.buffer:
            if not select.select([fd], [], [], max(0, deadline - time.monotonic()))[0]:
                raise TimeoutError('the semantic engine timed out')
            chunk = os.read(fd, 65536)
            if not chunk:
                raise OSError('the semantic engine stopped')
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b'\n', 1)
        try:
            value = json.loads(line)
        except ValueError:
            raise EngineError('the semantic engine sent an unreadable reply')
        return value if isinstance(value, dict) else {}

    def call(self, action, **fields):
        with self.lock:
            if self.stopping:
                raise EngineError('the semantic engine is shutting down')
            self.counter += 1
            ident = self.counter
            try:
                if not self.alive():
                    self.start()
                self.process.stdin.write((json.dumps({'id': ident, 'action': action, **fields}) + '\n').encode())
                self.process.stdin.flush()
                deadline = time.monotonic() + 180
                while True:
                    reply = self._read(deadline)
                    if reply.get('id') != ident or 'progress' in reply:
                        continue
                    if 'error' in reply:
                        raise EngineError(str(reply['error'])[:300])
                    return reply.get('result') or {}
            except OSError as error:
                self.close()
                raise EngineError(str(error)) from error

    def close(self):
        process = self.process
        self.process = None
        if process is not None:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()


class SemanticSearch(ui.App):
    def __init__(self, folder):
        super().__init__(folder)
        data = os.environ.get('PIGEON_PLUGIN_DATA') or os.path.join(folder, '.data')
        os.makedirs(data, exist_ok=True)
        self.database = os.path.join(data, 'index.db')
        self.engine = Engine(folder, self.database)
        self.settings = dict(DEFAULTS)
        self.settings_loaded = False
        self.ready = None
        self.device = None
        self.health_error = ''
        self.setup_running = False
        self.setup_text = ''
        self.paused = False
        self.manual_run = False
        self.status = 'Starting...'
        self.progress = None
        self.indexed = 0
        self.fingerprints = {}
        self.names = {}
        self.kinds = {}
        self.wake = threading.Event()
        self.stopping = threading.Event()
        self.confirm_remove = False
        self.revs = {}
        self.similar = {'title': '', 'rows': [], 'status': '', 'busy': False}
        self.request_slots = threading.BoundedSemaphore(4)
        self.scheduler_lock = threading.Lock()
        self.last_status = None

    # ---- lifecycle
    def start(self):
        threading.Thread(target=self.scheduler_loop, name='pigeon-index', daemon=True).start()
        self.spawn(self.load_state)

    def close(self):
        self.stopping.set()
        self.wake.set()
        self.engine.stopping = True
        process = self.engine.process
        if process is not None and process.poll() is None:
            process.kill()  # Unblock a worker reading the pipe before waiting for its lock.
        with self.engine.lock:
            self.engine.close()

    def call(self, method, **params):
        return super().call(method, params)

    def load_state(self):
        values = {}
        for key in DEFAULTS:
            values[key] = self.setting(key, DEFAULTS[key])
        try:
            info = ui.child_health(self.folder)
        except (OSError, subprocess.TimeoutExpired) as error:
            info = {'ready': False, 'error': str(error)}
        with self.lock:
            self.settings.update({k: v for k, v in values.items() if v is not None})
            self.settings_loaded = True
            self.set_ready(info)
        self.wake.set()

    def set_ready(self, info):
        self.ready = bool(info.get('ready'))
        self.device = info.get('device')
        self.health_error = '' if self.ready else str(info.get('error') or 'The model is not set up yet.')
        self.render_all()

    def refresh_health(self):
        info = ui.child_health(self.folder)
        with self.lock:
            self.set_ready(info)
        self.wake.set()

    # ---- host dispatch (dispatcher thread, lock held)
    def on_message(self, msg):
        super().on_message(msg)
        kind = msg.get('t')
        if kind == 'ui.event' and msg.get('instance') in self.opened:
            self.on_event(msg)
        elif kind == 'command' and msg.get('command') == 'similar' and not self.similar['busy']:
            self.similar['busy'] = True
            self.spawn(self.open_similar, msg.get('context') or {})
        elif kind == 'req' and msg.get('method') == 'search.query':
            if self.request_slots.acquire(blocking=False):
                self.spawn(self.answer_query, msg['id'], msg.get('params') or {})
            else:
                self.ctx.answer_error(msg['id'], 'busy', 'Semantic search is busy; try again shortly.')
        elif kind == 'shutdown':
            self.stopping.set()

    def answer_query(self, ident, params):
        try:
            self.ctx.answer(ident, self.search_query(params))
        except HostError as error:
            self.ctx.answer_error(ident, error.code, error.message)
        except Exception:
            self.ctx.answer_error(ident, 'internal', 'Semantic search failed; see the plugin log.')
            raise
        finally:
            self.request_slots.release()

    def on_event(self, msg):
        node, kind = msg.get('node', ''), msg.get('type')
        if kind == 'change' and node in ('workers', 'minimum', 'auto'):
            self.set_setting_local(node, msg.get('value'), msg.get('rev'))
            self.spawn(self.store_setting, node, self.settings[node])
            self.wake.set()
        elif kind == 'click':
            if node == 'pause':
                self.paused = not self.paused
                if not self.paused:
                    self.manual_run = True
                    self.wake.set()
                self.status = 'Paused.' if self.paused else 'Resuming...'
            elif node == 'index-now':
                self.manual_run = True
                self.paused = False
                self.wake.set()
            elif node == 'setup':
                self.start_setup()
            elif node == 'recheck':
                self.spawn(self.refresh_health)
            elif node == 'remove-index':
                self.confirm_remove = True
            elif node == 'remove-cancel':
                self.confirm_remove = False
            elif node == 'remove-confirm':
                self.confirm_remove = False
                self.paused = True
                self.spawn(self.remove_index)
            elif node.startswith('reveal-') and node[len('reveal-'):].isdigit():
                index = int(node[len('reveal-'):])
                if index < len(self.similar['rows']):
                    self.spawn(self._reveal, self.similar['rows'][index]['id'])
        self.render_all()

    def set_setting_local(self, key, value, rev):
        stored = self.revs.get(key, 0)
        if rev is not None and rev < stored:
            return
        self.revs[key] = stored + 1 if rev is None else rev
        if key == 'workers':
            try:
                self.settings[key] = max(1, min(4, int(value)))
            except (TypeError, ValueError):
                pass
        elif key == 'minimum':
            try:
                self.settings[key] = max(0.3, min(0.9, float(value)))
            except (TypeError, ValueError):
                pass
        elif key == 'auto':
            self.settings[key] = bool(value)

    def store_setting(self, key, value):
        self.put_setting(key, value)

    def _reveal(self, ident):
        try:
            self.call('library.reveal', id=ident)
        except HostError as error:
            self.toast('warning', f'Could not show the item: {error}')

    # ---- scheduler (own thread)
    def scheduler_loop(self):
        while not self.stopping.is_set():
            self.wake.wait(60)
            self.wake.clear()
            if self.stopping.is_set():
                break
            try:
                with self.scheduler_lock:
                    self.scheduler_pass()
            except (HostError, EngineError, OSError, ValueError) as error:
                with self.lock:
                    self.status = f'Indexing stopped: {error}'
                    self.progress = None
                    self.render_all()
            except Exception:
                ui.log('indexing failed', traceback.format_exc())
                with self.lock:
                    self.status = 'Indexing stopped after an internal error (see the plugin log).'
                    self.progress = None
                    self.render_all()

    def scheduler_pass(self):
        with self.lock:
            if self.ready is not True:
                return
            if self.paused:
                self.status = 'Paused.'
                self.render_all()
                return
            if not self.settings['auto'] and not self.manual_run:
                self.status = self.idle_status()
                self.render_all()
                return
            self.manual_run = False
            workers = self.settings['workers']
        with self.lock:
            self.status = 'Scanning the library...'
            self.progress = None
            self.render_all()
        records = self.scan_library()
        # A library query returns only accessible records; omission does not prove deletion.
        entries = list(records.values())
        for start in range(0, len(entries), LIBRARY_PAGE):
            if self.stopping.is_set() or self.paused:
                return
            self.engine_call('sync-assets', records=entries[start:start + LIBRARY_PAGE])
        with self.lock:
            self.fingerprints = {k: v['fingerprint'] for k, v in records.items()}
            self.names = {k: v['asset']['name'] for k, v in records.items()}
            self.kinds = {k: v['asset']['kind'] for k, v in records.items()}
        pending = [key for key in self.engine_call('plan').get('pending', []) if key in records]
        total = len(pending)
        done = 0
        rounds = 0
        while pending and not self.stopping.is_set():
            if self.paused:
                with self.lock:
                    self.status = 'Paused.'
                    self.progress = done / total if total else None
                    self.render_all()
                return
            rounds += 1
            if rounds > 4 * max(1, total) + 20:
                raise HostError('stalled', 'indexing made no progress; see the plugin log')
            batch = [records[ident] for ident in pending if ident in records][:BATCH]
            if not batch:
                break
            reply = self.engine_call('index_batch', jobs=[{'asset': r['asset'], 'fingerprint': r['fingerprint']}
                                                         for r in batch], workers=workers, steps=2)
            done += sum(1 for item in reply.get('results', []) if item.get('done'))
            with self.lock:
                self.status = f'Indexing {min(done, total)} of {total} files'
                self.progress = min(done, total) / total if total else None
                self.render_all()
            finished = {item['id'] for item in reply.get('results', []) if item.get('done')}
            pending = [key for key in pending if key not in finished]
        info = self.engine_call('info')
        with self.lock:
            self.indexed = int(info.get('indexed', 0))
            self.progress = None
            self.status = f'Up to date: {self.indexed} files indexed.' if not pending else self.status
            self.render_all()

    def idle_status(self):
        return f'{self.indexed} files indexed. Press Index now to update.'

    def scan_library(self):
        records = {}
        cursor = None
        seen = set()
        while not self.stopping.is_set() and not self.paused:
            reply = self.call('library.query', filter={'kinds': list(KINDS)}, sort='name', cursor=cursor,
                              limit=LIBRARY_PAGE)
            for item in reply.get('items') or reply.get('assets') or []:
                record = self.record_for(item)
                if record:
                    records[record['asset']['id']] = record
            cursor = reply.get('next')
            if not cursor or cursor in seen or len(records) >= MAX_ASSETS:
                break
            seen.add(cursor)
        return records

    @staticmethod
    def record_for(item):
        """The engine's view of one library item (local files only)."""
        path = item.get('path') if isinstance(item, dict) else None
        if not path or item.get('kind') not in KINDS or not item.get('id'):
            return None
        try:
            stat = os.stat(path)
        except OSError:
            return None
        fingerprint = f'{path}|{stat.st_size}|{stat.st_mtime_ns}'
        asset = {'id': str(item['id']), 'path': path, 'kind': item['kind'],
                 'name': str(item.get('name') or os.path.basename(path)),
                 'tags': [str(t) for t in (item.get('tags') or [])][:50],
                 'note': str(item.get('note') or '')[:2000], 'modified': stat.st_mtime * 1000,
                 'size': stat.st_size}
        return {'asset': asset, 'fingerprint': fingerprint, 'contentFingerprint': fingerprint}

    def engine_call(self, action, **fields):
        try:
            return self.engine.call(action, **fields)
        except EngineError as error:
            raise HostError('engine', str(error)) from error

    # ---- provider and asset action (their own threads)
    def search_query(self, params):
        if params.get('mode_id') != 'semantic':
            raise HostError('invalid', 'Unknown search mode.')
        query = str(params.get('query') or '').strip()[:4000]
        if not query:
            return {'results': []}
        if self.ready is not True:
            raise HostError('unavailable', 'Set up the semantic model in Preferences first.')
        try:
            offset = max(0, int(params.get('cursor') or 0))
            limit = max(1, min(200, int(params.get('limit') or 50)))
        except (TypeError, ValueError):
            raise HostError('invalid', 'cursor and limit must be numbers')
        with self.lock:
            kinds = (params.get('filters') or {}).get('kinds')
            allowed = {key: value for key, value in self.fingerprints.items() if not kinds or self.kinds[key] in kinds}
            minimum = self.settings['minimum']
        reply = self.engine_call('search', query=query, assets=allowed, minimum=minimum, limit=limit,
                                 offset=offset)
        results = [{'ref': str(item['id']), 'score': round(float(item.get('score', 0)), 4),
                    'snippet': self.snippet(item.get('match'))} for item in reply.get('results', [])]
        more = int(reply.get('totalMatches', 0)) > offset + limit
        return {'results': results, 'next': str(offset + limit) if more else None}

    @staticmethod
    def snippet(match):
        if isinstance(match, dict):
            return str(match.get('snippet') or match.get('label') or '')[:240]
        return ''

    def selected_assets(self, context):
        assets = [item for item in (context.get('assets') or context.get('selection') or [])
                  if isinstance(item, dict) and item.get('id') and item.get('path')]
        if assets:
            return assets
        try:
            reply = self.call('selection.get')
        except HostError:
            return []
        return [item for item in reply.get('assets') or [] if isinstance(item, dict) and item.get('path')]

    def open_similar(self, context):
        assets = self.selected_assets(context)
        if not assets:
            with self.lock:
                self.similar['busy'] = False
            return
        target = assets[0]
        record = self.record_for(target)
        with self.lock:
            self.similar = {'title': str(target.get('name') or target['id']), 'rows': [], 'busy': True,
                            'status': 'Searching...'}
            self.render_all()
        try:
            self.call('window.open_modal', contribution='similar-results', instance='semantic-search/similar-results',
                      props={'title': 'Similar items'})
        except HostError as error:
            with self.lock:
                self.similar['busy'] = False
            self.toast('warning', f'Could not open similar items: {error.message}')
            return
        try:
            if self.ready is not True:
                raise HostError('unavailable', 'Set up the semantic model in Preferences first.')
            if record is None:
                raise HostError('unavailable', 'Only files on this computer can be compared.')
            with self.lock:
                allowed = dict(self.fingerprints)
                minimum = self.settings['minimum']
            reply = self.engine_call('search', assetId=str(target['id']), sample=record['asset'],
                                     assets=allowed, minimum=minimum, limit=30)
            rows = [{'id': str(item['id']), 'name': self.names.get(str(item['id']), str(item['id'])),
                     'score': round(float(item.get('score', 0)), 3)} for item in reply.get('results', [])]
            status = f'{len(rows)} similar items.' if rows else 'Nothing similar above the threshold.'
        except HostError as error:
            rows, status = [], str(error)
        with self.lock:
            self.similar = {'title': self.similar['title'], 'rows': rows, 'busy': False, 'status': status}
            self.render_all()

    # ---- setup
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
            ui.run_setup(self.folder, on_text)
            with self.lock:
                self.setup_text = 'Setup finished.'
        except ui.OperationError as error:
            with self.lock:
                self.setup_text = f'Setup failed: {error}'
        finally:
            with self.lock:
                self.setup_running = False
            self.refresh_health()

    def remove_index(self):
        with self.scheduler_lock, self.engine.lock:
            self.engine.close()
            for name in (self.database, self.database + '-wal', self.database + '-shm'):
                try:
                    os.remove(name)
                except FileNotFoundError:
                    pass
        with self.lock:
            self.indexed = 0
            self.fingerprints = {}
            self.paused = True
            self.status = 'Index removed. Press Resume to rebuild it.'
            self.render_all()

    # ---- rendering (lock held)
    def render_all(self):
        state = {'message': self.status, 'indexed': self.indexed, 'paused': self.paused,
                 'progress': self.progress, 'ready': self.ready}
        if self.ctx and state != self.last_status:
            self.ctx.search_status('semantic', state)
            self.last_status = state
        super().render_all()

    def render(self, instance):
        scene = {'index': self.index_scene, 'status': self.status_scene, 'prefs': self.prefs_scene,
                 'similar-results': self.similar_scene}.get(self.slot_id(instance))
        if scene:
            self.publish(instance, scene())

    def model_line(self):
        if self.ready is None:
            return 'Checking the model...'
        if self.ready is False:
            return 'The model is not set up. Setup downloads about 1.5 GB once.'
        device = f' Device: {self.device}.' if self.device else ''
        return f'Model installed.{device} {self.indexed} files indexed.'

    def index_scene(self):
        children = [ui.text('title', 'Semantic index', style='heading')]
        children.append(ui.text('model', self.model_line(), style='caption'))
        if self.ready is False:
            children.append(ui.button('setup', 'Set up model', variant='primary', enabled=not self.setup_running))
            if self.setup_text:
                children.append(ui.text('setup-progress', self.setup_text, style='caption'))
        else:
            children.append(ui.text('status', self.status or 'Idle.', style='body'))
            if self.progress is not None or self.status.startswith('Indexing') or self.status.startswith('Scanning'):
                children.append(ui.progress_bar('progress', self.progress))
            paused = self.paused
            children.append(ui.row('controls', [
                ui.button('pause', 'Resume' if paused else 'Pause', enabled=self.ready is True),
                ui.button('index-now', 'Index now', enabled=self.ready is True and not self.settings['auto']),
            ], gap=8))
        return ui.column('index-root', children, gap=8, padding=10)

    def status_scene(self):
        if self.ready is not True:
            label = 'Semantic index: not set up'
        elif self.paused:
            label = 'Semantic index: paused'
        elif self.progress is not None:
            label = f'Semantic index: {int(self.progress * 100)}%'
        else:
            label = f'Semantic index: {self.indexed} files'
        return ui.text('label', label, style='caption')

    def prefs_scene(self):
        children = [
            ui.text('title', 'Semantic search', style='heading'),
            ui.text('model', self.model_line(), style='body'),
            ui.text('privacy', 'Your files and queries stay on this computer. Nothing is sent anywhere.',
                    style='caption'),
        ]
        if self.ready is False:
            children.append(ui.button('setup', 'Set up model', variant='primary', enabled=not self.setup_running))
            if self.setup_text:
                children.append(ui.text('setup-progress', self.setup_text, style='caption'))
        children.append(ui.number_input('workers', self.settings['workers'], self.revs.get('workers', 0), 1, 4,
                                        1, None, 0))
        children.append(ui.slider('minimum', self.settings['minimum'], self.revs.get('minimum', 0), 0.3, 0.9,
                                  0.01))
        children.append(ui.checkbox('auto', 'Index new and changed files automatically',
                                    bool(self.settings['auto'])).set('rev', self.revs.get('auto', 0)))
        children.append(ui.button('recheck', 'Check the model', variant='ghost'))
        if self.confirm_remove:
            children.append(ui.row('remove-confirm-row', [
                ui.button('remove-confirm', 'Remove the index', variant='danger'),
                ui.button('remove-cancel', 'Cancel'),
            ], gap=8))
        else:
            children.append(ui.button('remove-index', 'Remove the index', variant='danger'))
        return ui.column('prefs-root', children, gap=10, padding=12)

    def similar_scene(self):
        children = [ui.text('title', f"Similar to {self.similar['title']}" if self.similar['title'] else
                            'Similar items', style='heading')]
        children.append(ui.text('similar-status', self.similar['status'] or '', style='caption'))
        for index, row in enumerate(self.similar['rows']):
            children.append(ui.row(f'row-{index}', [
                ui.text(f'name-{index}', row['name'], style='body', width={'fill': 1}),
                ui.text(f'score-{index}', f"{round(row['score'] * 100)}%", style='caption'),
                ui.button(f'reveal-{index}', 'Reveal', variant='ghost'),
            ], gap=8))
        return ui.column('similar-root', children, gap=8, padding=12)


def main():
    from pigeon_plugin import Plugin, PluginSpec

    app = SemanticSearch(HERE)
    manifest = app.manifest
    plugin = Plugin.stdio(PluginSpec(manifest['id'], manifest['version'], manifest['ui']['contributions']))
    app.ctx = plugin.ctx
    try:
        app.start()
        plugin.run(app.handle)
    finally:
        app.close()
    return 0
