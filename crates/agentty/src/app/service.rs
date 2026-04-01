//! Shared app dependency container for managers and background workflows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_forge::ReviewRequestClient;
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::app::session::Clock;
use crate::db::Database;
use crate::domain::agent::AgentKind;
use crate::infra::app_server::AppServerClient;
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;

/// External clients and cached machine-scoped availability injected into
/// [`AppServices`].
pub(crate) struct AppServiceDeps {
    /// Shared provider-owned app-server client override used by tests and
    /// injected environments.
    pub(crate) app_server_client_override: Option<Arc<dyn AppServerClient>>,
    /// Cached locally runnable backends used to scope model selection.
    pub(crate) available_agent_kinds: Vec<AgentKind>,
    /// Shared filesystem client for async filesystem operations.
    pub(crate) fs_client: Arc<dyn FsClient>,
    /// Shared git client for async git operations.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// Shared forge review-request client.
    pub(crate) review_request_client: Arc<dyn ReviewRequestClient>,
}

/// Shared app dependencies used by managers and background workflows.
pub struct AppServices {
    available_agent_kinds: Arc<Vec<AgentKind>>,
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
    base_path: PathBuf,
    clock: Arc<dyn Clock>,
    db: Database,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn ReviewRequestClient>,
}

impl AppServices {
    /// Creates a shared service container with explicit external client
    /// dependencies.
    pub(crate) fn new(
        base_path: PathBuf,
        clock: Arc<dyn Clock>,
        db: Database,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        deps: AppServiceDeps,
    ) -> Self {
        let AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            fs_client,
            git_client,
            review_request_client,
        } = deps;

        Self {
            available_agent_kinds: Arc::new(available_agent_kinds),
            app_server_client_override,
            base_path,
            clock,
            db,
            event_tx,
            fs_client,
            git_client,
            review_request_client,
        }
    }

    /// Returns the session base path.
    pub(crate) fn base_path(&self) -> &Path {
        self.base_path.as_path()
    }

    /// Returns the cached locally runnable agent kinds.
    pub(crate) fn available_agent_kinds(&self) -> Vec<AgentKind> {
        self.available_agent_kinds.as_ref().clone()
    }

    /// Returns the application database handle.
    pub(crate) fn db(&self) -> &Database {
        &self.db
    }

    /// Returns the shared wall-clock used by session workflows.
    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// Enqueues an app event onto the internal event bus.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = self.event_tx.send(event);
    }

    /// Returns a clone of the app event sender.
    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.event_tx.clone()
    }

    /// Returns the shared filesystem client for async filesystem operations.
    pub(crate) fn fs_client(&self) -> Arc<dyn FsClient> {
        Arc::clone(&self.fs_client)
    }

    /// Returns the shared git client for async git operations.
    pub(crate) fn git_client(&self) -> Arc<dyn GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns the shared forge review-request client.
    pub(crate) fn review_request_client(&self) -> Arc<dyn ReviewRequestClient> {
        Arc::clone(&self.review_request_client)
    }

    /// Returns the optional app-server client override used by tests and
    /// injected environments.
    pub(crate) fn app_server_client_override(&self) -> Option<Arc<dyn AppServerClient>> {
        self.app_server_client_override.as_ref().map(Arc::clone)
    }
}
