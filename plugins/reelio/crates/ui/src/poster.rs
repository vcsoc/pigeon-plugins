//! Poster & cover studio: provider search (engine `poster_search`), cached pages with previous and
//! next, orientation and ratio filters, 12/24/48 page sizes, one-click apply (`poster_image`) with
//! project undo, and rotate/flip through `artwork_edit`. Port of `features/artwork/*`.

use crate::engine::{self, Request};
use crate::model::{Artwork, Project};
use crate::posters::{Orientation, Proportion};
use crate::state::{Pending, State};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct PosterState {
    pub query: String,
    pub proportion: Proportion,
    pub page_size: usize,
    /// Cached pages of raw provider results (`{title, source_host, image_url, width, height}`).
    pub pages: Vec<Vec<Value>>,
    pub page: usize,
    pub next_offset: u32,
    pub has_more: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
    pub pending: bool,
    pub editor: Option<crate::cover_editor::CoverEditor>,
    pub browser: bool,
    pub url: String,
    pub generation: u64,
    search_version: u64,
    page_buffer: Vec<Value>,
    seen: std::collections::BTreeSet<String>,
    batches: usize,
}

impl Default for PosterState {
    fn default() -> Self {
        PosterState {
            query: String::new(),
            proportion: Proportion::Any,
            page_size: 12,
            pages: vec![],
            page: 0,
            next_offset: 0,
            has_more: true,
            notice: None,
            error: None,
            pending: false,
            editor: None,
            browser: true,
            url: String::new(),
            generation: 0,
            search_version: 0,
            page_buffer: vec![],
            seen: Default::default(),
            batches: 0,
        }
    }
}

fn parse_proportion(text: &str) -> Proportion {
    match text {
        "landscape" => Proportion::Landscape,
        "portrait" => Proportion::Portrait,
        "square" => Proportion::Square,
        "any" => Proportion::Any,
        other => {
            let mut parts = other.split(':');
            match (
                parts.next().and_then(|v| v.parse().ok()),
                parts.next().and_then(|v| v.parse().ok()),
            ) {
                (Some(w), Some(h)) => Proportion::Ratio(w, h),
                _ => Proportion::Any,
            }
        }
    }
}

pub fn thumbnail_rid(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hash);
    format!("poster:thumb:{:016x}", hash.finish())
}

/// Provider string for a filter (`landscape`, `4:3`, ...), as the engine expects it.
pub fn proportion_key(p: Proportion) -> String {
    match p {
        Proportion::Any => "any".into(),
        Proportion::Landscape => "landscape".into(),
        Proportion::Portrait => "portrait".into(),
        Proportion::Square => "square".into(),
        Proportion::Ratio(w, h) => format!("{w}:{h}"),
    }
}

impl State {
    pub(crate) fn poster_mut(&mut self) -> &mut PosterState {
        self.poster.get_or_insert_with(PosterState::default)
    }

    fn poster_request_next(&mut self) {
        let (query, proportion, offset, version) = {
            let p = self.poster.get_or_insert_with(PosterState::default);
            p.pending = true;
            p.batches += 1;
            (
                p.query.clone(),
                proportion_key(p.proportion),
                p.next_offset,
                p.search_version,
            )
        };
        self.engine_request_with(
            Request::PosterSearch {
                query,
                proportion,
                offset,
            },
            Pending::PosterSearch { version },
        );
    }

    /// Start a new search from page one (query, proportion or page-size changed).
    fn poster_restart(&mut self) {
        let p = self.poster_mut();
        p.pages.clear();
        p.page_buffer.clear();
        p.seen.clear();
        p.batches = 0;
        p.pending = false;
        p.search_version += 1;
        p.page = 0;
        p.next_offset = 0;
        p.has_more = true;
        p.error = None;
        if !p.query.trim().is_empty() {
            self.poster_request_next();
        }
    }

    pub fn poster_action(&mut self, arg: &str, value: Value) {
        let (verb, rest) = arg
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((arg.to_string(), String::new()));
        match verb.as_str() {
            "query" => {
                let text = value.as_str().unwrap_or("").chars().take(160).collect();
                self.poster_mut().query = text;
            }
            "search" => {
                if self
                    .poster
                    .as_ref()
                    .map(|p| p.query.trim().is_empty())
                    .unwrap_or(true)
                {
                    self.poster_mut().error = Some("Enter the movie title and year first.".into());
                    return;
                }
                self.poster_restart();
            }
            "orient" => {
                let dir = if rest == "landscape" {
                    Orientation::Landscape
                } else {
                    Orientation::Portrait
                };
                let next = self.poster_mut().proportion.toggle_orientation(dir);
                self.poster_mut().proportion = next;
                self.poster_restart();
            }
            "ratio" => {
                self.poster_mut().proportion = parse_proportion(value.as_str().unwrap_or("any"));
                self.poster_restart();
            }
            "size" => {
                let size = value
                    .as_u64()
                    .or_else(|| value.as_str()?.parse().ok())
                    .unwrap_or(12) as usize;
                if [12, 24, 48].contains(&size) {
                    self.poster_mut().page_size = size;
                    self.poster_restart();
                } else {
                    self.poster_mut().error = Some("Choose 12, 24 or 48 results per page".into());
                }
            }
            "next" => {
                let (has_cached, has_more, page, pages) = {
                    let p = self.poster_mut();
                    (
                        p.page + 1 < p.pages.len(),
                        p.has_more,
                        p.page,
                        p.pages.len(),
                    )
                };
                if has_cached {
                    self.poster_mut().page = page + 1;
                } else if has_more && pages < 40 && !self.poster_mut().pending {
                    self.poster_mut().batches = 0;
                    self.poster_request_next();
                }
            }
            "prev" => {
                let p = self.poster_mut();
                if p.page > 0 {
                    p.page -= 1;
                }
            }
            "use" => {
                let index: usize = rest.parse().unwrap_or(usize::MAX);
                let url = self
                    .poster
                    .as_ref()
                    .and_then(|p| p.pages.get(p.page))
                    .and_then(|page| page.get(index))
                    .and_then(|r| r.get("image_url"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                match url {
                    Some(url) if self.busy.is_none() => {
                        let proportion = self.poster.as_ref().map(|p| proportion_key(p.proportion));
                        self.busy = Some("Downloading and validating the selected cover".into());
                        self.engine_request_with(
                            Request::PosterImage { url, proportion },
                            Pending::PosterImage,
                        );
                    }
                    Some(_) => {}
                    None => {
                        self.poster_mut().error = Some("That result is no longer available.".into())
                    }
                }
            }
            "browser" => {
                let name = self.project().name.clone();
                let p = self.poster_mut();
                p.browser = !p.browser;
                if p.query.is_empty() {
                    p.query = name;
                }
            }
            _ => self.cover_edit_action(&verb, &rest, &value),
        }
    }

    /// Set the project cover (one undo step) and leave the studio.
    pub fn apply_cover_artwork(&mut self, artwork: Artwork, notice: &str) {
        self.edit(|d: &mut Project| d.artwork = Some(artwork));
        self.incoming_artwork = None;
        self.set_notice(notice);
    }

    pub fn poster_reply(&mut self, pending: &Pending, reply: &engine::Reply) {
        match pending {
            Pending::PosterSearch { version } => {
                let p = self.poster_mut();
                if p.search_version != *version {
                    return;
                }
                p.pending = false;
                match reply.clone().and_then(|v| {
                    let results: Vec<Value> = v
                        .get("results")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let next = v.get("next_offset").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let more = v.get("has_more").and_then(Value::as_bool).unwrap_or(false);
                    Ok((results, next, more))
                }) {
                    Ok((results, next, more)) => {
                        if more && next <= p.next_offset {
                            p.error =
                                Some("The provider did not advance to the next batch.".into());
                            return;
                        }
                        p.next_offset = next;
                        for result in results.into_iter().take(100) {
                            if let Some(url) = result["image_url"].as_str() {
                                if p.seen.insert(url.to_owned()) {
                                    p.page_buffer.push(result);
                                }
                            }
                        }
                        let provider_more = more && next < 1400;
                        if p.page_buffer.len() < p.page_size && provider_more && p.batches < 4 {
                            self.poster_request_next();
                            return;
                        }
                        let count = p.page_buffer.len().min(p.page_size);
                        if count == 0 && !provider_more {
                            p.notice =
                                Some("No selectable matches. Refine the title or filters.".into());
                        }
                        p.pages.push(p.page_buffer.drain(..count).collect());
                        p.has_more = provider_more || !p.page_buffer.is_empty();
                        p.page = p.pages.len() - 1;
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e),
                }
            }
            Pending::PosterImage => {
                self.busy = None;
                match reply.clone().and_then(engine::decode::<Artwork>) {
                    Ok(art) => {
                        self.apply_cover_artwork(art.clone(), "Cover applied to this project. Export to a new file to embed it; sources are unchanged.");
                        self.load_cover_editor(art);
                    }
                    Err(e) => self.poster_mut().error = Some(e),
                }
            }
            Pending::ArtworkEdit {
                generation,
                revision,
            } => {
                let p = self.poster_mut();
                if p.generation != *generation {
                    return;
                }
                let Some(ed) = p.editor.as_mut() else { return };
                ed.rendering = false;
                if ed.revision != *revision {
                    return;
                }
                match reply.clone().and_then(engine::decode::<Artwork>) {
                    Ok(art) => {
                        ed.rendered = art;
                        ed.ready = true;
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e),
                }
            }
            Pending::CoverChoose => match reply.clone() {
                Ok(Value::Null) => {}
                Ok(v) => match engine::decode(v) {
                    Ok(art) => self.load_cover_editor(art),
                    Err(e) => self.poster_mut().error = Some(e),
                },
                Err(e) => self.poster_mut().error = Some(e),
            },
            Pending::CoverSave => match reply {
                Ok(Value::String(path)) => self.set_notice(format!(
                    "Saved a new preview image: {}",
                    crate::model::basename(path)
                )),
                Err(e) => self.poster_mut().error = Some(e.clone()),
                _ => {}
            },
            Pending::CoverOffer => match reply.clone().and_then(engine::decode) {
                Ok(offer) => self.queue_resource(offer),
                Err(e) => self.poster_mut().error = Some(e),
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::state::{Effect, Event, State};
    use serde_json::{json, Value};

    fn ui(s: &mut State, node: &str, value: Value) {
        s.dispatch(Event::Ui {
            node: node.into(),
            value,
        });
    }

    /// Answer the newest engine request that is not a silent one.
    fn answer(s: &mut State, reply: Value) {
        let id = s
            .drain()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Engine { id, request }
                    if !matches!(request, crate::engine::Request::SubtitleEditorDirty { .. }) =>
                {
                    Some((id, request))
                }
                _ => None,
            })
            .last()
            .map(|(id, _)| id)
            .expect("request");
        s.dispatch(Event::Engine {
            id,
            reply: Ok(reply),
        });
    }

    fn batch(n: usize, next: u32, more: bool) -> Value {
        let results: Vec<Value> = (0..n)
            .map(|i| json!({"title": format!("Poster {i}"), "source_host": "example.org", "image_url": format!("https://x/{next}-{i}.jpg"), "width": 1000, "height": 1500}))
            .collect();
        json!({"results": results, "next_offset": next, "has_more": more})
    }

    fn cover() -> Value {
        json!({"jpeg": "AAAA", "width": 300, "height": 450, "source_label": "Poster"})
    }

    #[test]
    fn search_fills_a_page_and_next_uses_cached_pages_before_fetching() {
        let mut s = State::new();
        s.section = crate::state::Section::Artwork;
        ui(&mut s, "art:query", json!("Movie 2024"));
        ui(&mut s, "art:size", json!(12));
        ui(&mut s, "art:search", Value::Null);
        answer(&mut s, batch(12, 12, true));
        assert_eq!(s.poster.as_ref().unwrap().pages.len(), 1);
        ui(&mut s, "art:next", Value::Null);
        answer(&mut s, batch(12, 24, true));
        assert_eq!(s.poster.as_ref().unwrap().page, 1);
        ui(&mut s, "art:prev", Value::Null);
        assert_eq!(
            s.poster.as_ref().unwrap().page,
            0,
            "previous page is cached, no request"
        );
        assert!(
            s.drain().is_empty(),
            "navigating cached pages sends nothing"
        );
    }

    #[test]
    fn orientation_toggle_and_ratio_restart_the_search_from_page_one() {
        let mut s = State::new();
        ui(&mut s, "art:query", json!("Movie"));
        ui(&mut s, "art:search", Value::Null);
        answer(&mut s, batch(3, 3, false));
        ui(&mut s, "art:orient:landscape", Value::Null);
        assert!(
            s.poster.as_ref().unwrap().pages.is_empty(),
            "filter change clears cached pages"
        );
        assert!(s.drain().iter().any(|e| matches!(e, Effect::Engine { request: crate::engine::Request::PosterSearch { proportion, .. }, .. } if proportion == "landscape")));
    }

    #[test]
    fn invalid_page_size_is_refused() {
        let mut s = State::new();
        ui(&mut s, "art:size", json!(30));
        assert_eq!(
            s.poster.as_ref().unwrap().error.as_deref(),
            Some("Choose 12, 24 or 48 results per page")
        );
    }

    #[test]
    fn use_this_cover_applies_once_and_is_one_undo_step() {
        let mut s = State::new();
        ui(&mut s, "art:query", json!("Movie"));
        ui(&mut s, "art:search", Value::Null);
        answer(&mut s, batch(2, 2, false));
        let history = s.store.history_len().0;
        ui(&mut s, "art:use:1", Value::Null);
        answer(&mut s, cover());
        assert!(s.project().artwork.is_some());
        assert_eq!(s.store.history_len().0, history + 1);
        s.dispatch(Event::Command {
            id: "edit.undo".into(),
        });
        assert!(
            s.project().artwork.is_none(),
            "undo restores the previous cover"
        );
    }

    #[test]
    fn a_failed_search_keeps_the_error_and_no_pages() {
        let mut s = State::new();
        s.drain();
        ui(&mut s, "art:query", json!("Movie"));
        ui(&mut s, "art:search", Value::Null);
        let id = s
            .drain()
            .into_iter()
            .find_map(|e| match e {
                Effect::Engine { id, .. } => Some(id),
                _ => None,
            })
            .unwrap();
        s.dispatch(Event::Engine {
            id,
            reply: Err("provider blocked".into()),
        });
        let p = s.poster.as_ref().unwrap();
        assert!(p.pages.is_empty());
        assert_eq!(p.error.as_deref(), Some("provider blocked"));
    }
}
