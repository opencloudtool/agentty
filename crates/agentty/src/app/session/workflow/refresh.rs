//! Session refresh scheduling and post-reload view state restoration.

use std::collections::HashSet;
use std::time::Instant;

use ag_forge as forge;

use super::SESSION_REFRESH_INTERVAL;
use crate::app::session::SessionError;
use crate::app::{AppServices, ProjectManager, SessionManager};
use crate::domain::session::{ForgeKind, ReviewRequest};
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode};

impl SessionManager {
    /// Reloads session rows when the metadata cache indicates a change.
    ///
    /// This is a low-frequency fallback safety poll; primary refreshes should
    /// come from explicit `RefreshSessions` events.
    pub async fn refresh_sessions_if_needed(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        if !self.is_session_refresh_due() {
            return;
        }

        self.state.refresh_deadline = self.next_refresh_deadline();

        let Ok(sessions_metadata) = services.db().load_sessions_metadata().await else {
            return;
        };
        let (sessions_row_count, sessions_updated_at_max) = sessions_metadata;
        if sessions_row_count == self.state.row_count
            && sessions_updated_at_max == self.state.updated_at_max
        {
            return;
        }

        self.reload_sessions(mode, projects, services, Some(sessions_metadata))
            .await;
    }

    /// Reloads sessions immediately, bypassing refresh deadline checks.
    pub(crate) async fn refresh_sessions_now(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        let sessions_metadata = services.db().load_sessions_metadata().await.ok();
        self.reload_sessions(mode, projects, services, sessions_metadata)
            .await;
        self.state.refresh_deadline = self.next_refresh_deadline();
    }

    /// Refreshes one linked review request and persists the latest normalized
    /// remote state.
    ///
    /// Linked review requests remain available for `Done` and `Canceled`
    /// sessions. When merge or cancel cleanup has removed the session
    /// worktree, this reconstructs the forge remote from the stored review
    /// request URL so refresh can continue without reviving legacy worktree
    /// behavior.
    ///
    /// # Errors
    /// Returns an error if the session is missing, has no linked review
    /// request, the forge remote cannot be resolved, the provider refresh
    /// fails, or persistence fails.
    pub async fn refresh_review_request(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<ReviewRequest, SessionError> {
        let session_index = self.session_index_or_err(session_id)?;
        let Some(session) = self.state.sessions.get(session_index) else {
            return Err(SessionError::NotFound);
        };
        let linked_review_request = session.review_request.clone().ok_or_else(|| {
            SessionError::Workflow("Session has no linked review request".to_string())
        })?;
        let remote = self
            .review_request_remote(services, session, &linked_review_request)
            .await?;
        let refreshed_summary = services
            .review_request_client()
            .refresh_review_request(remote, linked_review_request.summary.display_id.clone())
            .await
            .map_err(|error| SessionError::Workflow(error.detail_message()))?;
        self.store_review_request_summary(services, session_id, refreshed_summary)
            .await
    }

    /// Reloads sessions and derived statistics, then restores UI state.
    async fn reload_sessions(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
        sessions_metadata: Option<(i64, i64)>,
    ) {
        let selected_index = self.state.table_state.selected();
        let selected_session_id = selected_index
            .and_then(|index| self.state.sessions.get(index))
            .map(|session| session.id.clone());

        let (sessions, stats_activity) = Self::load_sessions(
            services.base_path(),
            services.db(),
            projects.active_project_id(),
            projects.working_dir(),
            &mut self.state.handles,
        )
        .await;
        self.state.sessions = sessions;
        self.stats_activity = stats_activity;
        self.restore_table_selection(selected_session_id.as_deref(), selected_index);
        self.ensure_mode_session_exists(mode);

        let active_session_ids: HashSet<String> = self
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        self.state
            .retain_follow_up_task_positions(&active_session_ids);
        self.state.retain_session_git_statuses(&active_session_ids);
        self.worker_service_mut()
            .retain_active_workers(&active_session_ids);

        if let Some((sessions_row_count, sessions_updated_at_max)) = sessions_metadata {
            self.state.row_count = sessions_row_count;
            self.state.updated_at_max = sessions_updated_at_max;
        } else {
            self.update_sessions_metadata_cache(services).await;
        }
    }

    /// Returns `true` when periodic session refresh should run.
    fn is_session_refresh_due(&self) -> bool {
        self.state.clock.now_instant() >= self.state.refresh_deadline
    }

    /// Computes the next refresh deadline from the injected clock.
    fn next_refresh_deadline(&self) -> Instant {
        self.state.clock.now_instant() + SESSION_REFRESH_INTERVAL
    }

    /// Resolves forge remote metadata for one persisted review-request link.
    ///
    /// Active sessions prefer the live worktree remote. Terminal sessions can
    /// fall back to the stored review-request URL after worktree cleanup has
    /// removed the local checkout.
    async fn review_request_remote(
        &self,
        services: &AppServices,
        session: &crate::domain::session::Session,
        review_request: &ReviewRequest,
    ) -> Result<forge::ForgeRemote, SessionError> {
        let repo_url = if services.fs_client().is_dir(session.folder.clone()) {
            services
                .git_client()
                .repo_url(session.folder.clone())
                .await
                .ok()
        } else {
            None
        }
        .or_else(|| Self::review_request_repo_url(review_request))
        .ok_or_else(|| {
            SessionError::Workflow(
                "Failed to resolve repository remote for linked review request".to_string(),
            )
        })?;

        services
            .review_request_client()
            .detect_remote(repo_url)
            .map_err(|error| SessionError::Workflow(error.detail_message()))
    }

    /// Derives a repository URL from one persisted review-request web URL.
    fn review_request_repo_url(review_request: &ReviewRequest) -> Option<String> {
        let web_url = review_request.summary.web_url.trim_end_matches('/');

        match review_request.summary.forge_kind {
            ForgeKind::GitHub => web_url
                .split_once("/pull/")
                .map(|(repo_url, _)| repo_url.to_string()),
        }
    }

    /// Restores table selection after session list reload.
    fn restore_table_selection(
        &mut self,
        selected_session_id: Option<&str>,
        selected_index: Option<usize>,
    ) {
        if self.state.sessions.is_empty() {
            self.state.table_state.select(None);

            return;
        }

        if let Some(session_id) = selected_session_id
            && let Some(index) = self
                .sessions
                .iter()
                .position(|session| session.id == session_id)
        {
            self.state.table_state.select(Some(index));

            return;
        }

        let restored_index = selected_index.map(|index| index.min(self.state.sessions.len() - 1));
        self.state.table_state.select(restored_index);
    }

    /// Switches back to list mode if the currently viewed session is missing.
    fn ensure_mode_session_exists(&self, mode: &mut AppMode) {
        let mode_session_id = match &*mode {
            AppMode::Confirmation {
                session_id: Some(session_id),
                ..
            }
            | AppMode::Prompt { session_id, .. }
            | AppMode::Question { session_id, .. }
            | AppMode::View { session_id, .. }
            | AppMode::Diff { session_id, .. }
            | AppMode::OpenCommandSelector {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            }
            | AppMode::PublishBranchInput {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            } => Some(session_id),
            _ => None,
        };
        let Some(session_id) = mode_session_id else {
            return;
        };
        if self.session_index_for_id(session_id).is_none() {
            *mode = AppMode::List;
        }
    }

    /// Refreshes cached session metadata used by incremental list reloads.
    pub(crate) async fn update_sessions_metadata_cache(&mut self, services: &AppServices) {
        if let Ok((sessions_row_count, sessions_updated_at_max)) =
            services.db().load_sessions_metadata().await
        {
            self.state.row_count = sessions_row_count;
            self.state.updated_at_max = sessions_updated_at_max;
        }
    }
}

/// Outcome from syncing one session's review request state with the forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SyncReviewRequestOutcome {
    /// The linked review request is still open.
    Open {
        /// Short display identifier (for example `#42`).
        display_id: String,
        /// Optional provider-specific status detail.
        status_summary: Option<String>,
    },
    /// The linked review request was merged upstream.
    Merged {
        /// Short display identifier (for example `#42`).
        display_id: String,
    },
    /// The linked review request was closed without merge.
    Closed {
        /// Short display identifier (for example `#42`).
        display_id: String,
    },
    /// No review request was found for the session branch.
    NoReviewRequest,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    use ag_forge as forge;
    use ratatui::widgets::TableState;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::app::session::{Clock, SessionDefaults};
    use crate::app::{AppServices, SessionState};
    use crate::domain::agent::{AgentKind, AgentModel};
    use crate::domain::session::{
        ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Session,
        SessionHandles, SessionSize, SessionStats, Status,
    };
    use crate::infra::db::Database;
    use crate::infra::{app_server, fs, git};

    /// Builds one mock app-server client wrapped in `Arc` for service
    /// fixtures.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds a filesystem mock that delegates directory checks to local disk.
    fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_create_dir_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_remove_dir_all()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_read_file()
            .times(0..)
            .returning(|path| {
                Box::pin(async move { tokio::fs::read(path).await.map_err(fs::FsError::from) })
            });
        mock_fs_client
            .expect_remove_file()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_fs_client
            .expect_is_dir()
            .times(0..)
            .returning(|path| path.is_dir());

        mock_fs_client
    }

    /// Persists one session row that matches the in-memory fixture.
    async fn database_with_session(session: &Session) -> Database {
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                &session.id,
                session.model.as_str(),
                &session.base_branch,
                &session.status.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert session");
        if let Some(review_request) = &session.review_request {
            database
                .update_session_review_request(&session.id, Some(review_request))
                .await
                .expect("failed to persist session review request");
        }

        database
    }

    /// Builds app services with caller-provided git and forge boundaries.
    fn test_services(
        database: Database,
        git_client: Arc<dyn git::GitClient>,
        review_request_client: Arc<dyn forge::ReviewRequestClient>,
    ) -> AppServices {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        AppServices::new(
            PathBuf::from("/tmp/agentty-tests"),
            Arc::new(crate::app::session::RealClock),
            database,
            event_tx,
            crate::app::service::AppServiceDeps {
                app_server_client_override: Some(mock_app_server()),
                available_agent_kinds: crate::domain::agent::AgentKind::ALL.to_vec(),
                fs_client: Arc::new(create_passthrough_mock_fs_client()),
                git_client,
                review_request_client,
            },
        )
    }

    /// Builds one session manager with deterministic time and one session.
    fn session_manager_with_session(clock: Arc<dyn Clock>, session: Session) -> SessionManager {
        let mut handles = HashMap::new();
        handles.insert(
            session.id.clone(),
            SessionHandles::new(session.output.clone(), session.status),
        );

        SessionManager::new(
            SessionDefaults {
                model: AgentKind::Gemini.default_model(),
            },
            Arc::new(git::MockGitClient::new()),
            SessionState::new(handles, vec![session], TableState::default(), clock, 1, 0),
            Vec::new(),
        )
    }

    /// Builds one session fixture with optional linked review-request data.
    fn test_session(
        folder: PathBuf,
        review_request: Option<ReviewRequest>,
        status: Status,
    ) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder,
            follow_up_tasks: Vec::new(),
            id: "session-id".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "Implement forge review support".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status,
            summary: None,
            title: Some("Add forge review support".to_string()),
            updated_at: 0,
        }
    }

    /// Builds one normalized GitHub review-request summary.
    fn review_request_summary(display_id: &str, state: ReviewRequestState) -> ReviewRequestSummary {
        ReviewRequestSummary {
            display_id: display_id.to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "agentty/session-".to_string(),
            state,
            status_summary: Some("Checks pending".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: format!(
                "https://github.com/agentty-xyz/agentty/pull/{}",
                &display_id[1..]
            ),
        }
    }

    #[tokio::test]
    async fn test_refresh_review_request_updates_done_session_from_stored_link_when_worktree_is_missing()
     {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let missing_folder = temp_dir.path().join("missing-session-folder");
        let linked_review_request = ReviewRequest {
            last_refreshed_at: 12,
            summary: review_request_summary("#42", ReviewRequestState::Open),
        };
        let session = test_session(missing_folder, Some(linked_review_request), Status::Done);
        let database = database_with_session(&session).await;
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(
            now,
            SystemTime::UNIX_EPOCH + Duration::from_secs(77),
        ));
        let clock: Arc<dyn Clock> = fake_clock;
        let mut session_manager = session_manager_with_session(clock, session);
        let remote = forge::ForgeRemote {
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        };
        let refreshed_summary = ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "agentty/session-".to_string(),
            state: ReviewRequestState::Merged,
            status_summary: Some("Approved and merged".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        };
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client.expect_repo_url().times(0);
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty")
            .returning({
                let remote = remote.clone();
                move |_| Ok(remote.clone())
            });
        mock_review_request_client
            .expect_refresh_review_request()
            .times(1)
            .withf({
                let remote = remote.clone();
                move |candidate_remote, display_id| {
                    candidate_remote == &remote && display_id == "#42"
                }
            })
            .returning(move |_, _| {
                let refreshed_summary = refreshed_summary.clone();

                Box::pin(async move { Ok(refreshed_summary) })
            });
        let services = test_services(
            database.clone(),
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
        );

        // Act
        let review_request = session_manager
            .refresh_review_request(&services, "session-id")
            .await
            .expect("linked review request should refresh");
        let persisted_row = database
            .load_sessions()
            .await
            .expect("failed to load session rows")
            .into_iter()
            .find(|row| row.id == "session-id")
            .expect("session row should exist");

        // Assert
        assert_eq!(review_request.last_refreshed_at, 77);
        assert_eq!(review_request.summary.state, ReviewRequestState::Merged);
        assert_eq!(
            session_manager.state.sessions[0]
                .review_request
                .as_ref()
                .map(|review_request| review_request.summary.state),
            Some(ReviewRequestState::Merged)
        );
        assert_eq!(
            persisted_row
                .review_request
                .as_ref()
                .map(|row| row.state.as_str()),
            Some("Merged")
        );
    }

    #[test]
    fn test_is_session_refresh_due_returns_false_before_deadline() {
        // Arrange
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn Clock> = fake_clock;
        let session_manager = session_manager_fixture(clock);

        // Act
        let refresh_due = session_manager.is_session_refresh_due();
        let wall_clock = session_manager.state().clock.now_system_time();

        // Assert
        assert!(!refresh_due);
        assert_eq!(wall_clock, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn test_is_session_refresh_due_returns_true_at_deadline() {
        // Arrange
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn Clock> = fake_clock.clone();
        let session_manager = session_manager_fixture(clock);
        fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

        // Act
        let refresh_due = session_manager.is_session_refresh_due();

        // Assert
        assert!(refresh_due);
    }

    /// Builds a session manager with deterministic time and empty state.
    fn session_manager_fixture(clock: Arc<dyn Clock>) -> SessionManager {
        let git_client: Arc<dyn git::GitClient> = Arc::new(git::MockGitClient::new());

        SessionManager::new(
            SessionDefaults {
                model: AgentKind::Gemini.default_model(),
            },
            git_client,
            SessionState::new(
                HashMap::new(),
                Vec::new(),
                TableState::default(),
                clock,
                0,
                0,
            ),
            Vec::new(),
        )
    }

    /// Test clock implementation with mutable `Instant` and `SystemTime`.
    struct FakeClock {
        instant: Mutex<Instant>,
        system_time: Mutex<SystemTime>,
    }

    impl FakeClock {
        /// Creates a fake clock seeded with deterministic wall-clock values.
        fn new(instant: Instant, system_time: SystemTime) -> Self {
            Self {
                instant: Mutex::new(instant),
                system_time: Mutex::new(system_time),
            }
        }

        /// Overrides the fake monotonic instant used by refresh checks.
        fn set_now_instant(&self, instant: Instant) {
            let mut current_instant = self
                .instant
                .lock()
                .expect("fake clock instant lock should not be poisoned");
            *current_instant = instant;
        }
    }

    impl Clock for FakeClock {
        fn now_instant(&self) -> Instant {
            *self
                .instant
                .lock()
                .expect("fake clock instant lock should not be poisoned")
        }

        fn now_system_time(&self) -> SystemTime {
            *self
                .system_time
                .lock()
                .expect("fake clock system-time lock should not be poisoned")
        }
    }
}
