#!/usr/bin/env python3
"""Local Semantic Search: private multimodal similarity search with EmbeddingGemma 2 (CPU).

Two ways to use it, both through `run`:

  run --serve <index.db>   Long-running engine (what Pigeon uses). One JSON object per line on stdin,
                           one JSON reply per line on stdout. First line out: {"ready": true}.
                           Actions: info, configure, sync-assets, prune, plan, index_batch, search,
                           embed, check. The index lives in the SQLite file you give it.
  run  (JSON on stdin)     One-shot standalone protocol: {"version":1,"operation":"embed","text":"..."}
                           replies {"version":1,"ok":true,"dimensions":768}.

`run --setup` installs the Python packages and downloads the pinned model (~1.5 GB) into
<plugin>/.data/models/embeddinggemma-2, verifying every file. Nothing is sent anywhere.
"""
import os
import runpy
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import plugin_runtime as rt  # noqa: E402

ENGINE = os.path.join(HERE, 'semantic-engine.py')
MODEL_DIR = os.path.join(rt.MODELS, 'embeddinggemma-2')


def _require_model():
    if not os.path.isfile(os.path.join(MODEL_DIR, 'model.safetensors')):
        raise rt.PluginError('the model is not installed; run the plugin with --setup')
    entry = next(m for m in rt.manifest()['models'] if m['file'].endswith('/model.safetensors'))
    if os.path.getsize(os.path.join(MODEL_DIR, 'model.safetensors')) != entry['bytes']:
        raise rt.PluginError('the model is incomplete or corrupt; run the plugin with --setup')


def serve(database):
    try:
        _require_model()
    except rt.PluginError as error:
        sys.stdout.write('{"ready":false,"error":%s}\n' % __import__('json').dumps(str(error)))
        sys.exit(1)
    os.environ.setdefault('PIGEON_SEMANTIC_MODEL_DIR', MODEL_DIR)
    sys.argv = [ENGINE, database]
    runpy.run_path(ENGINE, run_name='__main__')


def _ask(actions, timeout=600):
    """Run the engine on a throw-away index and return the replies to `actions`."""
    import json
    import tempfile
    _require_model()
    with tempfile.TemporaryDirectory(prefix='pigeon-semantic-') as tmp:
        child = subprocess.Popen([sys.executable, ENGINE, os.path.join(tmp, 'index.db')], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                 env={**os.environ, 'PIGEON_SEMANTIC_MODEL_DIR': MODEL_DIR})
        try:
            ready = json.loads(child.stdout.readline() or '{}')
            if not ready.get('ready'):
                raise rt.PluginError('the semantic engine did not start')
            replies = []
            for number, action in enumerate(actions, 1):
                child.stdin.write(json.dumps({**action, 'id': number}) + '\n')
                child.stdin.flush()
                reply = json.loads(child.stdout.readline() or '{}')
                if 'error' in reply:
                    raise rt.PluginError('semantic engine: ' + str(reply['error'])[:300])
                replies.append(reply['result'])
            return replies
        finally:
            child.kill()
            child.wait()


def embed(request):
    text = request.get('text')
    if not isinstance(text, str) or not text.strip():
        raise rt.PluginError('request field "text" is required')
    (result,) = _ask([{'action': 'embed', 'text': text}])
    return {'dimensions': result['dimensions']}


def self_test(tmp):
    (result,) = _ask([{'action': 'check'}])
    if result.get('dimensions') != 768:
        raise rt.PluginError('the model produced an unexpected embedding size')
    return {'operation': 'embed', 'text': 'a bird flying'}


if __name__ == '__main__':
    if '--plugin' in sys.argv[1:]:          # protocol v2: the UI process (ui.py) drives this file
        import ui
        sys.exit(ui.main())
    if '--serve' in sys.argv[1:] and len(sys.argv) > sys.argv.index('--serve') + 1:
        serve(sys.argv[sys.argv.index('--serve') + 1])
    else:
        rt.main({'embed': embed}, self_test=self_test)
