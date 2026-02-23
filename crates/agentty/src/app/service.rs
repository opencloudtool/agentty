//! Shared app dependency container for managers and background workflows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::db::Database;
use crate::infra::codex_app_server::{CodexAppServerClient, RealCodexAppServerClient};
use crate::infra::git::GitClient;

/// Shared app dependencies used by managers and background workflows.
pub struct AppServices {
    base_path: PathBuf,
    codex_app_server_client: Arc<dyn CodexAppServerClient>,
    db: Database,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    git_client: Arc<dyn GitClient>,
}

impl AppServices {
    /// Creates a shared service container.
    pub(crate) fn new(
        base_path: PathBuf,
        db: Database,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
    ) -> Self {
        Self::new_with_clients(
            base_path,
            db,
            event_tx,
            git_client,
            Arc::new(RealCodexAppServerClient::new()),
        )
    }

    /// Creates a shared service container with explicit external client
    /// dependencies.
    pub(crate) fn new_with_clients(
        base_path: PathBuf,
        db: Database,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        codex_app_server_client: Arc<dyn CodexAppServerClient>,
    ) -> Self {
        Self {
            base_path,
            codex_app_server_client,
            db,
            event_tx,
            git_client,
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

    /// Returns the shared git client for async git operations.
    pub(crate) fn git_client(&self) -> Arc<dyn GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns the shared Codex app-server client used by session workers.
    pub(crate) fn codex_app_server_client(&self) -> Arc<dyn CodexAppServerClient> {
        Arc::clone(&self.codex_app_server_client)
    }
}
