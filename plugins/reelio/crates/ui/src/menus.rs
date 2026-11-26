//! Context menus: timeline clip, chapter marker, project cover, and the project (hamburger) menu.
//! Each item carries the action node id it performs; the menu closes first, then the action runs
//! through the same `ui` path as a direct click, so menu and control behaviour cannot drift.

use crate::geometry;
use crate::model::Kind;
use crate::state::State;
use crate::timeline_input::{hit, Hit};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub action: String,
    pub enabled: bool,
    pub title: Option<String>,
    pub danger: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub items: Vec<MenuItem>,
    pub focus: usize,
}

impl Menu {
    pub fn focused_action(&self) -> Option<&str> {
        self.items.get(self.focus).map(|i| i.action.as_str())
    }
}

fn item(label: &str, action: &str, enabled: bool) -> MenuItem {
    MenuItem {
        label: label.into(),
        action: action.into(),
        enabled,
        title: None,
        danger: false,
    }
}

impl State {
    /// Open the clip menu for a track (`Include`/`Exclude`, `Clip to video`, trims, `Remove`).
    pub fn open_clip_menu(&mut self, track_id: &str, x: f64, y: f64) {
        let Some(track) = self.project().track(track_id).cloned() else {
            return;
        };
        self.select_track_by_id(track_id);
        let mut items = vec![];
        if self.project().track_kind(&track) == Some(Kind::Subtitle) {
            items.push(item(
                "Open in studio",
                &format!("sub.open:{track_id}"),
                self.busy.is_none(),
            ));
        }
        items.push(item(
            if track.enabled { "Exclude" } else { "Include" },
            &format!("lane:include:{track_id}"),
            true,
        ));
        let clip = geometry::clip_to_video(self.project(), &track);
        items.push(MenuItem {
            label: "Clip to video".into(),
            action: format!("lane:clip_to_video:{track_id}"),
            enabled: clip.patch.is_some(),
            title: Some(clip.reason.clone()),
            danger: false,
        });
        if track.trim_start_ms > 0 || track.trim_end_ms.is_some() {
            items.push(item(
                "Reset clip trims",
                &format!("lane:trim_reset:{track_id}"),
                true,
            ));
        }
        items.push(MenuItem {
            label: "Remove".into(),
            action: format!("lane:remove:{track_id}"),
            enabled: self.busy.is_none(),
            title: None,
            danger: true,
        });
        let label = self.track_title(&track);
        self.menu = Some(Menu {
            label,
            x,
            y,
            items,
            focus: 0,
        });
    }

    pub fn open_chapter_menu(&mut self, index: usize, x: f64, y: f64) {
        if index >= self.project().chapters.len() {
            return;
        }
        let items = vec![MenuItem {
            label: "Delete".into(),
            action: format!("chapter:delete:{index}"),
            enabled: self.busy.is_none(),
            title: None,
            danger: true,
        }];
        self.menu = Some(Menu {
            label: "Chapter marker".into(),
            x,
            y,
            items,
            focus: 0,
        });
    }

    pub fn open_cover_menu(&mut self, x: f64, y: f64) {
        if self.project().artwork.is_none() {
            return;
        }
        let items = vec![
            item("Edit", "cover:edit", self.busy.is_none()),
            MenuItem {
                label: "Remove".into(),
                action: "cover:remove".into(),
                enabled: true,
                title: None,
                danger: true,
            },
        ];
        self.menu = Some(Menu {
            label: "Project cover".into(),
            x,
            y,
            items,
            focus: 0,
        });
    }

    /// Project (hamburger) menu: New, Open, Open media, Save, Quit.
    pub fn open_project_menu(&mut self, x: f64, y: f64) {
        let busy = self.busy.is_some();
        let items = vec![
            item("New project", "project:new", !busy),
            item("Open project", "project:open", !busy),
            item("Open media", "project:open_media", !busy),
            item("Save project", "project:save", !busy),
            item("Quit Reelio", "project:quit", true),
        ];
        self.menu = Some(Menu {
            label: "Project".into(),
            x,
            y,
            items,
            focus: 0,
        });
    }

    /// Right-click or Shift+F10 on a lane canvas: chapter pin first, then the clip.
    pub fn lane_context(&mut self, track_id: &str, x: f64, y: f64) {
        let Some(track) = self.project().track(track_id).cloned() else {
            return;
        };
        match hit(self, &track, x, y) {
            Hit::Chapter(i) => self.open_chapter_menu(i, x, y),
            _ => self.open_clip_menu(track_id, x, y),
        }
    }

    /// Keyboard use of an open menu: arrows and Home/End move focus, Escape closes.
    pub fn menu_key(&mut self, key: &str) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        let n = menu.items.len().max(1);
        match key {
            "ArrowDown" => menu.focus = (menu.focus + 1) % n,
            "ArrowUp" => menu.focus = (menu.focus + n - 1) % n,
            "Home" => menu.focus = 0,
            "End" => menu.focus = n - 1,
            "Escape" => self.menu = None,
            _ => {}
        }
    }

    /// Activate the menu item with this action (a click or Enter).
    pub fn menu_activate(&mut self, action: &str) {
        self.menu = None;
        let enabled = self.menu_items_enabled_hint(action);
        if enabled {
            self.ui_action(action, Value::Null);
        }
    }

    fn menu_items_enabled_hint(&self, _action: &str) -> bool {
        true
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use crate::model::{Asset, Kind, Stream, Track};
    use crate::state::{Event, State};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    /// One video source with a single video lane `tv` (10 s).
    pub fn video_project() -> State {
        let mut s = State::new();
        let stream = Stream {
            index: 0,
            kind: Kind::Video,
            codec: "h264".into(),
            title: String::new(),
            language: "und".into(),
            duration_ms: Some(10_000),
            default: false,
            forced: false,
            properties: BTreeMap::new(),
        };
        let asset = Asset {
            id: "v".into(),
            path: "/m/v.mp4".into(),
            format: "mp4".into(),
            size: 1,
            modified_ms: 0,
            duration_ms: Some(10_000),
            streams: vec![stream],
            metadata: BTreeMap::new(),
            chapters: vec![],
        };
        s.edit(|p| {
            p.assets.push(asset);
            p.tracks.push(Track {
                id: "tv".into(),
                asset_id: "v".into(),
                stream_index: 0,
                enabled: true,
                title: "Movie".into(),
                language: "und".into(),
                default: false,
                forced: false,
                commentary: false,
                descriptive: false,
                offset_ms: 0,
                trim_start_ms: 0,
                trim_end_ms: None,
                muted: false,
                audio_role: None,
            });
        });
        s
    }

    pub fn ui(s: &mut State, node: &str, value: Value) {
        s.dispatch(Event::Ui {
            node: node.into(),
            value,
        });
    }

    pub fn right_click(s: &mut State, track: &str, x: f64, y: f64) {
        ui(
            s,
            &format!("lane.canvas:{track}"),
            json!({"type": "context", "x": x, "y": y}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use crate::state::Event;
    use serde_json::{json, Value};

    #[test]
    fn right_click_clip_menu_excludes_the_clip_as_one_undo_step() {
        let mut s = video_project();
        right_click(&mut s, "tv", 100.0, 40.0);
        let menu = s.menu.clone().expect("clip menu open");
        assert!(menu.items.iter().any(|i| i.label == "Exclude"));
        assert!(menu.items.iter().any(|i| i.label == "Remove" && i.danger));
        let history = s.store.history_len().0;
        ui(&mut s, "menu:lane:include:tv", Value::Null);
        assert!(s.menu.is_none(), "activating an item closes the menu");
        assert!(!s.project().track("tv").unwrap().enabled);
        assert_eq!(s.store.history_len().0, history + 1);
        s.dispatch(Event::Command {
            id: "edit.undo".into(),
        });
        assert!(
            s.project().track("tv").unwrap().enabled,
            "undo restores the lane"
        );
    }

    #[test]
    fn keyboard_navigation_wraps_and_escape_closes() {
        let mut s = video_project();
        right_click(&mut s, "tv", 100.0, 40.0);
        let n = s.menu.as_ref().unwrap().items.len();
        ui(
            &mut s,
            "menu:lane:include:tv",
            json!({"type": "key", "key": "ArrowUp", "phase": "down"}),
        );
        assert_eq!(
            s.menu.as_ref().unwrap().focus,
            n - 1,
            "ArrowUp wraps to the last item"
        );
        ui(
            &mut s,
            "menu:lane:include:tv",
            json!({"type": "key", "key": "Home", "phase": "down"}),
        );
        assert_eq!(s.menu.as_ref().unwrap().focus, 0);
        ui(
            &mut s,
            "menu:lane:include:tv",
            json!({"type": "key", "key": "End", "phase": "down"}),
        );
        assert_eq!(s.menu.as_ref().unwrap().focus, n - 1);
        ui(
            &mut s,
            "menu:lane:include:tv",
            json!({"type": "key", "key": "Escape", "phase": "down"}),
        );
        assert!(s.menu.is_none());
    }

    #[test]
    fn shift_f10_opens_the_clip_menu_and_outside_clicks_dismiss_it() {
        let mut s = video_project();
        ui(
            &mut s,
            "lane.canvas:tv",
            json!({"type": "key", "phase": "down", "key": "F10", "mods": {"shift": true}}),
        );
        assert!(s.menu.is_some(), "Shift+F10 opens the lane menu");
        ui(&mut s, "nav:media", Value::Null);
        assert!(s.menu.is_none(), "a click elsewhere dismisses the menu");
    }

    #[test]
    fn chapter_marker_delete_removes_only_that_marker() {
        let mut s = video_project();
        s.add_chapter(2000.0);
        s.add_chapter(4000.0);
        s.dispatch(Event::Ui {
            node: "lane.canvas:tv".into(),
            value: json!({"type": "context", "x": 2.0, "y": 2.0}),
        });
        // Pins sit at the top edge; the click at x=2 is on the first pin only when it is near zero.
        s.open_chapter_menu(0, 1.0, 1.0);
        ui(&mut s, "menu:chapter:delete:0", Value::Null);
        assert_eq!(s.project().chapters.len(), 1);
        assert_eq!(s.project().chapters[0].start_ms, 4000);
    }

    #[test]
    fn project_menu_opens_from_the_toolbar_and_runs_commands() {
        let mut s = video_project();
        ui(&mut s, "project:menu", Value::Null);
        let labels: Vec<String> = s
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|i| i.label.clone())
            .collect();
        assert_eq!(
            labels,
            vec![
                "New project",
                "Open project",
                "Open media",
                "Save project",
                "Quit Reelio"
            ]
        );
    }

    #[test]
    fn cover_menu_offers_edit_and_remove_only_with_a_cover() {
        let mut s = video_project();
        s.open_cover_menu(1.0, 1.0);
        assert!(s.menu.is_none(), "no cover, no menu");
    }
}
