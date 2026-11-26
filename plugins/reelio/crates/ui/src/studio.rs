//! Subtitle Studio: SRT document editing with a 40-step history, cue audition, list windowing and
//! engine-backed open/save. Port of `features/subtitles/SubtitleEditor.tsx` and `editorModel.ts`.
//! Controls use the reference node ids (`sub:<verb>[:<arg>]`); text and timing fields are controlled
//! inputs (`rev` is stamped by the transport).

use crate::engine::{self, Request};
use crate::state::{Pending, State};
use crate::subtitles::{self, SrtCue, SrtDocument};
use serde_json::Value;
use std::collections::BTreeSet;

/// Undo/redo depth of the studio (reference: 40 steps).
pub const STUDIO_HISTORY: usize = 40;
/// Rows materialized in the scene at once; the cue list is virtualized around the window.
pub const CUE_WINDOW: usize = 40;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Studio {
    pub doc: SrtDocument,
    pub saved: SrtDocument,
    pub past: Vec<SrtDocument>,
    pub future: Vec<SrtDocument>,
    pub selected: Option<String>,
    pub source_label: String,
    pub source_path: Option<String>,
    /// First visible cue index for the virtualized list.
    pub window_start: usize,
    pub notice: Option<String>,
    pub error: Option<String>,
    pub next_id: u64,
    /// Find/replace fields (literal, case-sensitive, selected or all cues).
    pub find: String,
    pub replacement: String,
    /// Shift amount for the shift control, in milliseconds.
    pub shift_ms: i64,
}

impl Studio {
    pub fn dirty(&self) -> bool {
        self.doc != self.saved
    }
}

impl State {
    fn studio_mut(&mut self) -> &mut Studio {
        self.studio.get_or_insert_with(Studio::default)
    }

    fn studio_set_error(&mut self, text: impl Into<String>) {
        self.studio_mut().error = Some(text.into());
        self.studio_mut().notice = None;
    }

    fn studio_edit(&mut self, next: SrtDocument) {
        let st = self.studio_mut();
        if next == st.doc {
            return;
        }
        let prior = std::mem::replace(&mut st.doc, next);
        st.past.push(prior);
        if st.past.len() > STUDIO_HISTORY {
            st.past.remove(0);
        }
        st.future.clear();
        st.error = None;
        self.studio_dirty_sync();
    }

    /// Mirror unsaved work to the engine so close protection and recovery apply.
    fn studio_dirty_sync(&mut self) {
        let dirty = self.studio.as_ref().map(|s| s.dirty()).unwrap_or(false);
        self.engine_request_with(Request::SubtitleEditorDirty { dirty }, Pending::Silent);
    }

    fn studio_fresh_id(&mut self) -> String {
        let st = self.studio_mut();
        st.next_id += 1;
        format!("cue-{}", st.next_id)
    }

    /// Open a document (from the engine reply or a test fixture).
    pub fn studio_load(&mut self, doc: SrtDocument, label: &str, path: Option<String>) {
        let st = self.studio_mut();
        st.saved = doc.clone();
        st.doc = doc;
        st.past.clear();
        st.future.clear();
        st.selected = st.doc.cues.first().map(|c| c.id.clone());
        st.source_label = label.into();
        st.source_path = path;
        st.window_start = 0;
        st.error = None;
        st.notice = Some(format!("Loaded {label}. The original file is unchanged."));
    }

    /// Dispatch a `sub:*` control.
    pub fn studio_action(&mut self, arg: &str, value: Value) {
        let (verb, rest) = arg
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((arg.to_string(), String::new()));
        match verb.as_str() {
            "open" => self.outbox_engine(
                Request::SubtitleEditorOpen { path: None },
                Pending::SubtitleOpen,
            ),
            "choose" => self.outbox_engine(Request::ChooseSubtitles, Pending::SubtitleChoose),
            "new" => {
                self.studio_load(SrtDocument::default(), "New subtitle document", None);
                self.studio_mut().notice =
                    Some("New SRT document. Add cues, then save a copy.".into());
            }
            "select" => {
                self.studio_mut().selected = Some(rest.clone());
                if let Some(cue) = self.studio_cue(&rest) {
                    let ms = cue.start_ms as f64;
                    self.seek_playhead(ms);
                }
            }
            "window" | "list" => {
                let first = value.get("first").and_then(Value::as_u64).unwrap_or(0) as usize;
                self.studio_mut().window_start = first;
            }
            "find" => self.studio_mut().find = value.as_str().unwrap_or("").to_string(),
            "replacement" => {
                self.studio_mut().replacement = value.as_str().unwrap_or("").to_string()
            }
            "shift_amount" => self.studio_mut().shift_ms = value.as_i64().unwrap_or(0),
            "text" => {
                let text = value.as_str().unwrap_or("").to_string();
                self.studio_patch_cue(&rest, |c| c.text = text.clone());
            }
            "start" | "end" => {
                let text = value.as_str().unwrap_or("").to_string();
                match subtitles::parse_cue_clock(&text) {
                    Ok(ms) => {
                        let start = verb == "start";
                        let ok = self.studio_patch_cue(&rest, |c| {
                            if start {
                                c.start_ms = ms
                            } else {
                                c.end_ms = ms
                            }
                        });
                        if !ok {
                            self.studio_set_error("Unknown cue.");
                        }
                    }
                    Err(e) => self.studio_set_error(e),
                }
            }
            "add" => {
                let id = self.studio_fresh_id();
                let mut doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                let start = doc.cues.last().map(|c| c.end_ms).unwrap_or(0);
                doc.cues.push(SrtCue {
                    id: id.clone(),
                    start_ms: start,
                    end_ms: start + 2000,
                    text: "New subtitle".into(),
                });
                self.studio_edit(doc);
                self.studio_mut().selected = Some(id);
            }
            "delete" => {
                let mut doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                doc.cues.retain(|c| c.id != rest);
                self.studio_edit(doc);
            }
            "split" => {
                let at = self.media_time.round() as i64;
                let caret = value
                    .get("caret")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                let id = self.studio_fresh_id();
                let doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                let caret = caret.unwrap_or_else(|| {
                    self.studio_cue(&rest)
                        .map(|c| c.text.chars().count() / 2)
                        .unwrap_or(0)
                });
                match subtitles::split_cue(&doc, &rest, at, caret, || id.clone()) {
                    Ok(next) => self.studio_edit(next),
                    Err(e) => self.studio_set_error(e),
                }
            }
            "merge" => {
                let doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                match subtitles::merge_cue(&doc, &rest) {
                    Ok(next) => self.studio_edit(next),
                    Err(e) => self.studio_set_error(e),
                }
            }
            "shift" => {
                let ms = value
                    .as_i64()
                    .unwrap_or_else(|| self.studio.as_ref().map(|s| s.shift_ms).unwrap_or(0));
                let doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                let ids = self.studio_target_ids(&rest);
                match subtitles::shift_cues(&doc, &ids, ms) {
                    Ok(next) => self.studio_edit(next),
                    Err(e) => self.studio_set_error(e),
                }
            }
            "replace" => {
                let find = self
                    .studio
                    .as_ref()
                    .map(|s| s.find.clone())
                    .unwrap_or_default();
                let with = self
                    .studio
                    .as_ref()
                    .map(|s| s.replacement.clone())
                    .unwrap_or_default();
                let doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                let ids = self.studio_target_ids(&rest);
                match subtitles::replace_text(&doc, &find, &with, &ids) {
                    Ok(next) => self.studio_edit(next),
                    Err(e) => self.studio_set_error(e),
                }
            }
            "undo" => {
                if let Some(st) = self.studio.as_mut() {
                    if let Some(prior) = st.past.pop() {
                        let current = std::mem::replace(&mut st.doc, prior);
                        st.future.push(current);
                    }
                }
                self.studio_dirty_sync();
            }
            "redo" => {
                if let Some(st) = self.studio.as_mut() {
                    if let Some(next) = st.future.pop() {
                        let current = std::mem::replace(&mut st.doc, next);
                        st.past.push(current);
                    }
                }
                self.studio_dirty_sync();
            }
            "discard" => {
                if let Some(st) = self.studio.as_mut() {
                    st.doc = st.saved.clone();
                    st.past.clear();
                    st.future.clear();
                }
                self.studio_dirty_sync();
            }
            "save" => {
                let doc = self
                    .studio
                    .as_ref()
                    .map(|s| s.doc.clone())
                    .unwrap_or_default();
                let issues = subtitles::cue_issues(&doc);
                if !issues.is_empty() {
                    self.studio_set_error(issues.join(" "));
                    return;
                }
                let document = serde_json::to_value(&doc).unwrap_or(Value::Null);
                self.outbox_engine(
                    Request::SubtitleEditorSave { document },
                    Pending::SubtitleSave,
                );
            }
            _ => {}
        }
    }

    fn outbox_engine(&mut self, request: Request, pending: Pending) {
        self.engine_request_with(request, pending);
    }

    pub fn studio_cue(&self, id: &str) -> Option<SrtCue> {
        self.studio
            .as_ref()?
            .doc
            .cues
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }

    fn studio_patch_cue(&mut self, id: &str, f: impl FnOnce(&mut SrtCue)) -> bool {
        let mut doc = match self.studio.as_ref() {
            Some(s) => s.doc.clone(),
            None => return false,
        };
        let Some(cue) = doc.cues.iter_mut().find(|c| c.id == id) else {
            return false;
        };
        f(cue);
        self.studio_edit(doc);
        true
    }

    /// Selected cues when `rest` names one, otherwise every cue (shift/replace all).
    fn studio_target_ids(&self, rest: &str) -> BTreeSet<String> {
        if !rest.is_empty() {
            return [rest.to_string()].into();
        }
        self.studio
            .as_ref()
            .map(|s| s.doc.cues.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default()
    }

    /// Engine replies for the studio.
    pub fn studio_reply(&mut self, pending: &Pending, reply: &engine::Reply) -> bool {
        match pending {
            Pending::SubtitleOpen | Pending::SubtitleChoose => {
                match reply {
                    Ok(v) if v.is_null() => {}
                    Ok(v) => {
                        let (doc, path) = if pending == &Pending::SubtitleOpen {
                            (
                                serde_json::from_value::<SrtDocument>(
                                    v.get("document").cloned().unwrap_or(Value::Null),
                                ),
                                v.get("path")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            )
                        } else {
                            let path = v
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            (Ok(SrtDocument::default()), path)
                        };
                        match doc {
                            Ok(d) if !path.is_empty() => {
                                let label = crate::model::basename(&path);
                                self.studio_load(d, &label, Some(path));
                            }
                            _ => self.studio_set_error(
                                "The subtitle file could not be read as UTF-8 SubRip.",
                            ),
                        }
                    }
                    Err(e) => self.studio_set_error(e.clone()),
                }
                true
            }
            Pending::SubtitleSave => {
                match reply {
                    Ok(v) if v.is_null() => {}
                    Ok(v) => {
                        let path = v.as_str().unwrap_or("").to_string();
                        let st = self.studio_mut();
                        st.saved = st.doc.clone();
                        st.notice = Some(format!(
                            "Saved a new SRT copy: {}",
                            crate::model::basename(&path)
                        ));
                        st.error = None;
                        self.studio_dirty_sync();
                    }
                    Err(e) => self.studio_set_error(e.clone()),
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::state::{Effect, Event, State};
    use serde_json::{json, Value};

    fn fixture_doc() -> Value {
        json!({"cues": [
            {"id": "a", "start_ms": 1000, "end_ms": 3000, "text": "Hello there"},
            {"id": "b", "start_ms": 4000, "end_ms": 6000, "text": "General Kenobi"}
        ]})
    }

    fn ui(s: &mut State, node: &str, value: Value) {
        s.dispatch(Event::Ui {
            node: node.into(),
            value,
        });
    }

    /// Answer the most recent engine request with `reply` (the fake engine of the studio tests).
    fn answer_last(s: &mut State, reply: Value) {
        let id = s
            .drain()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Engine { id, request }
                    if !matches!(request, crate::engine::Request::SubtitleEditorDirty { .. }) =>
                {
                    Some(id)
                }
                _ => None,
            })
            .last()
            .expect("an engine request was queued");
        s.dispatch(Event::Engine {
            id,
            reply: Ok(reply),
        });
    }

    fn opened(s: &mut State) {
        ui(s, "sub:open", Value::Null);
        answer_last(
            s,
            json!({"document": fixture_doc(), "path": "/subs/movie.srt"}),
        );
    }

    #[test]
    fn opening_a_document_through_the_engine_loads_every_cue_without_pagination() {
        let mut s = State::new();
        opened(&mut s);
        let st = s.studio.as_ref().unwrap();
        assert_eq!(st.doc.cues.len(), 2);
        assert_eq!(st.source_label, "movie.srt");
        assert!(!st.dirty());
    }

    #[test]
    fn cue_text_and_timing_are_controlled_edits_with_undo_and_redo() {
        let mut s = State::new();
        opened(&mut s);
        ui(&mut s, "sub:text:a", json!("Hi there"));
        assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].text, "Hi there");
        ui(&mut s, "sub:start:a", json!("00:00:01,500"));
        assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].start_ms, 1500);
        ui(&mut s, "sub:start:a", json!("1:2"));
        assert!(
            s.studio.as_ref().unwrap().error.is_some(),
            "malformed clock is reported"
        );
        ui(&mut s, "sub:undo", Value::Null);
        assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].start_ms, 1000);
        ui(&mut s, "sub:redo", Value::Null);
        assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].start_ms, 1500);
    }

    #[test]
    fn split_merge_shift_and_replace_match_the_editor_model() {
        let mut s = State::new();
        opened(&mut s);
        s.media_time = 2000.0;
        ui(&mut s, "sub:split:a", json!({"caret": 5}));
        let doc = &s.studio.as_ref().unwrap().doc;
        assert_eq!(doc.cues.len(), 3);
        assert_eq!(doc.cues[0].text, "Hello");
        assert_eq!(doc.cues[0].end_ms, 2000);
        ui(&mut s, "sub:merge:a", Value::Null);
        assert_eq!(s.studio.as_ref().unwrap().doc.cues.len(), 2);
        ui(&mut s, "sub:shift_amount", json!(500));
        ui(&mut s, "sub:shift", Value::Null);
        assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].start_ms, 1500);
        ui(&mut s, "sub:find", json!("Kenobi"));
        ui(&mut s, "sub:replacement", json!("Obi-Wan"));
        ui(&mut s, "sub:replace", Value::Null);
        assert_eq!(
            s.studio.as_ref().unwrap().doc.cues[1].text,
            "General Obi-Wan"
        );
    }

    #[test]
    fn history_keeps_forty_steps_and_discard_restores_the_saved_baseline() {
        let mut s = State::new();
        opened(&mut s);
        for i in 0..45 {
            ui(&mut s, "sub:text:a", json!(format!("v{i}")));
        }
        assert_eq!(s.studio.as_ref().unwrap().past.len(), super::STUDIO_HISTORY);
        ui(&mut s, "sub:discard", Value::Null);
        assert_eq!(s.studio.as_ref().unwrap().doc.cues[0].text, "Hello there");
        assert!(!s.studio.as_ref().unwrap().dirty());
    }

    #[test]
    fn saving_is_refused_while_issues_remain_and_marks_clean_on_success() {
        let mut s = State::new();
        opened(&mut s);
        ui(&mut s, "sub:text:b", json!(""));
        ui(&mut s, "sub:save", Value::Null);
        assert!(s
            .studio
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("enter subtitle text"));
        ui(&mut s, "sub:text:b", json!("Fixed"));
        ui(&mut s, "sub:save", Value::Null);
        answer_last(&mut s, json!("/subs/movie.copy.srt"));
        let st = s.studio.as_ref().unwrap();
        assert!(!st.dirty());
        assert!(st.notice.as_ref().unwrap().contains("movie.copy.srt"));
    }

    #[test]
    fn list_window_events_move_the_virtualized_rows() {
        let mut s = State::new();
        opened(&mut s);
        s.section = crate::state::Section::Subtitles;
        ui(&mut s, "sub:select:b", Value::Null);
        ui(
            &mut s,
            "sub:list",
            json!({"type": "list.window", "first": 1, "last": 2}),
        );
        assert_eq!(s.studio.as_ref().unwrap().window_start, 1);
        let scene = crate::scene::main_view(&s).to_string();
        assert!(
            scene.contains("General Kenobi"),
            "scene: {}",
            &scene[scene.find("sub:list").unwrap_or(0)
                ..(scene.find("sub:list").unwrap_or(0) + 400).min(scene.len())]
        );
        assert!(
            !scene.contains("Hello there"),
            "rows outside the window are not materialized"
        );
    }

    #[test]
    fn unsaved_cues_make_quit_ask_for_confirmation() {
        let mut s = State::new();
        opened(&mut s);
        ui(&mut s, "sub:text:a", json!("changed"));
        s.dispatch(Event::Quit);
        assert!(s.dialogs.quit_confirm);
    }
}
