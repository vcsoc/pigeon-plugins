//! Shared state handed to every command module. Crate-private: the public surface is `Engine`.

use crate::host::EngineHost;
use crate::types::EngineEvent;
use reelio_application::Application;
use reelio_subtitles::local_model::Service as SubtitleService;
use reelio_updates::UpdateService;
use std::path::{Path, PathBuf};
use std::sync::{mpsc::Sender, Arc};

pub(crate) struct Ctx {
    /// Application coordinator: source authorization, storage, jobs, backend, dependencies, resources.
    pub app: Arc<Application>,
    /// Host services (dialogs, URLs, window chrome).
    pub host: Arc<dyn EngineHost>,
    /// Event channel to the host. Sends never block; a closed receiver is ignored.
    pub events: Sender<EngineEvent>,
    /// Loopback preview registry.
    pub preview: Arc<crate::preview::PreviewService>,
    /// Verified update checker and downloader.
    pub updates: Arc<UpdateService>,
    /// Local subtitle model service and subtitle draft store.
    pub subtitles: Arc<SubtitleService>,
}

impl Ctx {
    /// Publish an event. Dropped silently when the host has stopped listening.
    pub fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    /// Authorize one canonical regular source path for this session (reference `grant`,
    /// without the Tauri asset-protocol scope, which the engine does not use).
    pub fn grant(&self, path: &Path) -> Result<PathBuf, String> {
        self.app.authorize(path)
    }

    /// The default folder for every native dialog (see `preferences::default_folder`).
    pub fn default_folder(&self) -> Result<PathBuf, String> {
        crate::preferences::default_folder(self)
    }
}
