//! App-layer composition root and shared state container.
//!
//! This module wires app submodules and exposes [`App`] used by runtime mode
//! handlers.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::env;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ag_forge as forge;
use ag_forge::{RealReviewRequestClient, ReviewRequestClient};
use app::merge_queue::{MergeQueue, MergeQueueProgress};
use app::project::ProjectManager;
use app::service::AppServices;
use app::session::SessionManager;
use app::session_state::SessionState;
use app::setting::SettingsManager;
use app::tab::TabManager;
use app::task;
use ignore::WalkBuilder;
use ratatui::Frame;
use ratatui::widgets::TableState;
use session::{SessionTaskService, SyncMainOutcome, SyncSessionStartError};
use tokio::sync::mpsc;

use crate::app::session;
use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::input::InputState;
use crate::domain::permission::PermissionMode;
use crate::domain::project::{Project, ProjectListItem, project_name_from_path};
use crate::domain::session::{PublishBranchAction, Session, SessionSize, Status};
use crate::infra::agent::AgentResponse;
use crate::infra::channel::TurnPrompt;
use crate::infra::db::Database;
use crate::infra::file_index::FileEntry;
use crate::infra::fs::{FsClient, RealFsClient};
use crate::infra::git::{GitClient, RealGitClient, detect_git_info};
use crate::infra::tmux::{RealTmuxClient, TmuxClient};
use crate::infra::{app_server, db};
use crate::runtime::mode::{question, sync_blocked};
use crate::ui::page::session_list::preferred_initial_session_index;
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, HelpContext, QuestionFocus};
use crate::ui::state::prompt::PromptAtMentionState;
use crate::{app, ui};

/// Relative directory name used for session git worktrees within the
/// `agentty` home directory.
pub const AGENTTY_WT_DIR: &str = "wt";

/// Maximum directory depth to scan under the user home for git repositories.
const HOME_PROJECT_SCAN_MAX_DEPTH: usize = 5;

/// Maximum number of repositories discovered from one home-directory scan.
const HOME_PROJECT_SCAN_MAX_RESULTS: usize = 200;

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
    /// Indicates latest ahead/behind information from the git status worker.
    GitStatusUpdated { status: Option<(u32, u32)> },
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
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Indicates compact live progress text for an in-progress session.
    SessionProgressUpdated {
        progress_message: Option<String>,
        session_id: String,
    },
    /// Indicates completion of a list-mode sync workflow.
    SyncMainCompleted {
        result: Result<SyncMainOutcome, SyncSessionStartError>,
    },
    /// Indicates a recomputed session-size bucket for one session.
    SessionSizeUpdated {
        session_id: String,
        session_size: SessionSize,
    },
    /// Indicates completion of a session-view branch-publish action.
    BranchPublishActionCompleted {
        restore_view: ConfirmationViewMode,
        result: Box<BranchPublishTaskResult>,
        session_id: String,
    },
    /// Indicates review assist output became available for a session.
    FocusedReviewPrepared {
        diff_hash: u64,
        review_text: String,
        session_id: String,
    },
    /// Indicates review assist failed for a session.
    FocusedReviewPreparationFailed {
        diff_hash: u64,
        error: String,
        session_id: String,
    },
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: String },
    /// Indicates that an agent turn completed with one structured response
    /// payload to be routed by the app reducer.
    AgentResponseReceived {
        response: AgentResponse,
        session_id: String,
    },
}

#[derive(Default)]
struct AppEventBatch {
    agent_responses: HashMap<String, AgentResponse>,
    at_mention_entries_updates: HashMap<String, Vec<FileEntry>>,
    focused_review_updates: HashMap<String, FocusedReviewUpdate>,
    git_status_update: Option<(u32, u32)>,
    has_git_status_update: bool,
    has_latest_available_version_update: bool,
    latest_available_version_update: Option<String>,
    branch_publish_action_update: Option<BranchPublishActionUpdate>,
    session_ids: HashSet<String>,
    session_model_updates: HashMap<String, AgentModel>,
    session_size_updates: HashMap<String, SessionSize>,
    session_progress_updates: HashMap<String, Option<String>>,
    should_force_reload: bool,
    sync_main_result: Option<Result<SyncMainOutcome, SyncSessionStartError>>,
    update_status: Option<UpdateStatus>,
}

/// Immutable context displayed in sync-main popup content.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SyncPopupContext {
    default_branch: String,
    project_name: String,
}

/// Aggregated review assist output keyed by session.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FocusedReviewUpdate {
    /// Hash of the diff that triggered this review, carried from the task.
    diff_hash: u64,
    result: Result<String, String>,
}

/// Session snapshot cloned into a branch-publish background task.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BranchPublishTaskSession {
    /// Base branch used as the review-request target when a forge link is
    /// generated after push.
    base_branch: String,
    /// Session worktree used for git push and remote inspection.
    folder: PathBuf,
    /// Stable session identifier.
    id: String,
    /// Current session lifecycle state checked before push.
    status: Status,
}

impl BranchPublishTaskSession {
    /// Builds one background-task snapshot from a live session row.
    fn from_session(session: &Session) -> Self {
        Self {
            base_branch: session.base_branch.clone(),
            folder: session.folder.clone(),
            id: session.id.clone(),
            status: session.status,
        }
    }
}

/// Final reducer payload for a completed branch-publish background action.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BranchPublishActionUpdate {
    restore_view: ConfirmationViewMode,
    result: BranchPublishTaskResult,
    session_id: String,
}

/// Error payload shown inside the session-view info popup for branch-publish
/// failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BranchPublishTaskFailure {
    message: String,
    title: String,
}

impl BranchPublishTaskFailure {
    /// Builds one blocked-state popup payload from an actionable message.
    fn blocked(message: String) -> Self {
        Self {
            message,
            title: "Branch push blocked".to_string(),
        }
    }

    /// Builds one failure-state popup payload from an execution error.
    fn failed(message: String) -> Self {
        Self {
            message,
            title: "Branch push failed".to_string(),
        }
    }
}

/// Successful outcome returned by a branch-publish background action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BranchPublishTaskSuccess {
    /// Carries the pushed branch name and persisted upstream reference for
    /// popup copy and session state updates.
    Pushed {
        /// Remote branch name that was pushed successfully.
        branch_name: String,
        /// Optional forge-native URL that opens a new review-request flow for
        /// the pushed branch.
        review_request_creation_url: Option<String>,
        /// Persisted upstream ref recorded after the successful push.
        upstream_reference: String,
    },
}

/// Reducer-friendly result for a completed branch-publish background action.
pub(crate) type BranchPublishTaskResult =
    Result<BranchPublishTaskSuccess, BranchPublishTaskFailure>;

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
            let _ = app_event_tx.send(AppEvent::SyncMainCompleted { result });
        });
    }
}

/// External clients used to compose [`App`] startup dependencies.
pub(crate) struct AppClients {
    app_server_client: Arc<dyn app_server::AppServerClient>,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    review_request_client: Arc<dyn ReviewRequestClient>,
    sync_main_runner: Arc<dyn SyncMainRunner>,
    tmux_client: Arc<dyn TmuxClient>,
}

impl AppClients {
    /// Builds one client bundle with real implementations for each external
    /// boundary except the required app-server client.
    pub(crate) fn new(app_server_client: Arc<dyn app_server::AppServerClient>) -> Self {
        Self {
            app_server_client,
            fs_client: Arc::new(RealFsClient),
            git_client: Arc::new(RealGitClient),
            review_request_client: Arc::new(RealReviewRequestClient::default()),
            sync_main_runner: Arc::new(TokioSyncMainRunner),
            tmux_client: Arc::new(RealTmuxClient),
        }
    }

    /// Replaces the tmux boundary while preserving the remaining clients.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_tmux_client(mut self, tmux_client: Arc<dyn TmuxClient>) -> Self {
        self.tmux_client = tmux_client;

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
    pub(crate) focused_review_cache: HashMap<String, FocusedReviewCacheEntry>,
    /// Owns project selection state, project metadata, and git status
    /// snapshots.
    pub(crate) projects: ProjectManager,
    /// Shares application-wide services and external clients across workflows.
    pub(crate) services: AppServices,
    /// Owns session state, runtime handles, and session workflow coordination.
    pub(crate) sessions: SessionManager,
    /// Runs sync-to-main workflows behind an injectable boundary.
    pub(crate) sync_main_runner: Arc<dyn SyncMainRunner>,
    /// Receives app events emitted by background tasks and workflows.
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Stores the latest available stable `agentty` version when one is
    /// detected.
    latest_available_version: Option<String>,
    /// Serializes local merge requests so only one merge workflow runs at a
    /// time.
    merge_queue: MergeQueue,
    /// Tracks per-session progress messages rendered while background work is
    /// active.
    session_progress_messages: HashMap<String, String>,
    /// Interacts with tmux panes for session-specific terminal workflows.
    tmux_client: Arc<dyn TmuxClient>,
    /// Stores the current auto-update progress state when an update is running.
    update_status: Option<UpdateStatus>,
}

/// Cached focused review state for a session.
#[derive(Debug)]
pub(crate) enum FocusedReviewCacheEntry {
    /// Review generation is in progress.
    Loading {
        /// Hash of the diff text that triggered this review generation.
        diff_hash: u64,
    },
    /// Review text was successfully generated.
    Ready {
        /// Hash of the diff text that was reviewed.
        diff_hash: u64,
        /// Generated review text.
        text: String,
    },
    /// Review generation failed with an error description.
    Failed {
        /// Hash of the diff text that triggered the failed review.
        diff_hash: u64,
        /// Human-readable error description.
        error: String,
    },
}

impl FocusedReviewCacheEntry {
    /// Returns the diff content hash stored in any variant.
    pub(crate) fn diff_hash(&self) -> u64 {
        match self {
            Self::Loading { diff_hash }
            | Self::Ready { diff_hash, .. }
            | Self::Failed { diff_hash, .. } => *diff_hash,
        }
    }
}

/// Computes a deterministic hash of the diff text for cache invalidation.
///
/// Uses [`DefaultHasher`] which is not guaranteed to produce stable hashes
/// across Rust versions. This is acceptable because the cache is purely
/// in-memory and lives only for the duration of the process.
pub(crate) fn diff_content_hash(diff: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    diff.hash(&mut hasher);

    hasher.finish()
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
        app_server_client: Arc<dyn app_server::AppServerClient>,
    ) -> Result<Self, String> {
        let clients = AppClients::new(app_server_client);

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
    ) -> Result<Self, String> {
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
    ) -> Result<Self, String> {
        let current_project_id = db
            .upsert_project(&working_dir.to_string_lossy(), git_branch.as_deref())
            .await
            .map_err(|error| {
                format!(
                    "Failed to persist startup project `{}`: {error}",
                    working_dir.display()
                )
            })?;

        db.backfill_session_project(current_project_id)
            .await
            .map_err(|error| {
                format!(
                    "Failed to backfill startup sessions for project `{}`: {error}",
                    working_dir.display()
                )
            })?;
        let (
            active_project_id,
            startup_working_dir,
            startup_git_branch,
            project_items,
            active_project_name,
        ) = Self::load_startup_project_context(
            &db,
            clients.fs_client.as_ref(),
            &clients.git_client,
            working_dir.as_path(),
            git_branch,
            current_project_id,
        )
        .await?;

        SessionManager::fail_unfinished_operations_from_previous_run(&db).await;

        let mut table_state = TableState::default();
        let mut handles = HashMap::new();
        let (sessions, stats_activity) = SessionManager::load_sessions_with_fs_client(
            &base_path,
            &db,
            active_project_id,
            startup_working_dir.as_path(),
            &mut handles,
            clients.fs_client.as_ref(),
        )
        .await;
        let (sessions_row_count, sessions_updated_at_max) =
            db.load_sessions_metadata().await.unwrap_or((0, 0));
        table_state.select(preferred_initial_session_index(&sessions));

        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let services = AppServices::new(
            base_path,
            db.clone(),
            event_tx.clone(),
            Arc::clone(&clients.fs_client),
            Arc::clone(&clients.git_client),
            Arc::clone(&clients.review_request_client),
            Arc::clone(&clients.app_server_client),
        );
        let projects = ProjectManager::new(
            active_project_id,
            active_project_name,
            startup_git_branch,
            Arc::clone(&git_status_cancel),
            project_items,
            startup_working_dir,
        );
        let settings = SettingsManager::new(&services, active_project_id).await;
        let default_session_model = SessionManager::load_default_session_model(
            &services,
            Some(active_project_id),
            AgentKind::Gemini.default_model(),
        )
        .await;
        let clock: Arc<dyn session::Clock> = Arc::new(session::RealClock);
        let sessions = SessionManager::new(
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

        Self::spawn_background_tasks(auto_update, &event_tx, &projects, &services);

        Ok(Self {
            mode: AppMode::List,
            settings,
            tabs: TabManager::new(),
            projects,
            services,
            sessions,
            event_rx,
            focused_review_cache: HashMap::new(),
            latest_available_version: None,
            merge_queue: MergeQueue::default(),
            update_status: None,
            session_progress_messages: HashMap::new(),
            sync_main_runner: clients.sync_main_runner,
            tmux_client: clients.tmux_client,
        })
    }

    /// Spawns background pollers for git status and version checks.
    ///
    /// When `auto_update` is `true`, a background `npm i -g agentty@latest`
    /// runs automatically after detecting a newer version.
    fn spawn_background_tasks(
        auto_update: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
        projects: &ProjectManager,
        services: &AppServices,
    ) {
        task::TaskService::spawn_version_check_task(event_tx, auto_update);
        if projects.has_git_branch() {
            task::TaskService::spawn_git_status_task(
                projects.working_dir(),
                projects.git_status_cancel(),
                event_tx.clone(),
                services.git_client(),
            );
        }
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

    /// Returns the latest ahead/behind snapshot from reducer-applied events.
    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.projects.git_status()
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
        let active_project_id = self.projects.active_project_id();
        let current_tab = self.tabs.current();
        let working_dir = self.projects.working_dir().to_path_buf();
        let git_branch = self.projects.git_branch().map(str::to_string);
        let git_status = self.projects.git_status();
        let latest_available_version = self.latest_available_version.as_deref().map(str::to_string);
        let update_status = self.update_status().cloned();
        let projects = self.projects.project_items().to_vec();
        let session_progress_messages = self.session_progress_messages.clone();
        let mode = &self.mode;
        let project_table_state = self.projects.project_table_state_mut();
        let (sessions, stats_activity, table_state) = self.sessions.render_parts();
        let settings = &mut self.settings;

        ui::render(
            frame,
            ui::RenderContext {
                active_project_id,
                current_tab,
                git_branch: git_branch.as_deref(),
                git_status,
                latest_available_version: latest_available_version.as_deref(),
                update_status: update_status.as_ref(),
                mode,
                project_table_state,
                projects: &projects,
                session_progress_messages: &session_progress_messages,
                settings,
                stats_activity,
                sessions,
                table_state,
                working_dir: &working_dir,
            },
        );
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
    pub async fn switch_selected_project(&mut self) -> Result<(), String> {
        let selected_project_id = self
            .projects
            .selected_project_id()
            .ok_or_else(|| "No project selected".to_string())?;

        self.switch_project(selected_project_id).await
    }

    /// Switches app context to one persisted project id.
    ///
    /// # Errors
    /// Returns an error if the project does not exist or session refresh fails.
    pub async fn switch_project(&mut self, project_id: i64) -> Result<(), String> {
        let project = self
            .services
            .db()
            .get_project(project_id)
            .await?
            .map(Self::project_from_row)
            .ok_or_else(|| format!("Project with id `{project_id}` was not found"))?;
        let git_branch = self
            .services
            .git_client()
            .detect_git_info(project.path.clone())
            .await;
        let _ = self
            .services
            .db()
            .upsert_project(&project.path.to_string_lossy(), git_branch.as_deref())
            .await;
        let _ = self.services.db().set_active_project_id(project.id).await;
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
            project.path,
        );
        self.settings = SettingsManager::new(&self.services, project.id).await;
        let default_session_model = SessionManager::load_default_session_model(
            &self.services,
            Some(project.id),
            AgentKind::Gemini.default_model(),
        )
        .await;
        self.sessions
            .set_default_session_model(default_session_model);
        self.restart_git_status_task();
        self.reload_projects().await;
        self.refresh_sessions_now().await;

        Ok(())
    }

    /// Creates a blank session and schedules list refresh through events.
    ///
    /// # Errors
    /// Returns an error if worktree or persistence setup fails.
    pub async fn create_session(&mut self) -> Result<String, String> {
        let session_id = self
            .sessions
            .create_session(&self.projects, &self.services)
            .await?;
        self.process_pending_app_events().await;
        self.reload_projects().await;

        let index = self
            .sessions
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .unwrap_or(0);
        self.sessions.table_state.select(Some(index));

        Ok(session_id)
    }

    /// Submits the initial prompt for a newly created session.
    ///
    /// # Errors
    /// Returns an error if the session is missing or task enqueue fails.
    pub async fn start_session(
        &mut self,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
    ) -> Result<(), String> {
        self.sessions
            .start_session(&self.services, session_id, prompt)
            .await
    }

    /// Submits a follow-up prompt for an existing session.
    pub async fn reply(&mut self, session_id: &str, prompt: impl Into<TurnPrompt>) {
        self.sessions
            .reply(&self.services, session_id, prompt)
            .await;
    }

    /// Persists and applies a model selection for a session.
    ///
    /// # Errors
    /// Returns an error if persistence fails.
    pub async fn set_session_model(
        &mut self,
        session_id: &str,
        session_model: AgentModel,
    ) -> Result<(), String> {
        self.sessions
            .set_session_model(&self.services, session_id, session_model)
            .await?;
        self.process_pending_app_events().await;

        Ok(())
    }

    /// Returns the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.selected_session()
    }

    /// Returns session id by list index.
    pub fn session_id_for_index(&self, session_index: usize) -> Option<String> {
        self.sessions.session_id_for_index(session_index)
    }

    /// Resolves a session id to current list index.
    pub fn session_index_for_id(&self, session_id: &str) -> Option<usize> {
        self.sessions.session_index_for_id(session_id)
    }

    /// Returns compact live progress text for a session, if available.
    pub fn session_progress_message(&self, session_id: &str) -> Option<&str> {
        self.session_progress_messages
            .get(session_id)
            .map(std::string::String::as_str)
    }

    /// Returns a snapshot of compact live progress text by session id.
    pub fn session_progress_snapshot(&self) -> HashMap<String, String> {
        self.session_progress_messages.clone()
    }

    /// Deletes the selected session and schedules list refresh.
    pub async fn delete_selected_session(&mut self) {
        let session_id = self.selected_session().map(|session| session.id.clone());
        self.sessions
            .delete_selected_session(&self.projects, &self.services)
            .await;

        if let Some(session_id) = session_id {
            self.focused_review_cache.remove(&session_id);
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
            self.focused_review_cache.remove(&session_id);
        }

        self.process_pending_app_events().await;
        self.reload_projects().await;
    }

    /// Cancels a session in review status.
    ///
    /// # Errors
    /// Returns an error if the session is not found or not in review status.
    pub async fn cancel_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions
            .cancel_session(&self.services, session_id)
            .await
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
    pub(crate) async fn open_session_worktree_in_tmux_with_command(
        &self,
        open_command: Option<&str>,
    ) {
        let Some(session) = self.selected_session() else {
            return;
        };

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
        let db = self.services.db().clone();
        let event_sender = self.services.event_sender();
        let git_client = self.services.git_client();
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
                git_client,
                remote_branch_name,
            )
            .await;
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
        let usage_rows_result =
            self.services
                .db()
                .load_session_usage(session_id)
                .await
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
    pub async fn merge_session(&mut self, session_id: &str) -> Result<(), String> {
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
    pub async fn rebase_session(&self, session_id: &str) -> Result<(), String> {
        self.sessions
            .rebase_session(&self.services, session_id)
            .await
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
    pub(crate) fn start_focused_review_assist(
        &self,
        session_id: &str,
        session_folder: &Path,
        diff_hash: u64,
        focused_review_diff: &str,
        session_summary: Option<&str>,
    ) {
        task::TaskService::spawn_focused_review_assist_task(task::FocusedReviewAssistTaskInput {
            app_event_tx: self.services.event_sender(),
            diff_hash,
            focused_review_diff: focused_review_diff.to_string(),
            session_folder: session_folder.to_path_buf(),
            session_id: session_id.to_string(),
            review_model: self.settings.default_review_model,
            session_summary: session_summary.map(str::to_string),
        });
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
    }

    /// Reloads project list snapshots from persistence.
    async fn reload_projects(&mut self) {
        let project_items =
            Self::load_project_items(self.services.db(), self.services.fs_client().as_ref()).await;
        self.projects.replace_project_items(project_items);
    }

    /// Restarts git status polling for the currently active project context.
    fn restart_git_status_task(&self) {
        if !self.projects.has_git_branch() {
            return;
        }

        task::TaskService::spawn_git_status_task(
            self.projects.working_dir(),
            self.projects.git_status_cancel(),
            self.services.event_sender(),
            self.services.git_client(),
        );
    }

    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = self.drain_app_events(first_event);
        let event_batch = Self::reduce_app_events(drained_events);

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

    fn drain_app_events(&mut self, first_event: AppEvent) -> Vec<AppEvent> {
        let mut events = vec![first_event];
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }

        events
    }

    fn reduce_app_events(events: Vec<AppEvent>) -> AppEventBatch {
        let mut event_batch = AppEventBatch::default();
        for event in events {
            event_batch.collect_event(event);
        }

        event_batch
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
        }

        if event_batch.has_git_status_update {
            self.projects.set_git_status(event_batch.git_status_update);
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

        for (session_id, session_size) in event_batch.session_size_updates {
            self.sessions
                .apply_session_size_updated(&session_id, session_size);
        }

        for (session_id, entries) in event_batch.at_mention_entries_updates {
            self.apply_prompt_at_mention_entries(&session_id, entries);
        }

        self.apply_focused_review_updates(event_batch.focused_review_updates);

        if let Some(branch_publish_action_update) = event_batch.branch_publish_action_update {
            self.apply_branch_publish_action_update(branch_publish_action_update);
        }

        for (session_id, progress_message) in event_batch.session_progress_updates {
            if let Some(progress_message) = progress_message {
                self.session_progress_messages
                    .insert(session_id, progress_message);
            } else {
                self.session_progress_messages.remove(&session_id);
            }
        }

        for (session_id, response) in event_batch.agent_responses {
            self.apply_agent_response_received(&session_id, &response);
        }

        for session_id in &event_batch.session_ids {
            self.sessions.sync_session_from_handle(session_id);
        }
        self.sessions
            .clear_terminal_session_workers(&event_batch.session_ids);

        self.auto_start_focused_reviews(&event_batch.session_ids, &previous_session_states)
            .await;

        if let Some(sync_main_result) = event_batch.sync_main_result {
            let sync_popup_context = self.sync_popup_context();

            self.mode = Self::sync_main_popup_mode(sync_main_result, &sync_popup_context);
        }

        self.handle_merge_queue_progress(&event_batch.session_ids, &previous_session_states)
            .await;
        self.retain_valid_session_progress_messages();
    }

    /// Routes one structured agent response to the currently focused session
    /// UI.
    ///
    /// At the app layer, only `question` messages require explicit mode
    /// routing. `answer` messages are already appended to transcript output by
    /// the session worker before this event is handled.
    fn apply_agent_response_received(&mut self, session_id: &str, response: &AgentResponse) {
        let questions = response.question_items();
        if questions.is_empty() {
            return;
        }

        if let Some(session) = self
            .sessions
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.questions.clone_from(&questions);
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

    /// Applies loaded at-mention entries to the currently focused prompt
    /// session, if the mention query is still active.
    fn apply_prompt_at_mention_entries(&mut self, session_id: &str, entries: Vec<FileEntry>) {
        if let AppMode::Prompt {
            at_mention_state,
            input,
            session_id: prompt_session_id,
            ..
        } = &mut self.mode
        {
            if prompt_session_id != session_id || input.at_mention_query().is_none() {
                return;
            }

            if let Some(state) = at_mention_state.as_mut() {
                state.all_entries = entries;
                state.selected_index = 0;

                return;
            }

            *at_mention_state = Some(PromptAtMentionState::new(entries));
        }
    }

    /// Applies review assist updates for all sessions in the batch.
    fn apply_focused_review_updates(
        &mut self,
        focused_review_updates: HashMap<String, FocusedReviewUpdate>,
    ) {
        for (session_id, focused_review_update) in focused_review_updates {
            self.apply_focused_review_update(&session_id, focused_review_update);
        }
    }

    /// Applies review assist output to the active view/help mode when
    /// session identifiers still match and updates the persistent cache.
    fn apply_focused_review_update(
        &mut self,
        session_id: &str,
        focused_review_update: FocusedReviewUpdate,
    ) {
        let FocusedReviewUpdate { diff_hash, result } = focused_review_update;
        let Some(cache_entry) = self.focused_review_cache.get(session_id) else {
            return;
        };

        if cache_entry.diff_hash() != diff_hash {
            return;
        }

        match &result {
            Ok(review_text) => {
                self.focused_review_cache.insert(
                    session_id.to_string(),
                    FocusedReviewCacheEntry::Ready {
                        diff_hash,
                        text: review_text.clone(),
                    },
                );
            }
            Err(error) => {
                self.focused_review_cache.insert(
                    session_id.to_string(),
                    FocusedReviewCacheEntry::Failed {
                        diff_hash,
                        error: error.clone(),
                    },
                );
            }
        }

        match &mut self.mode {
            AppMode::View {
                focused_review_status_message,
                focused_review_text,
                session_id: view_session_id,
                ..
            } if view_session_id == session_id => {
                Self::apply_focused_review_result(
                    focused_review_status_message,
                    focused_review_text,
                    result,
                );
            }
            AppMode::Help {
                context:
                    HelpContext::View {
                        focused_review_status_message,
                        focused_review_text,
                        session_id: view_session_id,
                        ..
                    },
                ..
            } if view_session_id == session_id => {
                Self::apply_focused_review_result(
                    focused_review_status_message,
                    focused_review_text,
                    result,
                );
            }
            AppMode::OpenCommandSelector {
                restore_view:
                    ConfirmationViewMode {
                        focused_review_status_message,
                        focused_review_text,
                        session_id: view_session_id,
                        ..
                    },
                ..
            } if view_session_id == session_id => {
                Self::apply_focused_review_result(
                    focused_review_status_message,
                    focused_review_text,
                    result,
                );
            }
            AppMode::PublishBranchInput {
                restore_view:
                    ConfirmationViewMode {
                        focused_review_status_message,
                        focused_review_text,
                        session_id: view_session_id,
                        ..
                    },
                ..
            } if view_session_id == session_id => {
                Self::apply_focused_review_result(
                    focused_review_status_message,
                    focused_review_text,
                    result,
                );
            }
            AppMode::ViewInfoPopup {
                restore_view:
                    ConfirmationViewMode {
                        focused_review_status_message,
                        focused_review_text,
                        session_id: view_session_id,
                        ..
                    },
                ..
            } if view_session_id == session_id => {
                Self::apply_focused_review_result(
                    focused_review_status_message,
                    focused_review_text,
                    result,
                );
            }
            AppMode::List
            | AppMode::Confirmation { .. }
            | AppMode::SyncBlockedPopup { .. }
            | AppMode::Prompt { .. }
            | AppMode::Question { .. }
            | AppMode::Diff { .. }
            | AppMode::Help { .. }
            | AppMode::OpenCommandSelector { .. }
            | AppMode::PublishBranchInput { .. }
            | AppMode::ViewInfoPopup { .. }
            | AppMode::View { .. } => {}
        }
    }

    /// Applies one review assist result to render-state fields.
    fn apply_focused_review_result(
        focused_review_status_message: &mut Option<String>,
        focused_review_text: &mut Option<String>,
        result: Result<String, String>,
    ) {
        match result {
            Ok(review_text) => {
                *focused_review_status_message = None;
                *focused_review_text = Some(review_text);
            }
            Err(error) => {
                *focused_review_status_message =
                    Some(format!("Review assist unavailable: {}", error.trim()));
                *focused_review_text = None;
            }
        }
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
                review_request_creation_url,
                upstream_reference,
            }) => {
                self.sessions
                    .apply_published_upstream_ref(&session_id, upstream_reference);

                Self::view_info_popup_mode(
                    Self::branch_publish_success_title(PublishBranchAction::Push),
                    Self::branch_publish_success_message(
                        &branch_name,
                        review_request_creation_url.as_deref(),
                    ),
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

    /// Detects sessions that just transitioned to `Review` and automatically
    /// starts focused review generation so the review text is ready when the
    /// user presses `f`.
    ///
    /// Also invalidates cached reviews when a session returns to
    /// `InProgress` (user sent a follow-up reply).
    async fn auto_start_focused_reviews(
        &mut self,
        session_ids: &HashSet<String>,
        previous_session_states: &HashMap<String, Status>,
    ) {
        for session_id in session_ids {
            let Some(session) = self
                .sessions
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
            else {
                continue;
            };

            let current_status = session.status;
            let previous_status = previous_session_states.get(session_id).copied();

            if current_status == Status::InProgress {
                self.focused_review_cache.remove(session_id);

                continue;
            }

            let transitioned_to_review = current_status == Status::Review
                && matches!(previous_status, Some(Status::InProgress));

            if !transitioned_to_review {
                continue;
            }

            let session_folder = session.folder.clone();
            let base_branch = session.base_branch.clone();
            let session_summary = session.summary.clone();

            let diff = self
                .services
                .git_client()
                .diff(session_folder.clone(), base_branch)
                .await
                .unwrap_or_default();

            if diff.trim().is_empty() || diff.starts_with("Failed to run git diff:") {
                continue;
            }

            let new_hash = diff_content_hash(&diff);

            let existing_hash =
                self.focused_review_cache
                    .get(session_id)
                    .and_then(|entry| match entry {
                        FocusedReviewCacheEntry::Ready { .. } => Some(entry.diff_hash()),
                        _ => None,
                    });

            if existing_hash == Some(new_hash) {
                continue;
            }

            self.focused_review_cache.insert(
                session_id.clone(),
                FocusedReviewCacheEntry::Loading {
                    diff_hash: new_hash,
                },
            );
            self.start_focused_review_assist(
                session_id,
                &session_folder,
                new_hash,
                &diff,
                session_summary.as_deref(),
            );
        }
    }

    /// Validates whether a session is currently eligible for merge queueing.
    ///
    /// Sessions are eligible while actively under review or already marked as
    /// `Queued` (for example, after app restart).
    ///
    /// # Errors
    /// Returns an error when the session does not exist or has an ineligible
    /// status.
    fn validate_merge_request(&self, session_id: &str) -> Result<(), String> {
        let session = self.sessions.session_or_err(session_id)?;
        if !matches!(session.status, Status::Review | Status::Queued) {
            return Err("Session must be in review or queued status".to_string());
        }

        Ok(())
    }

    /// Marks one session as waiting in the merge queue.
    ///
    /// # Errors
    /// Returns an error when status transition to `Queued` is invalid.
    async fn mark_session_as_queued_for_merge(&self, session_id: &str) -> Result<(), String> {
        let handles = self.sessions.session_handles_or_err(session_id)?;
        let app_event_tx = self.services.event_sender();
        let status_updated = SessionTaskService::update_status(
            handles.status.as_ref(),
            self.services.db(),
            &app_event_tx,
            session_id,
            Status::Queued,
        )
        .await;

        if !status_updated {
            return Err("Invalid status transition to Queued".to_string());
        }

        Ok(())
    }

    /// Restores a queued session to `Review` if merge start fails.
    async fn restore_queued_session_to_review(&self, session_id: &str) {
        let session_status = self
            .sessions
            .session_or_err(session_id)
            .map(|session| session.status);
        if session_status != Ok(Status::Queued) {
            return;
        }

        let Ok(handles) = self.sessions.session_handles_or_err(session_id) else {
            return;
        };
        let app_event_tx = self.services.event_sender();
        let _ = SessionTaskService::update_status(
            handles.status.as_ref(),
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
    async fn start_next_merge_from_queue(&mut self, stop_on_failure: bool) -> Result<(), String> {
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
                        return Err(error);
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
            let _ = self.start_next_merge_from_queue(false).await;
        }
    }

    fn retain_valid_session_progress_messages(&mut self) {
        self.session_progress_messages.retain(|session_id, _| {
            self.sessions
                .sessions
                .iter()
                .find(|session| session.id == *session_id)
                .is_some_and(|session| {
                    matches!(
                        session.status,
                        Status::InProgress | Status::Rebasing | Status::Merging
                    )
                })
        });
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
        match publish_branch_action {
            PublishBranchAction::Push => "Pushing branch".to_string(),
        }
    }

    /// Returns the loading popup body for one branch-publish action.
    fn branch_publish_loading_message(
        publish_branch_action: PublishBranchAction,
        remote_branch_name: Option<&str>,
    ) -> String {
        match (publish_branch_action, remote_branch_name) {
            (PublishBranchAction::Push, Some(remote_branch_name)) => format!(
                "Publishing the session branch to `{remote_branch_name}` on the configured Git \
                 remote."
            ),
            (PublishBranchAction::Push, None) => {
                "Publishing the session branch to the configured Git remote.".to_string()
            }
        }
    }

    /// Returns the loading spinner label for one branch-publish action.
    fn branch_publish_loading_label(publish_branch_action: PublishBranchAction) -> String {
        match publish_branch_action {
            PublishBranchAction::Push => "Pushing branch...".to_string(),
        }
    }

    /// Returns the success popup title for a completed branch-publish action.
    fn branch_publish_success_title(publish_branch_action: PublishBranchAction) -> String {
        match publish_branch_action {
            PublishBranchAction::Push => "Branch pushed".to_string(),
        }
    }

    /// Returns the success popup body for one completed branch push.
    fn branch_publish_success_message(
        branch_name: &str,
        review_request_creation_url: Option<&str>,
    ) -> String {
        match review_request_creation_url {
            Some(review_request_creation_url) => format!(
                "Pushed session branch `{branch_name}`.\n\nOpen this link to create the pull \
                 request or merge request:\n{review_request_creation_url}"
            ),
            None => format!(
                "Pushed session branch `{branch_name}`.\n\nCreate the pull request or merge \
                 request manually from your forge UI."
            ),
        }
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

    /// Resolves startup project state and persists the active project metadata.
    ///
    /// Persisted projects whose directories no longer exist are ignored so
    /// startup falls back to the current working directory instead of reviving
    /// stale project entries.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted.
    async fn load_startup_project_context(
        db: &Database,
        fs_client: &dyn FsClient,
        git_client: &Arc<dyn GitClient>,
        working_dir: &Path,
        git_branch: Option<String>,
        current_project_id: i64,
    ) -> Result<(i64, PathBuf, Option<String>, Vec<ProjectListItem>, String), String> {
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
        let active_project_id = db
            .upsert_project(
                &startup_working_dir.to_string_lossy(),
                startup_git_branch.as_deref(),
            )
            .await
            .map_err(|error| {
                format!(
                    "Failed to persist active startup project `{}`: {error}",
                    startup_working_dir.display()
                )
            })?;
        db.set_active_project_id(active_project_id)
            .await
            .map_err(|error| {
                format!(
                    "Failed to store active startup project `{}`: {error}",
                    startup_working_dir.display()
                )
            })?;
        db.touch_project_last_opened(active_project_id)
            .await
            .map_err(|error| {
                format!(
                    "Failed to update startup project activity for `{}`: {error}",
                    startup_working_dir.display()
                )
            })?;
        let project_items = Self::load_project_items(db, fs_client).await;
        let active_project_name =
            Self::project_title_for_id(&project_items, active_project_id, &startup_working_dir);

        Ok((
            active_project_id,
            startup_working_dir,
            startup_git_branch,
            project_items,
            active_project_name,
        ))
    }

    /// Resolves startup active project id from settings, falling back to the
    /// current working directory when the stored project row is stale.
    async fn resolve_startup_active_project_id(
        db: &Database,
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

    /// Loads one project and falls back to current working directory snapshot.
    async fn load_project(
        db: &Database,
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

    /// Loads project list entries for the projects tab.
    ///
    /// Repositories discovered in the user home directory are upserted first
    /// so the list can include projects even before they have sessions.
    ///
    /// Agentty-managed session worktrees and missing project directories are
    /// excluded so the list keeps only user-facing repository roots that still
    /// exist on disk.
    async fn load_project_items(db: &Database, fs_client: &dyn FsClient) -> Vec<ProjectListItem> {
        let session_worktree_root = agentty_home().join(AGENTTY_WT_DIR);
        Self::load_projects_from_home_directory(db, session_worktree_root.as_path()).await;

        Self::visible_project_rows(
            db.load_projects_with_stats().await.unwrap_or_default(),
            fs_client,
            &session_worktree_root,
        )
        .into_iter()
        .map(Self::project_list_item_from_row)
        .collect()
    }

    /// Discovers git repositories under the user home directory and persists
    /// them so the project list can render them.
    async fn load_projects_from_home_directory(db: &Database, session_worktree_root: &Path) {
        let Some(home_directory) = dirs::home_dir() else {
            return;
        };

        let session_worktree_root = session_worktree_root.to_path_buf();
        let Ok(discovered_project_paths) = tokio::task::spawn_blocking(move || {
            Self::discover_home_project_paths(
                home_directory.as_path(),
                session_worktree_root.as_path(),
            )
        })
        .await
        else {
            return;
        };

        for project_path in discovered_project_paths {
            let git_branch = detect_git_info(project_path.clone()).await;
            let project_path = project_path.to_string_lossy().to_string();
            let _ = db
                .upsert_project(project_path.as_str(), git_branch.as_deref())
                .await;
        }
    }

    /// Returns git repository roots discovered under the user home directory.
    ///
    /// A repository root is identified by a direct `.git` marker inside the
    /// directory and discovery stops after `HOME_PROJECT_SCAN_MAX_RESULTS`.
    fn discover_home_project_paths(
        home_directory: &Path,
        session_worktree_root: &Path,
    ) -> Vec<PathBuf> {
        let mut discovered_project_paths = Vec::new();

        let mut walker_builder = WalkBuilder::new(home_directory);
        walker_builder
            .max_depth(Some(HOME_PROJECT_SCAN_MAX_DEPTH))
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            .ignore(true);
        let walker = walker_builder.build();

        for directory_entry in walker.flatten() {
            let Some(file_type) = directory_entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }

            let directory_path = directory_entry.path();
            if directory_path == home_directory {
                continue;
            }
            if Self::is_session_worktree_project_path(
                directory_path.to_string_lossy().as_ref(),
                session_worktree_root,
            ) {
                continue;
            }
            if !directory_path.join(".git").exists() {
                continue;
            }

            discovered_project_paths.push(directory_path.to_path_buf());
            if discovered_project_paths.len() >= HOME_PROJECT_SCAN_MAX_RESULTS {
                break;
            }
        }

        discovered_project_paths.sort();
        discovered_project_paths.dedup();

        discovered_project_paths
    }

    /// Returns whether a persisted project path points to an agentty session
    /// worktree under `~/.agentty/wt`.
    fn is_session_worktree_project_path(project_path: &str, session_worktree_root: &Path) -> bool {
        Path::new(project_path).starts_with(session_worktree_root)
    }

    /// Filters persisted project rows down to entries that should remain
    /// visible in the Projects tab.
    fn visible_project_rows(
        project_rows: Vec<db::ProjectListRow>,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<db::ProjectListRow> {
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
    /// directory on disk.
    fn is_existing_project_path(fs_client: &dyn FsClient, project_path: &str) -> bool {
        fs_client.is_dir(PathBuf::from(project_path))
    }

    /// Converts a project row into domain project model.
    fn project_from_row(project_row: db::ProjectRow) -> Project {
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
    fn project_list_item_from_row(project_row: db::ProjectListRow) -> ProjectListItem {
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

    /// Resolves active project title for startup rendering.
    fn project_title_for_id(
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

    /// Returns loading-state popup copy for sync-main operation.
    fn sync_loading_message() -> String {
        "Synchronizing with its upstream.".to_string()
    }
}

/// Executes one background branch-publish action for a session snapshot.
async fn run_branch_publish_action(
    publish_branch_action: PublishBranchAction,
    branch_publish_session: BranchPublishTaskSession,
    db: db::Database,
    git_client: Arc<dyn GitClient>,
    remote_branch_name: Option<String>,
) -> BranchPublishTaskResult {
    match publish_branch_action {
        PublishBranchAction::Push => {
            push_session_branch(
                &branch_publish_session,
                db,
                git_client,
                remote_branch_name.as_deref(),
            )
            .await
        }
    }
}

/// Pushes one session branch to the configured Git remote.
async fn push_session_branch(
    branch_publish_session: &BranchPublishTaskSession,
    db: db::Database,
    git_client: Arc<dyn GitClient>,
    remote_branch_name: Option<&str>,
) -> BranchPublishTaskResult {
    if branch_publish_session.status != Status::Review {
        return Err(BranchPublishTaskFailure::failed(
            "Session must be in review to push the branch.".to_string(),
        ));
    }

    let folder = branch_publish_session.folder.clone();
    let branch_name = remote_branch_name.map_or_else(
        || session::session_branch(&branch_publish_session.id),
        str::to_string,
    );
    let upstream_reference = match remote_branch_name {
        Some(remote_branch_name) => {
            git_client
                .push_current_branch_to_remote_branch(folder, remote_branch_name.to_string())
                .await
        }
        None => git_client.push_current_branch(folder).await,
    }
    .map_err(|error| branch_push_failure(&error))?;

    db.update_session_published_upstream_ref(&branch_publish_session.id, Some(&upstream_reference))
        .await
        .map_err(|error| {
            BranchPublishTaskFailure::failed(format!(
                "Branch push succeeded, but Agentty could not persist the upstream reference: \
                 {error}"
            ))
        })?;
    let review_request_creation_url =
        branch_review_request_creation_url(branch_publish_session, git_client, &branch_name).await;

    Ok(BranchPublishTaskSuccess::Pushed {
        branch_name,
        review_request_creation_url,
        upstream_reference,
    })
}

/// Returns one forge-native review-request creation URL for a pushed session
/// branch when the repository remote maps to a supported forge.
async fn branch_review_request_creation_url(
    branch_publish_session: &BranchPublishTaskSession,
    git_client: Arc<dyn GitClient>,
    branch_name: &str,
) -> Option<String> {
    let repo_url = git_client
        .repo_url(branch_publish_session.folder.clone())
        .await
        .ok()?;
    let remote = forge::detect_remote(&repo_url).ok()?;

    remote
        .review_request_creation_url(branch_name, &branch_publish_session.base_branch)
        .ok()
}

/// Maps one branch-publish failure into blocked or failed popup copy.
fn branch_push_failure(error: &str) -> BranchPublishTaskFailure {
    if !is_git_push_authentication_error(error) {
        return BranchPublishTaskFailure::failed(format!(
            "Failed to publish session branch: {error}"
        ));
    }

    BranchPublishTaskFailure::blocked(git_push_authentication_message(
        None,
        "push the branch again",
    ))
}

/// Returns whether error output looks like a git push authentication failure.
fn is_git_push_authentication_error(detail_message: &str) -> bool {
    let normalized_detail = detail_message.to_ascii_lowercase();

    let is_push_context = normalized_detail.contains("git push failed")
        || (normalized_detail.contains("push")
            && (normalized_detail.contains("remote") || normalized_detail.contains("origin")));
    if !is_push_context {
        return false;
    }

    normalized_detail.contains("authentication failed")
        || normalized_detail.contains("terminal prompts disabled")
        || normalized_detail.contains("could not read username")
        || normalized_detail.contains("could not read password")
        || normalized_detail.contains("permission denied")
        || normalized_detail.contains("access denied")
        || normalized_detail.contains("not authorized")
        || normalized_detail.contains("support for password authentication was removed")
        || normalized_detail.contains("the requested url returned error: 403")
        || normalized_detail.contains("repository not found")
}

/// Attempts to infer one forge kind from a git push authentication failure.
fn detected_forge_kind_from_git_push_error(detail_message: &str) -> Option<forge::ForgeKind> {
    let normalized_detail = detail_message.to_ascii_lowercase();

    if let Some(forge_kind) = detected_forge_kind_from_push_auth_url(&normalized_detail) {
        return Some(forge_kind);
    }

    if normalized_detail.contains("github.com") || normalized_detail.contains(" gh ") {
        return Some(forge::ForgeKind::GitHub);
    }

    if normalized_detail.contains("gitlab") || normalized_detail.contains("glab") {
        return Some(forge::ForgeKind::GitLab);
    }

    None
}

/// Returns one forge family from the remote host shown in a credential prompt
/// error.
fn detected_forge_kind_from_push_auth_url(detail_message: &str) -> Option<forge::ForgeKind> {
    let host = extract_push_auth_prompt_host(detail_message)?;
    if host.is_empty() {
        return None;
    }

    let host = strip_port(host);
    if is_github_host(host) {
        return Some(forge::ForgeKind::GitHub);
    }

    if is_gitlab_host(host) {
        return Some(forge::ForgeKind::GitLab);
    }

    None
}

/// Returns whether `host` is a GitHub-style forge host.
fn is_github_host(host: &str) -> bool {
    host == "github.com" || host.ends_with(".github.com")
}

/// Returns whether `host` is a GitLab-style forge host.
fn is_gitlab_host(host: &str) -> bool {
    host == "gitlab.com"
        || host.ends_with(".gitlab.com")
        || host.split('.').any(|segment| segment == "gitlab")
}

/// Extracts one remote host from one `git push` authentication prompt.
fn extract_push_auth_prompt_host(detail_message: &str) -> Option<&str> {
    let username_marker = "could not read username for '";
    let password_marker = "could not read password for '";

    if let Some(host) = extract_host_from_prompt(detail_message, username_marker) {
        return Some(host);
    }

    extract_host_from_prompt(detail_message, password_marker)
}

/// Extracts the host payload from one quoted credential-prompt URL.
fn extract_host_from_prompt<'detail>(
    detail_message: &'detail str,
    marker: &str,
) -> Option<&'detail str> {
    let marker_start = detail_message.find(marker)?;
    let quoted_host = &detail_message[marker_start + marker.len()..];
    let host = quoted_host.split('\'').next()?;
    let host = host.trim().trim_end_matches('/');
    let host = host
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.split('/').next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);

    Some(host)
}

/// Removes one explicit host port, if present.
fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

/// Returns actionable copy for one git push authentication failure.
fn git_push_authentication_message(
    forge_kind: Option<forge::ForgeKind>,
    retry_action: &str,
) -> String {
    match forge_kind {
        Some(forge::ForgeKind::GitHub) => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nRun `gh auth login`, or configure credentials with a PAT/SSH key."
        ),
        Some(forge::ForgeKind::GitLab) => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nRun `glab auth login` for GitLab CLI access, and configure Git \
             credentials with a PAT/SSH key or credential helper."
        ),
        None => format!(
            "Git push requires authentication for this repository.\nAuthorize git access, then \
             {retry_action}.\nConfigure Git credentials with a PAT/SSH key or credential helper."
        ),
    }
}

impl AppEventBatch {
    fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                self.at_mention_entries_updates.insert(session_id, entries);
            }
            AppEvent::GitStatusUpdated { status } => {
                self.has_git_status_update = true;
                self.git_status_update = status;
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
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::SessionProgressUpdated {
                progress_message,
                session_id,
            } => {
                self.session_progress_updates
                    .insert(session_id, progress_message);
            }
            AppEvent::SyncMainCompleted { result } => {
                self.sync_main_result = Some(result);
            }
            AppEvent::SessionSizeUpdated {
                session_id,
                session_size,
            } => {
                self.session_size_updates.insert(session_id, session_size);
            }
            AppEvent::BranchPublishActionCompleted {
                restore_view,
                result,
                session_id,
            } => {
                self.branch_publish_action_update = Some(BranchPublishActionUpdate {
                    restore_view,
                    result: *result,
                    session_id,
                });
            }
            AppEvent::FocusedReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            } => {
                self.focused_review_updates.insert(
                    session_id,
                    FocusedReviewUpdate {
                        diff_hash,
                        result: Ok(review_text),
                    },
                );
            }
            AppEvent::FocusedReviewPreparationFailed {
                diff_hash,
                error,
                session_id,
            } => {
                self.focused_review_updates.insert(
                    session_id,
                    FocusedReviewUpdate {
                        diff_hash,
                        result: Err(error),
                    },
                );
            }
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
            AppEvent::AgentResponseReceived {
                response,
                session_id,
            } => {
                self.agent_responses.insert(session_id, response);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use mockall::predicate::eq;
    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{SESSION_DATA_DIR, Session, SessionSize, SessionStats, Status};
    use crate::domain::setting::SettingName;
    use crate::infra::agent::protocol::AgentResponseMessage;
    use crate::infra::db::Database;
    use crate::infra::file_index::FileEntry;
    use crate::infra::tmux::{MockTmuxClient, TmuxClient};
    use crate::ui::state::app_mode::DoneSessionOutputMode;

    /// Builds one mock app-server client wrapped in `Arc`.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(app_server::MockAppServerClient::new())
    }

    /// Builds one deterministic session snapshot rooted at `session_folder`.
    fn test_session(session_folder: PathBuf) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: session_folder,
            id: "session-1".to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "test-project".to_string(),
            prompt: "test prompt".to_string(),
            published_upstream_ref: None,
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

    /// Builds a test app rooted at one temporary workspace with an injected
    /// tmux boundary.
    async fn new_test_app_with_tmux_client(tmux_client: Arc<dyn TmuxClient>) -> App {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let clients = AppClients::new(mock_app_server()).with_tmux_client(tmux_client);

        App::new_with_clients(base_path.clone(), base_path, None, database, clients)
            .await
            .expect("failed to build test app")
    }

    /// Builds a test app rooted at one temporary workspace with a mocked tmux
    /// boundary.
    async fn new_test_app() -> App {
        new_test_app_with_tmux_client(Arc::new(MockTmuxClient::new())).await
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
                SettingName::DefaultSmartModel.as_str(),
                AgentModel::ClaudeHaiku4520251001.as_str(),
            )
            .await
            .expect("failed to persist first project smart model");
        database
            .upsert_project_setting(
                first_project_id,
                SettingName::OpenCommand.as_str(),
                "npm run dev",
            )
            .await
            .expect("failed to persist first project open command");
        database
            .upsert_project_setting(
                second_project_id,
                SettingName::DefaultSmartModel.as_str(),
                AgentModel::Gpt53Codex.as_str(),
            )
            .await
            .expect("failed to persist second project smart model");
        database
            .upsert_project_setting(
                second_project_id,
                SettingName::OpenCommand.as_str(),
                "cargo test",
            )
            .await
            .expect("failed to persist second project open command");
        database
            .set_active_project_id(first_project_id)
            .await
            .expect("failed to persist initial active project");
        let mut app = App::new(
            true,
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await
        .expect("failed to build app");

        // Act
        app.switch_project(second_project_id)
            .await
            .expect("failed to switch project");

        // Assert
        assert_eq!(app.settings.default_smart_model, AgentModel::Gpt53Codex);
        assert_eq!(app.settings.open_command, "cargo test");
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
        let app = App::new(
            true,
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
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
        let error = App::new(
            true,
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await
        .err()
        .expect("expected startup project upsert failure");

        // Assert
        assert!(error.contains("Failed to persist startup project"));
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
        let error = App::new(
            true,
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await
        .err()
        .expect("expected startup active project persistence failure");

        // Assert
        assert!(error.contains("Failed to store active startup project"));
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
            focused_review_status_message: None,
            focused_review_text: None,
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
            Some("https://github.com/org/repo/compare/main...agentty%2Fsession-1?expand=1"),
        );
        let fallback_success_message =
            App::branch_publish_success_message("agentty/session-1", None);
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
        assert!(
            success_message.contains("Open this link to create the pull request or merge request")
        );
        assert!(
            success_message.contains(
                "https://github.com/org/repo/compare/main...agentty%2Fsession-1?expand=1"
            )
        );
        assert!(
            fallback_success_message.contains("Create the pull request or merge request manually")
        );
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

    #[test]
    fn branch_push_failure_maps_blocked_and_failed_errors() {
        // Arrange
        let auth_error =
            "Git push failed: fatal: could not read Username for 'https://github.com': terminal \
             prompts disabled";
        let failed_error = "remote rejected";

        // Act
        let blocked = branch_push_failure(auth_error);
        let failed = branch_push_failure(failed_error);

        // Assert
        assert_eq!(blocked.title, "Branch push blocked");
        assert!(blocked.message.contains("Configure Git credentials"));
        assert_eq!(failed.title, "Branch push failed");
        assert!(
            failed
                .message
                .contains("Failed to publish session branch: remote rejected")
        );
    }

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
                    Err("Git push failed: fatal: could not read Username for \
                         'https://github.com': terminal prompts disabled"
                        .to_string())
                })
            });
        let git_client: Arc<dyn crate::infra::git::GitClient> = Arc::new(mock_git_client);
        let database = crate::infra::db::Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let result = push_session_branch(&branch_session, database, git_client, None).await;

        // Assert
        assert!(matches!(
            result,
            Err(BranchPublishTaskFailure {
                ref title,
                ref message,
            }) if title == "Branch push blocked"
                && message.contains("Configure Git credentials")
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
            app.services.git_client(),
            None,
        )
        .await;
        let helper_result = push_session_branch(
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
                "Session must be in review to push the branch.".to_string(),
            ))
        );
        assert_eq!(
            helper_result,
            Err(BranchPublishTaskFailure::failed(
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
                review_request_creation_url: Some(
                    "https://github.com/agentty-xyz/agentty/compare/main...review%2Fcustom-branch?expand=1"
                        .to_string()
                ),
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
        let result = push_session_branch(&branch_session, database, git_client, None).await;

        // Assert
        assert_eq!(
            result,
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: session::session_branch("session-1"),
                review_request_creation_url: None,
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
            focused_review_status_message: None,
            focused_review_text: None,
            scroll_offset: Some(1),
            session_id: "session-1".to_string(),
        };

        // Act
        app.apply_branch_publish_action_update(BranchPublishActionUpdate {
            restore_view: expected_restore_view.clone(),
            result: Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: "agentty/session-1".to_string(),
                review_request_creation_url: Some(
                    "https://github.com/agentty-xyz/agentty/compare/main...agentty%2Fsession-1?expand=1"
                        .to_string()
                ),
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
    fn sync_main_popup_mode_gitlab_auth_failure_shows_gitlab_guidance() {
        // Arrange
        let sync_popup_context = SyncPopupContext {
            default_branch: "main".to_string(),
            project_name: "agentty".to_string(),
        };
        let sync_error = SyncSessionStartError::Other(
            "Git push failed: fatal: could not read Username for 'https://gitlab.com': terminal \
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
                && message.contains("`glab auth login`")
                && message.contains("credential helper")
                && project_name.as_deref() == Some("agentty")
        ));
    }

    #[test]
    fn sync_push_auth_error_detects_gitlab_from_prompt_url() {
        // Arrange
        let detail = "Git push failed: fatal: could not read Username for 'https://gitlab.com/team/project': terminal \
                        prompts disabled\nConfigured remotes:\n  github.com";

        // Act
        let forge_kind = detected_forge_kind_from_git_push_error(detail);

        // Assert
        assert_eq!(forge_kind, Some(forge::ForgeKind::GitLab));
    }

    #[test]
    fn sync_push_auth_error_detects_gitlab_from_prompt_url_with_userinfo() {
        // Arrange
        let detail = "Git push failed: fatal: could not read Username for 'https://deploy-user@gitlab.example.com/team/project': terminal \
                        prompts disabled\nConfigured remotes:\n  github.com";

        // Act
        let forge_kind = detected_forge_kind_from_git_push_error(detail);

        // Assert
        assert_eq!(forge_kind, Some(forge::ForgeKind::GitLab));
    }

    #[test]
    fn sync_push_auth_error_detects_github_from_prompt_url() {
        // Arrange
        let detail = "Git push failed: fatal: could not read Password for 'https://github.com/team/project': terminal \
                        prompts disabled\nConfigured remotes:\n  gitlab.com";

        // Act
        let forge_kind = detected_forge_kind_from_git_push_error(detail);

        // Assert
        assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
    }

    #[test]
    fn sync_push_auth_error_prefers_github_when_fallback_markers_are_ambiguous() {
        // Arrange
        let detail = "Git push failed: authentication failed. Configure remotes:\n  github.com\n  \
                      gitlab.com";

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
    fn app_event_batch_collect_event_keeps_latest_agent_response_update() {
        // Arrange
        let mut event_batch = AppEventBatch::default();
        let latest_response = AgentResponse {
            messages: vec![
                AgentResponseMessage::question("Need branch?"),
                AgentResponseMessage::question("Need tests?"),
            ],
        };

        // Act
        event_batch.collect_event(AppEvent::AgentResponseReceived {
            response: AgentResponse {
                messages: vec![AgentResponseMessage::question("Old question")],
            },
            session_id: "session-1".to_string(),
        });
        event_batch.collect_event(AppEvent::AgentResponseReceived {
            response: latest_response.clone(),
            session_id: "session-1".to_string(),
        });

        // Assert
        assert_eq!(
            event_batch.agent_responses.get("session-1").cloned(),
            Some(latest_response)
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

        // Assert — last event wins
        assert_eq!(
            event_batch.update_status,
            Some(UpdateStatus::Complete {
                version: "v1.0.0".to_string()
            })
        );
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
    async fn apply_app_events_agent_response_switches_view_mode_to_question_mode() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-question-view")));
        app.mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            session_id: "session-1".to_string(),
            scroll_offset: None,
        };
        let response = AgentResponse {
            messages: vec![
                AgentResponseMessage::question_with_options(
                    "Need a target branch?",
                    vec!["main".to_string(), "develop".to_string()],
                ),
                AgentResponseMessage::question_with_options(
                    "Need integration tests?",
                    vec!["Yes".to_string(), "No".to_string()],
                ),
            ],
        };
        let expected_questions = response.question_items();

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            response,
            session_id: "session-1".to_string(),
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
        let response = AgentResponse {
            messages: vec![AgentResponseMessage::question("Need context?")],
        };

        // Act
        app.apply_app_events(AppEvent::AgentResponseReceived {
            response,
            session_id: "session-1".to_string(),
        })
        .await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
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

        let mut app = App::new(
            true,
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
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
            .update_session_status("session-active", &Status::Done.to_string())
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
    fn install_mock_git_client(
        app: &mut App,
        mock_git_client: crate::infra::git::MockGitClient,
    ) {
        let mock_git_client: Arc<dyn crate::infra::git::GitClient> = Arc::new(mock_git_client);
        let base_path = app.services.base_path().to_path_buf();
        let db = app.services.db().clone();
        let event_sender = app.services.event_sender();
        let app_server_client = app.services.app_server_client();
        let fs_client = app.services.fs_client();
        let review_request_client = app.services.review_request_client();

        app.services = AppServices::new(
            base_path,
            db,
            event_sender,
            fs_client,
            Arc::clone(&mock_git_client),
            review_request_client,
            app_server_client,
        );
    }

    #[tokio::test]
    async fn apply_focused_review_update_stores_success_in_cache() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-cache";
        let review_text = "## Review\nLooks good.";
        app.focused_review_cache.insert(
            session_id.to_string(),
            FocusedReviewCacheEntry::Loading { diff_hash: 123 },
        );

        // Act
        app.apply_focused_review_update(
            session_id,
            FocusedReviewUpdate {
                diff_hash: 123,
                result: Ok(review_text.to_string()),
            },
        );

        // Assert
        assert!(matches!(
            app.focused_review_cache.get(session_id),
            Some(FocusedReviewCacheEntry::Ready { text, diff_hash }) if text == review_text && *diff_hash == 123
        ));
    }

    #[tokio::test]
    async fn apply_focused_review_update_stores_failure_in_cache() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-fail";
        let error_message = "Review assist failed with exit code 1";
        app.focused_review_cache.insert(
            session_id.to_string(),
            FocusedReviewCacheEntry::Loading { diff_hash: 456 },
        );

        // Act
        app.apply_focused_review_update(
            session_id,
            FocusedReviewUpdate {
                diff_hash: 456,
                result: Err(error_message.to_string()),
            },
        );

        // Assert
        assert!(matches!(
            app.focused_review_cache.get(session_id),
            Some(FocusedReviewCacheEntry::Failed { error, diff_hash }) if error == error_message && *diff_hash == 456
        ));
    }

    #[tokio::test]
    async fn apply_focused_review_update_ignores_stale_diff_hash() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-review-stale";
        app.focused_review_cache.insert(
            session_id.to_string(),
            FocusedReviewCacheEntry::Loading { diff_hash: 999 },
        );

        // Act
        app.apply_focused_review_update(
            session_id,
            FocusedReviewUpdate {
                diff_hash: 111,
                result: Ok("stale review".to_string()),
            },
        );

        // Assert
        assert!(matches!(
            app.focused_review_cache.get(session_id),
            Some(FocusedReviewCacheEntry::Loading { diff_hash }) if *diff_hash == 999
        ));
    }

    #[tokio::test]
    async fn auto_start_focused_reviews_clears_cache_on_in_progress_transition() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-cache-clear")));
        app.sessions.sessions[0].status = Status::InProgress;
        app.focused_review_cache.insert(
            session_id.to_string(),
            FocusedReviewCacheEntry::Ready {
                diff_hash: 789,
                text: "old review".to_string(),
            },
        );
        let session_ids = HashSet::from([session_id.to_string()]);
        let previous_states = HashMap::from([(session_id.to_string(), Status::Review)]);

        // Act
        app.auto_start_focused_reviews(&session_ids, &previous_states)
            .await;

        // Assert
        assert!(!app.focused_review_cache.contains_key(session_id));
    }

    #[tokio::test]
    async fn auto_start_focused_reviews_skips_when_diff_hash_unchanged() {
        // Arrange
        let mut app = new_test_app().await;
        let session_id = "session-1";
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-hash-skip")));
        app.sessions.sessions[0].status = Status::Review;

        let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
        let hash = diff_content_hash(diff_text);
        app.focused_review_cache.insert(
            session_id.to_string(),
            FocusedReviewCacheEntry::Ready {
                diff_hash: hash,
                text: "existing review".to_string(),
            },
        );
        let session_ids = HashSet::from([session_id.to_string()]);
        let previous_states = HashMap::from([(session_id.to_string(), Status::InProgress)]);

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.auto_start_focused_reviews(&session_ids, &previous_states)
            .await;

        // Assert — cache should remain unchanged (review not regenerated)
        assert!(matches!(
            app.focused_review_cache.get(session_id),
            Some(FocusedReviewCacheEntry::Ready { text, .. }) if text == "existing review"
        ));
    }

    #[tokio::test]
    async fn auto_start_focused_reviews_starts_loading_for_review_transition() {
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
        let previous_states = HashMap::from([(session_id.to_string(), Status::InProgress)]);

        let mut mock_git_client = crate::infra::git::MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
        install_mock_git_client(&mut app, mock_git_client);

        // Act
        app.auto_start_focused_reviews(&session_ids, &previous_states)
            .await;

        // Assert
        assert!(matches!(
            app.focused_review_cache.get(session_id),
            Some(FocusedReviewCacheEntry::Loading { diff_hash }) if *diff_hash == expected_hash
        ));
    }

    #[tokio::test]
    async fn delete_selected_session_clears_focused_review_cache() {
        // Arrange
        let mut app = new_test_app().await;
        app.sessions
            .sessions
            .push(test_session(PathBuf::from("/tmp/session-delete-cache")));
        app.sessions.table_state.select(Some(0));
        let session_id = app.sessions.sessions[0].id.clone();
        app.focused_review_cache.insert(
            session_id.clone(),
            FocusedReviewCacheEntry::Ready {
                diff_hash: 42,
                text: "cached review".to_string(),
            },
        );

        // Act
        app.delete_selected_session().await;

        // Assert
        assert!(!app.focused_review_cache.contains_key(&session_id));
    }
}
