//! Subtitle search/import and the local model review workflow.
use crate::{
    engine::{self, Request},
    state::{Pending, Section, State},
    subtitles::SrtDocument,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ModelJob {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub done: usize,
    pub total: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StudioWorkflow {
    pub search_open: bool,
    pub query: String,
    pub url: String,
    pub files: Vec<(String, String)>,
    pub saved_copy: Option<String>,
    pub models: Vec<(String, bool)>,
    pub model: String,
    pub language: String,
    pub job: ModelJob,
    pub starting: bool,
    pub poll_in_flight: bool,
    pub next_poll: f64,
    pub checked_status: bool,
    pub review: Option<Review>,
    pub review_open: bool,
    pub review_page: usize,
    pub playing_cue: Option<String>,
    pub reveal_cue: Option<(String, u64)>,
    pub draft_due: Option<f64>,
    pub draft_writing: bool,
    pub loaded_draft: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    pub document: SrtDocument,
    pub translations: BTreeMap<String, String>,
    pub accepted: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StudioRequest {
    Search,
    Offer,
    Import,
    Embedded { project: String },
    Models,
    ModelStart,
    ModelStatus,
    ModelCancel,
    ModelResults(String),
    DraftLoad,
    DraftWrite,
}

impl State {
    fn studio_request(&mut self, request: Request, pending: StudioRequest) {
        self.engine_request_with(request, Pending::Studio(pending));
    }

    pub fn studio_workflow_action(&mut self, verb: &str, rest: &str, value: &Value) {
        match verb {
            "search_toggle" => {
                let empty = self.studio_mut().workflow.query.is_empty();
                if empty {
                    self.studio_mut().workflow.query = self.project().name.clone();
                }
                let w = &mut self.studio_mut().workflow;
                w.search_open = !w.search_open;
            }
            "query" => {
                self.studio_mut().workflow.query = value
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect()
            }
            "url" => {
                self.studio_mut().workflow.url = value
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(4096)
                    .collect()
            }
            "search" => {
                let query = self.studio_mut().workflow.query.trim().to_owned();
                if !query.is_empty() {
                    self.studio_request(
                        Request::SearchBrowserOpen {
                            kind: "subtitles".into(),
                            query,
                        },
                        StudioRequest::Search,
                    );
                }
            }
            "offer" => {
                let url = self.studio_mut().workflow.url.trim().to_owned();
                if !url.is_empty() {
                    self.studio_request(
                        Request::SearchBrowserDownload {
                            kind: "subtitles".into(),
                            url,
                        },
                        StudioRequest::Offer,
                    );
                }
            }
            "import" => self.studio_request(Request::ChooseSubtitles, StudioRequest::Import),
            "add_file" => {
                let file = rest
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.studio_mut().workflow.files.get(i))
                    .map(|(_, path)| path.clone());
                if let Some(path) = file {
                    self.import_and_test_subtitle(path);
                }
            }
            "add_saved" => {
                if let Some(path) = self.studio_mut().workflow.saved_copy.clone() {
                    self.import_and_test_subtitle(path);
                }
            }
            "test_track" => self.select_track_by_id(rest),
            "seek" => {
                if let Ok(ms) = rest.split(':').next().unwrap_or_default().parse::<i64>() {
                    self.set_media_time(ms as f64);
                }
            }
            "models" => self.studio_request(Request::SubtitleModels, StudioRequest::Models),
            "model" => self.studio_mut().workflow.model = value.as_str().unwrap_or_default().into(),
            "language" => {
                self.studio_mut().workflow.language = value
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(80)
                    .collect()
            }
            "translate" | "pull" if !self.studio_model_running() => {
                let st = self.studio_mut();
                let model = st.workflow.model.clone();
                let language = st.workflow.language.trim().to_owned();
                let document = if verb == "translate" {
                    if st.doc.cues.is_empty() || language.is_empty() {
                        self.studio_set_error(
                            "Add cues and enter a target language before translating.",
                        );
                        return;
                    }
                    serde_json::to_value(&st.doc).ok()
                } else {
                    None
                };
                if model.is_empty() {
                    self.studio_set_error("Refresh local models and choose a model first.");
                    return;
                }
                st.workflow.starting = true;
                self.studio_request(
                    Request::SubtitleModelStart {
                        model,
                        document,
                        language,
                    },
                    StudioRequest::ModelStart,
                );
            }
            "model_cancel" if self.studio_model_running() => {
                self.studio_request(Request::SubtitleModelCancel, StudioRequest::ModelCancel)
            }
            "review" => {
                let w = &mut self.studio_mut().workflow;
                w.review_open = true;
                w.review_page = 0;
            }
            "review_next" | "review_prev" => {
                let w = &mut self.studio_mut().workflow;
                if verb == "review_prev" {
                    w.review_page = w.review_page.saturating_sub(1);
                } else if w
                    .review
                    .as_ref()
                    .is_some_and(|r| (w.review_page + 1) * 10 < r.translations.len())
                {
                    w.review_page += 1;
                }
            }
            "review_accept" => {
                if let Some(review) = self.studio_mut().workflow.review.as_mut() {
                    if review.translations.contains_key(rest) && value.as_bool() == Some(true) {
                        review.accepted.insert(rest.into());
                    } else {
                        review.accepted.remove(rest);
                    }
                }
            }
            "review_apply" | "review_replace" => {
                let st = self.studio_mut();
                let Some(review) = st.workflow.review.as_ref() else {
                    return;
                };
                if st.doc != review.document && verb != "review_replace" {
                    self.studio_set_error("This result belongs to a different source snapshot. Use Replace with reviewed snapshot only if you want to restore it.");
                    return;
                }
                let mut doc = review.document.clone();
                for cue in &mut doc.cues {
                    if review.accepted.contains(&cue.id) {
                        if let Some(text) = review.translations.get(&cue.id) {
                            cue.text.clone_from(text);
                        }
                    }
                }
                self.studio_edit(doc);
                self.studio_mut().workflow.review_open = false;
            }
            _ => {}
        }
    }

    pub fn studio_open_track(&mut self, track_id: &str) {
        if self.studio_model_running() {
            return;
        }
        if self.studio.as_ref().is_some_and(|st| st.dirty()) {
            self.studio_set_error(
                "Save or discard the current subtitle draft before opening another track.",
            );
            self.studio_enter();
            return;
        }
        let mut project = self.project().clone();
        let Some(track) = project.track(track_id).cloned() else {
            return;
        };
        if project.track_kind(&track) != Some(crate::model::Kind::Subtitle) {
            return;
        }
        let path = project
            .track_asset(&track)
            .map(|asset| asset.path.clone())
            .unwrap_or_default();
        self.studio_enter();
        if path.to_ascii_lowercase().ends_with(".srt") && track.stream_index == 0 {
            self.engine_request_with(
                Request::SubtitleEditorOpen { path: Some(path) },
                Pending::SubtitleOpen,
            );
        } else {
            if let Some(t) = project.tracks.iter_mut().find(|t| t.id == track_id) {
                t.offset_ms = 0;
            }
            let project_id = project.id.clone();
            self.studio_request(
                Request::SubtitlePreview {
                    project: serde_json::to_value(project).unwrap_or_default(),
                    track_id: track_id.into(),
                },
                StudioRequest::Embedded {
                    project: project_id,
                },
            );
        }
    }

    pub fn studio_model_running(&self) -> bool {
        self.studio
            .as_ref()
            .is_some_and(|st| st.workflow.starting || st.workflow.job.status == "running")
    }

    pub fn studio_tick(&mut self) {
        let Some(st) = self.studio.as_mut() else {
            return;
        };
        let mut poll = false;
        let mut draft = None;
        if (!st.workflow.checked_status || st.workflow.job.status == "running")
            && !st.workflow.starting
            && !st.workflow.poll_in_flight
            && self.now >= st.workflow.next_poll
        {
            st.workflow.poll_in_flight = true;
            st.workflow.next_poll = self.now + 500.0;
            poll = true;
        }
        if !st.workflow.draft_writing && st.workflow.draft_due.is_some_and(|due| self.now >= due) {
            st.workflow.draft_writing = true;
            st.workflow.draft_due = None;
            draft = Some(if st.dirty() {
                serde_json::to_value(&st.doc).ok()
            } else {
                None
            });
        }
        if poll {
            self.studio_request(Request::SubtitleModelStatus, StudioRequest::ModelStatus);
        }
        if let Some(document) = draft {
            self.studio_request(
                Request::SubtitleEditorDraft {
                    clear: document.is_none(),
                    document,
                },
                StudioRequest::DraftWrite,
            );
        }
    }

    pub fn studio_follow(&mut self) {
        if self.section != Section::Subtitles {
            return;
        }
        let Some(st) = self.studio.as_mut() else {
            return;
        };
        let ms = self.media_time as i64;
        let active = st
            .doc
            .cues
            .iter()
            .enumerate()
            .find(|(_, c)| c.start_ms <= ms && ms < c.end_ms);
        let id = active.map(|(_, c)| c.id.clone());
        if id != st.workflow.playing_cue {
            if let Some((i, cue)) = active {
                if i < st.window_start || i >= st.window_start + crate::studio::CUE_WINDOW {
                    st.window_start = i.saturating_sub(2);
                }
                st.workflow.reveal_cue = Some((
                    cue.id.clone(),
                    st.workflow.reveal_cue.as_ref().map_or(0, |r| r.1 + 1),
                ));
            }
            st.workflow.playing_cue = id;
        }
    }

    pub fn studio_enter(&mut self) {
        self.section = Section::Subtitles;
        if !self.studio_mut().workflow.loaded_draft {
            self.studio_mut().workflow.loaded_draft = true;
            self.studio_request(
                Request::SubtitleEditorDraft {
                    document: None,
                    clear: false,
                },
                StudioRequest::DraftLoad,
            );
        }
    }

    pub fn studio_workflow_reply(&mut self, pending: StudioRequest, reply: engine::Reply) {
        match pending {
            StudioRequest::ModelStart => self.studio_mut().workflow.starting = false,
            StudioRequest::ModelStatus => {
                let w = &mut self.studio_mut().workflow;
                w.poll_in_flight = false;
                w.checked_status = true;
            }
            StudioRequest::DraftWrite => self.studio_mut().workflow.draft_writing = false,
            _ => {}
        }
        let v = match reply {
            Ok(v) => v,
            Err(e) => {
                self.studio_set_error(e);
                return;
            }
        };
        match pending {
            StudioRequest::Search => self.studio_mut().notice = Some("Search opened in your browser. Paste a public HTTPS subtitle download link here, or save it locally and choose Import subtitle files.".into()),
            StudioRequest::Offer => match engine::decode(v) { Ok(info) => self.queue_resource(info), Err(e) => self.studio_set_error(e) },
            StudioRequest::Import => self.studio_add_candidates(&v),
            StudioRequest::Embedded { project } if self.project().id == project => {
                if self.studio.as_ref().is_some_and(|st| st.dirty()) { return; }
                match crate::subtitles::inspect_vtt(v.as_str().unwrap_or_default()) {
                    Ok(cues) => {
                        let doc = SrtDocument { cues: cues.into_iter().enumerate().map(|(i, c)| crate::subtitles::SrtCue { id: format!("embedded-{}", i + 1), start_ms: c.start, end_ms: c.end, text: c.text }).collect() };
                        self.studio_load(SrtDocument::default(), "Editable subtitle copy", None);
                        self.studio_edit(doc);
                        let st = self.studio_mut();
                        st.selected = st.doc.cues.first().map(|c| c.id.clone());
                        st.notice = Some("Opened a converted text-subtitle copy. Original timing is preserved; styling may differ.".into());
                    }
                    Err(e) => self.studio_set_error(e),
                }
            }
            StudioRequest::Models => {
                let installed = v["installed"].as_array().cloned().unwrap_or_default();
                let models = v["supported"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|name| {
                    (name.to_owned(), installed.iter().any(|m| m["name"] == name))
                }).collect::<Vec<_>>();
                let w = &mut self.studio_mut().workflow;
                if !models.iter().any(|(name, _)| *name == w.model) { w.model = models.first().map(|(n, _)| n.clone()).unwrap_or_default(); }
                w.models = models;
            }
            StudioRequest::ModelStart | StudioRequest::ModelStatus => {
                let Ok(job) = engine::decode::<ModelJob>(v) else { self.studio_set_error("Invalid local model job report"); return };
                let st = self.studio_mut();
                if pending == StudioRequest::ModelStatus && st.workflow.starting { return; }
                let fetch = job.kind == "translate" && job.done > 0 && job.status != "running"
                    && (st.workflow.job.id != job.id || st.workflow.job.status == "running");
                let id = job.id.clone();
                if pending == StudioRequest::ModelStart {
                    st.workflow.review = None;
                    st.workflow.review_open = false;
                }
                st.workflow.job = job;
                if fetch { self.studio_request(Request::SubtitleModelResults { id: id.clone() }, StudioRequest::ModelResults(id)); }
            }
            StudioRequest::ModelCancel => {
                let w = &mut self.studio_mut().workflow;
                w.checked_status = false;
                w.next_poll = 0.0;
            }
            StudioRequest::ModelResults(id) => {
                let st = self.studio_mut();
                if st.workflow.job.id != id { return; }
                let Ok(document) = engine::decode::<SrtDocument>(v["document"].clone()) else { self.studio_set_error("Invalid translated source snapshot"); return };
                let translations: BTreeMap<String, String> = v["translations"].as_array().into_iter().flatten()
                    .filter_map(|t| Some((t["id"].as_str()?.to_owned(), t["text"].as_str()?.to_owned()))).collect();
                let accepted = translations.keys().cloned().collect();
                st.workflow.review = Some(Review { document, translations, accepted });
            }
            StudioRequest::DraftLoad if !v.is_null() => {
                if let Ok(doc) = engine::decode::<SrtDocument>(v) {
                    let st = self.studio_mut();
                    if !st.dirty() && st.source_label.is_empty() {
                        st.selected = doc.cues.first().map(|c| c.id.clone());
                        st.doc = doc;
                        st.notice = Some("Recovered an unsaved subtitle draft.".into());
                    }
                }
            }
            _ => {}
        }
    }

    pub fn studio_add_candidates(&mut self, files: &Value) {
        for f in files.as_array().into_iter().flatten().take(32) {
            if let (Some(name), Some(path)) = (f["name"].as_str(), f["path"].as_str()) {
                let w = &mut self.studio_mut().workflow;
                if !w.files.iter().any(|(_, p)| p == path) && w.files.len() < 64 {
                    w.files.push((name.into(), path.into()));
                }
            } else if let Some(error) = f["error"].as_str() {
                self.studio_set_error(error);
            }
        }
    }
}
