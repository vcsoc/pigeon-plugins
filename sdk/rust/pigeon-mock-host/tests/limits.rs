#![cfg(unix)]

use std::{fs, sync::Arc, time::Duration};

use pigeon_mock_host::{LaunchOptions, Schemas, Session};

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

#[test]
fn invalid_output_preserves_last_scene_and_transport_recovers() {
    let dir = Fixture(std::env::temp_dir().join(format!("pigeon-mock-limits-{}", std::process::id())));
    fs::create_dir(&dir.0).unwrap();
    fs::write(dir.0.join("plugin.json"), r#"{"id":"limits","version":"1.0.0"}"#).unwrap();
    fs::write(dir.0.join("run"), "#!/bin/sh\nexec python3 fixture.py\n").unwrap();
    fs::write(dir.0.join("fixture.py"), r#"
import json, sys

def emit(t, **fields):
    print(json.dumps(dict(v=2, t=t, **fields), separators=(',', ':')), flush=True)

json.loads(sys.stdin.readline())
emit('ready', plugin=dict(id='limits', version='1.0.0'), protocol=2, contributions=[])
for line in sys.stdin:
    msg = json.loads(line)
    if msg['t'] == 'visibility':
        emit('ui.set', instance='limits/main', scene=dict(id='root', kind='text', text='valid'))
        emit('ui.set', instance='limits/main', scene=dict(id='root', kind='column', children=[dict(id='root', kind='text', text='duplicate')]))
        emit('canvas.draw', instance='limits/main', node='root', frame=1, ops=[dict(op='unclip')] * 20001)
        emit('future', padding='x' * (1024 * 1024))
    if msg['t'] == 'ping':
        emit('pong', id=msg['id'])
    if msg['t'] == 'shutdown':
        break
"#).unwrap();
    let session = Session::launch(&dir.0, Arc::new(Schemas::load()), LaunchOptions { run_setup: false, ..LaunchOptions::default() }).unwrap();
    let timeout = Duration::from_secs(5);
    session.wait_ready(timeout).unwrap();
    session.show("limits/main").unwrap();
    assert!(session.ping(timeout).is_some(), "transport should recover after oversized output");
    assert!(session.shutdown(timeout));
    assert_eq!(session.scene("limits/main").unwrap()["text"], "valid");
    assert_eq!(session.scene_revision("limits/main"), 1);
    assert_eq!(session.with_state(|st| st.canvas_updates), 0);
    let problems = session.violations();
    assert_eq!(problems.len(), 3, "duplicate id, over-limit canvas, and oversized line");
    assert!(problems.iter().any(|(p, _)| p.message.contains("duplicate node id")));
    assert!(problems.iter().any(|(p, _)| p.message.contains("1 MiB limit")));
}
