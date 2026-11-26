//! Artwork and thumbnail publication; no filesystem, decoding or network I/O on the scene thread.
use pigeon_plugin_sdk::{protocol::ResourceKind, runtime::Ctx};
use reelio_ui::{
    model::Artwork,
    state::{Section, State},
};
use std::{
    collections::BTreeSet,
    sync::{
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
};

type Photo = (&'static str, Option<Artwork>);
type Thumb = (String, String);
pub struct Images {
    photo: SyncSender<Photo>,
    thumb: SyncSender<Thumb>,
    desired_thumbs: Arc<Mutex<BTreeSet<String>>>,
    known_thumbs: BTreeSet<String>,
    last_page: BTreeSet<String>,
    photos: [Photo; 3],
}
impl Images {
    pub fn new(ctx: Ctx) -> Self {
        let (photo, photos_rx) = mpsc::sync_channel(8);
        let (thumb, thumbs_rx) = mpsc::sync_channel(48);
        let desired_thumbs = Arc::new(Mutex::new(BTreeSet::new()));
        let photo_ctx = ctx.clone();
        std::thread::spawn(move || {
            for (rid, image) in photos_rx {
                if let Some(art) = image {
                    let _ = publish(&photo_ctx, rid, &art);
                } else {
                    let _ = photo_ctx.resources().drop_resource(rid);
                }
            }
        });
        let thumbs_rx = Arc::new(Mutex::new(thumbs_rx));
        for _ in 0..3 {
            let (ctx, rx, desired) = (ctx.clone(), thumbs_rx.clone(), desired_thumbs.clone());
            std::thread::spawn(move || thumbnail_worker(ctx, rx, desired));
        }
        Self {
            photo,
            thumb,
            desired_thumbs,
            known_thumbs: BTreeSet::new(),
            last_page: BTreeSet::new(),
            photos: [
                ("cover:project", None),
                ("cover:original", None),
                ("cover:preview", None),
            ],
        }
    }
    pub fn update(&mut self, ctx: &Ctx, state: &State, artwork_visible: bool) {
        let editor = state
            .poster
            .as_ref()
            .and_then(|p| p.editor.as_ref())
            .filter(|_| artwork_visible);
        for ((rid, last), next) in self.photos.iter_mut().zip([
            state.project().artwork.as_ref(),
            editor.map(|e| &e.original),
            editor.map(|e| &e.rendered),
        ]) {
            if last.as_ref() != next && self.photo.try_send((*rid, next.cloned())).is_ok() {
                *last = next.cloned();
            }
        }
        let page = state
            .poster
            .as_ref()
            .filter(|p| artwork_visible && p.browser && state.section == Section::Artwork)
            .and_then(|p| p.pages.get(p.page))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let links = page
            .iter()
            .filter_map(|p| p["thumbnail_url"].as_str().filter(|s| !s.is_empty()))
            .map(|url| (reelio_ui::poster::thumbnail_rid(url), url))
            .collect::<Vec<_>>();
        let current = links
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        if current != self.last_page {
            *self
                .desired_thumbs
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = current.clone();
            for rid in self.last_page.difference(&current) {
                let _ = ctx.resources().drop_resource(rid);
            }
            self.known_thumbs.retain(|rid| current.contains(rid));
            self.last_page = current;
        }
        for (rid, url) in links {
            if self.known_thumbs.contains(&rid) {
                continue;
            }
            match self.thumb.try_send((rid.clone(), url.to_owned())) {
                Ok(()) => {
                    self.known_thumbs.insert(rid);
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    }
}
fn thumbnail_worker(
    ctx: Ctx,
    rx: Arc<Mutex<Receiver<Thumb>>>,
    desired: Arc<Mutex<BTreeSet<String>>>,
) {
    let wanted = |rid: &str| {
        desired
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(rid)
    };
    loop {
        let job = rx.lock().unwrap_or_else(|p| p.into_inner()).recv();
        let Ok((rid, url)) = job else { break };
        if !wanted(&rid) {
            continue;
        }
        if let Ok(art) = reelio_resources::poster_image(&url)
            .and_then(|bytes| reelio_artwork::prepare(&bytes, "Search thumbnail"))
        {
            if wanted(&rid) {
                let size = art.width.max(art.height);
                let art = if size > 256 {
                    let edits = reelio_artwork::ImageEdits {
                        resize: Some(reelio_artwork::Resize {
                            width: (art.width * 256 / size).max(2),
                            height: (art.height * 256 / size).max(2),
                        }),
                        ..Default::default()
                    };
                    match reelio_artwork::edit(&art, &edits) {
                        Ok(thumb) => thumb,
                        Err(_) => continue,
                    }
                } else {
                    art
                };
                let _ = publish_encoded(&ctx, &rid, &art);
                if !wanted(&rid) {
                    let _ = ctx.resources().drop_resource(&rid);
                }
            }
        }
    }
}
fn publish(ctx: &Ctx, rid: &str, art: &Artwork) -> Result<(), String> {
    let engine = reelio_engine::Artwork {
        jpeg: art.jpeg.clone(),
        width: art.width,
        height: art.height,
        source_label: art.source_label.clone(),
    };
    publish_encoded(ctx, rid, &engine)
}
fn publish_encoded(ctx: &Ctx, rid: &str, art: &reelio_engine::Artwork) -> Result<(), String> {
    let bytes = reelio_artwork::jpeg_bytes(art)?;
    ctx.resources()
        .put_encoded(rid, ResourceKind::Jpeg, &bytes, art.width, art.height)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
