//! Session refresh scheduling and post-reload view state restoration.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ag_forge as forge;
use tokio::task::{JoinHandle, JoinSet};
use tracing::warn;

use super::SESSION_REFRESH_INTERVAL;
use super::load::SessionLoadInput;
use crate::app::session::SessionError;
use crate::app::{AppServices, ProjectManager, SessionManager};
use crate::domain::session::{
    DailyActivity, ForgeKind, ReviewRequest, Session, SessionHandles, SessionId,
};
use crate::infra::db::DbError;
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, HelpContext};

/// One coalesced snapshot load, isolated from live handles until reduction.
pub(in crate::app::session) struct BackgroundSessionRefresh {
    detail_session_id: Option<SessionId>,
    project_id: i64,
    refresh_again: bool,
    task: JoinHandle<Result<Option<SessionRefreshSnapshot>, DbError>>,
}

impl Drop for BackgroundSessionRefresh {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct SessionRefreshSnapshot {
    branch_names: HashMap<SessionId, String>,
    handles: HashMap<SessionId, SessionHandles>,
    metadata: Option<(i64, i64)>,
    sessions: Vec<Session>,
    stats_activity: Vec<DailyActivity>,
    worktree_availability: HashMap<SessionId, bool>,
}

impl SessionManager {
    /// Reloads session rows when the metadata cache indicates a change.
    ///
    /// This is a low-frequency fallback safety poll; primary refreshes should
    /// come from explicit `RefreshSessions` events. Returns `true` when the
    /// poll detected persisted changes and refreshed the in-memory snapshots.
    pub async fn refresh_sessions_if_needed(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> bool {
        let refreshed = self
            .finish_background_refresh(mode, projects, services)
            .await;
        if self.is_session_refresh_due() && self.pending_refresh.is_none() {
            self.start_session_refresh(mode, projects, services, false);
        }

        refreshed
    }

    /// Coalesces refresh requests without awaiting disk or repository work.
    pub(crate) fn request_session_refresh(
        &mut self,
        mode: &AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        self.start_session_refresh(mode, projects, services, true);
    }

    fn start_session_refresh(
        &mut self,
        mode: &AppMode,
        projects: &ProjectManager,
        services: &AppServices,
        force: bool,
    ) {
        if let Some(pending) = self.pending_refresh.as_mut()
            && pending.project_id == projects.active_project_id()
        {
            pending.refresh_again = true;
            return;
        }
        let project_id = projects.active_project_id();
        let detail_session_id = Self::mode_session_id(mode).cloned().or_else(|| {
            self.state
                .table_state
                .selected()
                .and_then(|index| self.state.sessions.get(index))
                .map(|session| session.id.clone())
        });
        let base = services.base_path().to_path_buf();
        let working_dir = projects.working_dir().to_path_buf();
        let repositories = services.db().clone();
        let clock = services.clock();
        let fs_client = services.fs_client();
        let git_client = self.git_client.clone();
        let task_detail_id = detail_session_id.clone();
        let previous_metadata = (self.state.row_count, self.state.updated_at_max);
        let mut handles = self
            .state
            .handles()
            .iter()
            .filter_map(|(id, handles)| {
                handles
                    .status
                    .lock()
                    .ok()
                    .map(|status| (id.clone(), SessionHandles::new_unloaded(*status)))
            })
            .collect();
        let task = tokio::spawn(async move {
            let metadata = repositories.sessions().load_sessions_metadata().await.ok();
            if !force && metadata == Some(previous_metadata) {
                return Ok(None);
            }
            let (sessions, stats_activity, worktree_availability) =
                Self::try_load_sessions_with_fs_client(
                    SessionLoadInput {
                        active_project_id: project_id,
                        active_session_id: task_detail_id.as_deref(),
                        base: &base,
                        clock: clock.as_ref(),
                        db: &repositories,
                        fs_client: fs_client.as_ref(),
                        working_dir: &working_dir,
                    },
                    &mut handles,
                )
                .await?;
            let mut branch_names = HashMap::new();
            let mut tasks = JoinSet::new();
            for session in &sessions {
                let git_client = git_client.clone();
                let session_id = session.id.clone();
                let folder = session.folder.clone();
                tasks.spawn(async move {
                    let name = git_client
                        .detect_git_info(folder)
                        .await
                        .unwrap_or_else(|| super::session_branch(&session_id));
                    (session_id, name)
                });
                if tasks.len() >= 8
                    && let Some(Ok((session_id, name))) = tasks.join_next().await
                {
                    branch_names.insert(session_id, name);
                }
            }
            while let Some(result) = tasks.join_next().await {
                if let Ok((session_id, name)) = result {
                    branch_names.insert(session_id, name);
                }
            }

            Ok(Some(SessionRefreshSnapshot {
                branch_names,
                handles,
                metadata,
                sessions,
                stats_activity,
                worktree_availability,
            }))
        });
        self.pending_refresh = Some(BackgroundSessionRefresh {
            detail_session_id,
            project_id,
            refresh_again: false,
            task,
        });
        self.state.refresh_deadline = self.next_refresh_deadline();
    }

    /// Applies completed I/O against current selection and live worker handles.
    async fn finish_background_refresh(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> bool {
        let Some(mut pending) = self
            .pending_refresh
            .take_if(|pending| pending.task.is_finished())
        else {
            return false;
        };
        if pending.project_id != projects.active_project_id() {
            return false;
        }
        if pending.refresh_again
            || Self::mode_session_id(mode)
                .is_some_and(|id| pending.detail_session_id.as_ref() != Some(id))
        {
            self.request_session_refresh(mode, projects, services);
            return false;
        }
        let Ok(Ok(Some(mut snapshot))) = (&mut pending.task).await else {
            return false;
        };
        let selected_index = self.state.table_state.selected();
        let selected_id = selected_index
            .and_then(|index| self.state.sessions.get(index))
            .map(|session| session.id.clone());
        let live_progress = self
            .state
            .sessions
            .iter()
            .filter_map(|session| {
                session
                    .orchestration_progress
                    .as_ref()
                    .filter(|progress| progress.starts_with("Phase:"))
                    .map(|progress| (session.id.clone(), progress.clone()))
            })
            .collect();
        for session in &mut snapshot.sessions {
            if let Some(handles) = self.state.handles().get(&session.id) {
                session.status = super::load::merge_loaded_session_status(
                    session.status,
                    handles
                        .status
                        .lock()
                        .map_or(session.status, |status| *status),
                );
                if let Ok(mut status) = handles.status.lock() {
                    *status = session.status;
                }
                handles.transcript_snapshot_with_loaded(session.transcript.as_ref());
            } else if let Some(handles) = snapshot.handles.remove(&session.id) {
                self.state.handles_mut().insert(session.id.clone(), handles);
            }
        }
        Self::preserve_live_orchestration_progress(&mut snapshot.sessions, &live_progress);
        self.state.replace_sessions(snapshot.sessions);
        self.state.sync_from_handles();
        self.state
            .replace_session_worktree_availability(snapshot.worktree_availability);
        self.replace_session_branch_names(snapshot.branch_names);
        self.stats_activity = snapshot.stats_activity;
        self.restore_table_selection(selected_id.as_deref(), selected_index);
        self.ensure_mode_session_exists(mode);
        let active_session_ids = self
            .state
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        self.state
            .retain_follow_up_task_positions(&active_session_ids);
        self.state.retain_session_git_statuses(&active_session_ids);
        if let Some((count, updated_at)) = snapshot.metadata {
            self.state.row_count = count;
            self.state.updated_at_max = updated_at;
        }

        true
    }

    /// Reloads sessions immediately, bypassing refresh deadline checks.
    pub(crate) async fn refresh_sessions_now(
        &mut self,
        mode: &mut AppMode,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        self.pending_refresh = None;
        let sessions_metadata = services.db().sessions().load_sessions_metadata().await.ok();
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
            .review_request_remote(services, session, Some(&linked_review_request))
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
        let detail_session_id = Self::mode_session_id(mode)
            .cloned()
            .or_else(|| selected_session_id.clone());
        let live_orchestration_progress = self
            .state
            .sessions
            .iter()
            .filter_map(|session| {
                session
                    .orchestration_progress
                    .as_deref()
                    .filter(|progress| progress.starts_with("Phase:"))
                    .map(|progress| (session.id.clone(), progress.to_string()))
            })
            .collect::<HashMap<_, _>>();

        let clock = services.clock();
        let fs_client = services.fs_client();
        let (mut sessions, stats_activity, session_worktree_availability) =
            match Self::try_load_sessions_with_fs_client(
                SessionLoadInput {
                    active_project_id: projects.active_project_id(),
                    active_session_id: detail_session_id.as_deref(),
                    base: services.base_path(),
                    clock: clock.as_ref(),
                    db: services.db(),
                    fs_client: fs_client.as_ref(),
                    working_dir: projects.working_dir(),
                },
                self.state.handles_mut(),
            )
            .await
            {
                Ok(loaded_sessions) => loaded_sessions,
                Err(error) => {
                    warn!(
                        error = %error,
                        "preserving active session state after refresh failure"
                    );

                    return;
                }
            };
        Self::preserve_live_orchestration_progress(&mut sessions, &live_orchestration_progress);
        self.state.replace_sessions(sessions);
        self.state
            .replace_session_worktree_availability(session_worktree_availability);
        self.refresh_session_branch_names().await;
        self.stats_activity = stats_activity;
        self.restore_table_selection(selected_session_id.as_deref(), selected_index);
        self.ensure_mode_session_exists(mode);

        let active_session_ids: HashSet<SessionId> = self
            .sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect();
        self.state
            .retain_follow_up_task_positions(&active_session_ids);
        self.state.retain_session_branch_names(&active_session_ids);
        self.state.retain_session_git_statuses(&active_session_ids);

        if let Some((sessions_row_count, sessions_updated_at_max)) = sessions_metadata {
            self.state.row_count = sessions_row_count;
            self.state.updated_at_max = sessions_updated_at_max;
        } else {
            self.update_sessions_metadata_cache(services).await;
        }
    }

    /// Returns the session whose detail is visible or being edited in the
    /// current application mode.
    fn mode_session_id(mode: &AppMode) -> Option<&SessionId> {
        match mode {
            AppMode::Confirmation {
                session_id: Some(session_id),
                ..
            }
            | AppMode::Prompt { session_id, .. }
            | AppMode::Question { session_id, .. }
            | AppMode::View { session_id, .. }
            | AppMode::DiffLoading { session_id, .. }
            | AppMode::Diff { session_id, .. }
            | AppMode::Help {
                context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
                ..
            }
            | AppMode::ViewInfoPopup {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            }
            | AppMode::LaunchConfigurationSelector {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            }
            | AppMode::PublishBranchInput {
                restore_view: ConfirmationViewMode { session_id, .. },
                ..
            } => Some(session_id),
            _ => None,
        }
    }

    /// Keeps a coordinator-emitted task snapshot across metadata reloads while
    /// the persisted campaign remains active.
    fn preserve_live_orchestration_progress(
        sessions: &mut [crate::domain::session::Session],
        live_progress: &HashMap<SessionId, String>,
    ) {
        for session in sessions {
            if session.orchestration_progress.is_some()
                && let Some(progress) = live_progress.get(&session.id)
            {
                session.orchestration_progress = Some(progress.clone());
            }
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
    pub(super) async fn review_request_remote(
        &self,
        services: &AppServices,
        session: &crate::domain::session::Session,
        review_request: Option<&ReviewRequest>,
    ) -> Result<forge::ForgeRemote, SessionError> {
        if let Ok(repo_url) = services.git_client().repo_url(session.folder.clone()).await {
            return services
                .review_request_client()
                .detect_remote(repo_url)
                .map(|remote| remote.with_command_working_directory(session.folder.clone()))
                .map_err(|error| SessionError::Workflow(error.detail_message()));
        }

        let repo_url = review_request
            .and_then(Self::review_request_repo_url)
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
    pub(crate) fn review_request_repo_url(review_request: &ReviewRequest) -> Option<String> {
        let web_url = review_request.summary.web_url.trim_end_matches('/');

        match review_request.summary.forge_kind {
            ForgeKind::GitHub => web_url
                .split_once("/pull/")
                .map(|(repo_url, _)| repo_url.to_string()),
            ForgeKind::GitLab => web_url
                .split_once("/-/merge_requests/")
                .or_else(|| web_url.split_once("/merge_requests/"))
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
                .sessions()
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
        let mode_session_id = Self::mode_session_id(mode);
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
            services.db().sessions().load_sessions_metadata().await
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
        /// Optional session branch `HEAD` hash observed during sync for
        /// continuation context.
        session_head_hash: Option<String>,
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
    use ag_git as git;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::app::session::{Clock, SessionDefaults};
    use crate::app::{AppServices, SessionState};
    use crate::domain::agent::AgentKind;
    use crate::domain::input::InputState;
    use crate::domain::selection::SelectionState;
    use crate::domain::session::{
        ForgeKind, PublishBranchAction, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
        Session, SessionHandles, Status,
    };
    use crate::infra::db::AppRepositories;
    use crate::infra::fs;
    use crate::presentation::app_mode::{
        DiffFocus, DiffLineComments, DiffPreview, DiffSidebarFocus,
    };
    use crate::presentation::help_action::ViewSessionState;

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
    async fn database_with_session(session: &Session) -> AppRepositories {
        let database = AppRepositories::in_memory().await.expect("db should open");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session(
                &session.id,
                session.agent.model().as_str(),
                &session.base_branch,
                &session.status.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert session");
        if let Some(review_request) = &session.review_request {
            database
                .reviews()
                .update_session_review_request(&session.id, Some(review_request.clone()))
                .await
                .expect("failed to persist session review request");
        }

        database
    }

    /// Builds app services with caller-provided git and forge boundaries.
    fn test_services(
        database: &AppRepositories,
        git_client: Arc<dyn git::GitClient>,
        review_request_client: Arc<dyn forge::ReviewRequestClient>,
    ) -> AppServices {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        AppServices::new_with_agent_clis(
            PathBuf::from("/tmp/agentty-tests"),
            Arc::new(crate::infra::clock::RealClock),
            event_tx,
            crate::app::service::AppServiceDeps {
                app_server_client_override: Some(crate::test_support::mock_app_server()),
                available_agent_kinds: crate::domain::agent::AgentKind::ALL.to_vec(),
                clipboard_image_client_override: None,
                fs_client: Arc::new(create_passthrough_mock_fs_client()),
                git_client,
                one_shot_client_override: None,
                personality_catalog_client_override: None,
                repositories: database.clone(),
                review_request_client,
            },
            crate::domain::agent::AgentCliInfo::from_kinds(crate::domain::agent::AgentKind::ALL),
        )
    }

    /// Builds one session manager with deterministic time and one session.
    fn session_manager_with_session(clock: Arc<dyn Clock>, session: Session) -> SessionManager {
        let mut handles = HashMap::new();
        handles.insert(
            session.id.clone(),
            SessionHandles::new_with_transcript(
                session.status,
                session.transcript.clone().unwrap_or_default(),
            ),
        );

        SessionManager::new(
            SessionDefaults {
                model: AgentKind::Antigravity.default_model(),
            },
            Arc::new(git::MockGitClient::new()),
            SessionState::new(
                handles,
                vec![session],
                SelectionState::default(),
                clock,
                1,
                0,
            ),
            Vec::new(),
        )
    }

    /// Builds one session fixture with optional linked review-request data.
    fn test_session(
        folder: PathBuf,
        review_request: Option<ReviewRequest>,
        status: Status,
    ) -> Session {
        crate::test_support::SessionFixtureBuilder::new()
            .folder(folder)
            .prompt("Implement forge review support")
            .review_request(review_request)
            .status(status)
            .title(Some("Add forge review support".to_string()))
            .build()
    }

    /// Builds one normalized GitHub review-request summary.
    fn review_request_summary(display_id: &str, state: ReviewRequestState) -> ReviewRequestSummary {
        ReviewRequestSummary {
            display_id: display_id.to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-".to_string(),
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
    async fn review_request_remote_uses_live_worktree_for_detected_remote() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session");
        let session = test_session(session_folder.clone(), None, Status::Review);
        let database = database_with_session(&session).await;
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn Clock> = fake_clock;
        let session_manager = session_manager_with_session(clock, session);
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_repo_url()
            .once()
            .withf({
                let session_folder = session_folder.clone();
                move |candidate_folder| candidate_folder == &session_folder
            })
            .returning(|_| {
                Box::pin(async { Ok("https://github.com/agentty-xyz/agentty".to_string()) })
            });
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty")
            .returning(|_| {
                Ok(forge::ForgeRemote {
                    command_working_directory: None,
                    forge_kind: ForgeKind::GitHub,
                    host: "github.com".to_string(),
                    namespace: "agentty-xyz".to_string(),
                    project: "agentty".to_string(),
                    repo_url: "https://github.com/agentty-xyz/agentty".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty".to_string(),
                })
            });
        let services = test_services(
            &database,
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
        );

        // Act
        let remote = session_manager
            .review_request_remote(&services, &session_manager.state.sessions[0], None)
            .await
            .expect("remote should resolve");

        // Assert
        assert_eq!(remote.command_working_directory, Some(session_folder));
        assert_eq!(remote.forge_kind, ForgeKind::GitHub);
    }

    #[test]
    fn review_request_repo_url_derives_gitlab_project_url() {
        // Arrange
        let review_request = ReviewRequest {
            last_refreshed_at: 10,
            summary: ReviewRequestSummary {
                display_id: "!42".to_string(),
                forge_kind: ForgeKind::GitLab,
                source_branch: "wt/session-".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Draft".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
            },
        };

        // Act
        let repo_url = SessionManager::review_request_repo_url(&review_request);

        // Assert
        assert_eq!(
            repo_url.as_deref(),
            Some("https://gitlab.com/agentty-xyz/agentty")
        );
    }

    #[tokio::test]
    async fn test_refresh_review_request_updates_done_session_from_stored_link_when_worktree_is_missing()
     {
        // Arrange
        let MissingWorktreeReviewRefreshFixture {
            database,
            services,
            mut session_manager,
        } = missing_worktree_review_refresh_fixture().await;

        // Act
        let review_request = session_manager
            .refresh_review_request(&services, "session-id")
            .await
            .expect("linked review request should refresh");
        let persisted_row = database
            .sessions()
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

    /// Test fixture for refreshing a stored review request after its worktree
    /// has been deleted.
    struct MissingWorktreeReviewRefreshFixture {
        /// Repository bundle containing the linked review request session.
        database: AppRepositories,
        /// App services wired with git and forge mocks.
        services: AppServices,
        /// Session manager seeded with the linked session.
        session_manager: SessionManager,
    }

    /// Builds the missing-worktree review refresh fixture.
    async fn missing_worktree_review_refresh_fixture() -> MissingWorktreeReviewRefreshFixture {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let missing_folder = temp_dir.path().join("missing-session-folder");
        let linked_review_request = ReviewRequest {
            last_refreshed_at: 12,
            summary: review_request_summary("#42", ReviewRequestState::Open),
        };
        let session = test_session(
            missing_folder.clone(),
            Some(linked_review_request),
            Status::Done,
        );
        let database = database_with_session(&session).await;
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(
            now,
            SystemTime::UNIX_EPOCH + Duration::from_secs(77),
        ));
        let clock: Arc<dyn Clock> = fake_clock;
        let session_manager = session_manager_with_session(clock, session);
        let remote = forge::ForgeRemote {
            command_working_directory: None,
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
            source_branch: "wt/session-".to_string(),
            state: ReviewRequestState::Merged,
            status_summary: Some("Approved and merged".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        };
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_repo_url()
            .times(1)
            .withf({
                let missing_folder = missing_folder.clone();
                move |candidate_folder| candidate_folder == &missing_folder
            })
            .returning(|_| {
                Box::pin(async {
                    Err(ag_git::GitError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "missing worktree",
                    )))
                })
            });
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
            .withf(move |candidate_remote, display_id| {
                candidate_remote == &remote && display_id == "#42"
            })
            .returning(move |_, _| {
                let refreshed_summary = refreshed_summary.clone();

                Box::pin(async move { Ok(refreshed_summary) })
            });
        let services = test_services(
            &database,
            Arc::new(mock_git_client),
            Arc::new(mock_review_request_client),
        );

        MissingWorktreeReviewRefreshFixture {
            database,
            services,
            session_manager,
        }
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
    fn preserve_live_orchestration_progress_keeps_active_board_and_respects_terminal_clear() {
        // Arrange
        let mut session = test_session(PathBuf::from("/tmp/session"), None, Status::Review);
        session.orchestration_progress = Some("2 running, 0 waiting on you".to_string());
        let live_progress = HashMap::from([(
            session.id.clone(),
            "Phase: Running\n- protocol [protocol]: running".to_string(),
        )]);

        // Act
        SessionManager::preserve_live_orchestration_progress(
            std::slice::from_mut(&mut session),
            &live_progress,
        );
        let active_progress = session.orchestration_progress.clone();
        session.orchestration_progress = None;
        SessionManager::preserve_live_orchestration_progress(
            std::slice::from_mut(&mut session),
            &live_progress,
        );

        // Assert
        assert_eq!(
            active_progress.as_deref(),
            Some("Phase: Running\n- protocol [protocol]: running")
        );
        assert!(session.orchestration_progress.is_none());
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

    #[test]
    fn mode_session_id_uses_view_info_popup_restore_view() {
        // Arrange
        let mode = AppMode::ViewInfoPopup {
            is_loading: false,
            loading_label: "Refreshing review request...".to_string(),
            message: "Review request refreshed.".to_string(),
            restore_view: ConfirmationViewMode {
                scroll_offset: Some(2),
                session_id: "popup-session".into(),
            },
            title: "Review request refreshed".to_string(),
        };

        // Act
        let session_id = SessionManager::mode_session_id(&mode);

        // Assert
        assert_eq!(session_id.map(SessionId::as_str), Some("popup-session"));
    }

    #[test]
    fn mode_session_id_uses_view_help_context() {
        // Arrange
        let mode = AppMode::Help {
            context: HelpContext::View {
                can_fork_session: false,
                can_merge_session_branch: false,
                can_mutate_session_branch: false,
                can_open_worktree: false,
                can_rebase_session_branch: false,
                can_show_diff: true,
                can_reply_to_session: false,
                can_start_staged_session: false,
                can_view_review_comments: false,
                publish_pull_request_action: None,
                scroll_offset: Some(2),
                session_id: "help-session".into(),
                session_state: ViewSessionState::Review,
            },
            scroll_offset: 0,
        };

        // Act
        let session_id = SessionManager::mode_session_id(&mode);

        // Assert
        assert_eq!(session_id.map(SessionId::as_str), Some("help-session"));
    }

    #[test]
    fn mode_session_id_uses_diff_and_session_overlay_contexts() {
        // Arrange
        let modes = [
            AppMode::DiffLoading {
                fallback_view_scroll_offset: None,
                request_id: 1,
                restore: None,
                session_id: "loading-session".into(),
                sidebar_focus: DiffSidebarFocus::Files,
            },
            AppMode::Diff {
                diff: String::new(),
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                line_comments: DiffLineComments::default(),
                selected_diff_line_index: 0,
                preview: DiffPreview::default(),
                review_comments: None,
                restore: None,
                scroll_cache: None,
                scroll_offset: 0,
                session_id: "diff-session".into(),
            },
            AppMode::LaunchConfigurationSelector {
                commands: vec!["cargo test".to_string()],
                restore_view: ConfirmationViewMode {
                    scroll_offset: None,
                    session_id: "launch-session".into(),
                },
                selected_command_index: 0,
            },
            AppMode::PublishBranchInput {
                default_branch_name: "wt/session".to_string(),
                input: InputState::default(),
                locked_upstream_ref: None,
                publish_branch_action: PublishBranchAction::Push,
                restore_view: ConfirmationViewMode {
                    scroll_offset: None,
                    session_id: "publish-session".into(),
                },
            },
        ];

        // Act
        let session_ids = modes
            .iter()
            .map(SessionManager::mode_session_id)
            .map(|session_id| session_id.map(SessionId::as_str))
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            session_ids,
            [
                Some("loading-session"),
                Some("diff-session"),
                Some("launch-session"),
                Some("publish-session"),
            ]
        );
    }

    #[test]
    fn test_ensure_mode_session_exists_closes_missing_diff_comments() {
        // Arrange
        let now = Instant::now();
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let session_manager = session_manager_fixture(clock);
        let mut mode = AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(crate::presentation::app_mode::DiffReviewComments::loading(
                1,
            )),
            restore: None,
            scroll_cache: None,
            session_id: "missing-session".into(),
            scroll_offset: 0,
        };

        // Act
        session_manager.ensure_mode_session_exists(&mut mode);

        // Assert
        assert!(matches!(mode, AppMode::List));
    }

    /// Builds a session manager with deterministic time and empty state.
    fn session_manager_fixture(clock: Arc<dyn Clock>) -> SessionManager {
        let mut git_client = git::MockGitClient::new();
        git_client
            .expect_detect_git_info()
            .times(0..)
            .returning(|_| Box::pin(async { None }));
        let git_client: Arc<dyn git::GitClient> = Arc::new(git_client);

        SessionManager::new(
            SessionDefaults {
                model: AgentKind::Antigravity.default_model(),
            },
            git_client,
            SessionState::new(
                HashMap::new(),
                Vec::new(),
                SelectionState::default(),
                clock,
                0,
                0,
            ),
            Vec::new(),
        )
    }

    /// Builds an empty project manager rooted at a temporary working directory.
    fn empty_project_manager(working_dir: PathBuf) -> ProjectManager {
        ProjectManager::new(
            1,
            "project".to_string(),
            Some("main".to_string()),
            None,
            Vec::new(),
            working_dir,
        )
    }

    #[tokio::test]
    async fn refresh_sessions_if_needed_skips_db_call_before_deadline_and_preserves_deadline() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn Clock> = fake_clock;
        let mut session_manager = session_manager_fixture(clock);
        let original_deadline = session_manager.state.refresh_deadline;
        let services = test_services(
            &database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let temp_dir = tempdir().expect("temp dir should be created");
        let projects = empty_project_manager(temp_dir.path().to_path_buf());
        let mut mode = AppMode::List;

        // Act
        session_manager
            .refresh_sessions_if_needed(&mut mode, &projects, &services)
            .await;

        // Assert
        assert_eq!(session_manager.state.refresh_deadline, original_deadline);
        assert_eq!(session_manager.state.row_count, 0);
        assert_eq!(session_manager.state.updated_at_max, 0);
    }

    #[tokio::test]
    async fn refresh_sessions_if_needed_advances_deadline_and_skips_reload_when_metadata_unchanged()
    {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn Clock> = fake_clock.clone();
        let mut session_manager = session_manager_fixture(clock);
        let services = test_services(
            &database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let temp_dir = tempdir().expect("temp dir should be created");
        let projects = empty_project_manager(temp_dir.path().to_path_buf());
        let mut mode = AppMode::List;
        let original_deadline = session_manager.state.refresh_deadline;
        fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

        // Act
        session_manager
            .refresh_sessions_if_needed(&mut mode, &projects, &services)
            .await;

        tokio::time::timeout(Duration::from_secs(2), async {
            while !session_manager
                .pending_refresh
                .as_ref()
                .expect("pending refresh")
                .task
                .is_finished()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("metadata check completes");
        assert!(
            !session_manager
                .refresh_sessions_if_needed(&mut mode, &projects, &services)
                .await
        );
        session_manager.request_session_refresh(&mode, &projects, &services);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !session_manager
                .pending_refresh
                .as_ref()
                .expect("pending refresh")
                .task
                .is_finished()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("forced snapshot completes");
        let other_projects = ProjectManager::new(
            2,
            "other".into(),
            None,
            None,
            Vec::new(),
            temp_dir.path().to_path_buf(),
        );
        assert!(
            !session_manager
                .refresh_sessions_if_needed(&mut mode, &other_projects, &services)
                .await
        );

        // Assert
        assert!(session_manager.state.refresh_deadline > original_deadline);
        assert_eq!(session_manager.state.row_count, 0);
        assert_eq!(session_manager.state.updated_at_max, 0);
    }

    #[tokio::test]
    async fn refresh_sessions_if_needed_reloads_sessions_when_metadata_row_count_changed() {
        // Arrange
        let session = test_session(PathBuf::from("/tmp/session"), None, Status::Done);
        let database = database_with_session(&session).await;
        for index in 0..9 {
            database
                .sessions()
                .insert_session(
                    &format!("extra-{index}"),
                    session.agent.model().as_str(),
                    "main",
                    "Done",
                    1,
                )
                .await
                .expect("extra session");
        }
        let now = Instant::now();
        let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
        let clock: Arc<dyn Clock> = fake_clock.clone();
        let mut session_manager = session_manager_fixture(clock);
        let services = test_services(
            &database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let temp_dir = tempdir().expect("temp dir should be created");
        let projects = empty_project_manager(temp_dir.path().to_path_buf());
        let mut mode = AppMode::List;
        fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

        // Act
        session_manager
            .refresh_sessions_if_needed(&mut mode, &projects, &services)
            .await;

        assert!(session_manager.state.sessions.is_empty());
        session_manager.request_session_refresh(&mode, &projects, &services);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session_manager
                .refresh_sessions_if_needed(&mut mode, &projects, &services)
                .await
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background snapshot must complete");

        // Assert
        assert_eq!(session_manager.state.row_count, 10);
        assert_eq!(session_manager.state.sessions.len(), 10);
        assert!(
            session_manager
                .state
                .sessions
                .iter()
                .any(|session| session.id == "session-id")
        );
    }

    #[tokio::test]
    async fn background_refresh_preserves_live_worker_updates() {
        // Arrange
        let session = test_session(PathBuf::from("/tmp/session"), None, Status::Review);
        let database = database_with_session(&session).await;
        let clock: Arc<dyn Clock> =
            Arc::new(FakeClock::new(Instant::now(), SystemTime::UNIX_EPOCH));
        let mut manager = session_manager_with_session(clock, session);
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let mut git_client = git::MockGitClient::new();
        git_client.expect_detect_git_info().once().returning({
            let entered = Arc::clone(&entered);
            let resume = Arc::clone(&resume);
            move |_| {
                let entered = Arc::clone(&entered);
                let resume = Arc::clone(&resume);
                Box::pin(async move {
                    entered.notify_one();
                    resume.notified().await;
                    Some("wt/live".to_string())
                })
            }
        });
        manager.git_client = Arc::new(git_client);
        let mut fs_client = create_passthrough_mock_fs_client();
        fs_client.checkpoint();
        fs_client.expect_is_dir().return_const(true);
        fs_client.expect_read_file().never();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let services = AppServices::new_with_agent_clis(
            PathBuf::from("/tmp/agentty-tests"),
            Arc::new(crate::infra::clock::RealClock),
            event_tx,
            crate::app::service::AppServiceDeps {
                app_server_client_override: Some(crate::test_support::mock_app_server()),
                available_agent_kinds: AgentKind::ALL.to_vec(),
                clipboard_image_client_override: None,
                fs_client: Arc::new(fs_client),
                git_client: manager.git_client.clone(),
                one_shot_client_override: None,
                personality_catalog_client_override: None,
                repositories: database.clone(),
                review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            },
            crate::domain::agent::AgentCliInfo::from_kinds(AgentKind::ALL),
        );
        let temp_dir = tempdir().expect("temporary directory");
        let projects = empty_project_manager(temp_dir.path().to_path_buf());
        let mut mode = AppMode::List;

        manager.state.table_state.select(Some(0));

        // Act
        manager.request_session_refresh(&mode, &projects, &services);
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("background probe starts");
        let handles = manager
            .state
            .handles()
            .get("session-id")
            .expect("live handles");
        *handles.status.lock().expect("status") = Status::InProgress;
        let live_transcript = crate::test_support::assistant_transcript("New streamed answer");
        *handles.transcript.lock().expect("transcript") = live_transcript.clone();
        mode = AppMode::View {
            session_id: "session-id".into(),
            scroll_offset: None,
        };
        resume.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !manager
                .refresh_sessions_if_needed(&mut mode, &projects, &services)
                .await
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("snapshot applied");

        // Assert
        assert_eq!(manager.state.sessions[0].status, Status::InProgress);
        assert_eq!(
            manager.state.sessions[0].transcript.as_ref(),
            Some(&live_transcript)
        );
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
