//! Poster/cover search: proportion filters and bounded pagination. Port of
//! `features/artwork/posterSearch.ts`. Network access is injected as a closure so the logic is
//! testable; the engine performs the actual provider request.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proportion {
    Any,
    Landscape,
    Portrait,
    Square,
    Ratio(u32, u32),
}

impl Proportion {
    pub const ALL: [Proportion; 12] = [
        Proportion::Any,
        Proportion::Landscape,
        Proportion::Portrait,
        Proportion::Square,
        Proportion::Ratio(4, 3),
        Proportion::Ratio(3, 4),
        Proportion::Ratio(16, 9),
        Proportion::Ratio(9, 16),
        Proportion::Ratio(2, 3),
        Proportion::Ratio(3, 2),
        Proportion::Ratio(21, 9),
        Proportion::Ratio(9, 21),
    ];

    pub fn label(self) -> String {
        match self {
            Proportion::Any => "Any proportions".into(),
            Proportion::Landscape => "Landscape".into(),
            Proportion::Portrait => "Portrait".into(),
            Proportion::Square => "Square (1:1)".into(),
            Proportion::Ratio(w, h) => format!("{w}:{h}"),
        }
    }

    /// Orientation implied by the filter: `None` for Any and Square.
    pub fn orientation(self) -> Option<Orientation> {
        match self {
            Proportion::Landscape => Some(Orientation::Landscape),
            Proportion::Portrait => Some(Orientation::Portrait),
            Proportion::Ratio(w, h) => Some(if w > h {
                Orientation::Landscape
            } else {
                Orientation::Portrait
            }),
            Proportion::Any | Proportion::Square => None,
        }
    }

    /// Numeric aspect ratio used to filter provider results (`None` = unfiltered).
    pub fn ratio(self) -> Option<f64> {
        match self {
            Proportion::Ratio(w, h) => Some(w as f64 / h as f64),
            Proportion::Square => Some(1.0),
            Proportion::Landscape => Some(16.0 / 9.0),
            Proportion::Portrait => Some(2.0 / 3.0),
            Proportion::Any => None,
        }
    }

    /// Clicking the active orientation clears it; the opposite orientation flips an exact ratio
    /// (16:9 becomes 9:16) and otherwise selects the orientation.
    pub fn toggle_orientation(self, direction: Orientation) -> Proportion {
        if self.orientation() == Some(direction) {
            return Proportion::Any;
        }
        match self {
            Proportion::Any | Proportion::Square | Proportion::Landscape | Proportion::Portrait => {
                match direction {
                    Orientation::Landscape => Proportion::Landscape,
                    Orientation::Portrait => Proportion::Portrait,
                }
            }
            Proportion::Ratio(w, h) => Proportion::Ratio(h, w),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poster {
    pub title: String,
    pub source_host: String,
    pub image_url: String,
    pub thumbnail_url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosterBatch {
    pub results: Vec<Poster>,
    pub next_offset: u64,
    pub has_more: bool,
}

pub const PAGE_SIZES: [usize; 3] = [12, 24, 48];
/// Safety limits: cached pages, and provider batches scanned per new UI page.
pub const MAX_PAGES: usize = 40;
pub const MAX_BATCHES_PER_PAGE: usize = 4;

#[derive(Debug, Clone)]
pub struct PosterSession {
    pub query: String,
    pub proportion: Proportion,
    pub page_size: usize,
    pub offset: u64,
    pub provider_more: bool,
    pub buffer: Vec<Poster>,
    pub seen: HashSet<String>,
    pub pages: Vec<Vec<Poster>>,
}

impl PosterSession {
    pub fn new(query: &str, proportion: Proportion, page_size: usize) -> Result<Self, String> {
        if !PAGE_SIZES.contains(&page_size) {
            return Err("Choose 12, 24 or 48 results per page".into());
        }
        Ok(Self {
            query: query.into(),
            proportion,
            page_size,
            offset: 0,
            provider_more: true,
            buffer: vec![],
            seen: HashSet::new(),
            pages: vec![],
        })
    }

    pub fn has_next_page(&self, page: usize) -> bool {
        page + 1 < self.pages.len()
            || (self.pages.len() < MAX_PAGES && (!self.buffer.is_empty() || self.provider_more))
    }

    /// Fill (up to 4 batches) and return the next page. Duplicate image URLs are removed across
    /// the session; a stalled cursor is an error rather than an endless loop.
    pub fn load_page(
        &mut self,
        mut fetch: impl FnMut(u64) -> Result<PosterBatch, String>,
    ) -> Result<Vec<Poster>, String> {
        if self.pages.len() >= MAX_PAGES {
            return Err("Search page limit reached; refine the query".into());
        }
        let mut batches = 0;
        while batches < MAX_BATCHES_PER_PAGE
            && self.buffer.len() < self.page_size
            && self.provider_more
        {
            batches += 1;
            let batch = fetch(self.offset)?;
            if batch.has_more && batch.next_offset <= self.offset {
                return Err("The search provider did not advance to more results".into());
            }
            self.offset = batch.next_offset;
            self.provider_more = batch.has_more;
            for poster in batch.results {
                if self.seen.insert(poster.image_url.clone()) {
                    self.buffer.push(poster);
                }
            }
        }
        let take = self.buffer.len().min(self.page_size);
        let page: Vec<Poster> = self.buffer.drain(..take).collect();
        self.pages.push(page.clone());
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poster(n: usize) -> Poster {
        Poster {
            title: format!("p{n}"),
            source_host: "example".into(),
            image_url: format!("https://x/{n}.jpg"),
            thumbnail_url: format!("https://x/t{n}.jpg"),
            width: Some(1000),
            height: Some(1500),
        }
    }

    fn batch(range: std::ops::Range<usize>, next: u64, more: bool) -> PosterBatch {
        PosterBatch {
            results: range.map(poster).collect(),
            next_offset: next,
            has_more: more,
        }
    }

    #[test]
    fn orientation_toggles_flip_reciprocal_ratios() {
        assert_eq!(
            Proportion::Ratio(16, 9).toggle_orientation(Orientation::Portrait),
            Proportion::Ratio(9, 16)
        );
        assert_eq!(
            Proportion::Ratio(9, 16).toggle_orientation(Orientation::Portrait),
            Proportion::Any
        );
        assert_eq!(
            Proportion::Square.toggle_orientation(Orientation::Landscape),
            Proportion::Landscape
        );
        assert_eq!(
            Proportion::Ratio(21, 9).orientation(),
            Some(Orientation::Landscape)
        );
    }

    #[test]
    fn page_sizes_are_validated() {
        assert!(PosterSession::new("q", Proportion::Any, 30).is_err());
        assert!(PosterSession::new("q", Proportion::Any, 24).is_ok());
    }

    #[test]
    fn pages_fill_across_batches_and_remove_duplicates() {
        let mut s = PosterSession::new("q", Proportion::Any, 12).unwrap();
        let mut calls = 0;
        let page = s
            .load_page(|offset| {
                calls += 1;
                Ok(match offset {
                    0 => batch(0..8, 8, true),
                    8 => batch(4..16, 16, true),
                    _ => batch(0..0, 16, false),
                })
            })
            .unwrap();
        assert_eq!(page.len(), 12);
        assert_eq!(calls, 2, "stops once the page is full");
        assert_eq!(s.buffer.len(), 4, "overflow is cached for the next page");
        assert_eq!(s.pages.len(), 1);
    }

    #[test]
    fn sparse_searches_are_capped_at_four_batches() {
        let mut s = PosterSession::new("q", Proportion::Any, 48).unwrap();
        let mut calls = 0;
        let page = s
            .load_page(|offset| {
                calls += 1;
                Ok(batch(
                    offset as usize..offset as usize + 1,
                    offset + 1,
                    true,
                ))
            })
            .unwrap();
        assert_eq!(calls, MAX_BATCHES_PER_PAGE);
        assert_eq!(page.len(), 4);
    }

    #[test]
    fn a_stalled_cursor_is_an_error() {
        let mut s = PosterSession::new("q", Proportion::Any, 12).unwrap();
        let err = s
            .load_page(|offset| Ok(batch(0..1, offset, true)))
            .unwrap_err();
        assert!(err.contains("did not advance"));
    }

    #[test]
    fn a_failed_continuation_keeps_already_fetched_data() {
        let mut s = PosterSession::new("q", Proportion::Any, 12).unwrap();
        let mut first = true;
        let page = s.load_page(|offset| {
            if first {
                first = false;
                Ok(batch(0..5, 5, true))
            } else {
                Err(format!("network failed at {offset}"))
            }
        });
        assert!(page.is_err());
        assert_eq!(s.buffer.len(), 5, "fetched results are retained");
        assert!(s.pages.is_empty());
    }
}
