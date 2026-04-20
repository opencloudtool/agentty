//! App construction and startup helpers for the core module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::events::AppEvent;
use super::state::{App, AppClients};
use crate::app::service::{AppServiceDeps, AppServices};
use crate::app::session::SessionManager;
use crate::app::setting::SettingsManager;
use crate::app::startup::{AppStartup, StartupProjectContext, StartupSessionLoadContext};
use crate::app::{AppError, session, task};
use crate::domain::agent::AgentKind;
use crate::infra::db;
use crate::infra::db::{AppRepositories, Database};
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;
#[cfg(test)]
use crate::infra::project_discovery::ProjectDiscoveryClient;

impl App {
    /// Builds the app state from persisted data and starts background
    /// housekeeping tasks.
    ///
    /// When `auto_update` is `true`, a background `npm i -g agentty@latest`
    /// runs automatically after detecting a newer version.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted or
    /// required startup state cannot be loaded from the database.
    pub async fn new(
        auto_update: bool,
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
    ) -> Result<Self, AppError> {
        let clients = AppClients::new();

        Self::new_with_options(auto_update, base_path, working_dir, git_branch, db, clients).await
    }

    /// Builds app state from persisted data with explicit external clients.
    ///
    /// Auto-update is disabled by default; use [`App::new`] with an explicit
    /// `auto_update` flag for production startup.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted or
    /// required startup state cannot be loaded from the database.
    #[cfg(test)]
    pub(crate) async fn new_with_clients(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
        clients: AppClients,
    ) -> Result<Self, AppError> {
        Self::new_with_options(false, base_path, working_dir, git_branch, db, clients).await
    }

    /// Core constructor with all options explicit.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted or
    /// required startup state cannot be loaded from the database.
    async fn new_with_options(
        auto_update: bool,
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
        clients: AppClients,
    ) -> Result<Self, AppError> {
        let (repositories, startup_project_context) =
            Self::load_startup_project_state(working_dir.as_path(), git_branch, &db, &clients)
                .await?;
        let StartupProjectContext {
            active_project_id,
            active_project_name,
            project_items,
            startup_git_branch,
            startup_git_upstream_ref,
            startup_working_dir,
        } = startup_project_context;

        let clock: Arc<dyn session::Clock> = Arc::new(session::RealClock);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let services = Self::build_services(
            base_path.clone(),
            Arc::clone(&clock),
            event_tx.clone(),
            repositories.clone(),
            &clients,
        )
        .await?;
        SessionManager::fail_unfinished_operations_from_previous_run(
            repositories.clone(),
            Arc::clone(&clock),
        )
        .await;
        let projects = crate::app::project::ProjectManager::new(
            active_project_id,
            active_project_name,
            startup_git_branch,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            startup_git_upstream_ref,
            project_items,
            startup_working_dir.clone(),
        );
        let active_project_roadmap =
            Self::load_project_roadmap(clients.fs_client.as_ref(), startup_working_dir.as_path())
                .await;
        let active_project_has_tasks_tab = active_project_roadmap.is_some();
        let settings = SettingsManager::new(&services, active_project_id).await;
        let default_session_model = SessionManager::load_default_session_model(
            &services,
            Some(active_project_id),
            AgentKind::Gemini.default_model(),
        )
        .await;
        let sessions = AppStartup::load_startup_sessions(
            &services,
            StartupSessionLoadContext {
                active_project_id,
                default_session_model,
                startup_working_dir: startup_working_dir.as_path(),
            },
        )
        .await;

        AppStartup::spawn_background_tasks(auto_update, &event_tx, &projects, &services, &sessions);

        Ok(Self {
            mode: crate::ui::state::app_mode::AppMode::List,
            settings,
            tabs: crate::app::tab::TabManager::new(),
            projects,
            services,
            sessions,
            active_project_has_tasks_tab,
            active_project_roadmap,
            task_roadmap_scroll_offset: 0,
            event_rx,
            review_cache: std::collections::HashMap::new(),
            latest_available_version: None,
            markdown_render_cache: crate::ui::markdown::MarkdownRenderCache::default(),
            merge_queue: crate::app::merge_queue::MergeQueue::default(),
            session_progress_messages: std::collections::HashMap::new(),
            update_status: None,
            sync_main_runner: clients.sync_main_runner,
            tmux_client: clients.tmux_client,
        })
    }

    /// Loads the startup project state and builds the repository bundle used
    /// by the app layer.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted or
    /// loaded from storage.
    async fn load_startup_project_state(
        working_dir: &Path,
        git_branch: Option<String>,
        db: &Database,
        clients: &AppClients,
    ) -> Result<(AppRepositories, StartupProjectContext), AppError> {
        let repositories = AppRepositories::from_database(db);
        let current_project_id =
            AppStartup::persist_startup_project(&repositories, working_dir, git_branch.as_deref())
                .await?;
        let startup_project_context = AppStartup::load_startup_project_context(
            &repositories,
            clients.fs_client.as_ref(),
            &clients.git_client,
            clients.project_discovery_client.as_ref(),
            working_dir,
            git_branch,
            current_project_id,
        )
        .await?;

        Ok((repositories, startup_project_context))
    }

    /// Builds the shared app services after validating startup agent
    /// availability.
    ///
    /// # Errors
    /// Returns an error when no supported agent backend is available.
    async fn build_services(
        base_path: PathBuf,
        clock: Arc<dyn session::Clock>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        repositories: AppRepositories,
        clients: &AppClients,
    ) -> Result<AppServices, AppError> {
        let available_agent_kinds = task::TaskService::load_agent_availability(Arc::clone(
            &clients.agent_availability_probe,
        ))
        .await;
        AppStartup::validate_startup_agent_availability(&available_agent_kinds)?;

        Ok(AppServices::new(
            base_path,
            clock,
            event_tx,
            AppServiceDeps {
                app_server_client_override: clients
                    .app_server_client_override
                    .as_ref()
                    .map(Arc::clone),
                available_agent_kinds,
                fs_client: Arc::clone(&clients.fs_client),
                git_client: Arc::clone(&clients.git_client),
                repositories,
                review_request_client: Arc::clone(&clients.review_request_client),
            },
        ))
    }

    /// Resolves the configured upstream reference for one project branch.
    pub(super) async fn load_git_upstream_ref(
        git_client: &dyn GitClient,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Option<String> {
        AppStartup::load_git_upstream_ref(git_client, working_dir, git_branch).await
    }

    /// Resolves startup active project id from settings, falling back to the
    /// current working directory when the stored project row is stale.
    #[cfg(test)]
    pub(super) async fn resolve_startup_active_project_id(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        current_project_id: i64,
    ) -> i64 {
        AppStartup::resolve_startup_active_project_id(db, fs_client, current_project_id).await
    }

    /// Loads project list entries for the projects tab.
    ///
    /// Agentty-managed session worktrees and missing project directories are
    /// excluded so the list keeps only user-facing repository roots that still
    /// exist on disk.
    pub(super) async fn load_project_items(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
    ) -> Vec<crate::domain::project::ProjectListItem> {
        AppStartup::load_project_items(db, fs_client).await
    }

    /// Loads project list entries with one caller-provided session worktree
    /// root for filtering.
    #[cfg(test)]
    pub(super) async fn load_project_items_with_session_worktree_root(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<crate::domain::project::ProjectListItem> {
        AppStartup::load_project_items_with_session_worktree_root(
            db,
            fs_client,
            session_worktree_root,
        )
        .await
    }

    /// Refreshes the persisted project catalog from the user's home directory
    /// during startup before the first project list render.
    #[cfg(test)]
    pub(super) async fn load_projects_from_home_directory(
        db: &AppRepositories,
        project_discovery_client: &dyn ProjectDiscoveryClient,
        session_worktree_root: &Path,
        home_directory: Option<&Path>,
    ) {
        AppStartup::load_projects_from_home_directory(
            db,
            project_discovery_client,
            session_worktree_root,
            home_directory,
        )
        .await;
    }

    /// Returns git repository roots discovered under the user home directory.
    ///
    /// A repository root is identified by a direct `.git` marker inside the
    /// directory and discovery stops after `HOME_PROJECT_SCAN_MAX_RESULTS`.
    #[cfg(test)]
    pub(super) fn discover_home_project_paths(
        home_directory: &Path,
        session_worktree_root: &Path,
    ) -> Vec<PathBuf> {
        AppStartup::discover_home_project_paths(home_directory, session_worktree_root)
    }

    /// Returns whether a persisted project path points to an agentty session
    /// worktree under `~/.agentty/wt`.
    #[cfg(test)]
    pub(super) fn is_session_worktree_project_path(
        project_path: &str,
        session_worktree_root: &Path,
    ) -> bool {
        AppStartup::is_session_worktree_project_path(project_path, session_worktree_root)
    }

    /// Filters persisted project rows down to entries that should remain
    /// visible in the Projects tab.
    #[cfg(test)]
    pub(super) fn visible_project_rows(
        project_rows: Vec<db::ProjectListRow>,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<db::ProjectListRow> {
        AppStartup::visible_project_rows(project_rows, fs_client, session_worktree_root)
    }

    /// Returns whether one persisted project path still resolves to a
    /// directory on disk.
    #[cfg(test)]
    pub(super) fn is_existing_project_path(fs_client: &dyn FsClient, project_path: &str) -> bool {
        AppStartup::is_existing_project_path(fs_client, project_path)
    }

    /// Converts a project row into domain project model.
    pub(super) fn project_from_row(project_row: db::ProjectRow) -> crate::domain::project::Project {
        AppStartup::project_from_row(project_row)
    }
}
