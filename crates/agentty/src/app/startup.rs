//! App startup and project-catalog helper workflows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use super::core::{AGENTTY_WT_DIR, AppEvent};
use super::task;
use crate::app::service::AppServices;
use crate::app::session::SessionManager;
use crate::app::session_state::SessionState;
use crate::app::{AppError, ProjectManager, session};
use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::project::{Project, ProjectListItem, project_name_from_path};
use crate::infra::db::AppRepositories;
#[cfg(test)]
use crate::infra::db::Database;
use crate::infra::fs::FsClient;
use crate::infra::git::{GitClient, detect_git_info};
use crate::infra::project_discovery::ProjectDiscoveryClient;
use crate::ui::page::session_list::preferred_initial_session_index;

/// Startup project context resolved before the first render.
pub(crate) struct StartupProjectContext {
    /// Persisted active project id used to initialize managers and settings.
    pub(crate) active_project_id: i64,
    /// Display label for the active project shown on first render.
    pub(crate) active_project_name: String,
    /// Initial project list shown in the projects tab.
    pub(crate) project_items: Vec<ProjectListItem>,
    /// Startup git branch for the active project when detected.
    pub(crate) startup_git_branch: Option<String>,
    /// Startup upstream reference for the active project when detected.
    pub(crate) startup_git_upstream_ref: Option<String>,
    /// Working directory used for the active project session list.
    pub(crate) startup_working_dir: PathBuf,
}

/// Startup-only inputs needed to hydrate the initial `SessionManager`.
pub(crate) struct StartupSessionLoadContext<'a> {
    /// Identifier for the project whose sessions should be loaded.
    pub(crate) active_project_id: i64,
    /// Default model applied when persisted session rows omit one.
    pub(crate) default_session_model: AgentModel,
    /// Working directory used to resolve session metadata at startup.
    pub(crate) startup_working_dir: &'a Path,
}

/// Shared startup coordinator for app construction and project catalog work.
pub(crate) struct AppStartup;

impl AppStartup {
    /// Returns a startup error when no supported backend CLI is installed.
    pub(crate) fn validate_startup_agent_availability(
        available_agent_kinds: &[AgentKind],
    ) -> Result<(), AppError> {
        if available_agent_kinds.is_empty() {
            return Err(AppError::Workflow(
                "No supported backend CLI found on `PATH`. Install `codex`, `claude`, or `gemini` \
                 and restart `agentty`."
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Persists the startup project row and backfills legacy session rows.
    pub(crate) async fn persist_startup_project(
        db: &AppRepositories,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Result<i64, AppError> {
        let current_project_id = db
            .upsert_project(&working_dir.to_string_lossy(), git_branch)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to persist startup project `{}`: {error}",
                    working_dir.display()
                ))
            })?;

        db.backfill_session_project(current_project_id)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to backfill startup sessions for project `{}`: {error}",
                    working_dir.display()
                ))
            })?;

        Ok(current_project_id)
    }

    /// Resolves startup project state and persists active-project metadata.
    pub(crate) async fn load_startup_project_context(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        git_client: &Arc<dyn GitClient>,
        project_discovery_client: &dyn ProjectDiscoveryClient,
        working_dir: &Path,
        git_branch: Option<String>,
        current_project_id: i64,
    ) -> Result<StartupProjectContext, AppError> {
        let startup_active_project_id =
            Self::resolve_startup_active_project_id(db, fs_client, current_project_id).await;
        let startup_active_project = Self::load_project(
            db,
            startup_active_project_id,
            working_dir,
            git_branch.as_deref(),
        )
        .await;
        let startup_working_dir = startup_active_project.path.clone();
        let startup_git_branch = if startup_working_dir.as_path() == working_dir {
            git_branch
        } else {
            git_client
                .detect_git_info(startup_working_dir.clone())
                .await
        };
        let startup_git_upstream_ref = Self::load_git_upstream_ref(
            git_client.as_ref(),
            startup_working_dir.as_path(),
            startup_git_branch.as_deref(),
        )
        .await;
        let active_project_id = db
            .upsert_project(
                &startup_working_dir.to_string_lossy(),
                startup_git_branch.as_deref(),
            )
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to persist active startup project `{}`: {error}",
                    startup_working_dir.display()
                ))
            })?;
        db.set_active_project_id(active_project_id)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to store active startup project `{}`: {error}",
                    startup_working_dir.display()
                ))
            })?;
        db.touch_project_last_opened(active_project_id)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to update startup project activity for `{}`: {error}",
                    startup_working_dir.display()
                ))
            })?;
        Self::refresh_project_catalog_on_startup(db, project_discovery_client).await;

        let project_items = Self::load_project_items(db, fs_client).await;
        let active_project_name =
            Self::project_title_for_id(&project_items, active_project_id, &startup_working_dir);

        Ok(StartupProjectContext {
            active_project_id,
            active_project_name,
            project_items,
            startup_git_branch,
            startup_git_upstream_ref,
            startup_working_dir,
        })
    }

    /// Loads startup session rows, metadata, and runtime handles.
    pub(crate) async fn load_startup_sessions(
        services: &AppServices,
        context: StartupSessionLoadContext<'_>,
    ) -> SessionManager {
        let StartupSessionLoadContext {
            active_project_id,
            default_session_model,
            startup_working_dir,
        } = context;
        let mut table_state = TableState::default();
        let mut handles = std::collections::HashMap::new();
        let fs_client = services.fs_client();
        let (sessions, stats_activity, session_worktree_availability) =
            SessionManager::load_sessions_with_fs_client(
                services.base_path(),
                services.db(),
                active_project_id,
                startup_working_dir,
                &mut handles,
                fs_client.as_ref(),
            )
            .await;
        let clock = services.clock();
        let (sessions_row_count, sessions_updated_at_max) = services
            .db()
            .load_sessions_metadata()
            .await
            .unwrap_or((0, 0));
        table_state.select(preferred_initial_session_index(&sessions));

        let mut session_manager = SessionManager::new(
            session::SessionDefaults {
                model: default_session_model,
            },
            services.git_client(),
            SessionState::new(
                handles,
                sessions,
                table_state,
                clock,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            stats_activity,
        );
        session_manager.replace_session_worktree_availability(session_worktree_availability);
        session_manager.refresh_session_branch_names().await;

        session_manager
    }

    /// Spawns background pollers for git status, review-request status, and
    /// version checks.
    pub(crate) fn spawn_background_tasks(
        auto_update: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
        projects: &ProjectManager,
        services: &AppServices,
        sessions: &SessionManager,
    ) {
        task::TaskService::spawn_version_check_task(event_tx, auto_update);
        if projects.has_git_branch() {
            task::TaskService::spawn_git_status_task(
                projects.working_dir(),
                projects.git_branch().unwrap_or_default().to_string(),
                super::core::App::session_git_status_targets(sessions),
                projects.git_status_cancel(),
                event_tx.clone(),
                services.git_client(),
            );
            task::TaskService::spawn_review_request_status_task(
                super::core::App::review_request_sync_targets(sessions),
                projects.git_status_cancel(),
                event_tx.clone(),
                services.git_client(),
                services.review_request_client(),
            );
        }
    }

    /// Loads project list entries for the projects tab.
    pub(crate) async fn load_project_items(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
    ) -> Vec<ProjectListItem> {
        let session_worktree_root = super::core::agentty_home().join(AGENTTY_WT_DIR);

        Self::load_project_items_with_session_worktree_root(
            db,
            fs_client,
            session_worktree_root.as_path(),
        )
        .await
    }

    /// Loads project list entries with one caller-provided worktree root.
    pub(crate) async fn load_project_items_with_session_worktree_root(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<ProjectListItem> {
        Self::visible_project_rows(
            db.load_projects_with_stats().await.unwrap_or_default(),
            fs_client,
            session_worktree_root,
        )
        .into_iter()
        .map(Self::project_list_item_from_row)
        .collect()
    }

    /// Refreshes the persisted project catalog from the user home directory
    /// through the injected project-discovery boundary.
    pub(crate) async fn refresh_project_catalog_on_startup(
        db: &AppRepositories,
        project_discovery_client: &dyn ProjectDiscoveryClient,
    ) {
        let session_worktree_root = super::core::agentty_home().join(AGENTTY_WT_DIR);
        let home_directory = dirs::home_dir();

        Self::load_projects_from_home_directory(
            db,
            project_discovery_client,
            session_worktree_root.as_path(),
            home_directory.as_deref(),
        )
        .await;
    }

    /// Discovers git repositories under the user home directory through the
    /// injected project-discovery boundary and persists them.
    pub(crate) async fn load_projects_from_home_directory(
        db: &AppRepositories,
        project_discovery_client: &dyn ProjectDiscoveryClient,
        session_worktree_root: &Path,
        home_directory: Option<&Path>,
    ) {
        let Some(home_directory) = home_directory.map(Path::to_path_buf) else {
            return;
        };

        let Ok(discovered_project_paths) = project_discovery_client
            .discover_home_project_paths(home_directory, session_worktree_root.to_path_buf())
            .await
        else {
            return;
        };

        for project_path in discovered_project_paths {
            let git_branch = detect_git_info(project_path.clone()).await;
            let project_path = project_path.to_string_lossy().to_string();
            // Best-effort: project metadata persistence is non-critical.
            let _ = db
                .upsert_project(project_path.as_str(), git_branch.as_deref())
                .await;
        }
    }

    /// Returns git repository roots discovered under the user home directory.
    ///
    /// Tests call through the real project-discovery implementation so app
    /// orchestration no longer owns raw filesystem walking logic.
    #[cfg(test)]
    pub(crate) fn discover_home_project_paths(
        home_directory: &Path,
        session_worktree_root: &Path,
    ) -> Vec<PathBuf> {
        crate::infra::project_discovery::discover_home_project_paths(
            home_directory,
            session_worktree_root,
        )
    }

    /// Returns whether a persisted project path points to an agentty worktree.
    pub(crate) fn is_session_worktree_project_path(
        project_path: &str,
        session_worktree_root: &Path,
    ) -> bool {
        Path::new(project_path).starts_with(session_worktree_root)
    }

    /// Filters persisted project rows down to visible project list entries.
    pub(crate) fn visible_project_rows(
        project_rows: Vec<crate::infra::db::ProjectListRow>,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<crate::infra::db::ProjectListRow> {
        project_rows
            .into_iter()
            .filter(|project_row| {
                !Self::is_session_worktree_project_path(
                    project_row.path.as_str(),
                    session_worktree_root,
                ) && Self::is_existing_project_path(fs_client, project_row.path.as_str())
            })
            .collect()
    }

    /// Returns whether one persisted project path still resolves to a
    /// directory.
    pub(crate) fn is_existing_project_path(fs_client: &dyn FsClient, project_path: &str) -> bool {
        fs_client.is_dir(PathBuf::from(project_path))
    }

    /// Converts a project row into the domain project model.
    pub(crate) fn project_from_row(project_row: crate::infra::db::ProjectRow) -> Project {
        Project {
            created_at: project_row.created_at,
            display_name: project_row.display_name,
            git_branch: project_row.git_branch,
            id: project_row.id,
            is_favorite: project_row.is_favorite,
            last_opened_at: project_row.last_opened_at,
            path: PathBuf::from(project_row.path),
            updated_at: project_row.updated_at,
        }
    }

    /// Converts an aggregated project row into list-friendly project metadata.
    pub(crate) fn project_list_item_from_row(
        project_row: crate::infra::db::ProjectListRow,
    ) -> ProjectListItem {
        let project = Project {
            created_at: project_row.created_at,
            display_name: project_row.display_name,
            git_branch: project_row.git_branch,
            id: project_row.id,
            is_favorite: project_row.is_favorite,
            last_opened_at: project_row.last_opened_at,
            path: PathBuf::from(project_row.path),
            updated_at: project_row.updated_at,
        };

        ProjectListItem {
            active_session_count: u32::try_from(project_row.active_session_count)
                .unwrap_or(u32::MAX),
            last_session_updated_at: project_row.last_session_updated_at,
            project,
            session_count: u32::try_from(project_row.session_count).unwrap_or(u32::MAX),
        }
    }

    /// Resolves the active project title used for startup rendering.
    pub(crate) fn project_title_for_id(
        project_items: &[ProjectListItem],
        project_id: i64,
        fallback_path: &Path,
    ) -> String {
        if let Some(project_item) = project_items
            .iter()
            .find(|project_item| project_item.project.id == project_id)
        {
            return project_item.project.display_label();
        }

        project_name_from_path(fallback_path)
    }

    /// Resolves the configured upstream reference for one project branch.
    pub(crate) async fn load_git_upstream_ref(
        git_client: &dyn GitClient,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Option<String> {
        git_branch?;

        git_client
            .current_upstream_reference(working_dir.to_path_buf())
            .await
            .ok()
    }

    /// Resolves startup active project id from settings.
    pub(crate) async fn resolve_startup_active_project_id(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        current_project_id: i64,
    ) -> i64 {
        let Some(stored_project_id) = db.load_active_project_id().await.ok().flatten() else {
            return current_project_id;
        };
        let Some(project_row) = db.get_project(stored_project_id).await.ok().flatten() else {
            return current_project_id;
        };
        if !Self::is_existing_project_path(fs_client, project_row.path.as_str()) {
            return current_project_id;
        }

        stored_project_id
    }

    /// Loads one project and falls back to the current working directory.
    pub(crate) async fn load_project(
        db: &AppRepositories,
        project_id: i64,
        fallback_working_dir: &Path,
        fallback_git_branch: Option<&str>,
    ) -> Project {
        if let Some(project_row) = db.get_project(project_id).await.ok().flatten() {
            return Self::project_from_row(project_row);
        }

        Project {
            created_at: 0,
            display_name: None,
            git_branch: fallback_git_branch.map(str::to_string),
            id: project_id,
            is_favorite: false,
            last_opened_at: None,
            path: fallback_working_dir.to_path_buf(),
            updated_at: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::infra::fs::MockFsClient;

    /// Builds one project-list row fixture for startup filtering tests.
    fn project_list_row_fixture(
        project_id: i64,
        project_path: &Path,
    ) -> crate::infra::db::ProjectListRow {
        crate::infra::db::ProjectListRow {
            active_session_count: 0,
            created_at: 0,
            display_name: None,
            git_branch: Some("main".to_string()),
            id: project_id,
            is_favorite: false,
            last_opened_at: None,
            last_session_updated_at: None,
            path: project_path.to_string_lossy().to_string(),
            session_count: 0,
            updated_at: 0,
        }
    }

    /// Builds one mock filesystem client that reports directories from the
    /// provided set.
    fn mock_fs_client_with_directories(existing_directories: HashSet<PathBuf>) -> MockFsClient {
        let mut fs_client = MockFsClient::new();
        fs_client
            .expect_is_dir()
            .returning(move |path| existing_directories.contains(&path));

        fs_client
    }

    /// Verifies startup project resolution prefers the persisted active
    /// project when its directory still exists.
    #[tokio::test]
    async fn resolve_startup_active_project_id_prefers_existing_stored_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");
        let current_project_path = PathBuf::from("/workspace/current");
        let stored_project_path = PathBuf::from("/workspace/stored");
        let current_project_id = database
            .upsert_project(&current_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to persist current project");
        let stored_project_id = database
            .upsert_project(&stored_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to persist stored project");
        database
            .set_active_project_id(stored_project_id)
            .await
            .expect("failed to persist active project id");
        let fs_client = mock_fs_client_with_directories(HashSet::from([
            current_project_path,
            stored_project_path,
        ]));

        // Act
        let resolved_project_id = AppStartup::resolve_startup_active_project_id(
            &database,
            &fs_client,
            current_project_id,
        )
        .await;

        // Assert
        assert_eq!(resolved_project_id, stored_project_id);
    }

    /// Verifies startup project resolution falls back to the current project
    /// when the persisted active path is stale.
    #[tokio::test]
    async fn resolve_startup_active_project_id_falls_back_for_missing_stored_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");
        let current_project_path = PathBuf::from("/workspace/current");
        let missing_project_path = PathBuf::from("/workspace/missing");
        let current_project_id = database
            .upsert_project(&current_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to persist current project");
        let missing_project_id = database
            .upsert_project(&missing_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to persist missing project");
        database
            .set_active_project_id(missing_project_id)
            .await
            .expect("failed to persist active project id");
        let fs_client = mock_fs_client_with_directories(HashSet::from([current_project_path]));

        // Act
        let resolved_project_id = AppStartup::resolve_startup_active_project_id(
            &database,
            &fs_client,
            current_project_id,
        )
        .await;

        // Assert
        assert_eq!(resolved_project_id, current_project_id);
    }

    /// Verifies visible project filtering removes missing directories and
    /// agentty-managed worktree paths.
    #[test]
    fn visible_project_rows_excludes_missing_and_worktree_projects() {
        // Arrange
        let visible_project_path = PathBuf::from("/workspace/visible");
        let missing_project_path = PathBuf::from("/workspace/missing");
        let session_worktree_root = Path::new("/workspace/.agentty/wt");
        let worktree_project_path = session_worktree_root.join("session-a");
        let project_rows = vec![
            project_list_row_fixture(1, &visible_project_path),
            project_list_row_fixture(2, &missing_project_path),
            project_list_row_fixture(3, &worktree_project_path),
        ];
        let fs_client = mock_fs_client_with_directories(HashSet::from([
            visible_project_path.clone(),
            worktree_project_path,
        ]));

        // Act
        let visible_rows =
            AppStartup::visible_project_rows(project_rows, &fs_client, session_worktree_root);

        // Assert
        assert_eq!(visible_rows.len(), 1);
        assert_eq!(visible_rows[0].path, visible_project_path.to_string_lossy());
    }

    /// Verifies project-item loading uses database rows and filters
    /// agentty-managed worktree paths before building UI items.
    #[tokio::test]
    async fn load_project_items_with_session_worktree_root_filters_database_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");
        let visible_project_path = PathBuf::from("/workspace/visible");
        let session_worktree_root = Path::new("/workspace/.agentty/wt");
        let worktree_project_path = session_worktree_root.join("session-a");
        database
            .upsert_project(&visible_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to persist visible project");
        database
            .upsert_project(&worktree_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to persist worktree project");
        let fs_client = mock_fs_client_with_directories(HashSet::from([
            visible_project_path.clone(),
            worktree_project_path,
        ]));

        // Act
        let project_items = AppStartup::load_project_items_with_session_worktree_root(
            &database,
            &fs_client,
            session_worktree_root,
        )
        .await;

        // Assert
        assert_eq!(project_items.len(), 1);
        assert_eq!(project_items[0].project.path, visible_project_path);
    }

    /// Verifies home catalog discovery finds repository roots while excluding
    /// agentty session worktrees.
    #[test]
    fn discover_home_project_paths_includes_git_repos_and_excludes_session_worktrees() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let home_directory = temp_dir.path();
        let project_a_path = home_directory.join("project-a");
        let project_b_path = home_directory.join("project-b");
        let session_worktree_root = home_directory.join(".agentty").join(AGENTTY_WT_DIR);
        let session_project_path = session_worktree_root.join("session-a");
        fs::create_dir_all(project_a_path.join(".git")).expect("failed to create first repo");
        fs::create_dir_all(project_b_path.join(".git")).expect("failed to create second repo");
        fs::create_dir_all(session_project_path.join(".git"))
            .expect("failed to create session worktree repo");

        // Act
        let discovered_project_paths =
            AppStartup::discover_home_project_paths(home_directory, &session_worktree_root);

        // Assert
        assert_eq!(
            discovered_project_paths,
            vec![project_a_path, project_b_path]
        );
    }
}
