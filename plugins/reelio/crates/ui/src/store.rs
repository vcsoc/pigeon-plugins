//! Project undo/redo store. Port of `stores/projectStore.ts` (`edit`, `replace`, `undo`, `redo`,
//! `markSaved`). Semantics kept exactly: no-op edits are not recorded, a new edit clears redo,
//! at most 100 past snapshots are retained, and dirtiness compares against the saved snapshot.

use crate::model::Project;

pub const MAX_HISTORY: usize = 100;

#[derive(Debug, Clone)]
pub struct ProjectStore {
    pub project: Project,
    past: Vec<Project>,
    future: Vec<Project>,
    saved: Project,
}

impl ProjectStore {
    pub fn new(project: Project) -> Self {
        Self {
            saved: project.clone(),
            project,
            past: vec![],
            future: vec![],
        }
    }

    /// Apply an edit to a copy. Returns `true` when the project actually changed.
    pub fn edit(&mut self, f: impl FnOnce(&mut Project)) -> bool {
        let mut next = self.project.clone();
        f(&mut next);
        if next == self.project {
            return false;
        }
        if self.past.len() >= MAX_HISTORY {
            self.past.remove(0);
        }
        let prior = std::mem::replace(&mut self.project, next);
        self.past.push(prior);
        self.future.clear();
        true
    }

    /// Replace the whole project (new/open/job restore). Clears history and sets the saved mark.
    pub fn replace(&mut self, project: Project) {
        self.saved = project.clone();
        self.project = project;
        self.past.clear();
        self.future.clear();
    }

    pub fn undo(&mut self) -> bool {
        match self.past.pop() {
            Some(prior) => {
                let current = std::mem::replace(&mut self.project, prior);
                self.future.insert(0, current);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.future.is_empty() {
            return false;
        }
        let next = self.future.remove(0);
        let current = std::mem::replace(&mut self.project, next);
        self.past.push(current);
        true
    }

    pub fn mark_saved(&mut self, snapshot: Project) {
        self.saved = snapshot;
    }

    pub fn saved(&self) -> &Project {
        &self.saved
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn dirty(&self) -> bool {
        self.project != self.saved
    }

    pub fn history_len(&self) -> (usize, usize) {
        (self.past.len(), self.future.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::empty_project;

    #[test]
    fn edit_undo_redo_and_branching_clear_redo() {
        let mut s = ProjectStore::new(empty_project());
        assert!(!s.edit(|_| {}), "a no-op edit is not recorded");
        assert!(!s.can_undo());
        s.edit(|p| p.name = "A".into());
        s.edit(|p| p.name = "B".into());
        assert!(s.undo());
        assert_eq!(s.project.name, "A");
        assert!(s.redo());
        assert_eq!(s.project.name, "B");
        assert!(s.undo());
        s.edit(|p| p.name = "C".into());
        assert!(!s.can_redo(), "a branching edit clears redo history");
        assert_eq!(s.project.name, "C");
    }

    #[test]
    fn snapshots_are_not_mutated_by_later_edits() {
        let mut s = ProjectStore::new(empty_project());
        let before = s.project.clone();
        s.edit(|p| {
            p.metadata
                .insert("title".into(), "x".into())
                .map(|_| ())
                .unwrap_or(())
        });
        s.undo();
        assert_eq!(s.project, before);
    }

    #[test]
    fn dirty_tracks_saved_snapshot_and_late_saves_do_not_mark_later_edits_saved() {
        let mut s = ProjectStore::new(empty_project());
        s.edit(|p| p.name = "one".into());
        let first = s.project.clone();
        s.edit(|p| p.name = "two".into());
        assert!(s.dirty());
        s.mark_saved(first);
        assert!(
            s.dirty(),
            "a save of an older snapshot leaves the newer edit dirty"
        );
    }

    #[test]
    fn replace_resets_history_and_saved_state() {
        let mut s = ProjectStore::new(empty_project());
        s.edit(|p| p.name = "x".into());
        s.replace(empty_project());
        assert!(!s.can_undo());
        assert!(!s.dirty());
    }

    #[test]
    fn history_is_bounded() {
        let mut s = ProjectStore::new(empty_project());
        for i in 0..150 {
            s.edit(|p| p.name = format!("n{i}"));
        }
        assert_eq!(s.history_len().0, MAX_HISTORY);
    }
}
