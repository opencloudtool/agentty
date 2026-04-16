//! App-layer composition root and shared state container.
//!
//! This module wires app submodules and exposes [`App`] used by runtime mode
//! handlers.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use ag_forge as forge;
use ag_forge::{RealReviewRequestClient, ReviewRequestClient};
use app::branch_publish::{
    BranchPublishActionUpdate, BranchPublishTaskResult, BranchPublishTaskSession,
    BranchPublishTaskSuccess, branch_publish_loading_label as branch_publish_loading_label_text,
    branch_publish_loading_message as branch_publish_loading_message_text,
    branch_publish_loading_title as branch_publish_loading_title_text,
    branch_publish_success_title as branch_publish_success_title_text,
    branch_push_success_message as branch_push_success_message_text,
    detected_forge_kind_from_git_push_error, git_push_authentication_message,
    is_git_push_authentication_error,
    pull_request_publish_success_message as pull_request_publish_success_message_text,
    run_branch_publish_action,
};
#[cfg(test)]
use app::branch_publish::{BranchPublishTaskFailure, branch_push_failure, push_session_branch};
use app::merge_queue::{MergeQueue, MergeQueueProgress};
use app::project::ProjectManager;
use app::reducer::AppEventReducer;
use app::review::{
    ReviewCacheEntry, ReviewUpdate, apply_review_updates, auto_start_reviews,
    mark_session_agent_review, review_view_state, start_review_assist as spawn_review_assist,
};
use app::service::{AppServiceDeps, AppServices};
use app::session::SessionManager;
use app::session_state::SessionGitStatus;
use app::setting::SettingsManager;
use app::startup::{AppStartup, StartupProjectContext, StartupSessionLoadContext};
use app::tab::TabManager;
use app::task;
use ratatui::Frame;
use session::{SessionTaskService, SyncMainOutcome, SyncSessionStartError, TurnAppliedState};
use tokio::sync::mpsc;

use super::AppError;
use crate::app::session;
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::input::InputState;
use crate::domain::permission::PermissionMode;
use crate::domain::project::{Project, ProjectListItem};
use crate::domain::session::{
    FollowUpTaskAction, PublishBranchAction, PublishedBranchSyncStatus, Session, SessionSize,
    Status,
};
use crate::infra::channel::TurnPrompt;
use crate::infra::db::Database;
use crate::infra::file_index::FileEntry;
use crate::infra::fs::{FsClient, RealFsClient};
use crate::infra::git::{GitClient, RealGitClient};
use crate::infra::tmux::{RealTmuxClient, TmuxClient};
use crate::infra::{agent, app_server, db};
use crate::runtime::mode::{question, sync_blocked};
use crate::ui::state::app_mode::{
    AppMode, ConfirmationViewMode, DoneSessionOutputMode, QuestionFocus,
};
use crate::ui::state::prompt::PromptAtMentionState;
use crate::{app, ui};

/// Relative directory name used for session git worktrees within the
/// `agentty` home directory.
pub const AGENTTY_WT_DIR: &str = "wt";
/// Repository-relative roadmap path that enables the project-specific
/// `Tasks` tab when present.
pub(crate) const TASKS_ROADMAP_PATH: &str = "docs/plan/roadmap.md";

/// Cached roadmap snapshot for the active project `Tasks` tab.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ActiveProjectRoadmap {
    /// Successfully loaded roadmap markdown from `docs/plan/roadmap.md`.
    Loaded(String),
    /// Roadmap file exists but could not be read.
    LoadError(String),
}

/// Returns the resolved `agentty` home directory.
///
/// The `AGENTTY_ROOT` environment variable takes precedence when set to a
/// non-empty path. Otherwise the resolver falls back to `~/.agentty`, then to
/// a relative `.agentty` directory when no home directory is available.
pub fn agentty_home() -> PathBuf {
    let agentty_root = env::var_os("AGENTTY_ROOT").map(PathBuf::from);
    let home_dir = dirs::home_dir();

    resolve_agentty_home(agentty_root, home_dir)
}

/// Resolves the agentty home directory from optional root and home paths.
///
/// When `agentty_root` is present and non-empty, it takes precedence. When no
/// override is available, the resolver falls back to `home_dir/.agentty`, then
/// finally to a relative `.agentty` directory.
fn resolve_agentty_home(agentty_root: Option<PathBuf>, home_dir: Option<PathBuf>) -> PathBuf {
    agentty_root
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home_dir.map(|path| path.join(".agentty")))
        .unwrap_or_else(|| PathBuf::from(".agentty"))
}

/// Background auto-update progress state for the status bar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    /// Background `npm i -g agentty@latest` is running.
    InProgress { version: String },
    /// Update installed successfully; restart to use the new version.
    Complete { version: String },
    /// Update failed; fall back to manual update hint.
    Failed { version: String },
}

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Indicates background-loaded prompt at-mention entries for one session.
    AtMentionEntriesLoaded {
        entries: Vec<FileEntry>,
        session_id: String,
    },
    /// Indicates the latest project-branch and session-branch ahead/behind
    /// information from the git status worker.
    GitStatusUpdated {
        session_statuses: HashMap<String, SessionGitStatus>,
        status: Option<(u32, u32)>,
    },
    /// Indicates whether a newer stable `agentty` release is available.
    VersionAvailabilityUpdated {
        latest_available_version: Option<String>,
    },
    /// Indicates progress of the background auto-update.
    UpdateStatusChanged { update_status: UpdateStatus },
    /// Indicates a session model selection has been persisted.
    SessionModelUpdated {
        session_id: String,
        session_model: AgentModel,
    },
    /// Indicates a session reasoning override selection has been persisted.
    SessionReasoningLevelUpdated {
        reasoning_level_override: Option<ReasoningLevel>,
        session_id: String,
    },
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Requests an immediate git-status refresh outside the periodic poll
    /// cadence.
    RefreshGitStatus,
    /// Indicates compact live thinking text for an in-progress session.
    SessionProgressUpdated {
        progress_message: Option<String>,
        session_id: String,
    },
    /// Indicates completion of a list-mode sync workflow.
    SyncMainCompleted {
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    },
    /// Indicates recomputed diff-derived size and line-count totals for one
    /// session.
    SessionSizeUpdated {
        added_lines: u64,
        deleted_lines: u64,
        session_id: String,
        session_size: SessionSize,
    },
    /// Indicates one tracked draft-title generation task reached a terminal
    /// outcome and can be pruned from in-memory task tracking.
    SessionTitleGenerationFinished { generation: u64, session_id: String },
    /// Indicates completion of a session-view branch-publish action.
    BranchPublishActionCompleted {
        restore_view: ConfirmationViewMode,
        result: Box<BranchPublishTaskResult>,
        session_id: String,
    },
    /// Indicates review assist output became available for a session.
    ReviewPrepared {
        diff_hash: u64,
        review_text: String,
        session_id: String,
    },
    /// Indicates review assist failed for a session.
    ReviewPreparationFailed {
        diff_hash: u64,
        error: String,
        session_id: String,
    },
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: String },
    /// Indicates that an agent turn completed and persisted one reducer-ready
    /// projection.
    AgentResponseReceived {
        session_id: String,
        turn_applied_state: TurnAppliedState,
    },
    /// Indicates that one published session branch started or finished a
    /// background auto-push after a completed turn.
    PublishedBranchSyncUpdated {
        session_id: String,
        sync_operation_id: String,
        sync_status: PublishedBranchSyncStatus,
    },
    /// Indicates completion of a session-view review request sync action.
    SyncReviewRequestCompleted {
        restore_view: ConfirmationViewMode,
        result: Result<SyncReviewRequestTaskResult, String>,
        session_id: String,
    },
}

/// Reduced representation of all app events currently queued for one tick.
#[derive(Default)]
struct AppEventBatch {
    applied_turns: HashMap<String, TurnAppliedState>,
    at_mention_entries_updates: HashMap<String, Vec<FileEntry>>,
    published_branch_sync_updates: Vec<(String, PublishedBranchSyncUpdate)>,
    review_updates: HashMap<String, ReviewUpdate>,
    git_status_update: Option<(u32, u32)>,
    has_git_status_update: bool,
    has_latest_available_version_update: bool,
    latest_available_version_update: Option<String>,
    branch_publish_action_update: Option<BranchPublishActionUpdate>,
    session_git_status_updates: HashMap<String, SessionGitStatus>,
    session_ids: HashSet<String>,
    session_model_updates: HashMap<String, AgentModel>,
    session_reasoning_level_updates: HashMap<String, Option<ReasoningLevel>>,
    session_progress_updates: HashMap<String, Option<String>>,
    session_size_updates: HashMap<String, (u64, u64, SessionSize)>,
    session_title_generation_finished: HashMap<String, u64>,
    should_refresh_git_status: bool,
    should_force_reload: bool,
    sync_review_request_update: Option<SyncReviewRequestUpdate>,
    sync_main_result: Option<Result<SyncMainOutcome, SyncSessionStartError>>,
    update_status: Option<UpdateStatus>,
}

/// One ordered published-branch sync update queued for one session.
struct PublishedBranchSyncUpdate {
    /// Operation identifier used to ignore stale terminal auto-push updates.
    sync_operation_id: String,
    /// Auto-push state carried by this update.
    sync_status: PublishedBranchSyncStatus,
}

impl AppEventBatch {
    /// Collects one app event into the coalesced batch state.
    ///
    /// Most per-session projections use latest-wins semantics, but queued
    /// `AgentResponseReceived` events merge token-usage deltas so one reducer
    /// tick preserves cumulative usage from multiple completed turns.
    fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                self.at_mention_entries_updates.insert(session_id, entries);
            }
            AppEvent::GitStatusUpdated {
                session_statuses,
                status,
            } => {
                self.has_git_status_update = true;
                self.git_status_update = status;
                self.session_git_status_updates = session_statuses;
            }
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            } => {
                self.has_latest_available_version_update = true;
                self.latest_available_version_update = latest_available_version;
            }
            AppEvent::UpdateStatusChanged { update_status } => {
                self.update_status = Some(update_status);
            }
            AppEvent::SessionModelUpdated {
                session_id,
                session_model,
            } => {
                self.session_model_updates.insert(session_id, session_model);
            }
            AppEvent::SessionReasoningLevelUpdated {
                reasoning_level_override,
                session_id,
            } => {
                self.session_reasoning_level_updates
                    .insert(session_id, reasoning_level_override);
            }
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::RefreshGitStatus => {
                self.should_refresh_git_status = true;
            }
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => {
                self.session_progress_updates
                    .insert(session_id, progress_message);
            }
            AppEvent::SyncMainCompleted { result } => {
                if result.is_ok() {
                    self.should_refresh_git_status = true;
                }
                self.sync_main_result = Some(result);
            }
            AppEvent::SessionSizeUpdated {
                added_lines,
                deleted_lines,
                session_id,
                session_size,
            } => {
                self.session_size_updates
                    .insert(session_id, (added_lines, deleted_lines, session_size));
            }
            AppEvent::SessionTitleGenerationFinished {
                generation,
                session_id,
            } => {
                self.session_title_generation_finished
                    .insert(session_id, generation);
            }
            AppEvent::BranchPublishActionCompleted {
                restore_view,
                result,
                session_id,
            } => {
                if result.is_ok() {
                    self.should_refresh_git_status = true;
                }
                self.branch_publish_action_update = Some(BranchPublishActionUpdate {
                    restore_view,
                    result: *result,
                    session_id,
                });
            }
            AppEvent::ReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            } => {
                self.review_updates.insert(
                    session_id,
                    ReviewUpdate {
                        diff_hash,
                        result: Ok(review_text),
                    },
                );
            }
            AppEvent::ReviewPreparationFailed {
                diff_hash,
                error,
                session_id,
            } => {
                self.review_updates.insert(
                    session_id,
                    ReviewUpdate {
                        diff_hash,
                        result: Err(error),
                    },
                );
            }
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
            AppEvent::AgentResponseReceived {
                session_id,
                turn_applied_state,
            } => self.collect_agent_response_received(session_id, turn_applied_state),
            AppEvent::PublishedBranchSyncUpdated {
                session_id,
                sync_operation_id,
                sync_status,
            } => {
                if matches!(
                    sync_status,
                    PublishedBranchSyncStatus::Idle | PublishedBranchSyncStatus::Succeeded
                ) {
                    self.should_refresh_git_status = true;
                }
                self.published_branch_sync_updates.push((
                    session_id,
                    PublishedBranchSyncUpdate {
                        sync_operation_id,
                        sync_status,
                    },
                ));
            }
            AppEvent::SyncReviewRequestCompleted {
                restore_view,
                result,
                session_id,
            } => {
                self.sync_review_request_update = Some(SyncReviewRequestUpdate {
                    restore_view,
                    result,
                    session_id,
                });
            }
        }
    }

    /// Merges one completed-turn projection into the per-session batch.
    ///
    /// Agent responses also mark the session as touched so the reducer still
    /// synchronizes handle-backed status and evaluates auto-review startup
    /// even when the matching `SessionUpdated` event lands in a later tick.
    /// Latest reducer-facing fields replace the older projection, while token
    /// deltas accumulate to preserve usage across multiple queued completions
    /// for the same session.
    fn collect_agent_response_received(
        &mut self,
        session_id: String,
        turn_applied_state: TurnAppliedState,
    ) {
        self.session_ids.insert(session_id.clone());

        match self.applied_turns.entry(session_id) {
            Entry::Occupied(mut occupied_entry) => {
                occupied_entry.get_mut().merge_newer(turn_applied_state);
            }
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(turn_applied_state);
            }
        }
    }
}

/// Immutable context displayed in sync-main popup content.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SyncPopupContext {
    default_branch: String,
    project_name: String,
}

/// Background sync task result carrying the normalized summary for
/// persistence alongside the UI outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncReviewRequestTaskResult {
    pub(crate) outcome: session::SyncReviewRequestOutcome,
    /// Normalized summary to persist when a review request was found or
    /// refreshed.
    pub(crate) summary: Option<crate::domain::session::ReviewRequestSummary>,
}

/// Completed review request sync payload ready for reducer application.
struct SyncReviewRequestUpdate {
    restore_view: ConfirmationViewMode,
    result: Result<SyncReviewRequestTaskResult, String>,
    session_id: String,
}

/// Token-usage totals for one model used by the `/stats` prompt command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionStatsUsage {
    pub input_tokens: u64,
    pub model: String,
    pub output_tokens: u64,
}

/// Session statistics payload returned by [`App::stats_for_session`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionStatsSnapshot {
    pub session_duration_seconds: Option<i64>,
    pub usage_rows_result: Result<Vec<SessionStatsUsage>, String>,
}

/// Starts project sync work and emits completion events for list-mode popups.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait SyncMainRunner: Send + Sync {
    /// Starts sync for one project and emits one
    /// [`AppEvent::SyncMainCompleted`] when work finishes.
    fn start_sync_main(
        &self,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        default_branch: Option<String>,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
        working_dir: PathBuf,
    );
}

/// Production [`SyncMainRunner`] that executes sync in one spawned task.
pub(crate) struct TokioSyncMainRunner;

impl SyncMainRunner for TokioSyncMainRunner {
    fn start_sync_main(
        &self,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        default_branch: Option<String>,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
        working_dir: PathBuf,
    ) {
        tokio::spawn(async move {
            let result = SessionManager::sync_main_for_project(
                default_branch,
                working_dir,
                git_client,
                session_model,
            )
            .await;
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(AppEvent::SyncMainCompleted { result });
        });
    }
}

/// External clients used to compose [`App`] startup dependencies.
pub(crate) struct AppClients {
    agent_availability_probe: Arc<dyn agent::AgentAvailabilityProbe>,
    app_server_client_override: Option<Arc<dyn app_server::AppServerClient>>,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn ReviewRequestClient>,
    sync_main_runner: Arc<dyn SyncMainRunner>,
    tmux_client: Arc<dyn TmuxClient>,
}

impl AppClients {
    /// Builds one client bundle with real implementations for each external
    /// boundary.
    pub(crate) fn new() -> Self {
        Self {
            agent_availability_probe: Arc::new(agent::RealAgentAvailabilityProbe),
            app_server_client_override: None,
            fs_client: Arc::new(RealFsClient),
            git_client: Arc::new(RealGitClient),
            review_request_client: Arc::new(RealReviewRequestClient::default()),
            sync_main_runner: Arc::new(TokioSyncMainRunner),
            tmux_client: Arc::new(RealTmuxClient),
        }
    }

    /// Replaces the startup agent-availability boundary while preserving the
    /// remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_agent_availability_probe(
        mut self,
        agent_availability_probe: Arc<dyn agent::AgentAvailabilityProbe>,
    ) -> Self {
        self.agent_availability_probe = agent_availability_probe;

        self
    }

    /// Replaces the default provider-owned app-server clients with one shared
    /// override.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_app_server_client_override(
        mut self,
        app_server_client_override: Arc<dyn app_server::AppServerClient>,
    ) -> Self {
        self.app_server_client_override = Some(app_server_client_override);

        self
    }

    /// Replaces the tmux boundary while preserving the remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_tmux_client(mut self, tmux_client: Arc<dyn TmuxClient>) -> Self {
        self.tmux_client = tmux_client;

        self
    }

    /// Replaces the filesystem boundary while preserving the remaining
    /// clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_fs_client(mut self, fs_client: Arc<dyn FsClient>) -> Self {
        self.fs_client = fs_client;

        self
    }
}

// SessionState definition moved to session_state.rs

/// Stores application state and coordinates session/project workflows.
pub struct App {
    /// Tracks the currently active UI mode and its transient state.
    pub mode: AppMode,
    /// Stores persisted and in-memory application settings for the active
    /// project.
    pub settings: SettingsManager,
    /// Manages the selected top-level list tab.
    pub tabs: TabManager,
    /// Caches generated focused review text per session so it survives
    /// mode switches and is ready when the user presses `f`.
    pub(crate) review_cache: HashMap<String, ReviewCacheEntry>,
    /// Owns project selection state, project metadata, and git status
    /// snapshots.
    pub(crate) projects: ProjectManager,
    /// Shares application-wide services and external clients across workflows.
    pub(crate) services: AppServices,
    /// Owns session state, runtime handles, and session workflow coordination.
    pub(crate) sessions: SessionManager,
    /// Runs sync-to-main workflows behind an injectable boundary.
    pub(crate) sync_main_runner: Arc<dyn SyncMainRunner>,
    /// Caches whether the active project exposes `docs/plan/roadmap.md`,
    /// avoiding a filesystem probe on every render tick.
    active_project_has_tasks_tab: bool,
    /// Caches the active project's roadmap content or load failure for the
    /// `Tasks` page.
    active_project_roadmap: Option<ActiveProjectRoadmap>,
    /// Stores the current vertical scroll offset for the active project's
    /// `Tasks` page.
    task_roadmap_scroll_offset: u16,
    /// Receives app events emitted by background tasks and workflows.
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Stores the latest available stable `agentty` version when one is
    /// detected.
    latest_available_version: Option<String>,
    /// Serializes local merge requests so only one merge workflow runs at a
    /// time.
    merge_queue: MergeQueue,
    /// Tracks per-session thinking text rendered while background work is
    /// active.
    session_progress_messages: HashMap<String, String>,
    /// Interacts with tmux panes for session-specific terminal workflows.
    tmux_client: Arc<dyn TmuxClient>,
    /// Stores the current auto-update progress state when an update is running.
    update_status: Option<UpdateStatus>,
}

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
        let current_project_id =
            AppStartup::persist_startup_project(&db, working_dir.as_path(), git_branch.as_deref())
                .await?;
        let StartupProjectContext {
            active_project_id,
            active_project_name,
            project_items,
            startup_git_branch,
            startup_git_upstream_ref,
            startup_working_dir,
        } = AppStartup::load_startup_project_context(
            &db,
            clients.fs_client.as_ref(),
            &clients.git_client,
            working_dir.as_path(),
            git_branch,
            current_project_id,
        )
        .await?;

        let clock: Arc<dyn session::Clock> = Arc::new(session::RealClock);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let available_agent_kinds = task::TaskService::load_agent_availability(Arc::clone(
            &clients.agent_availability_probe,
        ))
        .await;
        AppStartup::validate_startup_agent_availability(&available_agent_kinds)?;
        let services = AppServices::new(
            base_path.clone(),
            Arc::clone(&clock),
            db.clone(),
            event_tx.clone(),
            AppServiceDeps {
                app_server_client_override: clients
                    .app_server_client_override
                    .as_ref()
                    .map(Arc::clone),
                available_agent_kinds,
                fs_client: Arc::clone(&clients.fs_client),
                git_client: Arc::clone(&clients.git_client),
                review_request_client: Arc::clone(&clients.review_request_client),
            },
        );
        SessionManager::fail_unfinished_operations_from_previous_run(
            db.clone(),
            Arc::clone(&clock),
        )
        .await;
        let projects = ProjectManager::new(
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
            mode: AppMode::List,
            settings,
            tabs: TabManager::new(),
            projects,
            services,
            sessions,
            active_project_has_tasks_tab,
            active_project_roadmap,
            task_roadmap_scroll_offset: 0,
            event_rx,
            review_cache: HashMap::new(),
            latest_available_version: None,
            merge_queue: MergeQueue::default(),
            session_progress_messages: HashMap::new(),
            update_status: None,
            sync_main_runner: clients.sync_main_runner,
            tmux_client: clients.tmux_client,
        })
    }

    /// Returns the active project identifier.
    pub fn active_project_id(&self) -> i64 {
        self.projects.active_project_id()
    }

    /// Returns the working directory for the active project.
    pub fn working_dir(&self) -> &Path {
        self.projects.working_dir()
    }

    /// Returns the git branch of the active project, when available.
    pub fn git_branch(&self) -> Option<&str> {
        self.projects.git_branch()
    }

    /// Returns the upstream reference tracked by the active project branch,
    /// when available.
    pub fn git_upstream_ref(&self) -> Option<&str> {
        self.projects.git_upstream_ref()
    }

    /// Returns the latest ahead/behind snapshot from reducer-applied events.
    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.projects.git_status()
    }

    /// Builds prompt slash-menu state from the cached machine-scoped agent
    /// availability snapshot.
    pub(crate) fn prompt_slash_state(&self) -> crate::ui::state::prompt::PromptSlashState {
        crate::ui::state::prompt::PromptSlashState::with_available_agent_kinds(
            self.services.available_agent_kinds(),
        )
    }

    /// Returns the newer stable `agentty` version when an update is available.
    pub fn latest_available_version(&self) -> Option<&str> {
        self.latest_available_version.as_deref()
    }

    /// Returns the current background auto-update status, if any.
    pub fn update_status(&self) -> Option<&UpdateStatus> {
        self.update_status.as_ref()
    }

    /// Renders a complete UI frame by assembling a [`ui::RenderContext`] from
    /// current app state and dispatching to the UI render pipeline.
    pub fn draw(&mut self, frame: &mut Frame) {
        let has_tasks_tab = self.active_project_has_tasks_tab();
        self.tabs.normalize(has_tasks_tab);
        let active_project_id = self.projects.active_project_id();
        let current_tab = self.tabs.current();
        let working_dir = self.projects.working_dir().to_path_buf();
        let git_branch = self.projects.git_branch().map(str::to_string);
        let git_upstream_ref = self.projects.git_upstream_ref().map(str::to_string);
        let git_status = self.projects.git_status();
        let latest_available_version = self.latest_available_version.as_deref().map(str::to_string);
        let task_roadmap = self.active_project_roadmap.as_ref().and_then(|roadmap| {
            if let ActiveProjectRoadmap::Loaded(content) = roadmap {
                return Some(content.clone());
            }

            None
        });
        let task_roadmap_error = self.active_project_roadmap.as_ref().and_then(|roadmap| {
            if let ActiveProjectRoadmap::LoadError(message) = roadmap {
                return Some(message.clone());
            }

            None
        });
        let session_git_statuses = self.sessions.session_git_statuses().clone();
        let session_worktree_availability = self.sessions.session_worktree_availability().clone();
        let follow_up_task_positions = self.sessions.state().follow_up_task_positions.clone();
        let active_prompt_outputs = self.sessions.active_prompt_outputs().clone();
        let session_progress_messages = self.session_progress_messages.clone();
        let update_status = self.update_status().cloned();
        let wall_clock_unix_seconds =
            session::unix_timestamp_from_system_time(self.sessions.state().clock.now_system_time());
        let projects = self.projects.project_items().to_vec();
        let mode = &self.mode;
        let project_table_state = self.projects.project_table_state_mut();
        let (sessions, stats_activity, table_state) = self.sessions.render_parts();
        let settings = &mut self.settings;

        ui::render(
            frame,
            ui::RenderContext {
                active_project_id,
                current_tab,
                has_tasks_tab,
                git_branch: git_branch.as_deref(),
                git_upstream_ref: git_upstream_ref.as_deref(),
                git_status,
                latest_available_version: latest_available_version.as_deref(),
                update_status: update_status.as_ref(),
                mode,
                project_table_state,
                projects: &projects,
                task_roadmap: task_roadmap.as_deref(),
                task_roadmap_error: task_roadmap_error.as_deref(),
                task_roadmap_scroll_offset: self.task_roadmap_scroll_offset,
                active_prompt_outputs: &active_prompt_outputs,
                follow_up_task_positions: &follow_up_task_positions,
                session_git_statuses: &session_git_statuses,
                session_progress_messages: &session_progress_messages,
                session_worktree_availability: &session_worktree_availability,
                settings,
                stats_activity,
                sessions,
                table_state,
                working_dir: &working_dir,
                wall_clock_unix_seconds,
            },
        );
    }

    /// Returns whether the active project exposes the roadmap file required by
    /// the `Tasks` tab.
    pub fn active_project_has_tasks_tab(&self) -> bool {
        self.active_project_has_tasks_tab
    }

    /// Cycles the active list tab forward using the active project's available
    /// tab set.
    pub fn next_tab(&mut self) {
        self.tabs.next(self.active_project_has_tasks_tab());
    }

    /// Cycles the active list tab backward using the active project's
    /// available tab set.
    pub fn previous_tab(&mut self) {
        self.tabs.previous(self.active_project_has_tasks_tab());
    }

    /// Returns the current `Tasks`-tab vertical scroll offset.
    pub fn task_roadmap_scroll_offset(&self) -> u16 {
        self.task_roadmap_scroll_offset
    }

    /// Scrolls the active project's roadmap view down by one wrapped line.
    pub fn scroll_task_roadmap_down(&mut self) {
        self.task_roadmap_scroll_offset = self.task_roadmap_scroll_offset.saturating_add(1);
    }

    /// Scrolls the active project's roadmap view up by one wrapped line.
    pub fn scroll_task_roadmap_up(&mut self) {
        self.task_roadmap_scroll_offset = self.task_roadmap_scroll_offset.saturating_sub(1);
    }

    /// Resets the active project's roadmap view back to the top.
    pub fn reset_task_roadmap_scroll(&mut self) {
        self.task_roadmap_scroll_offset = 0;
    }

    /// Refreshes cached roadmap availability and content for the active
    /// project.
    async fn refresh_active_project_roadmap(&mut self) {
        self.active_project_roadmap = Self::load_project_roadmap(
            self.services.fs_client().as_ref(),
            self.projects.working_dir(),
        )
        .await;
        self.active_project_has_tasks_tab = self.active_project_roadmap.is_some();
        self.task_roadmap_scroll_offset = 0;
    }

    /// Refreshes cached roadmap state and re-normalizes top-level tabs.
    async fn refresh_active_project_roadmap_and_tabs(&mut self) {
        self.refresh_active_project_roadmap().await;
        self.tabs.normalize(self.active_project_has_tasks_tab());
    }

    /// Moves selection to the next session in the list.
    pub fn next(&mut self) {
        self.sessions.next();
    }

    /// Moves selection to the previous session in the list.
    pub fn previous(&mut self) {
        self.sessions.previous();
    }

    /// Moves selection to the next project in the projects list.
    pub fn next_project(&mut self) {
        self.projects.next_project();
    }

    /// Moves selection to the previous project in the projects list.
    pub fn previous_project(&mut self) {
        self.projects.previous_project();
    }

    /// Selects the currently selected project in the projects list.
    ///
    /// # Errors
    /// Returns an error if there is no selected project or project switching
    /// fails.
    pub async fn switch_selected_project(&mut self) -> Result<(), AppError> {
        let selected_project_id = self
            .projects
            .selected_project_id()
            .ok_or_else(|| AppError::Workflow("No project selected".to_string()))?;

        self.switch_project(selected_project_id).await
    }

    /// Switches app context to one persisted project id.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or session refresh fails.
    pub async fn switch_project(&mut self, project_id: i64) -> Result<(), AppError> {
        let project = self
            .services
            .db()
            .get_project(project_id)
            .await?
            .map(Self::project_from_row)
            .ok_or_else(|| {
                AppError::Workflow(format!("Project with id `{project_id}` was not found"))
            })?;
        let git_branch = self
            .services
            .git_client()
            .detect_git_info(project.path.clone())
            .await;
        let git_upstream_ref = Self::load_git_upstream_ref(
            self.services.git_client().as_ref(),
            project.path.as_path(),
            git_branch.as_deref(),
        )
        .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .upsert_project(&project.path.to_string_lossy(), git_branch.as_deref())
            .await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self.services.db().set_active_project_id(project.id).await;
        // Best-effort: project metadata persistence is non-critical.
        let _ = self
            .services
            .db()
            .touch_project_last_opened(project.id)
            .await;

        self.projects.replace_git_status_cancel();
        self.projects.update_active_project_context(
            project.id,
            project.display_label(),
            git_branch,
            git_upstream_ref,
            project.path,
        );
        self.refresh_active_project_roadmap().await;
        self.tabs.normalize(self.active_project_has_tasks_tab());
        self.settings = SettingsManager::new(&self.services, project.id).await;
        let default_session_model = SessionManager::load_default_session_model(
            &self.services,
            Some(project.id),
            AgentKind::Gemini.default_model(),
        )
        .await;
        self.sessions
            .set_default_session_model(default_session_model);
        self.reload_projects().await;
        self.refresh_sessions_now().await;

        Ok(())
    }

    /// Creates a blank session and schedules list refresh through events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_session(&mut self) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_session(&self.projects, &self.services)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Creates a blank draft session and schedules list refresh through
    /// events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_draft_session(&mut self) -> Result<String, AppError> {
        let session_id = self
            .sessions
            .create_draft_session(&self.projects, &self.services)
            .await?;
        self.finish_session_creation(&session_id).await;

        Ok(session_id)
    }

    /// Applies the shared post-create refresh and selection flow for a new
    /// session.
    async fn finish_session_creation(&mut self, session_id: &str) {
        self.process_pending_app_events().await;
        self.reload_projects().await;

        let index = self
            .sessions
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.sessions.table_state.select(Some(index));
    }

    /// Submits the initial prompt for a newly created session.
    ///
    /// Starting a new turn clears any cached focused-review output for that
    /// session so review text does not bleed into the next prompt cycle.
    ///
    /// # Errors
    /// Returns an error if the session is missing or task enqueue fails.
    pub async fn start_session(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), AppError> {
        self.review_cache.remove(session_id);

        Ok(self
            .sessions
            .start_session(&self.services, session_id, prompt)
            .await?)
    }

    /// Persists one staged draft message for a `New` session without
    /// launching the agent.
    ///
    /// # Errors
    /// Returns an error if the session cannot accept more staged drafts.
    pub async fn stage_draft_message(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), AppError> {
        let active_project_id = self.projects.active_project_id();
        let project_working_dir = self.projects.working_dir().to_path_buf();

        Ok(self
            .sessions
            .stage_draft_message(
                &self.services,
                active_project_id,
                project_working_dir.as_path(),
                session_id,
                prompt,
            )
            .await?)
    }

    /// Starts a `New` session from its persisted staged draft bundle.
    ///
    /// # Errors
    /// Returns an error if the session is missing, has no staged drafts, or
    /// launch enqueue fails.
    pub async fn start_staged_session(&mut self, session_id: &str) -> Result<(), AppError> {
        Ok(self
            .sessions
            .start_staged_session(&self.services, session_id)
            .await?)
    }

    /// Submits a follow-up prompt for an existing session.
    ///
    /// Starting a new turn clears any cached focused-review output for that
    /// session so review text does not persist past prompt submission.
    pub async fn reply(&mut self, session_id: &str, prompt: impl Into<TurnPrompt>) {
        self.review_cache.remove(session_id);
        self.sessions
            .reply(&self.services, session_id, prompt)
            .await;
    }

    /// Returns the focused-review output state that should be shown when one
    /// session view is reopened.
    pub(crate) fn review_view_state(&self, session_id: &str) -> (Option<String>, Option<String>) {
        review_view_state(
            &self.review_cache,
            session_id,
            self.settings.default_review_model,
        )
    }

    /// Persists and applies a model selection for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_model(
        &mut self,
        session_id: &str,
        session_model: AgentModel,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_model(&self.services, session_id, session_model)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Persists and applies a reasoning override for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_reasoning_level(
        &mut self,
        session_id: &str,
        reasoning_level_override: Option<ReasoningLevel>,
    ) -> Result<(), AppError> {
        self.sessions
            .set_session_reasoning_level(&self.services, session_id, reasoning_level_override)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.selected_session()
    }

    /// Returns the session snapshot for one list index, if it still exists.
    pub fn session_at(&self, session_index: usize) -> Option<&Session> {
        self.sessions.session_at(session_index)
    }

    /// Returns session id by list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.sessions.session_id_for_index(session_index)
    }

    /// Resolves a session id to current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.sessions.session_index_for_id(session_id)
    }

    /// Returns compact live thinking text for a session, if available.
    pub fn session_progress_message(&self, session_id: &str) -> Option<&str> {
        self.session_progress_messages
            .get(session_id)
            .map(std::string::String::as_str)
    }

    /// Returns the selected follow-up task action for one session, if that
    /// session currently exposes follow-up tasks.
    pub(crate) fn selected_follow_up_task_action(
        &self,
        session_id: &str,
    ) -> Option<FollowUpTaskAction> {
        self.sessions.selected_follow_up_task_action(session_id)
    }

    /// Returns whether one session has multiple follow-up tasks to cycle
    /// through in session view.
    pub(crate) fn has_multiple_follow_up_tasks(&self, session_id: &str) -> bool {
        self.sessions.has_multiple_follow_up_tasks(session_id)
    }

    /// Moves the selected follow-up task forward within one session.
    pub(crate) fn select_next_follow_up_task(&mut self, session_id: &str) {
        self.sessions.select_next_follow_up_task(session_id);
    }

    /// Moves the selected follow-up task backward within one session.
    pub(crate) fn select_previous_follow_up_task(&mut self, session_id: &str) {
        self.sessions.select_previous_follow_up_task(session_id);
    }

    /// Launches the selected follow-up task into a sibling session or opens
    /// the already launched sibling when one is linked.
    ///
    /// # Errors
    /// Returns an error if creating or starting the sibling session fails, or
    /// if persisting the launched-task link cannot be completed.
    pub(crate) async fn launch_or_open_selected_follow_up_task(
        &mut self,
        session_id: &str,
    ) -> Result<(), AppError> {
        let Some((position, task_text, launched_session_id)) =
            self.selected_follow_up_task_snapshot(session_id)
        else {
            return Ok(());
        };

        if let Some(launched_session_id) = launched_session_id {
            if self.open_session_if_present(&launched_session_id) {
                return Ok(());
            }

            self.set_follow_up_task_launched_session_id(session_id, position, None)
                .await?;
        }

        let sibling_session_id = self.create_session().await?;
        self.start_session(&sibling_session_id, TurnPrompt::from_text(task_text))
            .await?;
        self.set_follow_up_task_launched_session_id(
            session_id,
            position,
            Some(sibling_session_id.clone()),
        )
        .await?;
        self.open_session(&sibling_session_id);

        Ok(())
    }

    /// Deletes the selected session and schedules list refresh.
    pub async fn delete_selected_session(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            self.review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Deletes the selected session while deferring worktree filesystem cleanup
    /// to a background task.
    pub async fn delete_selected_session_deferred_cleanup(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session_deferred_cleanup(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            self.review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Cancels a session in review status.
    ///
    /// # Errors
    /// Returns an error if the session is not found or not in review status.
    pub async fn cancel_session(&self, session_id: &str) -> Result<(), AppError> {
        Ok(self
            .sessions
            .cancel_session(&self.services, session_id)
            .await?)
    }

    /// Opens the selected session worktree in tmux and optionally runs the
    /// first configured open command.
    pub async fn open_session_worktree_in_tmux(&self) {
        let selected_open_command = self.configured_open_commands().into_iter().next();

        self.open_session_worktree_in_tmux_with_command(selected_open_command.as_deref())
            .await;
    }

    /// Opens the selected session worktree in tmux and optionally runs one
    /// provided open command.
    ///
    /// Sessions without a materialized worktree are treated as a no-op.
    pub(crate) async fn open_session_worktree_in_tmux_with_command(
        &self,
        open_command: Option<&str>,
    ) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if !self.services.fs_client().is_dir(session.folder.clone()) {
            return;
        }

        let Some(window_id) = self
            .tmux_client
            .open_window_for_folder(session.folder.clone())
            .await
        else {
            return;
        };

        let Some(open_command) = open_command
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return;
        };

        self.tmux_client
            .run_command_in_window(window_id, open_command.to_string())
            .await;
    }

    /// Starts the session-view branch-publish action flow for one session.
    pub(crate) fn start_publish_branch_action(
        &mut self,
        restore_view: ConfirmationViewMode,
        session_id: &str,
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<String>,
    ) {
        let Some(branch_publish_session) = self.branch_publish_task_session(session_id) else {
            self.mode = Self::view_info_popup_mode(
                "Branch push failed".to_string(),
                "Session is no longer available.".to_string(),
                false,
                String::new(),
                restore_view,
            );

            return;
        };

        let loading_title = Self::branch_publish_loading_title(publish_branch_action);
        let loading_message = Self::branch_publish_loading_message(
            publish_branch_action,
            remote_branch_name.as_deref(),
        );
        let loading_label = Self::branch_publish_loading_label(publish_branch_action);
        let clock = self.services.clock();
        let db = self.services.db().clone();
        let event_sender = self.services.event_sender();
        let git_client = self.services.git_client();
        let review_request_client = self.services.review_request_client();
        let background_restore_view = restore_view.clone();
        let event_session_id = branch_publish_session.id.clone();

        self.mode = Self::view_info_popup_mode(
            loading_title,
            loading_message,
            true,
            loading_label,
            restore_view,
        );

        tokio::spawn(async move {
            let result = run_branch_publish_action(
                publish_branch_action,
                branch_publish_session,
                db,
                clock,
                git_client,
                review_request_client,
                remote_branch_name,
            )
            .await;
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = event_sender.send(AppEvent::BranchPublishActionCompleted {
                restore_view: background_restore_view,
                result: Box::new(result),
                session_id: event_session_id,
            });
        });
    }

    /// Returns all configured open commands in user-defined order.
    #[must_use]
    pub(crate) fn configured_open_commands(&self) -> Vec<String> {
        self.settings.open_commands()
    }

    /// Appends output text to a session stream and persists it.
    pub(crate) async fn append_output_for_session(&self, session_id: &str, output: &str) {
        self.sessions
            .append_output_for_session(&self.services, session_id, output)
            .await;
    }

    /// Removes prompt attachment files that still belong to the active
    /// composer state.
    pub(crate) async fn cleanup_prompt_attachment_files(&self, prompt: &TurnPrompt) {
        self.sessions
            .cleanup_prompt_attachment_files(&self.services, prompt)
            .await;
    }

    /// Loads slash-command stats data for one session through the app layer.
    pub(crate) async fn stats_for_session(&self, session_id: &str) -> SessionStatsSnapshot {
        let session_duration_seconds =
            match self.services.db().load_session_timestamps(session_id).await {
                Ok(Some((created_at, updated_at))) => Some((updated_at - created_at).max(0)),
                Ok(None) | Err(_) => None,
            };
        let usage_rows_result = self
            .services
            .db()
            .load_session_usage(session_id)
            .await
            .map_err(|e| e.to_string())
            .map(|usage_rows| {
                usage_rows
                    .into_iter()
                    .map(|row| SessionStatsUsage {
                        input_tokens: row.input_tokens.unsigned_abs(),
                        model: row.model,
                        output_tokens: row.output_tokens.unsigned_abs(),
                    })
                    .collect()
            });

        SessionStatsSnapshot {
            session_duration_seconds,
            usage_rows_result,
        }
    }

    /// Starts squash-merge workflow for a review-ready session.
    ///
    /// # Errors
    /// Returns an error if session is not mergeable, queueing fails, or
    /// immediate merge start fails while the queue is idle.
    pub async fn merge_session(&mut self, session_id: &str) -> Result<(), AppError> {
        if self.merge_queue.is_queued_or_active(session_id) {
            return Ok(());
        }

        self.validate_merge_request(session_id)?;
        if self.merge_queue.has_active() {
            self.mark_session_as_queued_for_merge(session_id).await?;
            self.merge_queue.enqueue(session_id.to_string());

            return Ok(());
        }

        self.merge_queue.enqueue(session_id.to_string());

        self.start_next_merge_from_queue(true).await
    }

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if session cannot start rebasing.
    pub async fn rebase_session(&self, session_id: &str) -> Result<(), AppError> {
        Ok(self
            .sessions
            .rebase_session(&self.services, session_id)
            .await?)
    }

    /// Starts selected-project branch sync in the background and immediately
    /// opens a loading popup with project and branch context.
    pub(crate) fn start_sync_main(&mut self) {
        let sync_popup_context = self.sync_popup_context();
        self.mode = AppMode::SyncBlockedPopup {
            project_name: Some(sync_popup_context.project_name.clone()),
            default_branch: Some(sync_popup_context.default_branch),
            is_loading: true,
            message: Self::sync_loading_message(),
            title: "Sync in progress".to_string(),
        };

        let app_event_tx = self.services.event_sender();
        let default_branch = self.projects.git_branch().map(str::to_string);
        let working_dir = self.projects.working_dir().to_path_buf();
        let git_client = self.services.git_client();
        let _permission_mode = PermissionMode::default();
        let session_model = self.sessions.default_session_model();

        self.sync_main_runner.start_sync_main(
            app_event_tx,
            default_branch,
            git_client,
            session_model,
            working_dir,
        );
    }

    /// Starts review assist generation for one session using the
    /// current diff text and the configured default review model.
    ///
    /// The review assist prompt enforces read-only review constraints
    /// and allows only internet lookup and non-editing verification commands.
    pub(crate) fn start_review_assist(
        &mut self,
        session_id: &str,
        session_folder: &Path,
        diff_hash: u64,
        review_diff: &str,
        session_summary: Option<&str>,
    ) {
        mark_session_agent_review(&mut self.sessions, session_id);

        spawn_review_assist(
            self.services.event_sender(),
            self.settings.default_review_model,
            session_id,
            session_folder,
            diff_hash,
            review_diff,
            session_summary,
        );
    }

    /// Reloads sessions when metadata cache indicates changes.
    pub async fn refresh_sessions_if_needed(&mut self) {
        self.sessions
            .refresh_sessions_if_needed(&mut self.mode, &self.projects, &self.services)
            .await;
    }

    /// Forces immediate session list reload.
    pub(crate) async fn refresh_sessions_now(&mut self) {
        self.sessions
            .refresh_sessions_now(&mut self.mode, &self.projects, &self.services)
            .await;
        self.restart_git_status_task();
    }

    /// Reloads project list snapshots from persistence.
    async fn reload_projects(&mut self) {
        let project_items =
            Self::load_project_items(self.services.db(), self.services.fs_client().as_ref()).await;
        self.projects.replace_project_items(project_items);
    }

    /// Restarts git status polling for the currently active project context.
    fn restart_git_status_task(&mut self) {
        let cancel = self.projects.replace_git_status_cancel();
        if !self.projects.has_git_branch() {
            return;
        }

        task::TaskService::spawn_git_status_task(
            self.projects.working_dir(),
            self.projects.git_branch().unwrap_or_default().to_string(),
            Self::session_git_status_targets(&self.sessions),
            cancel,
            self.services.event_sender(),
            self.services.git_client(),
        );
    }

    /// Builds git-status polling targets for active session branches in the
    /// current project.
    pub(crate) fn session_git_status_targets(
        sessions: &SessionManager,
    ) -> Vec<task::SessionGitStatusTarget> {
        sessions
            .state()
            .sessions
            .iter()
            .filter(|session| !matches!(session.status, Status::Canceled | Status::Done))
            .map(|session| task::SessionGitStatusTarget {
                base_branch: session.base_branch.clone(),
                branch_name: session::session_branch(&session.id),
                session_id: session.id.clone(),
            })
            .collect()
    }

    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = AppEventReducer::drain(&mut self.event_rx, first_event);
        let mut event_batch = AppEventBatch::default();
        for event in drained_events {
            event_batch.collect_event(event);
        }

        self.apply_app_event_batch(event_batch).await;
    }

    /// Processes currently queued app events without waiting.
    pub(crate) async fn process_pending_app_events(&mut self) {
        let Ok(first_event) = self.event_rx.try_recv() else {
            return;
        };

        self.apply_app_events(first_event).await;
    }

    /// Waits for the next internal app event.
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Applies one reduced app-event batch to in-memory app state.
    ///
    /// Session updates are synchronized from runtime handles first. Any touched
    /// session that reached terminal status (`Done`, `Canceled`) then drops its
    /// worker queue so background workers can shut down provider runtimes.
    async fn apply_app_event_batch(&mut self, event_batch: AppEventBatch) {
        let previous_session_states = event_batch
            .session_ids
            .iter()
            .filter_map(|session_id| {
                self.sessions
                    .sessions
                    .iter()
                    .find(|session| session.id == *session_id)
                    .map(|session| (session_id.clone(), session.status))
            })
            .collect::<HashMap<_, _>>();

        if event_batch.should_force_reload {
            self.refresh_sessions_now().await;
            self.reload_projects().await;
            self.refresh_active_project_roadmap_and_tabs().await;
        }

        if event_batch.should_refresh_git_status {
            self.restart_git_status_task();
        }

        if event_batch.has_git_status_update {
            self.projects.set_git_status(event_batch.git_status_update);
            self.sessions
                .replace_session_git_statuses(event_batch.session_git_status_updates);
        }

        if event_batch.has_latest_available_version_update {
            self.latest_available_version = event_batch.latest_available_version_update;
        }

        if let Some(update_status) = event_batch.update_status {
            self.update_status = Some(update_status);
        }

        for (session_id, session_model) in event_batch.session_model_updates {
            self.sessions
                .apply_session_model_updated(&session_id, session_model);
        }

        for (session_id, reasoning_level_override) in event_batch.session_reasoning_level_updates {
            self.sessions
                .apply_session_reasoning_level_updated(&session_id, reasoning_level_override);
        }

        for (session_id, (added_lines, deleted_lines, session_size)) in
            event_batch.session_size_updates
        {
            self.sessions.apply_session_size_updated(
                &session_id,
                added_lines,
                deleted_lines,
                session_size,
            );
        }

        for (session_id, generation) in event_batch.session_title_generation_finished {
            self.sessions
                .clear_title_generation_task_if_matches(&session_id, generation);
        }

        for (session_id, entries) in event_batch.at_mention_entries_updates {
            self.apply_prompt_at_mention_entries(&session_id, entries);
        }

        apply_review_updates(
            &mut self.review_cache,
            &mut self.mode,
            &mut self.sessions,
            event_batch.review_updates,
        );

        if let Some(branch_publish_action_update) = event_batch.branch_publish_action_update {
            self.apply_branch_publish_action_update(branch_publish_action_update);
        }

        if let Some(sync_review_request_update) = event_batch.sync_review_request_update {
            self.apply_sync_review_request_update(sync_review_request_update)
                .await;
        }

        for (session_id, progress_message) in event_batch.session_progress_updates {
            if let Some(progress_message) = progress_message {
                self.session_progress_messages
                    .insert(session_id, progress_message);
            } else {
                self.session_progress_messages.remove(&session_id);
            }
        }

        for (session_id, turn_applied_state) in event_batch.applied_turns {
            self.apply_agent_response_received(&session_id, &turn_applied_state);
        }
        for (session_id, sync_update) in event_batch.published_branch_sync_updates {
            self.apply_published_branch_sync_update(&session_id, sync_update);
        }

        for session_id in &event_batch.session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }
        self.sessions
            .clear_terminal_session_workers(&event_batch.session_ids);

        auto_start_reviews(
            &mut self.review_cache,
            &event_batch.session_ids,
            &mut self.sessions,
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_model,
        )
        .await;

        if let Some(sync_main_result) = event_batch.sync_main_result {
            let should_refresh_active_project_roadmap = sync_main_result.is_ok();
            let sync_popup_context = self.sync_popup_context();

            self.mode = Self::sync_main_popup_mode(sync_main_result, &sync_popup_context);
            if should_refresh_active_project_roadmap {
                self.refresh_active_project_roadmap_and_tabs().await;
            }
        }

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.retain_valid_session_progress_messages();
        self.sessions.retain_active_prompt_outputs();
    }

    /// Routes one persisted turn projection to the currently focused session
    /// UI.
    ///
    /// The session worker persists the canonical summary, clarification
    /// questions, follow-up tasks, and token-usage delta before sending this
    /// event, so the reducer can apply the exact same projection in memory
    /// without waiting for a forced reload.
    fn apply_agent_response_received(
        &mut self,
        session_id: &str,
        turn_applied_state: &TurnAppliedState,
    ) {
        if !self
            .sessions
            .sessions
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.sessions
            .apply_turn_applied_state(session_id, turn_applied_state);
        let questions = turn_applied_state.questions.clone();
        if questions.is_empty() {
            return;
        }

        let is_viewing_session = match &self.mode {
            AppMode::View {
                session_id: view_id,
                ..
            }
            | AppMode::Prompt {
                session_id: view_id,
                ..
            }
            | AppMode::Diff {
                session_id: view_id,
                ..
            }
            | AppMode::Question {
                session_id: view_id,
                ..
            }
            | AppMode::OpenCommandSelector {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            }
            | AppMode::PublishBranchInput {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            }
            | AppMode::ViewInfoPopup {
                restore_view:
                    ConfirmationViewMode {
                        session_id: view_id,
                        ..
                    },
                ..
            } => view_id == session_id,
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Help { .. } => false,
        };

        if is_viewing_session {
            self.mode = AppMode::Question {
                at_mention_state: None,
                selected_option_index: question::default_option_index(&questions, 0),
                session_id: session_id.to_string(),
                questions,
                responses: Vec::new(),
                current_index: 0,
                focus: QuestionFocus::Answer,
                input: InputState::default(),
                scroll_offset: None,
            };
        }
    }

    /// Routes one published-branch auto-push update to the matching in-memory
    /// session snapshot.
    fn apply_published_branch_sync_update(
        &mut self,
        session_id: &str,
        sync_update: PublishedBranchSyncUpdate,
    ) {
        let PublishedBranchSyncUpdate {
            sync_operation_id,
            sync_status,
        } = sync_update;

        match sync_status {
            PublishedBranchSyncStatus::InProgress => {
                self.sessions
                    .start_published_branch_sync(session_id, sync_operation_id);
            }
            PublishedBranchSyncStatus::Idle
            | PublishedBranchSyncStatus::Succeeded
            | PublishedBranchSyncStatus::Failed => {
                self.sessions.finish_published_branch_sync(
                    session_id,
                    &sync_operation_id,
                    sync_status,
                );
            }
        }
    }

    /// Returns the currently selected follow-up task payload for one session.
    fn selected_follow_up_task_snapshot(
        &self,
        session_id: &str,
    ) -> Option<(usize, String, Option<String>)> {
        let position = self.sessions.selected_follow_up_task_position(session_id)?;
        let session = self
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;
        let follow_up_task = session.follow_up_task(position)?;

        Some((
            follow_up_task.position,
            follow_up_task.text.clone(),
            follow_up_task.launched_session_id.clone(),
        ))
    }

    /// Persists one launched sibling-session link and mirrors it into the
    /// in-memory session snapshot.
    ///
    /// # Errors
    /// Returns an error if the launched-session link cannot be persisted.
    async fn set_follow_up_task_launched_session_id(
        &mut self,
        session_id: &str,
        position: usize,
        launched_session_id: Option<String>,
    ) -> Result<(), AppError> {
        self.services
            .db()
            .update_session_follow_up_task_launched_session_id(
                session_id,
                position,
                launched_session_id.as_deref(),
            )
            .await?;
        self.sessions.set_follow_up_task_launched_session_id(
            session_id,
            position,
            launched_session_id,
        );

        Ok(())
    }

    /// Opens one linked sibling session when it still exists in memory.
    ///
    /// Returns `true` when the target session was found and opened.
    fn open_session_if_present(&mut self, target_session_id: &str) -> bool {
        let Some(session_index) = self.session_index_for_id(target_session_id) else {
            return false;
        };
        self.open_session_by_index(target_session_id, session_index);

        true
    }

    /// Opens one session by id and preserves question mode for clarification
    /// sessions.
    fn open_session(&mut self, target_session_id: &str) {
        let Some(session_index) = self.session_index_for_id(target_session_id) else {
            return;
        };
        self.open_session_by_index(target_session_id, session_index);
    }

    /// Opens one session by list index and preserves question mode for
    /// clarification sessions.
    fn open_session_by_index(&mut self, target_session_id: &str, session_index: usize) {
        self.sessions.table_state.select(Some(session_index));

        let Some(session) = self.sessions.sessions.get(session_index) else {
            return;
        };
        if session.status == Status::Question {
            let questions = session.questions.clone();
            let selected_option_index = question::default_option_index(&questions, 0);
            self.mode = AppMode::Question {
                at_mention_state: None,
                session_id: target_session_id.to_string(),
                questions,
                responses: Vec::new(),
                current_index: 0,
                focus: QuestionFocus::Answer,
                input: InputState::default(),
                scroll_offset: None,
                selected_option_index,
            };

            return;
        }

        let (review_status_message, review_text) = self.review_view_state(target_session_id);

        self.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message,
            review_text,
            session_id: target_session_id.to_string(),
            scroll_offset: None,
        };
    }

    /// Applies loaded at-mention entries to the currently focused prompt or
    /// question session, if the mention query is still active.
    fn apply_prompt_at_mention_entries(&mut self, session_id: &str, entries: Vec<FileEntry>) {
        let (at_mention_state, has_query) = match &mut self.mode {
            AppMode::Prompt {
                at_mention_state,
                input,
                session_id: mode_session_id,
                ..
            } if mode_session_id == session_id => {
                (at_mention_state, input.at_mention_query().is_some())
            }
            AppMode::Question {
                at_mention_state,
                input,
                session_id: mode_session_id,
                ..
            } if mode_session_id == session_id => {
                (at_mention_state, input.at_mention_query().is_some())
            }
            _ => return,
        };

        if !has_query {
            return;
        }

        if let Some(state) = at_mention_state.as_mut() {
            state.all_entries = entries;
            state.selected_index = 0;

            return;
        }

        *at_mention_state = Some(PromptAtMentionState::new(entries));
    }

    /// Applies one review assist update to cache and focused render state.
    #[cfg(test)]
    fn apply_review_update(&mut self, session_id: &str, review_update: app::review::ReviewUpdate) {
        let mut review_updates = HashMap::new();
        review_updates.insert(session_id.to_string(), review_update);
        apply_review_updates(
            &mut self.review_cache,
            &mut self.mode,
            &mut self.sessions,
            review_updates,
        );
    }

    /// Starts focused review generation for sessions that just entered review.
    #[cfg(test)]
    async fn auto_start_reviews(&mut self, session_ids: &HashSet<String>) {
        auto_start_reviews(
            &mut self.review_cache,
            session_ids,
            &mut self.sessions,
            self.services.git_client(),
            self.services.event_sender(),
            self.settings.default_review_model,
        )
        .await;
    }

    /// Applies one completed branch-publish action and updates the popup.
    fn apply_branch_publish_action_update(
        &mut self,
        branch_publish_action_update: BranchPublishActionUpdate,
    ) {
        let BranchPublishActionUpdate {
            restore_view,
            result,
            session_id,
        } = branch_publish_action_update;

        let popup_mode = match result {
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name,
                review_request_creation,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);

                Self::view_info_popup_mode(
                    Self::branch_publish_success_title(PublishBranchAction::Push),
                    Self::branch_publish_success_message(
                        &branch_name,
                        review_request_creation.as_ref(),
                    ),
                    false,
                    String::new(),
                    restore_view,
                )
            }
            Ok(BranchPublishTaskSuccess::PullRequestPublished {
                branch_name,
                review_request,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);
                self.sessions
                    .apply_review_request(&session_id, review_request.clone());

                Self::view_info_popup_mode(
                    Self::review_request_publish_success_title(&review_request),
                    Self::pull_request_publish_success_message(&branch_name, &review_request),
                    false,
                    String::new(),
                    restore_view,
                )
            }
            Err(failure) => Self::view_info_popup_mode(
                failure.title,
                failure.message,
                false,
                String::new(),
                restore_view,
            ),
        };
        self.mode = popup_mode;
    }

    /// Starts a background review request sync for one session.
    ///
    /// Shows a loading popup while the sync runs, then emits a
    /// [`AppEvent::SyncReviewRequestCompleted`] when the forge responds.
    pub(crate) fn start_sync_review_request_action(
        &mut self,
        restore_view: ConfirmationViewMode,
        session_id: &str,
    ) {
        let Some(sync_session) = self.sessions.session_or_err(session_id).ok() else {
            return;
        };

        self.mode = Self::view_info_popup_mode(
            "Sync review request".to_string(),
            "Checking review request status\u{2026}".to_string(),
            true,
            "syncing".to_string(),
            restore_view.clone(),
        );

        let event_sender = self.services.event_sender();
        let git_client = self.services.git_client();
        let review_request_client = self.services.review_request_client();
        let linked_review_request = sync_session.review_request.clone();
        let published_upstream_ref = sync_session.published_upstream_ref.clone();
        let folder = sync_session.folder.clone();
        let background_session_id = session_id.to_string();
        let background_restore_view = restore_view;

        tokio::spawn(async move {
            let result = run_sync_review_request(
                folder,
                git_client,
                linked_review_request,
                published_upstream_ref,
                review_request_client,
            )
            .await;
            let _ = event_sender.send(AppEvent::SyncReviewRequestCompleted {
                restore_view: background_restore_view,
                result,
                session_id: background_session_id,
            });
        });
    }

    /// Applies the result of a completed review request sync action.
    async fn apply_sync_review_request_update(
        &mut self,
        sync_review_request_update: SyncReviewRequestUpdate,
    ) {
        let SyncReviewRequestUpdate {
            restore_view,
            result,
            session_id,
        } = sync_review_request_update;

        let popup_mode = match result {
            Ok(task_result) => {
                let persistence_warning = if let Some(summary) = task_result.summary {
                    self.sessions
                        .store_review_request_summary(&self.services, &session_id, summary)
                        .await
                        .err()
                        .map(|error| format!("Failed to persist review request: {error}"))
                } else {
                    None
                };

                let (title, mut body) = match task_result.outcome {
                    session::SyncReviewRequestOutcome::Merged { display_id } => {
                        let cleanup_warning =
                            self.complete_externally_merged_session(&session_id).await;
                        let mut merged_body = format!(
                            "Review request {display_id} was merged. Session moved to Done."
                        );
                        if let Some(warning) = cleanup_warning {
                            merged_body.push_str("\n\n");
                            merged_body.push_str(&warning);
                        }

                        ("Review request merged".to_string(), merged_body)
                    }
                    session::SyncReviewRequestOutcome::Open {
                        display_id,
                        status_summary,
                    } => {
                        let status_detail = status_summary
                            .map(|summary| format!(" ({summary})"))
                            .unwrap_or_default();

                        (
                            "Review request open".to_string(),
                            format!(
                                "Review request {display_id} is still open{status_detail}. No \
                                 changes made."
                            ),
                        )
                    }
                    session::SyncReviewRequestOutcome::Closed { display_id } => (
                        "Review request closed".to_string(),
                        format!(
                            "Review request {display_id} was closed without merge. Cancel the \
                             session to clean up."
                        ),
                    ),
                    session::SyncReviewRequestOutcome::NoReviewRequest => (
                        "No review request found".to_string(),
                        "No review request was found for this session branch.".to_string(),
                    ),
                };

                if let Some(warning) = persistence_warning {
                    body.push_str("\n\n");
                    body.push_str(&warning);
                }

                Self::view_info_popup_mode(title, body, false, String::new(), restore_view)
            }
            Err(error) => Self::view_info_popup_mode(
                "Sync failed".to_string(),
                error,
                false,
                String::new(),
                restore_view,
            ),
        };

        self.mode = popup_mode;
        self.refresh_sessions_now().await;
    }

    /// Transitions one externally merged session to `Done` with best-effort
    /// worktree and branch cleanup.
    ///
    /// Returns an optional warning message when worktree cleanup fails. The
    /// session is still moved to `Done` because the merge already happened
    /// upstream, but the caller should surface the warning to the user.
    async fn complete_externally_merged_session(&self, session_id: &str) -> Option<String> {
        let Ok(session) = self.sessions.session_or_err(session_id) else {
            return None;
        };
        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return None;
        };

        let folder = session.folder.clone();
        let source_branch = session::session_branch(session_id);

        let cleanup_warning = SessionManager::cleanup_merged_session_worktree(
            folder,
            self.services.fs_client(),
            self.services.git_client(),
            source_branch,
            None,
        )
        .await
        .err()
        .map(|error| format!("Worktree cleanup failed: {error}"));

        let app_event_tx = self.services.event_sender();

        SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Done,
        )
        .await;

        cleanup_warning
    }

    /// Validates whether a session is currently eligible for merge queueing.
    ///
    /// Sessions are eligible while actively under review or already marked as
    /// `Queued` (for example, after app restart).
    ///
    /// # Errors
    /// Returns an error when the session does not exist or has an ineligible
    /// status.
    fn validate_merge_request(&self, session_id: &str) -> Result<(), AppError> {
        let session = self.sessions.session_or_err(session_id)?;
        if !(session.status.allows_review_actions() || session.status == Status::Queued) {
            return Err(AppError::Workflow(
                "Session must be in review or queued status".to_string(),
            ));
        }

        Ok(())
    }

    /// Marks one session as waiting in the merge queue.
    ///
    /// # Errors
    /// Returns an error when status transition to `Queued` is invalid.
    async fn mark_session_as_queued_for_merge(&self, session_id: &str) -> Result<(), AppError> {
        let handles = self.sessions.session_handles_or_err(session_id)?;
        let app_event_tx = self.services.event_sender();
        let status_updated = SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Queued,
        )
        .await;

        if !status_updated {
            return Err(AppError::Workflow(
                "Invalid status transition to Queued".to_string(),
            ));
        }

        Ok(())
    }

    /// Restores a queued session to `Review` if merge start fails.
    async fn restore_queued_session_to_review(&self, session_id: &str) {
        let session_status = self
            .sessions
            .session_or_err(session_id)
            .map(|session| session.status);
        if !matches!(session_status, Ok(Status::Queued)) {
            return;
        }

        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = self.services.event_sender();
        // Best-effort: status transition failure is non-critical.
        let _ = SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.clock().as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Review,
        )
        .await;
    }

    /// Starts the next pending merge request when no merge is currently active.
    ///
    /// When `stop_on_failure` is `true`, returns the first start error.
    /// Otherwise, failed entries are skipped and the queue continues.
    ///
    /// # Errors
    /// Returns an error when starting a queued merge fails and
    /// `stop_on_failure` is enabled.
    async fn start_next_merge_from_queue(&mut self, stop_on_failure: bool) -> Result<(), AppError> {
        if self.merge_queue.has_active() {
            return Ok(());
        }

        while let Some(next_session_id) = self.merge_queue.pop_next() {
            match self
                .sessions
                .merge_session(&next_session_id, &self.projects, &self.services)
                .await
            {
                Ok(()) => {
                    self.merge_queue.set_active(next_session_id);

                    return Ok(());
                }
                Err(error) => {
                    self.restore_queued_session_to_review(&next_session_id)
                        .await;

                    let merge_error = format!("\n[Merge Error] {error}\n");
                    self.append_output_for_session(&next_session_id, &merge_error)
                        .await;

                    if stop_on_failure {
                        return Err(error.into());
                    }
                }
            }
        }

        Ok(())
    }

    /// Advances queue state after reducer-applied status changes.
    ///
    /// The queue advances when the active merge session transitions away from
    /// `Merging` or disappears from the refreshed session list.
    async fn handle_merge_queue_progress(
        &mut self,
        session_ids: &HashSet<String>,
        previous_session_states: &HashMap<String, Status>,
    ) {
        let current_status = self
            .merge_queue
            .active_session_id()
            .and_then(|active_session_id| {
                self.sessions
                    .sessions
                    .iter()
                    .find(|session| session.id == active_session_id)
                    .map(|session| session.status)
            });
        let progress = self.merge_queue.progress_from_status_updates(
            current_status,
            session_ids,
            previous_session_states,
        );
        if progress == MergeQueueProgress::StartNext {
            // Best-effort: merge queue progression failure is handled by status events.
            let _ = self.start_next_merge_from_queue(false).await;
        }
    }

    /// Builds one background-task snapshot for a branch-publish action.
    fn branch_publish_task_session(&self, session_id: &str) -> Option<BranchPublishTaskSession> {
        self.sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(BranchPublishTaskSession::from_session)
    }

    /// Builds a session-view info popup mode with explicit loading metadata.
    fn view_info_popup_mode(
        title: String,
        message: String,
        is_loading: bool,
        loading_label: String,
        restore_view: ConfirmationViewMode,
    ) -> AppMode {
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            restore_view,
            title,
        }
    }

    /// Returns the loading popup title for one branch-publish action.
    fn branch_publish_loading_title(publish_branch_action: PublishBranchAction) -> String {
        branch_publish_loading_title_text(publish_branch_action)
    }

    /// Drops thinking text for sessions that are no longer actively running.
    fn retain_valid_session_progress_messages(&mut self) {
        self.session_progress_messages.retain(|session_id, _| {
            self.sessions
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| matches!(session.status, Status::InProgress))
        });
    }

    /// Returns the loading popup body for one branch-publish action.
    fn branch_publish_loading_message(
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<&str>,
    ) -> String {
        branch_publish_loading_message_text(publish_branch_action, remote_branch_name)
    }

    /// Returns the loading spinner label for one branch-publish action.
    fn branch_publish_loading_label(publish_branch_action: PublishBranchAction) -> String {
        branch_publish_loading_label_text(publish_branch_action)
    }

    /// Returns the success popup title for a completed branch-publish action.
    fn branch_publish_success_title(publish_branch_action: PublishBranchAction) -> String {
        branch_publish_success_title_text(publish_branch_action)
    }

    /// Returns the success popup body for one completed branch push.
    fn branch_publish_success_message(
        branch_name: &str,
        review_request_creation: Option<&crate::app::branch_publish::ReviewRequestCreationInfo>,
    ) -> String {
        branch_push_success_message_text(branch_name, review_request_creation)
    }

    /// Returns the success popup title for one completed review-request
    /// publish.
    fn review_request_publish_success_title(
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        crate::app::branch_publish::review_request_publish_success_title(review_request)
    }

    /// Returns the success popup body for one completed review-request
    /// publish.
    fn pull_request_publish_success_message(
        branch_name: &str,
        review_request: &crate::domain::session::ReviewRequest,
    ) -> String {
        pull_request_publish_success_message_text(branch_name, review_request)
    }

    /// Builds final sync popup mode from background sync completion result.
    ///
    /// Authentication-related push failures are normalized to actionable
    /// authorization guidance so users can recover quickly.
    fn sync_main_popup_mode(
        sync_main_result: Result<SyncMainOutcome, SyncSessionStartError>,
        sync_popup_context: &SyncPopupContext,
    ) -> AppMode {
        match sync_main_result {
            Ok(sync_main_outcome) => AppMode::SyncBlockedPopup {
                project_name: Some(sync_popup_context.project_name.clone()),
                default_branch: Some(sync_popup_context.default_branch.clone()),
                is_loading: false,
                message: Self::sync_success_message(&sync_main_outcome),
                title: "Sync complete".to_string(),
            },
            Err(sync_error @ SyncSessionStartError::MainHasUncommittedChanges { .. }) => {
                AppMode::SyncBlockedPopup {
                    project_name: Some(sync_popup_context.project_name.clone()),
                    default_branch: Some(sync_popup_context.default_branch.clone()),
                    is_loading: false,
                    message: sync_error.detail_message(),
                    title: "Sync blocked".to_string(),
                }
            }
            Err(sync_error @ SyncSessionStartError::Other(_)) => AppMode::SyncBlockedPopup {
                project_name: Some(sync_popup_context.project_name.clone()),
                default_branch: Some(sync_popup_context.default_branch.clone()),
                is_loading: false,
                message: Self::sync_failure_message(&sync_error),
                title: "Sync failed".to_string(),
            },
        }
    }

    /// Builds success copy for sync completion with pull/push/conflict metrics
    /// rendered as markdown sections with empty lines separating pull, push,
    /// and conflict blocks.
    fn sync_success_message(sync_main_outcome: &SyncMainOutcome) -> String {
        let pulled_summary = Self::sync_commit_summary("pulled", sync_main_outcome.pulled_commits);
        let pulled_titles =
            Self::sync_pulled_commit_titles_summary(&sync_main_outcome.pulled_commit_titles);
        let pushed_titles =
            Self::sync_pushed_commit_titles_summary(&sync_main_outcome.pushed_commit_titles);
        let pushed_summary = Self::sync_commit_summary("pushed", sync_main_outcome.pushed_commits);
        let conflict_summary =
            Self::sync_conflict_summary(&sync_main_outcome.resolved_conflict_files);

        sync_blocked::format_sync_success_message(
            &pulled_summary,
            &pulled_titles,
            &pushed_summary,
            &pushed_titles,
            &conflict_summary,
        )
    }

    /// Returns pulled commit titles formatted as an indented list.
    fn sync_pulled_commit_titles_summary(pulled_commit_titles: &[String]) -> String {
        if pulled_commit_titles.is_empty() {
            return String::new();
        }

        pulled_commit_titles
            .iter()
            .map(|title| format!("  - {title}"))
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Returns pushed commit titles formatted as an indented list.
    fn sync_pushed_commit_titles_summary(pushed_commit_titles: &[String]) -> String {
        if pushed_commit_titles.is_empty() {
            return String::new();
        }

        pushed_commit_titles
            .iter()
            .map(|title| format!("  - {title}"))
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Returns sync failure copy with actionable guidance for auth failures.
    ///
    /// Authentication failures show a dismiss-only message so users can fix
    /// credentials first, then restart sync from the list. When the failing
    /// remote host is recognizable, the guidance names the matching forge CLI.
    fn sync_failure_message(sync_error: &SyncSessionStartError) -> String {
        let detail_message = sync_error.detail_message();
        if !is_git_push_authentication_error(&detail_message) {
            return detail_message;
        }

        git_push_authentication_message(
            detected_forge_kind_from_git_push_error(&detail_message),
            "run sync again",
        )
    }

    /// Returns one brief pull/push sentence fragment for sync completion.
    fn sync_commit_summary(direction: &str, commit_count: Option<u32>) -> String {
        match commit_count {
            Some(1) => format!("1 commit {direction}"),
            Some(commit_count) => format!("{commit_count} commits {direction}"),
            None => format!("commits {direction}: unknown"),
        }
    }

    /// Returns one brief conflict-resolution sentence fragment for sync
    /// completion.
    fn sync_conflict_summary(resolved_conflict_files: &[String]) -> String {
        if resolved_conflict_files.is_empty() {
            return "no conflicts fixed".to_string();
        }

        format!("conflicts fixed: {}", resolved_conflict_files.join(", "))
    }

    /// Returns popup context for the currently active project sync target.
    fn sync_popup_context(&self) -> SyncPopupContext {
        let default_branch = self
            .projects
            .git_branch()
            .map_or_else(|| "not detected".to_string(), str::to_string);
        let project_name = self.projects.project_name().to_string();

        SyncPopupContext {
            default_branch,
            project_name,
        }
    }

    /// Resolves the configured upstream reference for one project branch.
    async fn load_git_upstream_ref(
        git_client: &dyn GitClient,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Option<String> {
        AppStartup::load_git_upstream_ref(git_client, working_dir, git_branch).await
    }

    /// Resolves startup active project id from settings, falling back to the
    /// current working directory when the stored project row is stale.
    #[cfg(test)]
    async fn resolve_startup_active_project_id(
        db: &Database,
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
    async fn load_project_items(db: &Database, fs_client: &dyn FsClient) -> Vec<ProjectListItem> {
        AppStartup::load_project_items(db, fs_client).await
    }

    /// Loads project list entries with one caller-provided session worktree
    /// root for filtering.
    #[cfg(test)]
    async fn load_project_items_with_session_worktree_root(
        db: &Database,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<ProjectListItem> {
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
    async fn load_projects_from_home_directory(
        db: &Database,
        session_worktree_root: &Path,
        home_directory: Option<&Path>,
    ) {
        AppStartup::load_projects_from_home_directory(db, session_worktree_root, home_directory)
            .await;
    }

    /// Returns git repository roots discovered under the user home directory.
    ///
    /// A repository root is identified by a direct `.git` marker inside the
    /// directory and discovery stops after `HOME_PROJECT_SCAN_MAX_RESULTS`.
    #[cfg(test)]
    fn discover_home_project_paths(
        home_directory: &Path,
        session_worktree_root: &Path,
    ) -> Vec<PathBuf> {
        AppStartup::discover_home_project_paths(home_directory, session_worktree_root)
    }

    /// Returns whether a persisted project path points to an agentty session
    /// worktree under `~/.agentty/wt`.
    #[cfg(test)]
    fn is_session_worktree_project_path(project_path: &str, session_worktree_root: &Path) -> bool {
        AppStartup::is_session_worktree_project_path(project_path, session_worktree_root)
    }

    /// Filters persisted project rows down to entries that should remain
    /// visible in the Projects tab.
    #[cfg(test)]
    fn visible_project_rows(
        project_rows: Vec<db::ProjectListRow>,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<db::ProjectListRow> {
        AppStartup::visible_project_rows(project_rows, fs_client, session_worktree_root)
    }

    /// Returns whether one persisted project path still resolves to a
    /// directory on disk.
    #[cfg(test)]
    fn is_existing_project_path(fs_client: &dyn FsClient, project_path: &str) -> bool {
        AppStartup::is_existing_project_path(fs_client, project_path)
    }

    /// Loads the active project's roadmap snapshot when the roadmap file
    /// exists.
    async fn load_project_roadmap(
        fs_client: &dyn FsClient,
        working_dir: &Path,
    ) -> Option<ActiveProjectRoadmap> {
        let roadmap_path = working_dir.join(TASKS_ROADMAP_PATH);
        if !fs_client.is_file(roadmap_path.clone()) {
            return None;
        }

        match fs_client.read_file(roadmap_path).await {
            Ok(contents) => Some(ActiveProjectRoadmap::Loaded(
                String::from_utf8_lossy(&contents).into_owned(),
            )),
            Err(error) => Some(ActiveProjectRoadmap::LoadError(format!(
                "Failed to load `{TASKS_ROADMAP_PATH}`: {error}"
            ))),
        }
    }

    /// Converts a project row into domain project model.
    fn project_from_row(project_row: db::ProjectRow) -> Project {
        AppStartup::project_from_row(project_row)
    }

    /// Returns loading-state popup copy for sync-main operation.
    fn sync_loading_message() -> String {
        "Synchronizing with its upstream.".to_string()
    }
}

/// Runs a review request sync against the forge in a background task.
///
/// When the session has a linked review request, this refreshes it by display
/// id. Otherwise, when the branch was published, this searches for an
/// externally created review request by source branch name.
async fn run_sync_review_request(
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    linked_review_request: Option<crate::domain::session::ReviewRequest>,
    published_upstream_ref: Option<String>,
    review_request_client: Arc<dyn ReviewRequestClient>,
) -> Result<SyncReviewRequestTaskResult, String> {
    let repo_url = git_client
        .repo_url(folder.clone())
        .await
        .map_err(|error| format!("Failed to resolve repository remote: {error}"))?;
    let remote = review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(folder))
        .map_err(|error| error.detail_message())?;

    if let Some(review_request) = linked_review_request {
        let refreshed_summary = review_request_client
            .refresh_review_request(remote, review_request.summary.display_id)
            .await
            .map_err(|error| error.detail_message())?;

        return Ok(sync_task_result_from_summary(refreshed_summary));
    }

    let upstream_ref = published_upstream_ref
        .ok_or_else(|| "Session branch has not been published yet".to_string())?;
    let source_branch = session::remote_branch_name_from_upstream_ref(&upstream_ref);
    let found_summary = review_request_client
        .find_by_source_branch(remote, source_branch)
        .await
        .map_err(|error| error.detail_message())?;

    match found_summary {
        Some(summary) => Ok(sync_task_result_from_summary(summary)),
        None => Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
            summary: None,
        }),
    }
}

/// Builds a sync task result from one normalized review request summary.
fn sync_task_result_from_summary(
    summary: crate::domain::session::ReviewRequestSummary,
) -> SyncReviewRequestTaskResult {
    let display_id = summary.display_id.clone();
    let outcome = match summary.state {
        crate::domain::session::ReviewRequestState::Open => {
            session::SyncReviewRequestOutcome::Open {
                display_id,
                status_summary: summary.status_summary.clone(),
            }
        }
        crate::domain::session::ReviewRequestState::Merged => {
            session::SyncReviewRequestOutcome::Merged { display_id }
        }
        crate::domain::session::ReviewRequestState::Closed => {
            session::SyncReviewRequestOutcome::Closed { display_id }
        }
    };

    SyncReviewRequestTaskResult {
        outcome,
        summary: Some(summary),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use mockall::predicate::eq;
    use serde_json;
    use tempfile::tempdir;

    use super::*;
    use crate::app::branch_publish::{BranchPublishTaskResult, BranchPublishTaskSuccess};
    use crate::app::review::ReviewUpdate;
    use crate::app::startup::HOME_PROJECT_SCAN_MAX_RESULTS;
    use crate::app::{diff_content_hash, review_loading_message};
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{
        ForgeKind, PublishedBranchSyncStatus, ReviewRequestState, ReviewRequestSummary,
        SESSION_DATA_DIR, Session, SessionFollowUpTask, SessionHandles, SessionSize, SessionStats,
        Status,
    };
    use crate::domain::setting::SettingName;
    use crate::infra::agent::protocol::{AgentResponseSummary, QuestionItem};
    use crate::infra::db::Database;
    use crate::infra::file_index::FileEntry;
    use crate::infra::tmux::{MockTmuxClient, TmuxClient};
    use crate::ui::state::app_mode::{ConfirmationViewMode, DoneSessionOutputMode};

    /// Builds one mock app-server client wrapped in `Arc`.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds one client bundle with one injected availability snapshot for
    /// test app startup.
    fn test_app_clients_with_available_agent_kinds(
        available_agent_kinds: Vec<AgentKind>,
    ) -> AppClients {
        AppClients::new().with_agent_availability_probe(Arc::new(
            agent::StaticAgentAvailabilityProbe {
                available_agent_kinds,
            },
        ))
    }

    /// Builds one client bundle with deterministic agent availability for
    /// test app startup.
    fn test_app_clients() -> AppClients {
        test_app_clients_with_available_agent_kinds(AgentKind::ALL.to_vec())
    }

    /// Builds one deterministic session snapshot rooted at `session_folder`.
    fn test_session(session_folder: PathBuf) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: session_folder,
            follow_up_tasks: Vec::new(),
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "test-project".to_string(),
            prompt: "test prompt".to_string(),
            reasoning_level_override: None,
            published_upstream_ref: None,
            published_branch_sync_status: crate::domain::session::PublishedBranchSyncStatus::Idle,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: None,
            title: None,
            updated_at: 0,
        }
    }

    /// Builds one reducer-ready turn projection for tests.
    fn test_turn_applied_state(
        questions: Vec<QuestionItem>,
        follow_up_tasks: Vec<&str>,
        summary: Option<AgentResponseSummary>,
        token_usage_delta: SessionStats,
    ) -> TurnAppliedState {
        TurnAppliedState {
            follow_up_tasks: follow_up_tasks
                .into_iter()
                .enumerate()
                .map(|(position, text)| SessionFollowUpTask {
                    id: i64::try_from(position).unwrap_or(i64::MAX),
                    launched_session_id: None,
                    position,
                    text: text.to_string(),
                })
                .collect(),
            questions,
            summary: summary.and_then(|summary| serde_json::to_string(&summary).ok()),
            token_usage_delta,
        }
    }

    /// Builds a restore-view snapshot used by branch-publish event-batch
    /// tests.
    fn test_confirmation_view_mode(session_id: &str) -> ConfirmationViewMode {
        ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: session_id.to_string(),
        }
    }

    /// Builds a successful branch-publish batch payload for one session.
    fn test_pushed_branch_result(branch_name: &str) -> BranchPublishTaskResult {
        Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: branch_name.to_string(),
            review_request_creation: None,
            upstream_reference: format!("origin/{branch_name}"),
        })
    }

    /// Builds a test app rooted at one temporary workspace with an injected
    /// tmux boundary.
    async fn new_test_app_with_tmux_client(tmux_client: Arc<dyn TmuxClient>) -> App {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = test_app_clients()
            .with_app_server_client_override(mock_app_server())
            .with_tmux_client(tmux_client);

        App::new_with_clients(base_path.clone(), base_path, None, database, clients)
            .await
            .expect("failed to build test app")
    }

    /// Builds a test app rooted at one temporary workspace with a mocked tmux
    /// boundary.
    async fn new_test_app() -> App {
        new_test_app_with_tmux_client(Arc::new(MockTmuxClient::new())).await
    }

    /// Builds a test app rooted at `working_dir` with one injected filesystem
    /// boundary.
    async fn app_with_fs_client(working_dir: PathBuf, fs_client: Arc<dyn FsClient>) -> App {
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = test_app_clients()
            .with_app_server_client_override(mock_app_server())
            .with_tmux_client(Arc::new(MockTmuxClient::new()))
            .with_fs_client(fs_client);

        App::new_with_clients(working_dir.clone(), working_dir, None, database, clients)
            .await
            .expect("failed to build test app")
    }

    #[tokio::test]
    async fn test_new_with_clients_fails_when_no_backend_cli_is_available() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = test_app_clients_with_available_agent_kinds(Vec::new())
            .with_app_server_client_override(mock_app_server())
            .with_tmux_client(Arc::new(MockTmuxClient::new()));

        // Act
        let result =
            App::new_with_clients(base_path.clone(), base_path, None, database, clients).await;

        // Assert
        assert!(matches!(
            result,
            Err(AppError::Workflow(message))
                if message
                    == "No supported backend CLI found on `PATH`. Install `codex`, `claude`, or `gemini` and restart `agentty`."
        ));
    }

    #[tokio::test]
    async fn session_git_status_targets_include_active_unpublished_sessions() {
        // Arrange
        let mut app = new_test_app().await;
        let review_session = test_session(PathBuf::from("/tmp/session-review"));
        let mut done_session = test_session(PathBuf::from("/tmp/session-done"));
        done_session.id = "session-2".to_string();
        done_session.status = Status::Done;
        app.sessions.sessions.push(review_session);
        app.sessions.sessions.push(done_session);

        // Act
        let targets = App::session_git_status_targets(&app.sessions);

        // Assert
        assert_eq!(
            targets,
            vec![task::SessionGitStatusTarget {
                base_branch: "main".to_string(),
                branch_name: "agentty/session-".to_string(),
                session_id: "session-1".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn test_switch_project_reloads_project_scoped_settings() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let second_project_dir = tempdir().expect("failed to create second temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project(&base_path.to_string_lossy(), None)
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project(&second_project_dir.path().to_string_lossy(), None)
            .await
            .expect("failed to insert second project");
        database
            .upsert_project_setting(
                first_project_id,
                SettingName::DefaultSmartModel,
                AgentModel::ClaudeHaiku4520251001.as_str(),
            )
            .await
            .expect("failed to persist first project smart model");
        database
            .upsert_project_setting(first_project_id, SettingName::OpenCommand, "npm run dev")
            .await
            .expect("failed to persist first project open command");
        database
            .upsert_project_setting(
                second_project_id,
                SettingName::DefaultSmartModel,
                AgentModel::Gpt54.as_str(),
            )
            .await
            .expect("failed to persist second project smart model");
        database
            .upsert_project_setting(second_project_id, SettingName::OpenCommand, "cargo test")
            .await
            .expect("failed to persist second project open command");
        database
            .set_active_project_id(first_project_id)
            .await
            .expect("failed to persist initial active project");
        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");

        // Act
        app.switch_project(second_project_id)
            .await
            .expect("failed to switch project");

        // Assert
        assert_eq!(app.settings.default_smart_model, AgentModel::Gpt54);
        assert_eq!(app.settings.open_command, "cargo test");
    }

    #[tokio::test]
    async fn test_switch_project_refreshes_tasks_tab_cache() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let second_project_dir = tempdir().expect("failed to create second temp dir");
        let roadmap_dir = second_project_dir.path().join("docs/plan");
        tokio::fs::create_dir_all(&roadmap_dir)
            .await
            .expect("failed to create roadmap dir");
        tokio::fs::write(roadmap_dir.join("roadmap.md"), "# roadmap")
            .await
            .expect("failed to create roadmap file");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project(&base_path.to_string_lossy(), None)
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project(&second_project_dir.path().to_string_lossy(), None)
            .await
            .expect("failed to insert second project");
        database
            .set_active_project_id(first_project_id)
            .await
            .expect("failed to persist initial active project");
        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");
        app.scroll_task_roadmap_down();
        app.scroll_task_roadmap_down();

        // Act
        let before_switch = app.active_project_has_tasks_tab();
        app.switch_project(second_project_id)
            .await
            .expect("failed to switch project");
        let after_switch = app.active_project_has_tasks_tab();
        let task_scroll_offset = app.task_roadmap_scroll_offset();

        // Assert
        assert!(!before_switch);
        assert!(after_switch);
        assert_eq!(task_scroll_offset, 0);
    }

    #[tokio::test]
    async fn test_switch_project_updates_active_git_upstream_reference() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let second_project_dir = tempdir().expect("failed to create second temp dir");
        let base_path = base_dir.path().to_path_buf();
        let second_project_path = second_project_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .upsert_project(&base_path.to_string_lossy(), None)
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .upsert_project(&second_project_path.to_string_lossy(), None)
            .await
            .expect("failed to insert second project");
        database
            .set_active_project_id(first_project_id)
            .await
            .expect("failed to persist initial active project");
        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("feature/footer-bar".to_string()) }));
        mock_git_client
            .expect_current_upstream_reference()
            .once()
            .returning(|_| Box::pin(async { Ok("origin/feature/footer-bar".to_string()) }));
        mock_git_client
            .expect_find_git_repo_root()
            .times(0..)
            .returning(|path| Box::pin(async move { Some(path) }));
        mock_git_client
            .expect_fetch_remote()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_branch_tracking_statuses()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.switch_project(second_project_id)
            .await
            .expect("failed to switch project");

        // Assert
        assert_eq!(app.git_branch(), Some("feature/footer-bar"));
        assert_eq!(app.git_upstream_ref(), Some("origin/feature/footer-bar"));
    }

    #[tokio::test]
    /// Ensures startup selection prefers active sessions over archive rows.
    async fn test_new_prefers_active_session_for_initial_selection() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&base_path.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        let active_session_id = "z-active-session";
        let archive_session_id = "a-archive-session";
        database
            .insert_session(
                active_session_id,
                "gemini-3-flash-preview",
                "main",
                &Status::Review.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert active session");
        database
            .insert_session(
                archive_session_id,
                "gemini-3-flash-preview",
                "main",
                &Status::Done.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert archived session");

        let active_folder_name = active_session_id.chars().take(8).collect::<String>();
        let active_session_data_dir = base_path.join(active_folder_name).join(SESSION_DATA_DIR);
        fs::create_dir_all(active_session_data_dir).expect("failed to create active session dir");

        // Act
        let app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");

        // Assert
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some(active_session_id)
        );
    }

    #[tokio::test]
    async fn test_new_returns_error_when_startup_project_upsert_fails() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        sqlx::query("DROP TABLE project")
            .execute(database.pool())
            .await
            .expect("failed to drop project table");

        // Act
        let error = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .err()
        .expect("expected startup project upsert failure");

        // Assert
        assert!(
            error
                .to_string()
                .contains("Failed to persist startup project")
        );
    }

    #[tokio::test]
    async fn test_new_returns_error_when_startup_active_project_persistence_fails() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        sqlx::query("DROP TABLE setting")
            .execute(database.pool())
            .await
            .expect("failed to drop setting table");

        // Act
        let error = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .err()
        .expect("expected startup active project persistence failure");

        // Assert
        assert!(
            error
                .to_string()
                .contains("Failed to store active startup project")
        );
    }

    #[tokio::test]
    async fn test_new_with_clients_falls_back_from_stale_active_project_and_loads_current_sessions()
    {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let agentty_home = temp_dir.path().join("agentty-home");
        let current_project_path = temp_dir.path().join("current-project");
        fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
        fs::create_dir_all(&current_project_path).expect("failed to create current project");
        let missing_project_path = temp_dir.path().join("missing-project");
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let current_project_id = database
            .upsert_project(&current_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to insert current project");
        let missing_project_id = database
            .upsert_project(&missing_project_path.to_string_lossy(), Some("missing"))
            .await
            .expect("failed to insert missing project");
        database
            .set_active_project_id(missing_project_id)
            .await
            .expect("failed to persist stale active project");
        let current_session_id = "session-current";
        let missing_session_id = "session-missing";
        database
            .insert_session(
                current_session_id,
                "gemini-3-flash-preview",
                "main",
                &Status::Review.to_string(),
                current_project_id,
            )
            .await
            .expect("failed to insert current project session");
        database
            .insert_session(
                missing_session_id,
                "gemini-3-flash-preview",
                "main",
                &Status::Review.to_string(),
                missing_project_id,
            )
            .await
            .expect("failed to insert stale project session");
        let current_session_folder =
            agentty_home.join(current_session_id.chars().take(8).collect::<String>());
        fs::create_dir_all(current_session_folder.join(SESSION_DATA_DIR))
            .expect("failed to create current session folder");

        // Act
        let app = App::new_with_clients(
            agentty_home.clone(),
            current_project_path.clone(),
            Some("main".to_string()),
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");

        // Assert
        assert_eq!(app.active_project_id(), current_project_id);
        assert_eq!(app.working_dir(), current_project_path.as_path());
        assert_eq!(app.git_branch(), Some("main"));
        assert_eq!(
            app.selected_session().map(|session| session.id.as_str()),
            Some(current_session_id)
        );
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(app.sessions.sessions[0].id, current_session_id);
        assert!(
            app.projects
                .project_items()
                .iter()
                .any(|item| item.project.id == current_project_id)
        );
        assert!(
            !app.projects
                .project_items()
                .iter()
                .any(|item| item.project.id == missing_project_id)
        );
    }

    /// Builds a test app with one selected session, configurable open command,
    /// and injected tmux boundary.
    async fn new_test_app_with_selected_session(
        session_folder: PathBuf,
        open_command: &str,
        tmux_client: Arc<dyn TmuxClient>,
    ) -> App {
        // Arrange
        let mut app = new_test_app_with_tmux_client(tmux_client).await;
        if !session_folder.as_os_str().is_empty() {
            std::fs::create_dir_all(&session_folder).expect("failed to create session folder");
        }

        // Act
        app.settings.open_command = open_command.to_string();
        app.sessions.sessions.push(test_session(session_folder));
        app.sessions.table_state.select(Some(0));

        // Assert
        app
    }

    #[test]
    fn branch_publish_popup_helpers_format_copy() {
        // Arrange
        let expected_restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(2),
            session_id: "session-1".to_string(),
        };

        // Act
        let loading_title = App::branch_publish_loading_title(PublishBranchAction::Push);
        let loading_message = App::branch_publish_loading_message(PublishBranchAction::Push, None);
        let custom_loading_message = App::branch_publish_loading_message(
            PublishBranchAction::Push,
            Some("review/custom-branch"),
        );
        let loading_label = App::branch_publish_loading_label(PublishBranchAction::Push);
        let success_title = App::branch_publish_success_title(PublishBranchAction::Push);
        let success_message = App::branch_publish_success_message(
            "agentty/session-1",
            Some(&crate::app::branch_publish::ReviewRequestCreationInfo {
                forge_kind: forge::ForgeKind::GitHub,
                web_url: Some(
                    "https://github.com/org/repo/compare/main...agentty%2Fsession-1?expand=1"
                        .to_string(),
                ),
            }),
        );
        let fallback_success_message =
            App::branch_publish_success_message("agentty/session-1", None);
        let pull_request_loading_title =
            App::branch_publish_loading_title(PublishBranchAction::PublishPullRequest);
        let pull_request_loading_message =
            App::branch_publish_loading_message(PublishBranchAction::PublishPullRequest, None);
        let pull_request_loading_label =
            App::branch_publish_loading_label(PublishBranchAction::PublishPullRequest);
        let pull_request_success_title =
            App::branch_publish_success_title(PublishBranchAction::PublishPullRequest);
        let popup_mode = App::view_info_popup_mode(
            "Working".to_string(),
            "Publishing branch".to_string(),
            true,
            "Pushing branch...".to_string(),
            expected_restore_view.clone(),
        );

        // Assert
        assert_eq!(loading_title, "Pushing branch");
        assert_eq!(
            loading_message,
            "Publishing the session branch to the configured Git remote."
        );
        assert_eq!(
            custom_loading_message,
            "Publishing the session branch to `review/custom-branch` on the configured Git remote."
        );
        assert_eq!(loading_label, "Pushing branch...");
        assert_eq!(success_title, "Branch pushed");
        assert!(success_message.contains("Pushed session branch `agentty/session-1`."));
        assert!(success_message.contains("Open this link to create the pull request"));
        assert!(
            success_message.contains(
                "https://github.com/org/repo/compare/main...agentty%2Fsession-1?expand=1"
            )
        );
        assert!(fallback_success_message.contains("Create the review request manually"));
        assert_eq!(pull_request_loading_title, "Publishing review request");
        assert_eq!(
            pull_request_loading_message,
            "Pushing the session branch and creating or refreshing the active forge review \
             request."
        );
        assert_eq!(pull_request_loading_label, "Publishing review request...");
        assert_eq!(pull_request_success_title, "Review request published");
        assert!(matches!(
            popup_mode,
            AppMode::ViewInfoPopup {
                is_loading: true,
                ref loading_label,
                ref message,
                ref restore_view,
                ref title,
            } if title == "Working"
                && message == "Publishing branch"
                && loading_label == "Pushing branch..."
                && restore_view == &expected_restore_view
        ));
    }

    /// Verifies generic and authentication-related branch-push failures map
    /// to the correct popup severity and current recovery guidance.
    #[test]
    fn branch_push_failure_maps_blocked_and_failed_errors() {
        // Arrange
        let auth_error =
            "Git push failed: fatal: could not read Username for 'https://github.com': terminal \
             prompts disabled";
        let failed_error = "remote rejected";

        // Act
        let blocked = branch_push_failure(PublishBranchAction::Push, auth_error);
        let failed = branch_push_failure(PublishBranchAction::Push, failed_error);

        // Assert
        assert_eq!(blocked.title, "Branch push blocked");
        assert!(blocked.message.contains("Git push requires authentication"));
        assert!(blocked.message.contains("gh auth login"));
        assert_eq!(failed.title, "Branch push failed");
        assert!(
            failed
                .message
                .contains("Failed to publish session branch: remote rejected")
        );
    }

    /// Verifies pushing a review session surfaces forge-specific git
    /// authentication guidance when the remote rejects credentials.
    #[tokio::test]
    async fn push_session_branch_auth_failure_shows_git_guidance() {
        // Arrange
        let branch_session = BranchPublishTaskSession::from_session(&test_session(PathBuf::from(
            "/tmp/review-session",
        )));
        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch()
            .once()
            .returning(|_| {
                Box::pin(async {
                    Err(crate::infra::git::GitError::OutputParse(
                        "Git push failed: fatal: could not read Username for \
                         'https://github.com': terminal prompts disabled"
                            .to_string(),
                    ))
                })
            });
        let git_client: Arc<dyn crate::infra::git::GitClient> = Arc::new(mock_git_client);
        let database = crate::infra::db::Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let result = push_session_branch(
            PublishBranchAction::Push,
            &branch_session,
            database,
            git_client,
            None,
        )
        .await;

        // Assert
        assert!(matches!(
            result,
            Err(BranchPublishTaskFailure {
                ref title,
                ref message,
            }) if title == "Branch push blocked"
                && message.contains("Git push requires authentication")
                && message.contains("gh auth login")
        ));
    }

    #[tokio::test]
    async fn branch_publish_task_helpers_reject_unsupported_session_states() {
        // Arrange
        let app = new_test_app().await;
        let mut review_session = test_session(PathBuf::from("/tmp/review-session"));
        review_session.status = Status::Done;
        let done_snapshot = BranchPublishTaskSession::from_session(&review_session);

        // Act
        let push_result = run_branch_publish_action(
            PublishBranchAction::Push,
            done_snapshot.clone(),
            app.services.db().clone(),
            app.services.clock(),
            app.services.git_client(),
            app.services.review_request_client(),
            None,
        )
        .await;
        let helper_result = push_session_branch(
            PublishBranchAction::Push,
            &done_snapshot,
            app.services.db().clone(),
            app.services.git_client(),
            None,
        )
        .await;

        // Assert
        assert_eq!(
            push_result,
            Err(BranchPublishTaskFailure::failed(
                PublishBranchAction::Push,
                "Session must be in review to push the branch.".to_string(),
            ))
        );
        assert_eq!(
            helper_result,
            Err(BranchPublishTaskFailure::failed(
                PublishBranchAction::Push,
                "Session must be in review to push the branch.".to_string(),
            ))
        );
    }

    #[tokio::test]
    async fn push_session_branch_uses_custom_remote_branch_name_when_provided() {
        // Arrange
        let branch_session = BranchPublishTaskSession::from_session(&test_session(PathBuf::from(
            "/tmp/review-session",
        )));
        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .with(
                mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
                mockall::predicate::eq("review/custom-branch".to_string()),
            )
            .once()
            .returning(|_, _| Box::pin(async { Ok("origin/review/custom-branch".to_string()) }));
        mock_git_client
            .expect_repo_url()
            .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
            .once()
            .returning(|_| {
                Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
            });
        let git_client: Arc<dyn crate::infra::git::GitClient> = Arc::new(mock_git_client);
        let database = crate::infra::db::Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let result = push_session_branch(
            PublishBranchAction::Push,
            &branch_session,
            database.clone(),
            git_client,
            Some("review/custom-branch"),
        )
        .await;

        // Assert
        assert_eq!(
            result,
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: "review/custom-branch".to_string(),
                review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                    forge_kind: forge::ForgeKind::GitHub,
                    web_url: Some(
                        "https://github.com/agentty-xyz/agentty/compare/main...review%2Fcustom-branch?expand=1"
                            .to_string()
                    ),
                }),
                upstream_reference: "origin/review/custom-branch".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn push_session_branch_succeeds_without_review_request_link_for_unsupported_remote() {
        // Arrange
        let branch_session = BranchPublishTaskSession::from_session(&test_session(PathBuf::from(
            "/tmp/review-session",
        )));
        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_push_current_branch()
            .once()
            .returning(|_| Box::pin(async { Ok("origin/agentty/session-1".to_string()) }));
        mock_git_client
            .expect_repo_url()
            .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
            .once()
            .returning(|_| {
                Box::pin(async { Ok("https://example.com/team/project.git".to_string()) })
            });
        let git_client: Arc<dyn crate::infra::git::GitClient> = Arc::new(mock_git_client);
        let database = crate::infra::db::Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let result = push_session_branch(
            PublishBranchAction::Push,
            &branch_session,
            database,
            git_client,
            None,
        )
        .await;

        // Assert
        assert_eq!(
            result,
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: session::session_branch("session-1"),
                review_request_creation: None,
                upstream_reference: "origin/agentty/session-1".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn apply_branch_publish_action_update_sets_success_popup() {
        // Arrange
        let session_folder = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_selected_session(
            session_folder.path().to_path_buf(),
            "",
            Arc::new(MockTmuxClient::new()),
        )
        .await;
        let expected_restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(1),
            session_id: "session-1".to_string(),
        };

        // Act
        app.apply_branch_publish_action_update(BranchPublishActionUpdate {
            restore_view: expected_restore_view.clone(),
            result: Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: "agentty/session-1".to_string(),
                review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                    forge_kind: forge::ForgeKind::GitHub,
                    web_url: Some(
                        "https://github.com/agentty-xyz/agentty/compare/main...agentty%2Fsession-1?expand=1"
                            .to_string()
                    ),
                }),
                upstream_reference: "origin/agentty/session-1".to_string(),
            }),
            session_id: "session-1".to_string(),
        });

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ViewInfoPopup {
                is_loading: false,
                ref message,
                ref restore_view,
                ref title,
                ..
            } if title == "Branch pushed"
                && message.contains("Pushed session branch `agentty/session-1`.")
                && message.contains("https://github.com/agentty-xyz/agentty/compare/main...agentty%2Fsession-1?expand=1")
                && restore_view == &expected_restore_view
        ));
        assert_eq!(
            app.sessions
                .state()
                .sessions
                .first()
                .and_then(|session| session.published_upstream_ref.as_deref()),
            Some("origin/agentty/session-1")
        );
    }

    #[tokio::test]
    async fn apply_branch_publish_action_update_sets_pull_request_success_popup() {
        // Arrange
        let session_folder = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_selected_session(
            session_folder.path().to_path_buf(),
            "",
            Arc::new(MockTmuxClient::new()),
        )
        .await;
        let expected_restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(1),
            session_id: "session-1".to_string(),
        };
        let review_request = crate::domain::session::ReviewRequest {
            last_refreshed_at: 55,
            summary: crate::domain::session::ReviewRequestSummary {
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                ..test_review_request_summary("#42", ReviewRequestState::Open)
            },
        };

        // Act
        app.apply_branch_publish_action_update(BranchPublishActionUpdate {
            restore_view: expected_restore_view.clone(),
            result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
                branch_name: "agentty/session-1".to_string(),
                review_request: review_request.clone(),
                upstream_reference: "origin/agentty/session-1".to_string(),
            }),
            session_id: "session-1".to_string(),
        });

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ViewInfoPopup {
                is_loading: false,
                ref message,
                ref restore_view,
                ref title,
                ..
            } if title == "GitHub pull request published"
                && message.contains("Published session branch `agentty/session-1`.")
                && message.contains("GitHub pull request #42 is ready")
                && message.contains("https://github.com/agentty-xyz/agentty/pull/42")
                && restore_view == &expected_restore_view
        ));
        assert_eq!(
            app.sessions
                .state()
                .sessions
                .first()
                .and_then(|session| session.review_request.clone()),
            Some(review_request)
        );
    }

    #[tokio::test]
    async fn apply_branch_publish_action_update_sets_gitlab_merge_request_success_popup() {
        // Arrange
        let session_folder = tempdir().expect("failed to create temp dir");
        let mut app = new_test_app_with_selected_session(
            session_folder.path().to_path_buf(),
            "",
            Arc::new(MockTmuxClient::new()),
        )
        .await;
        let expected_restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: Some(2),
            session_id: "session-1".to_string(),
        };
        let review_request = crate::domain::session::ReviewRequest {
            last_refreshed_at: 77,
            summary: crate::domain::session::ReviewRequestSummary {
                display_id: "!24".to_string(),
                forge_kind: ForgeKind::GitLab,
                source_branch: "agentty/session-1".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Draft".to_string()),
                target_branch: "main".to_string(),
                title: "Add GitLab support".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
            },
        };

        // Act
        app.apply_branch_publish_action_update(BranchPublishActionUpdate {
            restore_view: expected_restore_view.clone(),
            result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
                branch_name: "agentty/session-1".to_string(),
                review_request,
                upstream_reference: "origin/agentty/session-1".to_string(),
            }),
            session_id: "session-1".to_string(),
        });

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ViewInfoPopup {
                is_loading: false,
                ref message,
                ref restore_view,
                ref title,
                ..
            } if title == "GitLab merge request published"
                && message.contains("Published session branch `agentty/session-1`.")
                && message.contains("GitLab merge request !24 is ready")
                && message.contains("https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24")
                && restore_view == &expected_restore_view
        ));
    }

    #[tokio::test]
    async fn stats_for_session_returns_duration_and_usage_rows() {
        // Arrange
        let app = new_test_app().await;
        let session_id = "session-stats";
        let project_id = app
            .services
            .db()
            .upsert_project("/tmp/stats-project", Some("main"))
            .await
            .expect("failed to upsert project");
        app.services
            .db()
            .insert_session(
                session_id,
                "gemini-2.5-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let usage = SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            input_tokens: 1_200,
            output_tokens: 650,
        };
        app.services
            .db()
            .upsert_session_usage(session_id, "gemini-2.5-flash", &usage)
            .await
            .expect("failed to upsert usage");

        // Act
        let stats = app.stats_for_session(session_id).await;

        // Assert
        assert_eq!(stats.session_duration_seconds, Some(0));
        assert_eq!(
            stats.usage_rows_result,
            Ok(vec![SessionStatsUsage {
                input_tokens: 1_200,
                model: "gemini-2.5-flash".to_string(),
                output_tokens: 650,
            }])
        );
    }

    #[tokio::test]
    async fn stats_for_session_returns_none_duration_for_unknown_session() {
        // Arrange
        let app = new_test_app().await;

        // Act
        let stats = app.stats_for_session("missing-session").await;

        // Assert
        assert_eq!(stats.session_duration_seconds, None);
        assert_eq!(stats.usage_rows_result, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn configured_open_commands_returns_trimmed_non_empty_entries() {
        // Arrange
        let mut app = new_test_app().await;
        app.settings.open_command = "  cargo test \n npm run dev \n".to_string();

        // Act
        let open_commands = app.configured_open_commands();

        // Assert
        assert_eq!(
            open_commands,
            vec!["cargo test".to_string(), "npm run dev".to_string()]
        );
    }

    #[tokio::test]
    async fn open_session_worktree_in_tmux_runs_configured_open_command_when_window_opens() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session-open-command");
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .with(eq(session_folder))
            .times(1)
            .returning(|_| Box::pin(async { Some("@42".to_string()) }));
        mock_tmux_client
            .expect_run_command_in_window()
            .with(eq("@42".to_string()), eq("npm run dev".to_string()))
            .times(1)
            .returning(|_, _| Box::pin(async {}));
        let app = new_test_app_with_selected_session(
            PathBuf::from("/tmp/session-open-command"),
            "  npm run dev  ",
            Arc::new(mock_tmux_client),
        )
        .await;

        // Act
        app.open_session_worktree_in_tmux().await;

        // Assert
        // Expectations are validated by `mockall`.
    }

    #[tokio::test]
    async fn open_session_worktree_in_tmux_skips_open_command_when_setting_is_blank() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session-empty-open-command");
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .with(eq(session_folder))
            .times(1)
            .returning(|_| Box::pin(async { Some("@42".to_string()) }));
        mock_tmux_client.expect_run_command_in_window().times(0);
        let app = new_test_app_with_selected_session(
            PathBuf::from("/tmp/session-empty-open-command"),
            "   ",
            Arc::new(mock_tmux_client),
        )
        .await;

        // Act
        app.open_session_worktree_in_tmux().await;

        // Assert
        // Expectations are validated by `mockall`.
    }

    #[tokio::test]
    async fn open_session_worktree_in_tmux_skips_missing_worktree_folder() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let missing_session_folder = temp_dir.path().join("missing-session-worktree");
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client.expect_open_window_for_folder().times(0);
        mock_tmux_client.expect_run_command_in_window().times(0);
        let mut app = new_test_app_with_tmux_client(Arc::new(mock_tmux_client)).await;
        app.settings.open_command = "npm run dev".to_string();
        app.sessions
            .sessions
            .push(test_session(missing_session_folder));
        app.sessions.table_state.select(Some(0));

        // Act
        app.open_session_worktree_in_tmux().await;

        // Assert
        // Expectations are validated by `mockall`.
    }

    #[tokio::test]
    async fn open_session_worktree_in_tmux_uses_first_configured_command() {
        // Arrange
        let session_folder = PathBuf::from("/tmp/session-multiple-open-commands");
        let mut mock_tmux_client = MockTmuxClient::new();
        mock_tmux_client
            .expect_open_window_for_folder()
            .with(eq(session_folder))
            .times(1)
            .returning(|_| Box::pin(async { Some("@42".to_string()) }));
        mock_tmux_client
            .expect_run_command_in_window()
            .with(eq("@42".to_string()), eq("cargo test".to_string()))
            .times(1)
            .returning(|_, _| Box::pin(async {}));
        let app = new_test_app_with_selected_session(
            PathBuf::from("/tmp/session-multiple-open-commands"),
            " cargo test \n npm run dev ",
            Arc::new(mock_tmux_client),
        )
        .await;

        // Act
        app.open_session_worktree_in_tmux().await;

        // Assert
        // Expectations are validated by `mockall`.
    }

    #[test]
    fn sync_main_popup_mode_success_message_tracks_project_and_branch() {
        // Arrange
        let sync_popup_context = SyncPopupContext {
            default_branch: "develop".to_string(),
            project_name: "agentty".to_string(),
        };
        let sync_main_outcome = SyncMainOutcome {
            pulled_commit_titles: vec![
                "Add audit log indexing".to_string(),
                "Fix merge conflict prompt wording".to_string(),
            ],
            pulled_commits: Some(2),
            pushed_commit_titles: vec!["Polish sync popup alignment".to_string()],
            pushed_commits: Some(1),
            resolved_conflict_files: vec!["src/lib.rs".to_string()],
        };

        // Act
        let mode = App::sync_main_popup_mode(Ok(sync_main_outcome), &sync_popup_context);
        let expected_message = concat!(
            "Successfully synchronized with its upstream.\n",
            "\n",
            "## 1. 2 commits pulled\n",
            "  - Add audit log indexing\n",
            "  - Fix merge conflict prompt wording\n",
            "\n",
            "## 2. 1 commit pushed\n",
            "  - Polish sync popup alignment\n",
            "\n",
            "## 3. conflicts fixed: src/lib.rs"
        );

        // Assert
        assert!(matches!(mode, AppMode::SyncBlockedPopup { .. }));
        if let AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } = mode
        {
            assert_eq!(title, "Sync complete");
            assert_eq!(default_branch.as_deref(), Some("develop"));
            assert!(!is_loading);
            assert_eq!(message, expected_message);
            assert_eq!(project_name.as_deref(), Some("agentty"));
        }
    }

    #[test]
    fn sync_main_popup_mode_blocked_message_tracks_project_and_branch() {
        // Arrange
        let sync_popup_context = SyncPopupContext {
            default_branch: "develop".to_string(),
            project_name: "agentty".to_string(),
        };

        // Act
        let mode = App::sync_main_popup_mode(
            Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: "develop".to_string(),
            }),
            &sync_popup_context,
        );

        // Assert
        assert!(matches!(
            mode,
            AppMode::SyncBlockedPopup {
                ref default_branch,
                is_loading: false,
                ref title,
                ref message,
                ref project_name,
            } if title == "Sync blocked"
                && default_branch.as_deref() == Some("develop")
                && message.contains("uncommitted changes")
                && project_name.as_deref() == Some("agentty")
        ));
    }

    #[test]
    fn sync_main_popup_mode_auth_failure_shows_authorization_guidance() {
        // Arrange
        let sync_popup_context = SyncPopupContext {
            default_branch: "main".to_string(),
            project_name: "agentty".to_string(),
        };
        let sync_error = SyncSessionStartError::Other(
            "Git push failed: fatal: could not read Username for 'https://github.com': terminal \
             prompts disabled"
                .to_string(),
        );

        // Act
        let mode = App::sync_main_popup_mode(Err(sync_error), &sync_popup_context);

        // Assert
        assert!(matches!(
            mode,
            AppMode::SyncBlockedPopup {
                ref default_branch,
                is_loading: false,
                ref title,
                ref message,
                ref project_name,
            } if title == "Sync failed"
                && default_branch.as_deref() == Some("main")
                && message.contains("Git push requires authentication")
                && message.contains("`gh auth login`")
                && message.contains("then run sync again")
                && project_name.as_deref() == Some("agentty")
        ));
    }

    #[test]
    fn sync_push_auth_error_detects_github_from_prompt_url() {
        // Arrange
        let detail = "Git push failed: fatal: could not read Password for 'https://github.com/team/project': terminal \
                        prompts disabled\nConfigured remotes:\n  github.com";

        // Act
        let forge_kind = detected_forge_kind_from_git_push_error(detail);

        // Assert
        assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
    }

    #[test]
    fn sync_push_auth_error_prefers_github_when_fallback_markers_are_ambiguous() {
        // Arrange
        let detail = "Git push failed: authentication failed. Configure remotes:\n  github.com";

        // Act
        let forge_kind = detected_forge_kind_from_git_push_error(detail);

        // Assert
        assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
    }

    #[test]
    fn app_event_batch_collect_event_keeps_latest_at_mention_entries_update() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        let first_entries = vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }];
        let second_entries = vec![FileEntry {
            is_dir: true,
            path: "crates".to_string(),
        }];

        // Act
        event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
            entries: first_entries,
            session_id: "session-1".to_string(),
        });
        event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
            entries: second_entries.clone(),
            session_id: "session-1".to_string(),
        });

        // Assert
        assert_eq!(
            event_batch
                .at_mention_entries_updates
                .get("session-1")
                .cloned(),
            Some(second_entries)
        );
    }

    #[test]
    /// Verifies repeated `AgentResponseReceived` events keep the newest
    /// reducer projection while accumulating token usage for the session.
    fn app_event_batch_collect_event_merges_agent_response_token_usage() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        let latest_turn = test_turn_applied_state(
            vec![
                QuestionItem::new("Need branch?"),
                QuestionItem::new("Need tests?"),
            ],
            vec!["Document the batched reducer path."],
            Some(AgentResponseSummary {
                session: "Session summary".to_string(),
                turn: "Latest turn summary".to_string(),
            }),
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                input_tokens: 7,
                output_tokens: 11,
            },
        );

        // Act
        event_batch.collect_event(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: test_turn_applied_state(
                vec![QuestionItem::new("Old question")],
                vec!["Old follow-up task"],
                Some(AgentResponseSummary {
                    session: "Old session summary".to_string(),
                    turn: "Old turn summary".to_string(),
                }),
                SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 3,
                    output_tokens: 5,
                },
            ),
        });
        event_batch.collect_event(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: latest_turn.clone(),
        });

        // Assert
        let merged_turn = event_batch.applied_turns.get("session-1");
        assert_eq!(
            merged_turn.map(|turn| turn.questions.clone()),
            Some(latest_turn.questions.clone())
        );
        assert_eq!(
            merged_turn.map(|turn| {
                turn.follow_up_tasks
                    .iter()
                    .map(|task| task.text.clone())
                    .collect::<Vec<_>>()
            }),
            Some(vec!["Document the batched reducer path.".to_string()])
        );
        assert_eq!(
            merged_turn.and_then(|turn| turn.summary.as_deref()),
            latest_turn.summary.as_deref()
        );
        assert_eq!(
            merged_turn.map(|turn| turn.token_usage_delta.input_tokens),
            Some(10)
        );
        assert_eq!(
            merged_turn.map(|turn| turn.token_usage_delta.output_tokens),
            Some(16)
        );
    }

    #[test]
    /// Verifies that `UpdateStatusChanged` events update the event batch so
    /// the reducer can apply the latest update progress state.
    fn app_event_batch_collect_event_stores_update_status() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: "v1.0.0".to_string(),
            },
        });
        event_batch.collect_event(AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::Complete {
                version: "v1.0.0".to_string(),
            },
        });

        // Assert
        assert_eq!(
            event_batch.update_status,
            Some(UpdateStatus::Complete {
                version: "v1.0.0".to_string()
            })
        );
    }

    #[test]
    fn app_event_batch_collect_event_keeps_latest_same_session_updates() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::SessionModelUpdated {
            session_id: "session-a".to_string(),
            session_model: AgentModel::Gemini3FlashPreview,
        });
        event_batch.collect_event(AppEvent::SessionModelUpdated {
            session_id: "session-a".to_string(),
            session_model: AgentModel::Gemini31ProPreview,
        });
        event_batch.collect_event(AppEvent::SessionProgressUpdated {
            progress_message: Some("first".to_string()),
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionProgressUpdated {
            progress_message: Some("second".to_string()),
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionSizeUpdated {
            added_lines: 1,
            deleted_lines: 2,
            session_id: "session-a".to_string(),
            session_size: SessionSize::S,
        });
        event_batch.collect_event(AppEvent::SessionSizeUpdated {
            added_lines: 8,
            deleted_lines: 13,
            session_id: "session-a".to_string(),
            session_size: SessionSize::L,
        });
        event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
            generation: 1,
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
            generation: 2,
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionUpdated {
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionUpdated {
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::AgentResponseReceived {
            session_id: "session-a".to_string(),
            turn_applied_state: test_turn_applied_state(
                vec![QuestionItem::new("first question")],
                Vec::new(),
                None,
                SessionStats::default(),
            ),
        });
        event_batch.collect_event(AppEvent::AgentResponseReceived {
            session_id: "session-a".to_string(),
            turn_applied_state: test_turn_applied_state(
                vec![QuestionItem::new("second question")],
                Vec::new(),
                None,
                SessionStats::default(),
            ),
        });

        // Assert
        assert_eq!(
            event_batch.session_model_updates.get("session-a"),
            Some(&AgentModel::Gemini31ProPreview)
        );
        assert_eq!(
            event_batch.session_progress_updates.get("session-a"),
            Some(&Some("second".to_string()))
        );
        assert_eq!(
            event_batch.session_size_updates.get("session-a"),
            Some(&(8, 13, SessionSize::L))
        );
        assert_eq!(
            event_batch
                .session_title_generation_finished
                .get("session-a"),
            Some(&2)
        );
        assert_eq!(event_batch.session_ids.len(), 1);
        assert_eq!(
            event_batch
                .applied_turns
                .get("session-a")
                .map(|turn_applied_state| turn_applied_state.questions.clone()),
            Some(vec![QuestionItem::new("second question")])
        );
    }

    #[test]
    fn app_event_batch_collect_event_uses_final_wins_for_review_and_branch_publish() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::ReviewPrepared {
            diff_hash: 11,
            review_text: "first review".to_string(),
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::ReviewPreparationFailed {
            diff_hash: 12,
            error: "latest failure".to_string(),
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::ReviewPrepared {
            diff_hash: 21,
            review_text: "stable review".to_string(),
            session_id: "session-b".to_string(),
        });
        event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
            restore_view: test_confirmation_view_mode("session-a"),
            result: Box::new(test_pushed_branch_result("feature/first")),
            session_id: "session-a".to_string(),
        });
        event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
            restore_view: test_confirmation_view_mode("session-b"),
            result: Box::new(test_pushed_branch_result("feature/final")),
            session_id: "session-b".to_string(),
        });

        // Assert
        assert_eq!(
            event_batch.review_updates.get("session-a"),
            Some(&ReviewUpdate {
                diff_hash: 12,
                result: Err("latest failure".to_string()),
            })
        );
        assert_eq!(
            event_batch.review_updates.get("session-b"),
            Some(&ReviewUpdate {
                diff_hash: 21,
                result: Ok("stable review".to_string()),
            })
        );
        assert_eq!(
            event_batch.branch_publish_action_update,
            Some(BranchPublishActionUpdate {
                restore_view: test_confirmation_view_mode("session-b"),
                result: test_pushed_branch_result("feature/final"),
                session_id: "session-b".to_string(),
            })
        );
        assert!(event_batch.should_refresh_git_status);
    }

    #[test]
    /// Verifies successful sync completion requests an immediate git-status
    /// refresh in the reducer batch.
    fn app_event_batch_collect_event_marks_successful_sync_for_git_status_refresh() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::SyncMainCompleted {
            result: Ok(SyncMainOutcome {
                pulled_commit_titles: vec!["Upstream fix".to_string()],
                pulled_commits: Some(1),
                pushed_commit_titles: vec!["Local tweak".to_string()],
                pushed_commits: Some(2),
                resolved_conflict_files: Vec::new(),
            }),
        });

        // Assert
        assert!(event_batch.should_refresh_git_status);
        assert!(matches!(
            event_batch.sync_main_result,
            Some(Ok(SyncMainOutcome {
                pulled_commits: Some(1),
                pushed_commits: Some(2),
                ..
            }))
        ));
    }

    #[tokio::test]
    /// Verifies that the reducer applies `UpdateStatusChanged` events to
    /// `App.update_status`.
    async fn apply_app_events_update_status_changed_updates_app_state() {
        // Arrange
        let mut app = new_test_app().await;
        assert!(app.update_status().is_none());

        // Act
        app.apply_app_events(AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: "v2.0.0".to_string(),
            },
        })
        .await;

        // Assert
        assert_eq!(
            app.update_status().cloned(),
            Some(UpdateStatus::InProgress {
                version: "v2.0.0".to_string()
            })
        );
    }

    #[tokio::test]
    /// Verifies that one combined git-status event updates the in-memory
    /// session snapshot cache.
    async fn apply_app_events_git_status_updated_updates_project_and_session_state() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-git-status")));

        // Act
        app.apply_app_events(AppEvent::GitStatusUpdated {
            session_statuses: HashMap::from([(
                "session-1".to_string(),
                SessionGitStatus {
                    base_status: Some((4, 2)),
                    remote_status: Some((1, 0)),
                },
            )]),
            status: Some((1, 3)),
        })
        .await;

        // Assert
        assert_eq!(app.git_status_info(), Some((1, 3)));
        assert_eq!(
            app.sessions.session_git_statuses().get("session-1"),
            Some(&SessionGitStatus {
                base_status: Some((4, 2)),
                remote_status: Some((1, 0)),
            })
        );
    }

    #[tokio::test]
    /// Verifies explicit git-status refresh events restart polling
    /// immediately instead of waiting for the periodic cadence.
    async fn apply_app_events_refresh_git_status_restarts_task() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = test_app_clients()
            .with_app_server_client_override(mock_app_server())
            .with_tmux_client(Arc::new(MockTmuxClient::new()));
        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path.clone(),
            Some("main".to_string()),
            database,
            clients,
        )
        .await
        .expect("failed to build test app");
        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(|dir| Box::pin(async move { Some(dir) }));
        mock_git_client
            .expect_fetch_remote()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_branch_tracking_statuses()
            .times(1)
            .returning(|_| {
                Box::pin(async { Ok(HashMap::from([("main".to_string(), Some((2_u32, 1_u32)))])) })
            });
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.apply_app_events(AppEvent::RefreshGitStatus).await;
        let next_event = tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
            .await
            .expect("git status refresh event should arrive");

        // Assert
        assert_eq!(
            next_event,
            Some(AppEvent::GitStatusUpdated {
                session_statuses: HashMap::new(),
                status: Some((2, 1)),
            })
        );
    }

    #[tokio::test]
    async fn apply_app_events_agent_response_switches_view_mode_to_question_mode() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-question-view")));
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: "session-1".to_string(),
            scroll_offset: None,
        };
        let expected_questions = vec![
            QuestionItem::with_options(
                "Need a target branch?",
                vec!["main".to_string(), "develop".to_string()],
            ),
            QuestionItem::with_options(
                "Need integration tests?",
                vec!["Yes".to_string(), "No".to_string()],
            ),
        ];
        let turn_applied_state = test_turn_applied_state(
            vec![
                QuestionItem::with_options(
                    "Need a target branch?",
                    vec!["main".to_string(), "develop".to_string()],
                ),
                QuestionItem::with_options(
                    "Need integration tests?",
                    vec!["Yes".to_string(), "No".to_string()],
                ),
            ],
            Vec::new(),
            None,
            SessionStats::default(),
        );

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state,
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                ref questions,
                ref responses,
                current_index: 0,
                ref input,
                selected_option_index: Some(0),
                ..
            } if session_id == "session-1"
                && questions == &expected_questions
                && responses.is_empty()
                && input.text().is_empty()
        ));
    }

    #[tokio::test]
    async fn apply_app_events_agent_response_keeps_list_mode_when_not_viewing_session() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::List;

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: test_turn_applied_state(
                vec![QuestionItem::new("Need context?")],
                Vec::new(),
                None,
                SessionStats::default(),
            ),
        })
        .await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    /// Verifies reducer-applied turn projections update the cached session
    /// summary immediately.
    async fn apply_app_events_agent_response_updates_session_summary() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-summary-view")));
        let expected_summary = serde_json::to_string(&AgentResponseSummary {
            turn: "- Added structured protocol summary fields.".to_string(),
            session: "- Session output now renders persisted summary separately.".to_string(),
        })
        .expect("summary should serialize");

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: test_turn_applied_state(
                Vec::new(),
                Vec::new(),
                Some(AgentResponseSummary {
                    turn: "- Added structured protocol summary fields.".to_string(),
                    session: "- Session output now renders persisted summary separately."
                        .to_string(),
                }),
                SessionStats::default(),
            ),
        })
        .await;

        // Assert
        assert_eq!(
            app.sessions.sessions[0].summary.as_deref(),
            Some(expected_summary.as_str())
        );
    }

    #[tokio::test]
    /// Verifies agent responses update cached follow-up tasks immediately for
    /// the active session.
    async fn apply_app_events_agent_response_updates_session_follow_up_tasks() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-follow-up-view")));

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: test_turn_applied_state(
                Vec::new(),
                vec![
                    "Document the new shortcut.",
                    "Add a focused regression test.",
                ],
                None,
                SessionStats::default(),
            ),
        })
        .await;

        // Assert
        assert_eq!(
            app.sessions.sessions[0]
                .follow_up_tasks
                .iter()
                .map(|task| task.text.clone())
                .collect::<Vec<_>>(),
            vec![
                "Document the new shortcut.".to_string(),
                "Add a focused regression test.".to_string()
            ]
        );
    }

    #[tokio::test]
    /// Verifies stale published-branch sync completions do not overwrite the
    /// latest in-progress auto-push state.
    async fn apply_app_events_ignores_stale_published_branch_sync_updates() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-branch-sync-view")));

        // Act
        app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
            session_id: "session-1".to_string(),
            sync_operation_id: "sync-1".to_string(),
            sync_status: PublishedBranchSyncStatus::InProgress,
        })
        .await;
        app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
            session_id: "session-1".to_string(),
            sync_operation_id: "sync-2".to_string(),
            sync_status: PublishedBranchSyncStatus::InProgress,
        })
        .await;
        app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
            session_id: "session-1".to_string(),
            sync_operation_id: "sync-1".to_string(),
            sync_status: PublishedBranchSyncStatus::Failed,
        })
        .await;

        // Assert
        assert_eq!(
            app.sessions.sessions[0].published_branch_sync_status,
            PublishedBranchSyncStatus::InProgress
        );
    }

    #[tokio::test]
    /// Verifies one reducer tick preserves a completed auto-push message even
    /// when start and success updates are drained together.
    async fn apply_app_events_preserves_completed_published_branch_sync_updates() {
        // Arrange
        let mut app = new_test_app().await;
        let event_sender = app.services.event_sender();
        app.sessions.sessions.push(test_session(PathBuf::from(
            "/tmp/session-branch-sync-success",
        )));

        event_sender
            .send(AppEvent::PublishedBranchSyncUpdated {
                session_id: "session-1".to_string(),
                sync_operation_id: "sync-1".to_string(),
                sync_status: PublishedBranchSyncStatus::Succeeded,
            })
            .expect("queued event should send");

        // Act
        app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
            session_id: "session-1".to_string(),
            sync_operation_id: "sync-1".to_string(),
            sync_status: PublishedBranchSyncStatus::InProgress,
        })
        .await;

        // Assert
        assert_eq!(
            app.sessions.sessions[0].published_branch_sync_status,
            PublishedBranchSyncStatus::Succeeded
        );
    }

    #[tokio::test]
    /// Verifies reducer-applied turn projections clear stale questions and add
    /// token deltas to cached session stats.
    async fn apply_app_events_agent_response_updates_questions_and_token_usage() {
        // Arrange
        let mut app = new_test_app().await;
        let mut session = test_session(PathBuf::from("/tmp/session-stats-view"));
        session.questions = vec![QuestionItem::new("Old question?")];
        session.stats.input_tokens = 5;
        session.stats.output_tokens = 8;
        app.sessions.sessions.push(session);

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: test_turn_applied_state(
                Vec::new(),
                Vec::new(),
                None,
                SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 13,
                    output_tokens: 21,
                },
            ),
        })
        .await;

        // Assert
        assert!(app.sessions.sessions[0].questions.is_empty());
        assert_eq!(app.sessions.sessions[0].stats.input_tokens, 18);
        assert_eq!(app.sessions.sessions[0].stats.output_tokens, 29);
    }

    #[tokio::test]
    /// Verifies agent-response events still trigger auto review when the
    /// handle has already advanced to `Review` but the paired
    /// `SessionUpdated` event has not been reduced yet.
    async fn apply_app_events_agent_response_starts_auto_review_from_synced_handle_status() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
        let expected_hash = diff_content_hash(diff_text);

        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-auto-review-sync")));
        app.sessions.sessions[0].status = Status::InProgress;
        app.sessions.handles.insert(
            session_id.to_string(),
            SessionHandles::new(String::new(), Status::InProgress),
        );
        *app.sessions
            .handles
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock") = Status::Review;

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: session_id.to_string(),
            turn_applied_state: test_turn_applied_state(
                Vec::new(),
                Vec::new(),
                None,
                SessionStats::default(),
            ),
        })
        .await;

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { diff_hash }) if *diff_hash == expected_hash
        ));
        assert_eq!(app.sessions.sessions[0].status, Status::AgentReview);
        assert_eq!(
            *app.sessions
                .handles
                .get(session_id)
                .expect("expected session handles")
                .status
                .lock()
                .expect("expected handle status lock"),
            Status::AgentReview
        );
    }

    #[tokio::test]
    /// Verifies auto review still triggers when the render-loop
    /// `sync_from_handles()` has already synced the session snapshot to
    /// `Review` before the reducer processes the `AgentResponseReceived`
    /// event. This is the primary race condition that caused unreliable
    /// auto-review triggering.
    async fn apply_app_events_agent_response_starts_auto_review_when_snapshot_already_review() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
        let expected_hash = diff_content_hash(diff_text);

        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-already-review")));
        // Simulate sync_from_handles() having already updated the snapshot
        // to `Review` in a prior render tick.
        app.sessions.sessions[0].status = Status::Review;
        app.sessions.handles.insert(
            session_id.to_string(),
            SessionHandles::new(String::new(), Status::Review),
        );

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: session_id.to_string(),
            turn_applied_state: test_turn_applied_state(
                Vec::new(),
                Vec::new(),
                None,
                SessionStats::default(),
            ),
        })
        .await;

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { diff_hash }) if *diff_hash == expected_hash
        ));
        assert_eq!(app.sessions.sessions[0].status, Status::AgentReview);
    }

    #[tokio::test]
    /// Verifies one reducer tick preserves the latest turn projection while
    /// accumulating token usage from multiple queued completions.
    async fn apply_app_events_agent_response_batches_same_session_turns() {
        // Arrange
        let mut app = new_test_app().await;
        let event_sender = app.services.event_sender();
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-batched-turns")));

        let first_turn = test_turn_applied_state(
            vec![QuestionItem::new("First question?")],
            Vec::new(),
            None,
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                input_tokens: 2,
                output_tokens: 3,
            },
        );
        let second_turn = test_turn_applied_state(
            vec![QuestionItem::new("Latest question?")],
            vec!["Capture reducer batching coverage."],
            None,
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                input_tokens: 5,
                output_tokens: 8,
            },
        );

        event_sender
            .send(AppEvent::AgentResponseReceived {
                session_id: "session-1".to_string(),
                turn_applied_state: second_turn,
            })
            .expect("queued event should send");

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            session_id: "session-1".to_string(),
            turn_applied_state: first_turn,
        })
        .await;

        // Assert
        assert_eq!(
            app.sessions.sessions[0].questions,
            vec![QuestionItem::new("Latest question?")]
        );
        assert_eq!(app.sessions.sessions[0].stats.input_tokens, 7);
        assert_eq!(app.sessions.sessions[0].stats.output_tokens, 11);
        assert_eq!(
            app.sessions.sessions[0]
                .follow_up_tasks
                .iter()
                .map(|task| task.text.clone())
                .collect::<Vec<_>>(),
            vec!["Capture reducer batching coverage.".to_string()]
        );
    }

    #[tokio::test]
    /// Verifies launching an already-linked follow-up task opens its sibling
    /// session instead of creating another session.
    async fn launch_or_open_selected_follow_up_task_opens_existing_sibling_session() {
        // Arrange
        let mut app = new_test_app().await;
        let mut source_session = test_session(PathBuf::from("/tmp/source-session"));
        source_session.follow_up_tasks = vec![SessionFollowUpTask {
            id: 1,
            launched_session_id: Some("session-2".to_string()),
            position: 0,
            text: "Open the sibling session.".to_string(),
        }];
        let mut sibling_session = test_session(PathBuf::from("/tmp/sibling-session"));
        sibling_session.id = "session-2".to_string();
        sibling_session.title = Some("Sibling session".to_string());
        app.sessions.sessions.push(source_session);
        app.sessions.sessions.push(sibling_session);

        // Act
        app.launch_or_open_selected_follow_up_task("session-1")
            .await
            .expect("follow-up task should open the linked sibling session");

        // Assert
        assert_eq!(app.sessions.table_state.selected(), Some(1));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                ..
            } if session_id == "session-2"
        ));
    }

    #[tokio::test]
    /// Verifies a stale launched-session link is cleared before replacement
    /// session creation starts, so a failed launch does not keep retrying the
    /// same orphaned sibling id.
    async fn launch_or_open_selected_follow_up_task_clears_stale_sibling_link_before_launch() {
        // Arrange
        let mut app = new_test_app().await;
        let mut source_session = test_session(PathBuf::from("/tmp/source-session"));
        source_session.follow_up_tasks = vec![SessionFollowUpTask {
            id: 1,
            launched_session_id: Some("missing-session".to_string()),
            position: 0,
            text: "Open the sibling session.".to_string(),
        }];
        app.sessions.sessions.push(source_session);

        // Act
        let result = app
            .launch_or_open_selected_follow_up_task("session-1")
            .await;

        // Assert
        assert!(matches!(
            result,
            Err(AppError::Session(crate::app::SessionError::Workflow(message)))
                if message == "Git branch is required to create a session"
        ));
        assert_eq!(app.sessions.sessions.len(), 1);
        assert_eq!(
            app.sessions.sessions[0].follow_up_tasks[0].launched_session_id,
            None
        );
    }

    #[tokio::test]
    /// Verifies a viewed session keeps summary mode when its live status
    /// transition reaches `Done`.
    async fn apply_app_events_session_updated_keeps_done_view_in_summary_mode() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-done-view")));
        app.sessions.handles.insert(
            "session-1".to_string(),
            SessionHandles::new("Merge finished".to_string(), Status::Done),
        );
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: Some(review_loading_message(AgentModel::Gpt54)),
            review_text: Some("Review text".to_string()),
            session_id: "session-1".to_string(),
            scroll_offset: Some(9),
        };

        // Act
        app.apply_app_events(AppEvent::SessionUpdated {
            session_id: "session-1".to_string(),
        })
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::View {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                scroll_offset: Some(9),
                ..
            }
        ));
    }

    #[tokio::test]
    /// Verifies refresh keeps the active session view when merge cleanup has
    /// removed the worktree just before `Done` persists.
    async fn apply_app_events_refresh_keeps_viewed_merging_session_without_worktree() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&base_path.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-1",
                AgentModel::Gemini3FlashPreview.as_str(),
                "main",
                &Status::Merging.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert merging session");

        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path.clone(),
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");
        let session_folder = base_path.join("session-1");
        let mut viewed_session = test_session(session_folder);
        viewed_session.status = Status::Merging;
        app.sessions.sessions.push(viewed_session);
        app.sessions.handles.insert(
            "session-1".to_string(),
            SessionHandles::new("Merging".to_string(), Status::Merging),
        );
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: "session-1".to_string(),
            scroll_offset: None,
        };

        // Act
        app.apply_app_events(AppEvent::RefreshSessions).await;

        // Assert
        assert!(
            app.sessions
                .sessions
                .iter()
                .any(|session| session.id == "session-1" && session.status == Status::Merging)
        );
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id, ..
            } if session_id == "session-1"
        ));
    }

    #[test]
    fn discover_home_project_paths_includes_git_repos_and_excludes_session_worktrees() {
        // Arrange
        let home_directory = tempdir().expect("failed to create temp dir");
        let top_level_repo = home_directory.path().join("agentty");
        create_git_repo_marker(top_level_repo.as_path());
        let nested_repo = home_directory.path().join("code").join("service");
        create_git_repo_marker(nested_repo.as_path());
        let session_worktree_root = home_directory.path().join("agentty-worktrees");
        let session_worktree_repo = session_worktree_root.join("a1b2c3d4");
        create_git_repo_marker(session_worktree_repo.as_path());

        // Act
        let discovered_project_paths = App::discover_home_project_paths(
            home_directory.path(),
            session_worktree_root.as_path(),
        );

        // Assert
        assert!(
            discovered_project_paths.contains(&top_level_repo),
            "top-level git repository should be discovered"
        );
        assert!(
            discovered_project_paths.contains(&nested_repo),
            "nested git repository should be discovered"
        );
        assert!(
            !discovered_project_paths.contains(&session_worktree_repo),
            "session worktree repositories must be excluded"
        );
    }

    #[test]
    fn discover_home_project_paths_respects_repository_limit() {
        // Arrange
        let home_directory = tempdir().expect("failed to create temp dir");
        for index in 0..=HOME_PROJECT_SCAN_MAX_RESULTS {
            let repository = home_directory.path().join(format!("repo-{index}"));
            create_git_repo_marker(repository.as_path());
        }

        // Act
        let discovered_project_paths = App::discover_home_project_paths(
            home_directory.path(),
            Path::new("/tmp/non-session-worktree"),
        );

        // Assert
        assert_eq!(
            discovered_project_paths.len(),
            HOME_PROJECT_SCAN_MAX_RESULTS
        );
    }

    #[test]
    fn resolve_agentty_home_returns_env_override_when_set() {
        // Arrange
        let agentty_root = Some(PathBuf::from("/tmp/custom-agentty"));
        let home_dir = Some(PathBuf::from("/home/test-user"));

        // Act
        let home = resolve_agentty_home(agentty_root, home_dir);

        // Assert
        assert_eq!(home, PathBuf::from("/tmp/custom-agentty"));
    }

    #[test]
    fn resolve_agentty_home_falls_back_to_home_directory_when_override_is_empty() {
        // Arrange
        let agentty_root = Some(PathBuf::new());
        let home_dir = Some(PathBuf::from("/home/test-user"));

        // Act
        let home = resolve_agentty_home(agentty_root, home_dir);

        // Assert
        assert_eq!(home, PathBuf::from("/home/test-user/.agentty"));
    }

    #[test]
    fn resolve_agentty_home_falls_back_to_relative_directory_without_home_dir() {
        // Arrange
        let agentty_root = None;
        let home_dir = None;

        // Act
        let home = resolve_agentty_home(agentty_root, home_dir);

        // Assert
        assert_eq!(home, PathBuf::from(".agentty"));
    }

    #[test]
    fn is_session_worktree_project_path_returns_true_for_agentty_worktree_path() {
        // Arrange
        let session_worktree_root = Path::new("/home/test/.agentty/wt");
        let project_path = "/home/test/.agentty/wt/a1b2c3d4";

        // Act
        let is_session_worktree =
            App::is_session_worktree_project_path(project_path, session_worktree_root);

        // Assert
        assert!(is_session_worktree);
    }

    #[test]
    fn is_session_worktree_project_path_returns_false_for_main_repository_path() {
        // Arrange
        let session_worktree_root = Path::new("/home/test/.agentty/wt");
        let project_path = "/home/test/src/agentty";

        // Act
        let is_session_worktree =
            App::is_session_worktree_project_path(project_path, session_worktree_root);

        // Assert
        assert!(!is_session_worktree);
    }

    #[test]
    fn is_existing_project_path_returns_true_when_fs_client_reports_directory() {
        // Arrange
        let project_path = "/home/test/src/agentty";
        let expected_path = PathBuf::from(project_path);
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client
            .expect_is_dir()
            .once()
            .withf(move |path| path == &expected_path)
            .return_const(true);

        // Act
        let project_exists = App::is_existing_project_path(&fs_client, project_path);

        // Assert
        assert!(project_exists);
    }

    #[tokio::test]
    async fn active_project_has_tasks_tab_returns_true_when_roadmap_file_exists() {
        // Arrange
        let project_path = PathBuf::from("/home/test/src/agentty");
        let roadmap_path = project_path.join(TASKS_ROADMAP_PATH);
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client.expect_is_dir().returning(|_| true);
        fs_client
            .expect_is_file()
            .once()
            .withf(move |path| path == &roadmap_path)
            .return_const(true);
        let roadmap_path = project_path.join(TASKS_ROADMAP_PATH);
        fs_client
            .expect_read_file()
            .once()
            .withf(move |path| path == &roadmap_path)
            .return_once(|_| Box::pin(async { Ok(b"# roadmap".to_vec()) }));
        let app = app_with_fs_client(project_path, Arc::new(fs_client)).await;

        // Act
        let has_tasks_tab = app.active_project_has_tasks_tab();
        let cached_has_tasks_tab = app.active_project_has_tasks_tab();

        // Assert
        assert!(has_tasks_tab);
        assert!(cached_has_tasks_tab);
    }

    #[tokio::test]
    async fn active_project_has_tasks_tab_returns_false_when_roadmap_file_is_missing() {
        // Arrange
        let project_path = PathBuf::from("/home/test/src/agentty");
        let roadmap_path = project_path.join(TASKS_ROADMAP_PATH);
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client.expect_is_dir().returning(|_| true);
        fs_client
            .expect_is_file()
            .once()
            .withf(move |path| path == &roadmap_path)
            .return_const(false);
        let app = app_with_fs_client(project_path, Arc::new(fs_client)).await;

        // Act
        let has_tasks_tab = app.active_project_has_tasks_tab();
        let cached_has_tasks_tab = app.active_project_has_tasks_tab();

        // Assert
        assert!(!has_tasks_tab);
        assert!(!cached_has_tasks_tab);
    }

    #[tokio::test]
    async fn apply_app_events_refreshes_roadmap_after_successful_sync_main() {
        // Arrange
        let project_path = PathBuf::from("/home/test/src/agentty");
        let roadmap_path = project_path.join(TASKS_ROADMAP_PATH);
        let roadmap_path_for_file_check = roadmap_path.clone();
        let roadmap_path_for_read = roadmap_path.clone();
        let roadmap_snapshots = Arc::new(Mutex::new(VecDeque::from([
            b"# roadmap before sync".to_vec(),
            b"# roadmap after sync".to_vec(),
        ])));
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client.expect_is_dir().returning(|_| true);
        fs_client
            .expect_is_file()
            .times(2)
            .withf(move |path| path == &roadmap_path_for_file_check)
            .return_const(true);
        fs_client
            .expect_read_file()
            .times(2)
            .withf(move |path| path == &roadmap_path_for_read)
            .returning(move |_| {
                let roadmap_snapshots = Arc::clone(&roadmap_snapshots);
                Box::pin(async move {
                    let mut roadmap_snapshots = roadmap_snapshots
                        .lock()
                        .expect("roadmap snapshot lock should not be poisoned");
                    Ok(roadmap_snapshots
                        .pop_front()
                        .expect("expected a queued roadmap snapshot"))
                })
            });
        let mut app = app_with_fs_client(project_path, Arc::new(fs_client)).await;

        // Act
        app.apply_app_events(AppEvent::SyncMainCompleted {
            result: Ok(SyncMainOutcome {
                pulled_commit_titles: vec!["Refresh roadmap".to_string()],
                pulled_commits: Some(1),
                pushed_commit_titles: Vec::new(),
                pushed_commits: Some(0),
                resolved_conflict_files: Vec::new(),
            }),
        })
        .await;

        // Assert
        assert_eq!(
            app.active_project_roadmap,
            Some(ActiveProjectRoadmap::Loaded(
                "# roadmap after sync".to_string()
            ))
        );
        assert!(app.active_project_has_tasks_tab());
    }

    #[tokio::test]
    async fn apply_app_events_refreshes_roadmap_after_full_session_refresh() {
        // Arrange
        let project_path = PathBuf::from("/home/test/src/agentty");
        let roadmap_path = project_path.join(TASKS_ROADMAP_PATH);
        let roadmap_path_for_file_check = roadmap_path.clone();
        let roadmap_path_for_read = roadmap_path.clone();
        let roadmap_snapshots = Arc::new(Mutex::new(VecDeque::from([
            b"# roadmap before merge".to_vec(),
            b"# roadmap after merge".to_vec(),
        ])));
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client.expect_is_dir().returning(|_| true);
        fs_client
            .expect_is_file()
            .times(2)
            .withf(move |path| path == &roadmap_path_for_file_check)
            .return_const(true);
        fs_client
            .expect_read_file()
            .times(2)
            .withf(move |path| path == &roadmap_path_for_read)
            .returning(move |_| {
                let roadmap_snapshots = Arc::clone(&roadmap_snapshots);
                Box::pin(async move {
                    let mut roadmap_snapshots = roadmap_snapshots
                        .lock()
                        .expect("roadmap snapshot lock should not be poisoned");
                    Ok(roadmap_snapshots
                        .pop_front()
                        .expect("expected a queued roadmap snapshot"))
                })
            });
        let mut app = app_with_fs_client(project_path, Arc::new(fs_client)).await;

        // Act
        app.apply_app_events(AppEvent::RefreshSessions).await;

        // Assert
        assert_eq!(
            app.active_project_roadmap,
            Some(ActiveProjectRoadmap::Loaded(
                "# roadmap after merge".to_string()
            ))
        );
        assert!(app.active_project_has_tasks_tab());
    }

    #[test]
    fn visible_project_rows_excludes_missing_and_session_worktree_projects() {
        // Arrange
        let existing_project_path = "/home/test/src/agentty".to_string();
        let session_worktree_project_path = "/home/test/.agentty/wt/a1b2c3d4".to_string();
        let missing_project_path = "/home/test/src/removed".to_string();
        let session_worktree_root = Path::new("/home/test/.agentty/wt");
        let project_rows = vec![
            project_list_row_fixture(1, existing_project_path.clone()),
            project_list_row_fixture(2, session_worktree_project_path),
            project_list_row_fixture(3, missing_project_path.clone()),
        ];
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        let existing_project_path_for_match = PathBuf::from(existing_project_path.clone());
        let missing_project_path_for_match = PathBuf::from(missing_project_path);
        fs_client
            .expect_is_dir()
            .once()
            .withf(move |path| path == &existing_project_path_for_match)
            .return_const(true);
        fs_client
            .expect_is_dir()
            .once()
            .withf(move |path| path == &missing_project_path_for_match)
            .return_const(false);

        // Act
        let visible_rows =
            App::visible_project_rows(project_rows, &fs_client, session_worktree_root);

        // Assert
        assert_eq!(visible_rows.len(), 1);
        assert_eq!(visible_rows[0].path, existing_project_path);
    }

    #[tokio::test]
    async fn resolve_startup_active_project_id_falls_back_when_stored_project_path_is_missing() {
        // Arrange
        let current_project_dir = tempdir().expect("failed to create current project dir");
        let current_project_path = current_project_dir.path().to_path_buf();
        let missing_project_path = current_project_path.join("removed-project");
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let current_project_id = database
            .upsert_project(&current_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to insert current project");
        let missing_project_id = database
            .upsert_project(&missing_project_path.to_string_lossy(), Some("main"))
            .await
            .expect("failed to insert missing project");
        database
            .set_active_project_id(missing_project_id)
            .await
            .expect("failed to persist active project");
        let missing_project_path = missing_project_path.clone();
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client
            .expect_is_dir()
            .once()
            .withf(move |path| path == &missing_project_path)
            .return_const(false);

        // Act
        let resolved_project_id =
            App::resolve_startup_active_project_id(&database, &fs_client, current_project_id).await;

        // Assert
        assert_eq!(resolved_project_id, current_project_id);
    }

    #[tokio::test]
    async fn apply_app_events_refresh_sessions_reloads_project_active_session_count() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&base_path.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-active",
                "gemini-3-flash-preview",
                "main",
                &Status::Review.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert active session");

        let session_folder_name = "session-".chars().take(8).collect::<String>();
        let session_data_dir = base_path.join(session_folder_name).join(SESSION_DATA_DIR);
        fs::create_dir_all(session_data_dir).expect("failed to create session dir");

        let mut app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build app");

        let initial_active_count = app
            .projects
            .project_items()
            .iter()
            .find(|item| item.project.id == project_id)
            .map_or(0, |item| item.active_session_count);
        assert_eq!(initial_active_count, 1);

        app.services
            .db()
            .update_session_status_with_timing_at("session-active", &Status::Done.to_string(), 0)
            .await
            .expect("failed to update session status");

        // Act
        app.apply_app_events(AppEvent::RefreshSessions).await;

        // Assert
        let updated_active_count = app
            .projects
            .project_items()
            .iter()
            .find(|item| item.project.id == project_id)
            .map_or(0, |item| item.active_session_count);
        assert_eq!(updated_active_count, 0);
    }

    #[tokio::test]
    /// Verifies project list loads reuse only persisted rows and do not
    /// discover repositories implicitly.
    async fn load_project_items_uses_persisted_rows_without_home_scan() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let home_directory = tempdir().expect("failed to create temp dir");
        let discovered_repo = home_directory.path().join("agentty");
        create_git_repo_marker(discovered_repo.as_path());
        let fs_client = RealFsClient;
        let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);

        // Act
        let project_items = App::load_project_items_with_session_worktree_root(
            &database,
            &fs_client,
            session_worktree_root.as_path(),
        )
        .await;

        // Assert
        assert!(project_items.is_empty());
        assert!(
            database
                .load_projects_with_stats()
                .await
                .expect("failed to load projects")
                .is_empty()
        );
    }

    #[tokio::test]
    /// Verifies the startup-only catalog refresh discovers repositories before
    /// the first project list load.
    async fn refresh_project_catalog_on_startup_discovers_home_directory_repositories() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let home_directory = tempdir().expect("failed to create temp dir");
        let discovered_repo = home_directory.path().join("agentty");
        create_git_repo_marker(discovered_repo.as_path());
        let fs_client = RealFsClient;
        let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);

        // Act
        App::load_projects_from_home_directory(
            &database,
            session_worktree_root.as_path(),
            Some(home_directory.path()),
        )
        .await;

        let project_items = App::load_project_items_with_session_worktree_root(
            &database,
            &fs_client,
            session_worktree_root.as_path(),
        )
        .await;

        // Assert
        assert_eq!(project_items.len(), 1);
        assert_eq!(project_items[0].project.path, discovered_repo);
    }

    /// Creates one directory with a `.git` marker for repository discovery
    /// tests.
    fn create_git_repo_marker(repository_path: &Path) {
        fs::create_dir_all(repository_path.join(".git"))
            .expect("failed to create repository .git marker");
    }

    /// Builds one lightweight project row fixture for project list tests.
    fn project_list_row_fixture(project_id: i64, project_path: String) -> db::ProjectListRow {
        db::ProjectListRow {
            active_session_count: 0,
            created_at: 0,
            display_name: None,
            git_branch: Some("main".to_string()),
            id: project_id,
            is_favorite: false,
            last_opened_at: None,
            last_session_updated_at: None,
            path: project_path,
            session_count: 0,
            updated_at: 0,
        }
    }

    /// Replaces the app-level git dependencies with one caller-provided mock.
    fn install_mock_git_client(app: &mut App, mock_git_client: crate::infra::git::MockGitClient) {
        let mock_git_client: Arc<dyn crate::infra::git::GitClient> = Arc::new(mock_git_client);
        let base_path = app.services.base_path().to_path_buf();
        let db = app.services.db().clone();
        let event_sender = app.services.event_sender();
        let available_agent_kinds = app.services.available_agent_kinds();
        let app_server_client_override = app.services.app_server_client_override();
        let fs_client = app.services.fs_client();
        let review_request_client = app.services.review_request_client();

        app.services = AppServices::new(
            base_path,
            app.services.clock(),
            db,
            event_sender,
            AppServiceDeps {
                app_server_client_override,
                available_agent_kinds,
                fs_client,
                git_client: Arc::clone(&mock_git_client),
                review_request_client,
            },
        );
    }
    #[tokio::test]
    async fn apply_review_update_stores_success_in_cache() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-cache";
        let review_text = "## Review\nLooks good.";
        let mut session = test_session(PathBuf::from("/tmp/session-review-cache"));
        session.id = session_id.to_string();
        session.status = Status::AgentReview;
        app.sessions.sessions.push(session);
        app.sessions.handles.insert(
            session_id.to_string(),
            SessionHandles::new(String::new(), Status::AgentReview),
        );
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Loading { diff_hash: 123 },
        );

        // Act
        app.apply_review_update(
            session_id,
            ReviewUpdate {
                diff_hash: 123,
                result: Ok(review_text.to_string()),
            },
        );

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Ready { text, diff_hash }) if text == review_text && *diff_hash == 123
        ));
        assert_eq!(app.sessions.sessions[0].status, Status::Review);
        assert_eq!(
            *app.sessions
                .handles
                .get(session_id)
                .expect("expected session handles")
                .status
                .lock()
                .expect("expected handle status lock"),
            Status::Review
        );
    }

    #[tokio::test]
    async fn apply_review_update_stores_failure_in_cache() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-fail";
        let error_message = "Review assist failed with exit code 1";
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Loading { diff_hash: 456 },
        );

        // Act
        app.apply_review_update(
            session_id,
            ReviewUpdate {
                diff_hash: 456,
                result: Err(error_message.to_string()),
            },
        );

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Failed { error, diff_hash }) if error == error_message && *diff_hash == 456
        ));
    }

    #[tokio::test]
    async fn apply_review_update_ignores_stale_diff_hash() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-stale";
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Loading { diff_hash: 999 },
        );

        // Act
        app.apply_review_update(
            session_id,
            ReviewUpdate {
                diff_hash: 111,
                result: Ok("stale review".to_string()),
            },
        );

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { diff_hash }) if *diff_hash == 999
        ));
    }

    #[tokio::test]
    async fn apply_review_update_keeps_non_agent_review_status_unchanged() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-progress";
        let mut session = test_session(PathBuf::from("/tmp/session-review-progress"));
        session.id = session_id.to_string();
        session.status = Status::InProgress;
        app.sessions.sessions.push(session);
        app.sessions.handles.insert(
            session_id.to_string(),
            SessionHandles::new(String::new(), Status::InProgress),
        );
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Loading { diff_hash: 222 },
        );

        // Act
        app.apply_review_update(
            session_id,
            ReviewUpdate {
                diff_hash: 222,
                result: Ok("## Review\nBackground review".to_string()),
            },
        );

        // Assert
        assert_eq!(app.sessions.sessions[0].status, Status::InProgress);
        assert_eq!(
            *app.sessions
                .handles
                .get(session_id)
                .expect("expected session handles")
                .status
                .lock()
                .expect("expected handle status lock"),
            Status::InProgress
        );
    }

    #[tokio::test]
    async fn auto_start_reviews_clears_cache_on_in_progress_transition() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-cache-clear")));
        app.sessions.sessions[0].status = Status::InProgress;
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Ready {
                diff_hash: 789,
                text: "old review".to_string(),
            },
        );
        let session_ids = HashSet::from([session_id.to_string()]);

        // Act
        app.auto_start_reviews(&session_ids).await;

        // Assert
        assert!(!app.review_cache.contains_key(session_id));
    }

    #[tokio::test]
    async fn auto_start_reviews_skips_when_diff_hash_unchanged() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-hash-skip")));
        app.sessions.sessions[0].status = Status::Review;

        let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
        let hash = diff_content_hash(diff_text);
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Ready {
                diff_hash: hash,
                text: "existing review".to_string(),
            },
        );
        let session_ids = HashSet::from([session_id.to_string()]);

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.auto_start_reviews(&session_ids).await;

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Ready { text, .. }) if text == "existing review"
        ));
    }

    #[tokio::test]
    /// Verifies that a review already in `Loading` state with matching diff
    /// hash is not re-triggered by a subsequent reducer tick.
    async fn auto_start_reviews_skips_when_already_loading_with_same_hash() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-loading-skip")));
        app.sessions.sessions[0].status = Status::Review;

        let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
        let hash = diff_content_hash(diff_text);
        app.review_cache.insert(
            session_id.to_string(),
            ReviewCacheEntry::Loading { diff_hash: hash },
        );
        let session_ids = HashSet::from([session_id.to_string()]);

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.auto_start_reviews(&session_ids).await;

        // Assert — still Loading, not re-triggered
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { diff_hash }) if *diff_hash == hash
        ));
        // Status remains Review because mark_session_agent_review was not called.
        assert_eq!(app.sessions.sessions[0].status, Status::Review);
    }

    #[tokio::test]
    async fn auto_start_reviews_starts_loading_for_review_session() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-hash-start")));
        app.sessions.sessions[0].status = Status::Review;

        let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
        let expected_hash = diff_content_hash(diff_text);
        let session_ids = HashSet::from([session_id.to_string()]);

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.auto_start_reviews(&session_ids).await;

        // Assert
        assert!(matches!(
            app.review_cache.get(session_id),
            Some(ReviewCacheEntry::Loading { diff_hash }) if *diff_hash == expected_hash
        ));
        assert_eq!(app.sessions.sessions[0].status, Status::AgentReview);
    }

    #[tokio::test]
    async fn delete_selected_session_clears_review_cache() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-delete-cache")));
        app.sessions.table_state.select(Some(0));
        let session_id = app.sessions.sessions[0].id.clone();
        app.review_cache.insert(
            session_id.clone(),
            ReviewCacheEntry::Ready {
                diff_hash: 42,
                text: "cached review".to_string(),
            },
        );

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(!app.review_cache.contains_key(&session_id));
    }

    // -- sync_task_result_from_summary tests ---------------------------------

    /// Builds one test review request summary for sync tests.
    fn test_review_request_summary(
        display_id: &str,
        state: ReviewRequestState,
    ) -> ReviewRequestSummary {
        ReviewRequestSummary {
            display_id: display_id.to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "agentty/session-id".to_string(),
            state,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        }
    }

    #[test]
    fn test_sync_task_result_from_open_summary() {
        // Arrange
        let mut summary = test_review_request_summary("#42", ReviewRequestState::Open);
        summary.status_summary = Some("Checks passing".to_string());

        // Act
        let result = sync_task_result_from_summary(summary);

        // Assert
        assert_eq!(
            result.outcome,
            session::SyncReviewRequestOutcome::Open {
                display_id: "#42".to_string(),
                status_summary: Some("Checks passing".to_string()),
            }
        );
        assert!(result.summary.is_some());
    }

    #[tokio::test]
    async fn run_sync_review_request_attaches_worktree_to_detected_remote() {
        // Arrange
        let folder = PathBuf::from("/tmp/session-worktree");
        let linked_review_request = crate::domain::session::ReviewRequest {
            last_refreshed_at: 42,
            summary: test_review_request_summary("#42", ReviewRequestState::Open),
        };
        let expected_remote = forge::ForgeRemote {
            command_working_directory: Some(folder.clone()),
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        };
        let expected_summary = test_review_request_summary("#42", ReviewRequestState::Merged);
        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_repo_url()
            .once()
            .withf({
                let folder = folder.clone();
                move |candidate_folder| candidate_folder == &folder
            })
            .returning(|_| {
                Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
            });
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty.git")
            .returning(|_| {
                Ok(forge::ForgeRemote {
                    command_working_directory: None,
                    forge_kind: ForgeKind::GitHub,
                    host: "github.com".to_string(),
                    namespace: "agentty-xyz".to_string(),
                    project: "agentty".to_string(),
                    repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty".to_string(),
                })
            });
        mock_review_request_client
            .expect_refresh_review_request()
            .once()
            .withf({
                let expected_remote = expected_remote.clone();
                move |candidate_remote, display_id| {
                    candidate_remote == &expected_remote && display_id == "#42"
                }
            })
            .returning({
                let expected_summary = expected_summary.clone();
                move |_, _| {
                    let expected_summary = expected_summary.clone();

                    Box::pin(async move { Ok(expected_summary) })
                }
            });

        // Act
        let result = run_sync_review_request(
            folder,
            Arc::new(mock_git_client),
            Some(linked_review_request),
            None,
            Arc::new(mock_review_request_client),
        )
        .await
        .expect("sync should succeed");

        // Assert
        assert_eq!(
            result.outcome,
            session::SyncReviewRequestOutcome::Merged {
                display_id: "#42".to_string(),
            }
        );
    }

    #[test]
    fn test_sync_task_result_from_merged_summary() {
        // Arrange
        let summary = test_review_request_summary("#99", ReviewRequestState::Merged);

        // Act
        let result = sync_task_result_from_summary(summary);

        // Assert
        assert_eq!(
            result.outcome,
            session::SyncReviewRequestOutcome::Merged {
                display_id: "#99".to_string(),
            }
        );
        assert!(result.summary.is_some());
    }

    #[test]
    fn test_sync_task_result_from_closed_summary() {
        // Arrange
        let summary = test_review_request_summary("#7", ReviewRequestState::Closed);

        // Act
        let result = sync_task_result_from_summary(summary);

        // Assert
        assert_eq!(
            result.outcome,
            session::SyncReviewRequestOutcome::Closed {
                display_id: "#7".to_string(),
            }
        );
        assert!(result.summary.is_some());
    }

    // -- apply_sync_review_request_update tests ------------------------------

    #[tokio::test]
    async fn test_apply_sync_review_request_update_open_shows_open_popup() {
        // Arrange
        let mut app = new_test_app().await;
        let session_folder = PathBuf::from("/tmp/session-sync");
        let mut sync_session = test_session(session_folder);
        sync_session.status = Status::Review;
        sync_session.published_upstream_ref = Some("origin/agentty/session-1".to_string());
        app.sessions.sessions.push(sync_session);

        let restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: "session-1".to_string(),
        };

        let summary = test_review_request_summary("#10", ReviewRequestState::Open);
        let task_result = SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Open {
                display_id: "#10".to_string(),
                status_summary: None,
            },
            summary: Some(summary),
        };

        let update = SyncReviewRequestUpdate {
            restore_view,
            result: Ok(task_result),
            session_id: "session-1".to_string(),
        };

        // Act
        app.apply_sync_review_request_update(update).await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::ViewInfoPopup { title, .. } if title == "Review request open"
        ));
    }

    #[tokio::test]
    async fn test_apply_sync_review_request_update_error_shows_sync_failed() {
        // Arrange
        let mut app = new_test_app().await;
        let restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: "session-1".to_string(),
        };

        let update = SyncReviewRequestUpdate {
            restore_view,
            result: Err("network timeout".to_string()),
            session_id: "session-1".to_string(),
        };

        // Act
        app.apply_sync_review_request_update(update).await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::ViewInfoPopup { title, .. } if title == "Sync failed"
        ));
    }

    #[tokio::test]
    async fn test_apply_sync_review_request_update_persists_summary() {
        // Arrange
        let mut app = new_test_app().await;
        let project_id = app.active_project_id();
        let session_id = "session-1";
        app.services
            .db()
            .insert_session(
                session_id,
                "gemini-3-flash-preview",
                "main",
                &Status::Review.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert session");
        let session_folder_name = session_id.chars().take(8).collect::<String>();
        let session_data_dir = app
            .services
            .base_path()
            .join(session_folder_name)
            .join(SESSION_DATA_DIR);
        fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
        app.refresh_sessions_now().await;

        let restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: "session-1".to_string(),
        };

        let summary = test_review_request_summary("#5", ReviewRequestState::Open);
        let task_result = SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Open {
                display_id: "#5".to_string(),
                status_summary: None,
            },
            summary: Some(summary),
        };

        let update = SyncReviewRequestUpdate {
            restore_view,
            result: Ok(task_result),
            session_id: "session-1".to_string(),
        };

        // Act
        app.apply_sync_review_request_update(update).await;

        // Assert — the in-memory session now has the linked review request.
        assert_eq!(app.sessions.sessions.len(), 1);
        let session = &app.sessions.sessions[0];
        let review_request = session
            .review_request
            .as_ref()
            .expect("expected linked review request after sync");
        assert_eq!(review_request.summary.display_id, "#5");
    }

    #[tokio::test]
    async fn test_apply_sync_review_request_update_no_review_request_shows_not_found() {
        // Arrange
        let mut app = new_test_app().await;
        let restore_view = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: "session-1".to_string(),
        };

        let task_result = SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
            summary: None,
        };

        let update = SyncReviewRequestUpdate {
            restore_view,
            result: Ok(task_result),
            session_id: "session-1".to_string(),
        };

        // Act
        app.apply_sync_review_request_update(update).await;

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::ViewInfoPopup { title, .. } if title == "No review request found"
        ));
    }
}
