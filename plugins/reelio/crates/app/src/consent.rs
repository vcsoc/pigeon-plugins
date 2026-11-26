//! Moves blocking engine confirmations onto the scene thread without granting implicit approval.
use reelio_engine::ConfirmDialog;
use std::sync::{mpsc, Arc};
use std::time::Duration;

pub type Ask = (ConfirmDialog, mpsc::SyncSender<bool>);

pub fn channel() -> (mpsc::SyncSender<Ask>, mpsc::Receiver<Ask>) {
    mpsc::sync_channel(8)
}

pub fn ask(queue: &mpsc::SyncSender<Ask>, prompt: ConfirmDialog) -> bool {
    let (tx, rx) = mpsc::sync_channel(1);
    queue.try_send((prompt, tx)).is_ok()
        && rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false)
}

pub fn forward(app: &Arc<crate::transport::App>, rx: mpsc::Receiver<Ask>) {
    let weak = Arc::downgrade(app);
    std::thread::spawn(move || {
        for (prompt, decision) in rx {
            let Some(app) = weak.upgrade() else { break };
            crate::transport::show_consent(&app, prompt, decision);
        }
    });
}
