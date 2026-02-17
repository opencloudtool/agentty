//! Shared app dependency container for managers and background workflows.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::db::Database;

/// Shared app dependencies used by managers and background workflows.
pub struct AppServices {
    base_path: PathBuf,
    db: Database,
    event_tx: mpsc::UnboundedSender<AppEvent>,
}

impl AppServices {
    /// Creates a shared service container.
    pub(crate) fn new(
        base_path: PathBuf,
        db: Database,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            base_path,
            db,
            event_tx,
        }
    }

    /// Returns the session base path.
    pub(crate) fn base_path(&self) -> &Path {
        self.base_path.as_path()
    }

    /// Returns the application database handle.
    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    /// Enqueues an app event onto the internal event bus.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Returns a clone of the app event sender.
    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.event_tx.clone()
    }
}
