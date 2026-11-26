//! Black-box helpers for event → scene tests. Each wait has a caller-controlled deadline.

use std::time::Duration;

use pigeon_plugin_sdk::protocol::NodeEvent;
use serde_json::Value;

use crate::session::{find_node, Session};

/// An owned scene snapshot, with the session-local change cursor for its instance.
#[derive(Debug, Clone)]
pub struct SceneSnapshot {
    pub revision: u64,
    pub root: Value,
}

impl SceneSnapshot {
    /// Search by stable node id, including materialized list rows.
    pub fn node(&self, id: &str) -> Option<&Value> {
        find_node(&self.root, id)
    }

    /// First node whose displayed text or label exactly matches `text`.
    pub fn by_text(&self, text: &str) -> Option<&Value> {
        find_matching(&self.root, &|n| {
            ["text", "label", "title"].iter().any(|key| n[*key].as_str() == Some(text))
                || n["spans"].as_array().is_some_and(|spans| {
                    spans.iter().filter_map(|span| span["text"].as_str()).collect::<String>() == text
                })
        })
    }

    #[track_caller]
    pub fn assert_node(&self, id: &str) -> &Value {
        self.node(id).unwrap_or_else(|| panic!("scene revision {} contains no node {id:?}", self.revision))
    }

    #[track_caller]
    pub fn assert_text(&self, text: &str) -> &Value {
        self.by_text(text).unwrap_or_else(|| panic!("scene revision {} contains no text {text:?}", self.revision))
    }
}

pub(crate) fn find_matching<'a>(node: &'a Value, matches: &impl Fn(&Value) -> bool) -> Option<&'a Value> {
    if matches(node) {
        return Some(node);
    }
    node_children(node).find_map(|child| find_matching(child, matches))
}

pub(crate) fn node_children(node: &Value) -> impl Iterator<Item = &Value> {
    node["children"].as_array().into_iter().flatten()
        .chain(node["window"]["rows"].as_array().into_iter().flatten())
}

impl Session {
    /// Current scene cursor; zero before the first scene. Take it *before* sending an event.
    pub fn scene_revision(&self, instance: &str) -> u64 {
        self.with_state(|st| st.scene_revisions.get(instance).copied().unwrap_or_default())
    }

    /// Send a validated UI event and return the cursor taken before the send, for [`Self::wait_next_scene`].
    pub fn event(&self, instance: &str, node: &str, event: NodeEvent) -> Result<u64, String> {
        let before = self.scene_revision(instance);
        self.send_event(instance, node, event)?;
        Ok(before)
    }

    /// Wait for the latest scene to be newer than `after`. Intermediate frames can be coalesced.
    pub fn wait_next_scene(&self, instance: &str, after: u64, timeout: Duration) -> Result<SceneSnapshot, String> {
        self.wait_scene_matching(instance, after, timeout, |_| true)
    }

    /// Wait for both a new scene and the expected content (e.g. a changed label after async work).
    pub fn wait_scene_matching(&self, instance: &str, after: u64, timeout: Duration, matches: impl Fn(&SceneSnapshot) -> bool) -> Result<SceneSnapshot, String> {
        self.wait_until(timeout, |st| {
            let revision = *st.scene_revisions.get(instance)?;
            if revision <= after { return None; }
            let snapshot = SceneSnapshot { revision, root: st.scenes.get(instance)?.clone() };
            matches(&snapshot).then_some(snapshot)
        }, "scene").ok_or_else(|| format!("no matching scene for {instance:?} after revision {after} within {timeout:?}"))
    }

    /// Fail the calling test with the first recorded protocol violation, if any.
    #[track_caller]
    pub fn assert_no_violations(&self) {
        let violations = self.violations();
        if let Some((problem, line)) = violations.first() {
            panic!("{} protocol violation(s), first: {problem}; line: {line}", violations.len());
        }
    }
}
