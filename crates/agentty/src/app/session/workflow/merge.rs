//! Merge, rebase, and cleanup workflows for session branches.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use askama::Template;
use tokio::sync::mpsc;

use super::access::{SESSION_HANDLES_NOT_FOUND_ERROR, SESSION_NOT_FOUND_ERROR};
use super::{SessionTaskService, session_branch};
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::Status;
use crate::infra::agent;
use crate::infra::agent::protocol::AgentResponseSummary;
use crate::infra::db::Database;
use crate::infra::fs::{self as fs, FsClient};
use crate::infra::git::{self as git, GitClient};

const REBASE_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 3,
    // Allow up to 3 consecutive identical-content observations before
    // giving up, so the agent gets a genuine second chance when partial
    // progress is made inside a file without fully clearing all markers.
    max_identical_failure_streak: 3,
};

/// Coordinates merge/rebase session workflows behind a dedicated service
/// boundary.
pub(crate) struct SessionMergeService;

/// Askama view model for rendering rebase conflict-assistance prompts.
#[derive(Template)]
#[template(path = "rebase_assist_prompt.md", escape = "none")]
struct RebaseAssistPromptTemplate<'a> {
    base_branch: &'a str,
    conflicted_files: &'a str,
}

/// Boxed async result used by sync conflict assistance boundary methods.
type SyncAssistFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

struct MergeTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    child_pid: Arc<Mutex<Option<u32>>>,
    db: Database,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    id: String,
    output: Arc<Mutex<String>>,
    repo_root: PathBuf,
    session_model: AgentModel,
    source_branch: String,
    status: Arc<Mutex<Status>>,
}

#[derive(Clone)]
struct RebaseAssistInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    child_pid: Arc<Mutex<Option<u32>>>,
    db: Database,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    id: String,
    output: Arc<Mutex<String>>,
    session_model: AgentModel,
}

struct RebaseTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    child_pid: Arc<Mutex<Option<u32>>>,
    db: Database,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    id: String,
    output: Arc<Mutex<String>>,
    session_model: AgentModel,
    status: Arc<Mutex<Status>>,
}

/// Input context for assisted conflict resolution during `sync main`.
struct SyncRebaseAssistInput {
    base_branch: String,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    session_model: AgentModel,
    sync_assist_client: Arc<dyn SyncAssistClient>,
}

/// Polymorphic input for shared assisted rebase loop orchestration.
enum RebaseAssistLoopInput {
    Session(RebaseAssistInput),
    Project(SyncRebaseAssistInput),
}

impl RebaseAssistLoopInput {
    /// Returns worktree path used for conflict fingerprinting.
    fn folder(&self) -> &Path {
        match self {
            Self::Session(input) => &input.folder,
            Self::Project(input) => &input.folder,
        }
    }

    /// Returns the filesystem boundary used by assist fingerprinting.
    fn fs_client(&self) -> &dyn FsClient {
        match self {
            Self::Session(input) => input.fs_client.as_ref(),
            Self::Project(input) => input.fs_client.as_ref(),
        }
    }

    /// Builds conflict-state no-progress error text for the active workflow.
    fn repeated_conflict_state_error(&self, detail: &str) -> String {
        match self {
            Self::Session(_) => format!(
                "Rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {detail}"
            ),
            Self::Project(_) => format!(
                "Sync rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {detail}"
            ),
        }
    }

    /// Builds unchanged-conflict-files no-progress error text for the active
    /// workflow.
    fn unchanged_conflict_files_error(&self) -> String {
        match self {
            Self::Session(_) => {
                "Rebase assistance made no progress: conflicted files did not change".to_string()
            }
            Self::Project(_) => "Sync rebase assistance made no progress: conflicted files did \
                                 not change"
                .to_string(),
        }
    }

    /// Builds unresolved-conflict-after-assistance error text for the active
    /// workflow.
    fn still_conflicted_error(&self, detail: &str) -> String {
        match self {
            Self::Session(_) => format!("Rebase still has conflicts after assistance: {detail}"),
            Self::Project(_) => {
                format!("Sync rebase still has conflicts after assistance: {detail}")
            }
        }
    }

    /// Returns final exhausted-attempts error text for the active workflow.
    fn exhausted_error(&self) -> String {
        match self {
            Self::Session(_) => "Failed to complete assisted rebase".to_string(),
            Self::Project(_) => "Failed to complete assisted sync rebase".to_string(),
        }
    }

    /// Loads conflicted files for the active workflow.
    ///
    /// # Errors
    /// Returns an error if conflicted-file inspection fails.
    async fn load_conflicted_files(
        &self,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, String> {
        match self {
            Self::Session(input) => {
                SessionManager::load_conflicted_files(input, previous_conflict_files).await
            }
            Self::Project(input) => {
                SessionManager::load_sync_conflicted_files(input, previous_conflict_files).await
            }
        }
    }

    /// Executes one assistance attempt for the active workflow.
    ///
    /// # Errors
    /// Returns an error when assistance command execution fails.
    async fn run_assist_attempt(
        &self,
        assist_attempt: usize,
        conflicted_files: &[String],
    ) -> Result<(), String> {
        match self {
            Self::Session(input) => {
                SessionManager::append_rebase_assist_header(
                    input,
                    assist_attempt,
                    conflicted_files,
                )
                .await;
                SessionManager::run_rebase_assist_agent(input, conflicted_files).await
            }
            Self::Project(input) => {
                SessionManager::run_sync_rebase_assist_agent(input, conflicted_files).await
            }
        }
    }

    /// Stages edits and checks whether conflicts remain for the active
    /// workflow.
    ///
    /// # Errors
    /// Returns an error when staging or conflict checks fail.
    async fn stage_and_check_for_conflicts(
        &self,
        conflict_files: &[String],
    ) -> Result<bool, String> {
        match self {
            Self::Session(input) => {
                SessionManager::stage_and_check_for_conflicts(input, conflict_files).await
            }
            Self::Project(input) => {
                SessionManager::stage_and_check_for_sync_conflicts(input, conflict_files).await
            }
        }
    }

    /// Continues in-progress rebase for the active workflow.
    ///
    /// # Errors
    /// Returns an error when `git rebase --continue` fails with non-conflict
    /// errors.
    async fn run_rebase_continue(&self) -> Result<git::RebaseStepResult, String> {
        match self {
            Self::Session(input) => SessionManager::run_rebase_continue(input).await,
            Self::Project(input) => SessionManager::run_sync_rebase_continue(input).await,
        }
    }

    /// Aborts rebase for the active workflow after assistance failure.
    async fn abort_rebase_after_assist_failure(&self) {
        match self {
            Self::Session(input) => {
                SessionManager::abort_rebase_after_assist_failure(input).await;
            }
            Self::Project(input) => {
                SessionManager::abort_sync_rebase_after_assist_failure(input).await;
            }
        }
    }
}

/// Async boundary for one sync rebase assistance attempt.
#[cfg_attr(test, mockall::automock)]
trait SyncAssistClient: Send + Sync {
    /// Executes one agent-assisted edit attempt for the provided rebase prompt.
    fn resolve_rebase_conflicts(
        &self,
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> SyncAssistFuture<Result<(), String>>;
}

/// Production sync-assistance executor backed by real agent commands.
struct RealSyncAssistClient;

impl RealSyncAssistClient {
    /// Runs one sync conflict assistance command through the shared one-shot
    /// agent submission path.
    ///
    /// # Errors
    /// Returns an error when the one-shot agent command fails.
    async fn run_assist_command(
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> Result<(), String> {
        let _ = agent::submit_one_shot(agent::OneShotRequest {
            child_pid: None,
            folder: &folder,
            model: session_model,
            prompt: &prompt,
            request_kind: crate::infra::channel::AgentRequestKind::UtilityPrompt,
            reasoning_level: ReasoningLevel::default(),
        })
        .await?;

        Ok(())
    }
}

impl SyncAssistClient for RealSyncAssistClient {
    fn resolve_rebase_conflicts(
        &self,
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> SyncAssistFuture<Result<(), String>> {
        Box::pin(async move { Self::run_assist_command(folder, prompt, session_model).await })
    }
}

/// User-facing reasons why repository branch sync cannot be started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SyncSessionStartError {
    /// Sync cannot run while the selected project branch has local
    /// modifications.
    MainHasUncommittedChanges { default_branch: String },
    /// Generic start failure outside sync-specific policy constraints.
    Other(String),
}

/// Summary of one completed main-branch sync run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncMainOutcome {
    /// Commit titles discovered upstream and pulled during sync.
    pub(crate) pulled_commit_titles: Vec<String>,
    /// Number of commits rebased from upstream into the local branch.
    pub(crate) pulled_commits: Option<u32>,
    /// Commit titles discovered locally and pushed during sync.
    pub(crate) pushed_commit_titles: Vec<String>,
    /// Number of local commits pushed to upstream during sync.
    pub(crate) pushed_commits: Option<u32>,
    /// Paths that were conflicted and resolved during assisted sync rebase.
    pub(crate) resolved_conflict_files: Vec<String>,
}

/// Successful assisted rebase completion details.
#[derive(Debug)]
struct RebaseAssistOutcome {
    resolved_conflict_files: Vec<String>,
}

impl RebaseAssistOutcome {
    /// Creates an empty assisted-rebase completion payload.
    fn empty() -> Self {
        Self {
            resolved_conflict_files: Vec::new(),
        }
    }

    /// Adds conflicted file names and keeps the list unique and sorted.
    fn extend_resolved_conflict_files(&mut self, conflict_files: &[String]) {
        for conflict_file in conflict_files {
            if !self.resolved_conflict_files.contains(conflict_file) {
                self.resolved_conflict_files.push(conflict_file.clone());
            }
        }

        self.resolved_conflict_files.sort_unstable();
    }
}

impl SyncSessionStartError {
    /// Returns user-facing detail for non-popup sync start errors.
    pub(crate) fn detail_message(&self) -> String {
        match self {
            Self::MainHasUncommittedChanges { default_branch } => format!(
                "Sync cannot run while `{default_branch}` has uncommitted changes.\nCommit or \
                 stash changes in `{default_branch}`, then try again."
            ),
            Self::Other(detail) => detail.clone(),
        }
    }
}

impl SessionMergeService {
    /// Starts a squash merge for a review-ready or queued session branch in
    /// the background.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the status transition to `Merging` fails.
    async fn merge_session(
        &self,
        manager: &SessionManager,
        session_id: &str,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<(), String> {
        let session = manager
            .session_or_err(session_id)
            .map_err(|_| SESSION_NOT_FOUND_ERROR.to_string())?;
        if !matches!(session.status, Status::Review | Status::Queued) {
            return Err("Session must be in review or queued status".to_string());
        }

        let db = services.db().clone();
        let folder = session.folder.clone();
        let id = session.id.clone();
        let session_model = session.model;
        let app_event_tx = services.event_sender();
        let fs_client = services.fs_client();
        let git_client = manager.git_client();

        let handles = manager
            .session_handles_or_err(session_id)
            .map_err(|_| SESSION_HANDLES_NOT_FOUND_ERROR.to_string())?;
        let child_pid = Arc::clone(&handles.child_pid);
        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);

        if !SessionTaskService::update_status(&status, &db, &app_event_tx, &id, Status::Merging)
            .await
        {
            return Err("Invalid status transition to Merging".to_string());
        }

        let base_branch = match db.get_session_base_branch(&id).await {
            Ok(Some(base_branch)) => base_branch,
            Ok(None) => {
                let _ = SessionTaskService::update_status(
                    &status,
                    &db,
                    &app_event_tx,
                    &id,
                    Status::Review,
                )
                .await;

                return Err("No git worktree for this session".to_string());
            }
            Err(error) => {
                let _ = SessionTaskService::update_status(
                    &status,
                    &db,
                    &app_event_tx,
                    &id,
                    Status::Review,
                )
                .await;

                return Err(error.to_string());
            }
        };

        let working_dir = projects.working_dir().to_path_buf();
        let Some(repo_root) = git_client.find_git_repo_root(working_dir).await else {
            let _ =
                SessionTaskService::update_status(&status, &db, &app_event_tx, &id, Status::Review)
                    .await;

            return Err("Failed to find git repository root".to_string());
        };

        let merge_task_input = MergeTaskInput {
            app_event_tx,
            base_branch,
            child_pid,
            db,
            folder,
            fs_client,
            git_client,
            id: id.clone(),
            output,
            repo_root,
            session_model,
            source_branch: session_branch(&id),
            status,
        };
        tokio::spawn(async move {
            SessionManager::run_merge_task(merge_task_input).await;
        });

        Ok(())
    }

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for rebase, required git
    /// metadata is missing, or starting the rebase task fails.
    async fn rebase_session(
        &self,
        manager: &SessionManager,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), String> {
        let session = manager
            .session_or_err(session_id)
            .map_err(|_| SESSION_NOT_FOUND_ERROR.to_string())?;
        if session.status != Status::Review {
            return Err("Session must be in review status".to_string());
        }

        let base_branch = services
            .db()
            .get_session_base_branch(&session.id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        let handles = manager
            .session_handles_or_err(session_id)
            .map_err(|_| SESSION_HANDLES_NOT_FOUND_ERROR.to_string())?;
        let child_pid = Arc::clone(&handles.child_pid);
        let output = Arc::clone(&handles.output);

        let status = Arc::clone(&handles.status);
        let db = services.db().clone();
        let app_event_tx = services.event_sender();
        let fs_client = services.fs_client();
        let git_client = manager.git_client();

        if !SessionTaskService::update_status(
            &status,
            &db,
            &app_event_tx,
            &session.id,
            Status::Rebasing,
        )
        .await
        {
            return Err("Invalid status transition to Rebasing".to_string());
        }

        let id = session.id.clone();
        let session_model = session.model;

        let rebase_task_input = RebaseTaskInput {
            app_event_tx,
            base_branch,
            child_pid,
            db,
            folder: session.folder.clone(),
            fs_client,
            git_client,
            id,
            output,
            session_model,
            status,
        };
        tokio::spawn(async move {
            SessionManager::run_rebase_task(rebase_task_input).await;
        });

        Ok(())
    }
}

impl SessionManager {
    /// Starts a squash merge for a review-ready or queued session branch in
    /// the background.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the status transition to `Merging` fails.
    pub async fn merge_session(
        &self,
        session_id: &str,
        projects: &ProjectManager,
        services: &AppServices,
    ) -> Result<(), String> {
        self.merge_service()
            .merge_session(self, session_id, projects, services)
            .await
    }

    async fn run_merge_task(input: MergeTaskInput) {
        let output = Arc::clone(&input.output);
        let db = input.db.clone();
        let app_event_tx = input.app_event_tx.clone();
        let id = input.id.clone();
        let status = Arc::clone(&input.status);

        let merge_result = Self::execute_merge_workflow(input).await;

        Self::finalize_merge_task(merge_result, &output, &db, &app_event_tx, &id, &status).await;
    }

    /// Executes the merge workflow for one session branch.
    ///
    /// # Errors
    /// Returns an error when the rebase step fails, the canonical session
    /// commit message cannot be loaded, squash-merge git commands fail, status
    /// transitions are invalid, or worktree cleanup fails.
    async fn execute_merge_workflow(input: MergeTaskInput) -> Result<String, String> {
        let rebase_input = Self::merge_rebase_input(&input);
        let MergeTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            fs_client,
            git_client,
            id,
            output: _,
            repo_root,
            source_branch,
            status,
            ..
        } = input;

        // Rebase onto the base branch first to ensure the merge is clean and
        // includes all recent changes. This also handles auto-commit and
        // conflict resolution via the agent.
        if let Err(error) = Self::execute_rebase_workflow(rebase_input).await {
            return Err(format!("Merge failed during rebase step: {error}"));
        }

        let squash_diff = Self::load_squash_diff(
            git_client.as_ref(),
            repo_root.clone(),
            source_branch.clone(),
            base_branch.clone(),
        )
        .await?;
        let authoritative_commit_message = if squash_diff.trim().is_empty() {
            None
        } else {
            Some(
                Self::load_authoritative_session_commit_message(
                    git_client.as_ref(),
                    folder.clone(),
                )
                .await?,
            )
        };
        let merge_outcome = if let Some(commit_message) = authoritative_commit_message.as_ref() {
            let repo_root = repo_root.clone();
            let source_branch = source_branch.clone();
            let base_branch = base_branch.clone();
            let commit_message = commit_message.clone();

            git_client
                .squash_merge(repo_root, source_branch, base_branch, commit_message)
                .await
                .map_err(|error| error.to_string())?
        } else {
            git::SquashMergeOutcome::AlreadyPresentInTarget
        };

        Self::cleanup_merged_session_worktree(
            folder.clone(),
            Arc::clone(&fs_client),
            Arc::clone(&git_client),
            source_branch.clone(),
            Some(repo_root),
        )
        .await
        .map_err(|error| format!("Merged successfully but failed to remove worktree: {error}"))?;

        if let Some(commit_message) = authoritative_commit_message {
            Self::update_session_title_from_commit_message(
                &db,
                &id,
                &commit_message,
                &app_event_tx,
            )
            .await;
            Self::update_done_session_summary_from_commit_message(&db, &id, &commit_message).await;
        }

        if !SessionTaskService::update_status(&status, &db, &app_event_tx, &id, Status::Done).await
        {
            return Err("Invalid status transition to Done".to_string());
        }

        Ok(Self::merge_success_message(
            &source_branch,
            &base_branch,
            merge_outcome,
        ))
    }

    /// Builds rebase input used by merge workflows.
    fn merge_rebase_input(input: &MergeTaskInput) -> RebaseAssistInput {
        RebaseAssistInput {
            app_event_tx: input.app_event_tx.clone(),
            base_branch: input.base_branch.clone(),
            child_pid: Arc::clone(&input.child_pid),
            db: input.db.clone(),
            folder: input.folder.clone(),
            fs_client: Arc::clone(&input.fs_client),
            git_client: Arc::clone(&input.git_client),
            id: input.id.clone(),
            output: Arc::clone(&input.output),
            session_model: input.session_model,
        }
    }

    /// Loads the squash-diff preview for one merge candidate.
    ///
    /// # Errors
    /// Returns an error when the diff cannot be generated.
    async fn load_squash_diff(
        git_client: &dyn GitClient,
        repo_root: PathBuf,
        source_branch: String,
        base_branch: String,
    ) -> Result<String, String> {
        git_client
            .squash_merge_diff(repo_root, source_branch, base_branch)
            .await
            .map_err(|error| format!("Failed to inspect merge diff: {error}"))
    }

    /// Loads the canonical session commit message from the worktree `HEAD` for
    /// reuse during squash merge.
    ///
    /// # Errors
    /// Returns an error when `HEAD` cannot be inspected or does not contain a
    /// non-blank commit message.
    async fn load_authoritative_session_commit_message(
        git_client: &dyn GitClient,
        folder: PathBuf,
    ) -> Result<String, String> {
        let commit_message = git_client
            .head_commit_message(folder)
            .await
            .map_err(|error| error.to_string())?;
        let Some(commit_message) = commit_message else {
            return Err("Session branch has no commit message to reuse for merge".to_string());
        };
        let trimmed_commit_message = commit_message.trim();
        if trimmed_commit_message.is_empty() {
            return Err("Session branch has a blank commit message to reuse for merge".to_string());
        }

        Ok(trimmed_commit_message.to_string())
    }

    async fn finalize_merge_task(
        merge_result: Result<String, String>,
        output: &Arc<Mutex<String>>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        status: &Arc<Mutex<Status>>,
    ) {
        match merge_result {
            Ok(message) => {
                let merge_message = format!("\n[Merge] {message}\n");
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    id,
                    &merge_message,
                )
                .await;
            }
            Err(error) => {
                let merge_error = format!("\n[Merge Error] {error}\n");
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    id,
                    &merge_error,
                )
                .await;
                let _ =
                    SessionTaskService::update_status(status, db, app_event_tx, id, Status::Review)
                        .await;
            }
        }
    }

    /// Builds merge success output text for commit and no-op outcomes.
    fn merge_success_message(
        source_branch: &str,
        base_branch: &str,
        merge_outcome: git::SquashMergeOutcome,
    ) -> String {
        match merge_outcome {
            git::SquashMergeOutcome::Committed => {
                format!("Successfully merged {source_branch} into {base_branch}")
            }
            git::SquashMergeOutcome::AlreadyPresentInTarget => {
                format!("Session changes from {source_branch} are already present in {base_branch}")
            }
        }
    }

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for rebase, required git
    /// metadata is missing, or starting the rebase task fails.
    pub async fn rebase_session(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), String> {
        self.merge_service()
            .rebase_session(self, services, session_id)
            .await
    }

    /// Synchronizes a project branch with upstream using only project context.
    ///
    /// This helper is reused by background-triggered sync workflows and tests.
    /// On success returns a [`SyncMainOutcome`] for popup status messaging.
    ///
    /// # Errors
    /// Returns a [`SyncSessionStartError`] when project context is invalid or
    /// updating the default branch fails (`git pull --rebase`, assisted
    /// conflict resolution, or `git push`).
    pub(crate) async fn sync_main_for_project(
        default_branch: Option<String>,
        working_dir: PathBuf,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
    ) -> Result<SyncMainOutcome, SyncSessionStartError> {
        let fs_client: Arc<dyn FsClient> = Arc::new(fs::RealFsClient);
        let sync_assist_client: Arc<dyn SyncAssistClient> = Arc::new(RealSyncAssistClient);

        Self::sync_main_for_project_with_assist_client(
            default_branch,
            working_dir,
            fs_client,
            git_client,
            session_model,
            sync_assist_client,
        )
        .await
    }

    /// Synchronizes the selected project branch with optional mocked assistance
    /// support for tests.
    ///
    /// # Errors
    /// Returns a [`SyncSessionStartError`] when project context is invalid,
    /// git operations fail, or rebase conflicts remain unresolved after
    /// assisted attempts.
    async fn sync_main_for_project_with_assist_client(
        default_branch: Option<String>,
        working_dir: PathBuf,
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn GitClient>,
        session_model: AgentModel,
        sync_assist_client: Arc<dyn SyncAssistClient>,
    ) -> Result<SyncMainOutcome, SyncSessionStartError> {
        let default_branch = default_branch.ok_or_else(|| {
            SyncSessionStartError::Other("Active project has no git branch".to_string())
        })?;

        let _repo_root = git_client
            .find_git_repo_root(working_dir.clone())
            .await
            .ok_or_else(|| {
                SyncSessionStartError::Other("Failed to find git repository root".to_string())
            })?;

        let is_default_branch_clean = git_client
            .is_worktree_clean(working_dir.clone())
            .await
            .map_err(|error| SyncSessionStartError::Other(error.to_string()))?;
        if !is_default_branch_clean {
            return Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: default_branch.clone(),
            });
        }

        let ahead_behind_before_pull = git_client.get_ahead_behind(working_dir.clone()).await.ok();
        let pulled_commit_titles = git_client
            .list_upstream_commit_titles(working_dir.clone())
            .await
            .unwrap_or_default();

        let pull_result = git_client
            .pull_rebase(working_dir.clone())
            .await
            .map_err(|error| SyncSessionStartError::Other(error.to_string()))?;
        let mut resolved_conflict_files = Vec::new();
        if let git::PullRebaseResult::Conflict { detail } = pull_result {
            let sync_rebase_input = SyncRebaseAssistInput {
                base_branch: default_branch.clone(),
                folder: working_dir.clone(),
                fs_client: Arc::clone(&fs_client),
                git_client: Arc::clone(&git_client),
                session_model,
                sync_assist_client,
            };
            resolved_conflict_files =
                match Self::run_sync_rebase_assist_loop(sync_rebase_input, detail.clone()).await {
                    Ok(resolved_conflict_files) => resolved_conflict_files,
                    Err(error) => {
                        return Err(SyncSessionStartError::Other(format!(
                            "Sync stopped on rebase conflicts while updating `{default_branch}`: \
                             {detail}. Assisted resolution failed: {error}"
                        )));
                    }
                };
        }
        let ahead_behind_after_pull = git_client.get_ahead_behind(working_dir.clone()).await.ok();
        let pushed_commit_titles = git_client
            .list_local_commit_titles(working_dir.clone())
            .await
            .unwrap_or_default();

        git_client
            .push_current_branch(working_dir)
            .await
            .map_err(|error| SyncSessionStartError::Other(error.to_string()))?;

        let (pulled_commits, pushed_commits) = Self::summarize_sync_ahead_behind_counts(
            ahead_behind_before_pull,
            ahead_behind_after_pull,
        );

        Ok(SyncMainOutcome {
            pulled_commit_titles,
            pulled_commits,
            pushed_commit_titles,
            pushed_commits,
            resolved_conflict_files,
        })
    }

    /// Runs assisted conflict resolution for a main-project rebase in progress.
    ///
    /// # Errors
    /// Returns an error when conflicts remain unresolved after all attempts or
    /// when git/agent operations fail. On success returns resolved conflict
    /// file paths observed during assistance.
    async fn run_sync_rebase_assist_loop(
        input: SyncRebaseAssistInput,
        initial_conflict_detail: String,
    ) -> Result<Vec<String>, String> {
        Self::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Project(input),
            Some(initial_conflict_detail),
        )
        .await
        .map(|outcome| outcome.resolved_conflict_files)
    }

    /// Returns pull/push counts inferred around one completed sync run.
    fn summarize_sync_ahead_behind_counts(
        ahead_behind_before_pull: Option<(u32, u32)>,
        ahead_behind_after_pull: Option<(u32, u32)>,
    ) -> (Option<u32>, Option<u32>) {
        let pulled_commits = ahead_behind_before_pull.map(|(_ahead, behind)| behind);
        let pushed_commits = ahead_behind_after_pull
            .map(|(ahead, _behind)| ahead)
            .or_else(|| ahead_behind_before_pull.map(|(ahead, _behind)| ahead));

        (pulled_commits, pushed_commits)
    }

    /// Loads current conflicted files for sync assistance.
    ///
    /// # Errors
    /// Returns an error if conflicted-file inspection fails.
    async fn load_sync_conflicted_files(
        input: &SyncRebaseAssistInput,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, String> {
        let folder = input.folder.clone();
        let mut conflicted = input
            .git_client
            .list_conflicted_files(folder.clone())
            .await
            .map_err(|error| error.to_string())?;

        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
            .await
            .map_err(|error| error.to_string())?;
        for file in staged_with_markers {
            if !conflicted.contains(&file) {
                conflicted.push(file);
            }
        }
        conflicted.sort_unstable();

        Ok(conflicted)
    }

    /// Executes one agent-assisted sync rebase conflict resolution attempt.
    ///
    /// # Errors
    /// Returns an error when the assistance command fails.
    async fn run_sync_rebase_assist_agent(
        input: &SyncRebaseAssistInput,
        conflicted_files: &[String],
    ) -> Result<(), String> {
        let prompt = Self::rebase_assist_prompt(&input.base_branch, conflicted_files)?;
        input
            .sync_assist_client
            .resolve_rebase_conflicts(input.folder.clone(), prompt, input.session_model)
            .await
            .map_err(|error| format!("Sync rebase assistance failed: {error}"))
    }

    /// Stages sync edits and checks whether rebase conflicts remain.
    ///
    /// # Errors
    /// Returns an error when staging or conflict checks fail.
    async fn stage_and_check_for_sync_conflicts(
        input: &SyncRebaseAssistInput,
        conflict_files: &[String],
    ) -> Result<bool, String> {
        let folder = input.folder.clone();
        input
            .git_client
            .stage_all(folder)
            .await
            .map_err(|error| error.to_string())?;

        let folder = input.folder.clone();
        if input
            .git_client
            .has_unmerged_paths(folder)
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, conflict_files.to_vec())
            .await
            .map_err(|error| error.to_string())?;

        Ok(!staged_with_markers.is_empty())
    }

    /// Continues the in-progress sync rebase.
    ///
    /// # Errors
    /// Returns an error when `git rebase --continue` fails with non-conflict
    /// errors.
    async fn run_sync_rebase_continue(
        input: &SyncRebaseAssistInput,
    ) -> Result<git::RebaseStepResult, String> {
        let folder = input.folder.clone();
        let result = input
            .git_client
            .rebase_continue(folder)
            .await
            .map_err(|error| error.to_string())?;

        Ok(result)
    }

    /// Aborts an in-progress sync rebase after assistance failure.
    async fn abort_sync_rebase_after_assist_failure(input: &SyncRebaseAssistInput) {
        let folder = input.folder.clone();
        let _ = input.git_client.abort_rebase(folder).await;
    }

    async fn run_rebase_task(input: RebaseTaskInput) {
        let RebaseTaskInput {
            app_event_tx,
            base_branch,
            child_pid,
            db,
            folder,
            fs_client,
            git_client,
            id,
            output,
            session_model,
            status,
        } = input;

        let rebase_result: Result<String, String> = async {
            let rebase_input = RebaseAssistInput {
                app_event_tx: app_event_tx.clone(),
                base_branch: base_branch.clone(),
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: folder.clone(),
                fs_client: Arc::clone(&fs_client),
                git_client: Arc::clone(&git_client),
                id: id.clone(),
                output: Arc::clone(&output),
                session_model,
            };

            Self::execute_rebase_workflow(rebase_input).await
        }
        .await;

        Self::finalize_rebase_task(rebase_result, &output, &db, &app_event_tx, &id, &status).await;
    }

    /// Executes one assisted rebase workflow for a session worktree.
    ///
    /// Aborts any in-progress rebase when the assist loop fails so stale
    /// rebase metadata does not leak into later merge/rebase operations.
    ///
    /// # Errors
    /// Returns an error when pre-rebase auto-commit fails or assisted rebase
    /// cannot be completed.
    ///
    /// Emits user-visible commit output before rebase starts so users can see
    /// whether pending changes were committed or there was nothing to commit.
    async fn execute_rebase_workflow(input: RebaseAssistInput) -> Result<String, String> {
        // Auto-commit any pending changes before rebasing to avoid
        // "cannot rebase: You have unstaged changes".
        let include_coauthored_by_agentty =
            SessionTaskService::load_include_coauthored_by_agentty_setting(&input.db, &input.id)
                .await;
        match SessionTaskService::commit_session_changes(
            input.git_client.as_ref(),
            &input.folder,
            &input.base_branch,
            input.session_model,
            true,
            include_coauthored_by_agentty,
        )
        .await
        {
            Ok(outcome) => {
                Self::update_session_title_from_commit_message(
                    &input.db,
                    &input.id,
                    &outcome.commit_message,
                    &input.app_event_tx,
                )
                .await;

                let commit_message =
                    format!("\n[Commit] committed with hash `{}`\n", outcome.commit_hash);
                SessionTaskService::append_session_output(
                    &input.output,
                    &input.db,
                    &input.app_event_tx,
                    &input.id,
                    &commit_message,
                )
                .await;
            }
            Err(error) if error.contains("Nothing to commit") => {
                let commit_message = "\n[Commit] No changes to commit.\n";
                SessionTaskService::append_session_output(
                    &input.output,
                    &input.db,
                    &input.app_event_tx,
                    &input.id,
                    commit_message,
                )
                .await;
            }
            Err(error) => {
                return Err(format!(
                    "Failed to commit pending changes before rebase: {error}"
                ));
            }
        }

        if let Err(error) = Self::run_rebase_assist_loop(input.clone()).await {
            Self::abort_rebase_after_assist_failure(&input).await;

            return Err(format!("Failed to rebase: {error}"));
        }

        let source_branch = session_branch(&input.id);
        let base_branch = &input.base_branch;

        Ok(format!(
            "Successfully rebased {source_branch} onto {base_branch}"
        ))
    }

    async fn finalize_rebase_task(
        rebase_result: Result<String, String>,
        output: &Arc<Mutex<String>>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        status: &Arc<Mutex<Status>>,
    ) {
        match rebase_result {
            Ok(message) => {
                let rebase_message = format!("\n[Rebase] {message}\n");
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    id,
                    &rebase_message,
                )
                .await;
            }
            Err(error) => {
                let rebase_error = format!("\n[Rebase Error] {error}\n");
                SessionTaskService::append_session_output(
                    output,
                    db,
                    app_event_tx,
                    id,
                    &rebase_error,
                )
                .await;
            }
        }

        let _ =
            SessionTaskService::update_status(status, db, app_event_tx, id, Status::Review).await;
    }

    /// Updates the persisted session title from the canonical commit message.
    pub(crate) async fn update_session_title_from_commit_message(
        db: &Database,
        session_id: &str,
        commit_message: &str,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
    ) {
        let title = Self::session_title_from_commit_message(commit_message);

        let _ = db.update_session_title(session_id, &title).await;

        let _ = app_event_tx.send(AppEvent::RefreshSessions);
    }

    /// Updates the persisted done-session summary by formatting the latest
    /// persisted agent session-summary text, extracting `summary.session`
    /// from raw JSON payloads when needed, and canonical commit message into
    /// markdown sections.
    async fn update_done_session_summary_from_commit_message(
        db: &Database,
        session_id: &str,
        commit_message: &str,
    ) {
        let summary = Self::session_summary_with_commit_message(
            Self::persisted_session_summary(db, session_id)
                .await
                .as_deref(),
            commit_message,
        );

        let _ = db.update_session_summary(session_id, &summary).await;
    }

    /// Loads the currently persisted session summary text for one session.
    async fn persisted_session_summary(db: &Database, session_id: &str) -> Option<String> {
        db.load_session_summary(session_id).await.ok().flatten()
    }

    /// Extracts the first non-empty line from one session commit message for
    /// use as the session title.
    fn session_title_from_commit_message(commit_message: &str) -> String {
        let trimmed_message = commit_message.trim();
        if trimmed_message.is_empty() {
            return "Apply session updates".to_string();
        }

        trimmed_message
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("Apply session updates")
            .to_string()
    }

    /// Builds the persisted done-session summary with markdown sections.
    ///
    /// Includes `# Summary` from the final agent session-summary text,
    /// extracting `summary.session` from persisted JSON payloads when needed,
    /// and `# Commit` from the canonical session commit message.
    fn session_summary_with_commit_message(
        session_summary: Option<&str>,
        commit_message: &str,
    ) -> String {
        let trimmed_summary = session_summary.map(str::trim).unwrap_or_default();
        let summary_text = serde_json::from_str::<AgentResponseSummary>(trimmed_summary)
            .map_or_else(
                |_| trimmed_summary.to_string(),
                |summary_payload| summary_payload.session,
            );
        let trimmed_commit_message = commit_message.trim();

        format!("# Summary\n\n{summary_text}\n\n# Commit\n\n{trimmed_commit_message}")
    }

    /// Runs a bounded rebase-assistance loop until conflicts are resolved.
    ///
    /// # Errors
    /// Returns an error when conflict resolution fails after all attempts or
    /// when git/agent operations fail.
    async fn run_rebase_assist_loop(input: RebaseAssistInput) -> Result<(), String> {
        let rebase_in_progress = Self::is_rebase_in_progress(&input).await?;
        if !rebase_in_progress {
            let initial_step = Self::run_rebase_start(&input).await?;
            if initial_step == git::RebaseStepResult::Completed {
                return Ok(());
            }
        }

        Self::run_rebase_assist_loop_core(RebaseAssistLoopInput::Session(input), None)
            .await
            .map(|_| ())
    }

    /// Executes shared bounded assistance loop for both session rebases and
    /// main-project sync rebases.
    ///
    /// # Errors
    /// Returns an error when assistance fails to make progress, rebase remains
    /// conflicted after all attempts, or git operations fail. On every error
    /// path (including early `?` failures), the in-progress rebase is aborted
    /// before returning.
    async fn run_rebase_assist_loop_core(
        assist_input: RebaseAssistLoopInput,
        initial_conflict_detail: Option<String>,
    ) -> Result<RebaseAssistOutcome, String> {
        let assist_result: Result<RebaseAssistOutcome, String> = async {
            let mut failure_tracker =
                FailureTracker::new(REBASE_ASSIST_POLICY.max_identical_failure_streak);
            let mut assist_outcome = RebaseAssistOutcome::empty();
            if let Some(initial_conflict_detail) = initial_conflict_detail {
                let _ = failure_tracker.observe(&initial_conflict_detail);
            }

            let mut previous_conflict_files: Vec<String> = vec![];

            for assist_attempt in 1..=REBASE_ASSIST_POLICY.max_attempts {
                let conflicted_files = assist_input
                    .load_conflicted_files(&previous_conflict_files)
                    .await?;
                if conflicted_files.is_empty() {
                    let continue_step = assist_input.run_rebase_continue().await?;
                    match continue_step {
                        git::RebaseStepResult::Completed => {
                            return Ok(assist_outcome);
                        }
                        git::RebaseStepResult::Conflict { detail } => {
                            if failure_tracker.observe(&detail) {
                                return Err(assist_input.repeated_conflict_state_error(&detail));
                            }

                            if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
                                return Err(assist_input.still_conflicted_error(&detail));
                            }
                        }
                    }

                    continue;
                }

                let conflict_fingerprint = Self::conflicted_file_fingerprint(
                    assist_input.fs_client(),
                    assist_input.folder(),
                    &conflicted_files,
                )
                .await;
                if failure_tracker.observe(&conflict_fingerprint) {
                    return Err(assist_input.unchanged_conflict_files_error());
                }
                assist_outcome.extend_resolved_conflict_files(&conflicted_files);

                assist_input
                    .run_assist_attempt(assist_attempt, &conflicted_files)
                    .await?;

                let still_has_conflicts = assist_input
                    .stage_and_check_for_conflicts(&conflicted_files)
                    .await?;
                previous_conflict_files = conflicted_files;
                if still_has_conflicts {
                    if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
                        return Err("Conflicts remain unresolved after maximum assistance \
                                    attempts"
                            .to_string());
                    }

                    continue;
                }

                let continue_step = assist_input.run_rebase_continue().await?;
                match continue_step {
                    git::RebaseStepResult::Completed => {
                        return Ok(assist_outcome);
                    }
                    git::RebaseStepResult::Conflict { detail } => {
                        if failure_tracker.observe(&detail) {
                            return Err(assist_input.repeated_conflict_state_error(&detail));
                        }

                        if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
                            return Err(assist_input.still_conflicted_error(&detail));
                        }
                    }
                }
            }

            Err(assist_input.exhausted_error())
        }
        .await;

        match assist_result {
            Ok(assist_outcome) => Ok(assist_outcome),
            Err(error) => {
                assist_input.abort_rebase_after_assist_failure().await;

                Err(error)
            }
        }
    }

    /// Returns whether the session worktree has an in-progress rebase.
    ///
    /// # Errors
    /// Returns an error if git state cannot be queried.
    async fn is_rebase_in_progress(input: &RebaseAssistInput) -> Result<bool, String> {
        let folder = input.folder.clone();
        let is_rebase_in_progress = input
            .git_client
            .is_rebase_in_progress(folder)
            .await
            .map_err(|error| error.to_string())?;

        Ok(is_rebase_in_progress)
    }

    /// Starts the rebase step for an assisted rebase flow.
    ///
    /// When git reports stale rebase metadata (`rebase-merge`/`rebase-apply`)
    /// this helper attempts one cleanup pass via `git rebase --abort` and then
    /// retries the start command once.
    ///
    /// # Errors
    /// Returns an error if spawning the git process fails or git returns a
    /// non-conflict failure that cannot be recovered.
    async fn run_rebase_start(input: &RebaseAssistInput) -> Result<git::RebaseStepResult, String> {
        let folder = input.folder.clone();
        let base_branch = input.base_branch.clone();
        match input
            .git_client
            .rebase_start(folder.clone(), base_branch.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(error) => {
                let error_string = error.to_string();
                if !Self::is_stale_rebase_state_error(&error_string) {
                    return Err(error_string);
                }

                Self::recover_from_stale_rebase_start_error(input, &error_string).await?;

                input
                    .git_client
                    .rebase_start(folder, base_branch)
                    .await
                    .map_err(|error| error.to_string())
            }
        }
    }

    /// Returns whether a rebase start error indicates stale rebase metadata.
    fn is_stale_rebase_state_error(error: &str) -> bool {
        let normalized_error = error.to_ascii_lowercase();

        normalized_error.contains("already a rebase-merge directory")
            || normalized_error.contains("already a rebase-apply directory")
            || normalized_error.contains("middle of another rebase")
    }

    /// Tries to clean stale rebase metadata before retrying rebase start.
    ///
    /// # Errors
    /// Returns an error when `git rebase --abort` cannot clean up stale state.
    async fn recover_from_stale_rebase_start_error(
        input: &RebaseAssistInput,
        start_error: &str,
    ) -> Result<(), String> {
        let folder = input.folder.clone();
        input
            .git_client
            .abort_rebase(folder)
            .await
            .map_err(|abort_error| {
                format!(
                    "Detected stale rebase metadata after failed rebase start: {start_error}. \
                     Cleanup with `git rebase --abort` failed: {abort_error}"
                )
            })?;

        Ok(())
    }

    /// Loads all conflicted files from the worktree.
    ///
    /// Returns the union of two sets:
    /// - Files with *unmerged* index entries (classic rebase conflict state).
    /// - Files that were staged (`git add`) while still containing `<<<<<<<`
    ///   conflict markers, scoped to the provided `previous_conflict_files`.
    ///   This catches the case where an agent partially resolves a conflict and
    ///   stages the file without removing all markers, which would otherwise
    ///   make the file appear resolved.
    ///
    /// On the first call (no known prior conflicts), pass an empty slice for
    /// `previous_conflict_files`; only unmerged entries will be returned.
    ///
    /// # Errors
    /// Returns an error if either git query fails.
    async fn load_conflicted_files(
        input: &RebaseAssistInput,
        previous_conflict_files: &[String],
    ) -> Result<Vec<String>, String> {
        let folder = input.folder.clone();
        let mut conflicted = input
            .git_client
            .list_conflicted_files(folder.clone())
            .await
            .map_err(|error| error.to_string())?;

        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
            .await
            .map_err(|error| error.to_string())?;
        for file in staged_with_markers {
            if !conflicted.contains(&file) {
                conflicted.push(file);
            }
        }
        conflicted.sort_unstable();

        Ok(conflicted)
    }

    /// Appends an informational header for one rebase assistance attempt.
    async fn append_rebase_assist_header(
        input: &RebaseAssistInput,
        assist_attempt: usize,
        conflicted_files: &[String],
    ) {
        let conflict_summary = Self::format_conflicted_file_list(conflicted_files);
        append_assist_header(
            &Self::assist_context(input),
            "Rebase",
            assist_attempt,
            REBASE_ASSIST_POLICY.max_attempts,
            "Resolving conflicts in:",
            &conflict_summary,
        )
        .await;
    }

    /// Runs an agent task to resolve the provided conflicted files.
    ///
    /// # Errors
    /// Returns an error if the agent process fails.
    async fn run_rebase_assist_agent(
        input: &RebaseAssistInput,
        conflicted_files: &[String],
    ) -> Result<(), String> {
        let prompt = Self::rebase_assist_prompt(&input.base_branch, conflicted_files)?;
        let assist_context = Self::assist_context(input);

        run_agent_assist(&assist_context, &prompt)
            .await
            .map_err(|error| format!("Rebase assistance failed: {error}"))
    }

    /// Stages all worktree edits and checks whether any conflicts remain.
    ///
    /// Performs two checks after staging:
    /// 1. Unmerged index entries — files that were never resolved (`git add`d).
    /// 2. Staged content with `<<<<<<<` markers in `conflict_files` — files
    ///    that were staged while still containing residual conflict markers.
    ///
    /// Both checks are required because `git add` transitions a file from
    /// "Unmerged" to "Modified-in-index", making it invisible to the unmerged
    /// check even when conflict markers remain in its content.
    ///
    /// # Errors
    /// Returns an error when staging or either conflict check fails.
    async fn stage_and_check_for_conflicts(
        input: &RebaseAssistInput,
        conflict_files: &[String],
    ) -> Result<bool, String> {
        let folder = input.folder.clone();
        input
            .git_client
            .stage_all(folder)
            .await
            .map_err(|error| error.to_string())?;

        let folder = input.folder.clone();
        if input
            .git_client
            .has_unmerged_paths(folder)
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, conflict_files.to_vec())
            .await
            .map_err(|error| error.to_string())?;

        Ok(!staged_with_markers.is_empty())
    }

    /// Continues an in-progress rebase after conflict edits are applied.
    ///
    /// # Errors
    /// Returns an error if git reports a non-conflict failure.
    async fn run_rebase_continue(
        input: &RebaseAssistInput,
    ) -> Result<git::RebaseStepResult, String> {
        let folder = input.folder.clone();
        let result = input
            .git_client
            .rebase_continue(folder)
            .await
            .map_err(|error| error.to_string())?;

        Ok(result)
    }

    /// Renders the rebase-assist prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error if Askama template rendering fails.
    fn rebase_assist_prompt(
        base_branch: &str,
        conflicted_files: &[String],
    ) -> Result<String, String> {
        let conflicted_files = Self::format_conflicted_file_list(conflicted_files);
        let template = RebaseAssistPromptTemplate {
            base_branch,
            conflicted_files: &conflicted_files,
        };

        template
            .render()
            .map_err(|error| format!("Failed to render `rebase_assist_prompt.md`: {error}"))
    }

    /// Formats conflicted file paths as a bullet list for prompt rendering.
    fn format_conflicted_file_list(conflicted_files: &[String]) -> String {
        format_detail_lines(&conflicted_files.join("\n"))
    }

    /// Computes a content-based fingerprint for the current set of conflicted
    /// files.
    ///
    /// Reads each file from disk and hashes both its path and content so that
    /// partial progress made by the rebase-assist agent (e.g. removing some
    /// but not all conflict markers) changes the fingerprint and prevents the
    /// [`FailureTracker`] from firing prematurely. The fingerprint is
    /// order-independent because paths are sorted before hashing.
    async fn conflicted_file_fingerprint(
        fs_client: &dyn FsClient,
        folder: &Path,
        conflicted_files: &[String],
    ) -> String {
        let mut sorted_files = conflicted_files.to_vec();
        sorted_files.sort_unstable();

        let mut hasher = DefaultHasher::new();
        for file in &sorted_files {
            file.hash(&mut hasher);
            let file_path = folder.join(file);
            if let Ok(content) = fs_client.read_file(file_path).await {
                content.hash(&mut hasher);
            }
        }

        format!("{:016x}", hasher.finish())
    }

    /// Builds shared assistance context from rebase input state.
    fn assist_context(input: &RebaseAssistInput) -> AssistContext {
        AssistContext {
            app_event_tx: input.app_event_tx.clone(),
            child_pid: Arc::clone(&input.child_pid),
            db: input.db.clone(),
            folder: input.folder.clone(),
            git_client: Arc::clone(&input.git_client),
            id: input.id.clone(),
            output: Arc::clone(&input.output),
            session_model: input.session_model,
        }
    }

    /// Aborts rebase after assistance fails to keep worktree state clean.
    async fn abort_rebase_after_assist_failure(input: &RebaseAssistInput) {
        let folder = input.folder.clone();
        let _ = input.git_client.abort_rebase(folder).await;
    }

    /// Removes a merged session worktree and deletes its source branch.
    ///
    /// When `repo_root` is not provided, this resolves the shared repository
    /// root through `git rev-parse` via `GitClient`.
    ///
    /// # Errors
    /// Returns an error if worktree or branch cleanup fails.
    pub(crate) async fn cleanup_merged_session_worktree(
        folder: PathBuf,
        fs_client: Arc<dyn FsClient>,
        git_client: Arc<dyn GitClient>,
        source_branch: String,
        repo_root: Option<PathBuf>,
    ) -> Result<(), String> {
        let repo_root = match repo_root {
            Some(repo_root) => Some(repo_root),
            None => git_client.main_repo_root(folder.clone()).await.ok(),
        };

        git_client
            .remove_worktree(folder.clone())
            .await
            .map_err(|error| error.to_string())?;

        if let Some(repo_root) = repo_root {
            git_client
                .delete_branch(repo_root, source_branch)
                .await
                .map_err(|error| error.to_string())?;
        }

        let _ = fs_client.remove_dir_all(folder).await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mockall::Sequence;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::infra::git::GitError;

    /// Builds a filesystem mock that delegates operations to local disk.
    fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_fs_client
            .expect_create_dir_all()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::create_dir_all(path)
                        .await
                        .map_err(|error| error.to_string())
                })
            });
        mock_fs_client
            .expect_remove_dir_all()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::remove_dir_all(path)
                        .await
                        .map_err(|error| error.to_string())
                })
            });
        mock_fs_client
            .expect_read_file()
            .times(0..)
            .returning(|path| {
                Box::pin(async move {
                    tokio::fs::read(path)
                        .await
                        .map_err(|error| error.to_string())
                })
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

    /// Returns a fresh mocked filesystem client trait object for tests.
    fn test_fs_client() -> Arc<dyn FsClient> {
        Arc::new(create_passthrough_mock_fs_client())
    }

    /// Builds rebase assistance input with the provided git client for unit
    /// tests.
    async fn build_rebase_assist_input_for_test(
        git_client: Arc<dyn GitClient>,
    ) -> (TempDir, RebaseAssistInput) {
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let db = Database::open_in_memory().await.expect("failed to open db");
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().to_path_buf();

        (
            temp_dir,
            RebaseAssistInput {
                app_event_tx,
                base_branch: "main".to_string(),
                child_pid: Arc::new(Mutex::new(None)),
                db,
                folder,
                fs_client: test_fs_client(),
                git_client,
                id: "session-123".to_string(),
                output: Arc::new(Mutex::new(String::new())),
                session_model: AgentModel::Gemini3FlashPreview,
            },
        )
    }

    /// Builds merge-task input with injected git client for deterministic
    /// workflow tests.
    async fn build_merge_task_input_for_test(
        git_client: Arc<dyn GitClient>,
    ) -> (TempDir, MergeTaskInput) {
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let db = Database::open_in_memory().await.expect("failed to open db");
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().join("session-worktree");
        let repo_root = temp_dir.path().join("repo-root");

        (
            temp_dir,
            MergeTaskInput {
                app_event_tx,
                base_branch: "main".to_string(),
                child_pid: Arc::new(Mutex::new(None)),
                db,
                folder,
                fs_client: test_fs_client(),
                git_client,
                id: "session-123".to_string(),
                output: Arc::new(Mutex::new(String::new())),
                repo_root,
                session_model: AgentModel::Gemini3FlashPreview,
                source_branch: "agentty/session-123".to_string(),
                status: Arc::new(Mutex::new(Status::Merging)),
            },
        )
    }

    /// Builds sync rebase assistance input with injected git and assistance
    /// clients for project-level conflict tests.
    fn build_sync_rebase_input_for_test(
        folder: PathBuf,
        git_client: Arc<dyn GitClient>,
        sync_assist_client: Arc<dyn SyncAssistClient>,
    ) -> SyncRebaseAssistInput {
        SyncRebaseAssistInput {
            base_branch: "main".to_string(),
            folder,
            fs_client: test_fs_client(),
            git_client,
            session_model: AgentModel::Gemini3FlashPreview,
            sync_assist_client,
        }
    }

    #[test]
    fn test_rebase_assist_prompt_includes_branch_and_files() {
        // Arrange
        let base_branch = "main";
        let conflicted_files = vec!["src/lib.rs".to_string(), "README.md".to_string()];

        // Act
        let prompt = SessionManager::rebase_assist_prompt(base_branch, &conflicted_files)
            .expect("rebase assist prompt should render");

        // Assert
        assert!(prompt.contains("rebasing onto `main`"));
        assert!(prompt.contains("- src/lib.rs"));
        assert!(prompt.contains("- README.md"));
    }

    #[test]
    fn test_format_conflicted_file_list_returns_bulleted_lines() {
        // Arrange
        let conflicted_files = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];

        // Act
        let summary = SessionManager::format_conflicted_file_list(&conflicted_files);

        // Assert
        assert_eq!(summary, "- src/main.rs\n- src/lib.rs");
    }

    #[test]
    fn test_session_title_from_commit_message() {
        // Arrange
        let commit_message = "Refine merge flow\n\n- Update title handling";

        // Act
        let title = SessionManager::session_title_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Refine merge flow");
    }

    #[test]
    fn test_session_title_from_commit_message_skips_blank_prefix() {
        // Arrange
        let commit_message = "\n\nRefine merge flow\n\n- Update title handling";

        // Act
        let title = SessionManager::session_title_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Refine merge flow");
    }

    #[test]
    fn test_session_title_from_commit_message_empty_uses_fallback() {
        // Arrange
        let commit_message = "  \n";

        // Act
        let title = SessionManager::session_title_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Apply session updates");
    }

    #[test]
    fn test_session_summary_with_commit_message_builds_markdown_sections() {
        // Arrange
        let session_summary = Some("- Session branch now handles refresh races.");
        let commit_message = "Refine session summary\n\n- Append commit context";

        // Act
        let summary =
            SessionManager::session_summary_with_commit_message(session_summary, commit_message);

        // Assert
        assert_eq!(
            summary,
            "# Summary\n\n- Session branch now handles refresh races.\n\n# Commit\n\nRefine \
             session summary\n\n- Append commit context"
        );
    }

    #[test]
    fn test_session_summary_with_commit_message_formats_empty_summary_section() {
        // Arrange
        let session_summary = Some("   ");
        let commit_message = "Refine session summary";

        // Act
        let summary =
            SessionManager::session_summary_with_commit_message(session_summary, commit_message);

        // Assert
        assert_eq!(
            summary,
            "# Summary\n\n\n\n# Commit\n\nRefine session summary"
        );
    }

    #[test]
    fn test_session_summary_with_commit_message_extracts_session_text_from_json_payload() {
        // Arrange
        let session_summary = Some(
            r#"{"turn":"Updated the greeting flow.","session":"Session now greets users on startup."}"#,
        );
        let commit_message = "Refine session summary";

        // Act
        let summary =
            SessionManager::session_summary_with_commit_message(session_summary, commit_message);

        // Assert
        assert_eq!(
            summary,
            "# Summary\n\nSession now greets users on startup.\n\n# Commit\n\nRefine session \
             summary"
        );
    }

    #[tokio::test]
    async fn test_update_session_title_from_commit_message_preserves_existing_summary() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-id",
                AgentModel::ClaudeSonnet46.as_str(),
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let existing_summary = "- Session branch updates README.";
        database
            .update_session_summary("session-id", existing_summary)
            .await
            .expect("failed to persist existing summary");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let commit_message = "Refine session commit message\n\n- Keep title in sync";

        // Act
        SessionManager::update_session_title_from_commit_message(
            &database,
            "session-id",
            commit_message,
            &app_event_tx,
        )
        .await;
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(
            sessions[0].title.as_deref(),
            Some("Refine session commit message")
        );
        assert_eq!(sessions[0].summary.as_deref(), Some(existing_summary));
        assert_eq!(
            app_event_rx.try_recv().ok(),
            Some(AppEvent::RefreshSessions)
        );
    }

    #[tokio::test]
    async fn test_update_done_session_summary_from_commit_message_appends_commit_message() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "session-id",
                AgentModel::ClaudeSonnet46.as_str(),
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let existing_summary = "- Session branch updates README.";
        let commit_message = "Refine session commit message\n\n- Keep title in sync";
        database
            .update_session_summary("session-id", existing_summary)
            .await
            .expect("failed to persist existing summary");

        // Act
        SessionManager::update_done_session_summary_from_commit_message(
            &database,
            "session-id",
            commit_message,
        )
        .await;
        let sessions = database
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(
            sessions[0].summary.as_deref(),
            Some(
                "# Summary\n\n- Session branch updates README.\n\n# Commit\n\nRefine session \
                 commit message\n\n- Keep title in sync"
            )
        );
    }

    #[tokio::test]
    async fn test_execute_merge_workflow_reuses_session_head_commit_message() {
        // Arrange
        let canonical_commit_message = "Refine merge flow\n\n- Reuse the session commit body";
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock_git_client
            .expect_squash_merge_diff()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _| Box::pin(async { Ok("diff --git a/file b/file".to_string()) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let canonical_commit_message = canonical_commit_message.to_string();

                move |_| {
                    let canonical_commit_message = canonical_commit_message.clone();

                    Box::pin(async move { Ok(Some(canonical_commit_message)) })
                }
            });
        mock_git_client
            .expect_squash_merge()
            .times(1)
            .in_sequence(&mut sequence)
            .returning({
                let canonical_commit_message = canonical_commit_message.to_string();

                move |_, _, _, commit_message| {
                    let canonical_commit_message = canonical_commit_message.clone();

                    Box::pin(async move {
                        assert_eq!(commit_message, canonical_commit_message);

                        Ok(git::SquashMergeOutcome::Committed)
                    })
                }
            });
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) = build_merge_task_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::execute_merge_workflow(input).await;

        // Assert
        assert_eq!(
            result,
            Ok("Successfully merged agentty/session-123 into main".to_string())
        );
    }

    #[tokio::test]
    async fn test_execute_merge_workflow_skips_commit_creation_for_empty_squash_diff() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock_git_client
            .expect_squash_merge_diff()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _| Box::pin(async { Ok("   ".to_string()) }));
        mock_git_client.expect_head_commit_message().times(0);
        mock_git_client.expect_squash_merge().times(0);
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) = build_merge_task_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::execute_merge_workflow(input).await;

        // Assert
        assert_eq!(
            result,
            Ok("Session changes from agentty/session-123 are already present in main".to_string(),)
        );
    }

    #[tokio::test]
    async fn test_rebase_assist_input_clone() {
        // Arrange
        let (tx, _rx) = mpsc::unbounded_channel();
        let db = Database::open_in_memory().await.expect("failed to open db");
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = RebaseAssistInput {
            app_event_tx: tx,
            base_branch: "main".to_string(),
            child_pid: Arc::new(Mutex::new(None)),
            db,
            folder: temp_dir.path().to_path_buf(),
            fs_client: test_fs_client(),
            git_client: Arc::new(git::RealGitClient),
            id: "session-123".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            session_model: AgentModel::Gemini3FlashPreview,
        };

        // Act
        #[allow(clippy::clone_on_copy)] // Explicit clone to test derivation
        let cloned_input = input.clone();

        // Assert
        assert_eq!(input.base_branch, cloned_input.base_branch);
        assert_eq!(input.id, cloned_input.id);
        assert_eq!(input.folder, cloned_input.folder);
        assert_eq!(input.session_model, cloned_input.session_model);
    }

    #[tokio::test]
    async fn test_execute_rebase_workflow_aborts_when_assist_loop_fails() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_has_commits_since()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(Some("Existing session commit".to_string())) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async { Err(GitError::OutputParse("state query failed".to_string())) })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::execute_rebase_workflow(input).await;

        // Assert
        let error = result.expect_err("rebase workflow should fail");
        assert!(
            error.contains("Failed to rebase: state query failed"),
            "workflow error should include assist-loop failure reason"
        );
    }

    #[tokio::test]
    async fn test_run_rebase_assist_loop_core_aborts_on_early_error() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "failed to list conflicts".to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            None,
        )
        .await;

        // Assert
        let error = result.expect_err("assist loop should fail");
        assert_eq!(error, "failed to list conflicts");
    }

    /// Verifies session rebase assistance stops when the same conflict detail
    /// repeats after the initial conflict state.
    #[tokio::test]
    async fn test_run_rebase_assist_loop_core_stops_on_repeated_conflict_detail() {
        // Arrange
        let repeated_detail = "CONFLICT (content): Merge conflict in src/lib.rs".to_string();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
        mock_git_client
            .expect_rebase_continue()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning({
                let repeated_detail = repeated_detail.clone();

                move |_| {
                    let repeated_detail = repeated_detail.clone();

                    Box::pin(async move {
                        Ok(git::RebaseStepResult::Conflict {
                            detail: repeated_detail,
                        })
                    })
                }
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            Some(repeated_detail.clone()),
        )
        .await;

        // Assert
        let error = result.expect_err("assist loop should stop on repeated conflict detail");
        assert_eq!(
            error,
            format!(
                "Rebase assistance made no progress: repeated identical conflict state. Last \
                 detail: {repeated_detail}"
            )
        );
    }

    /// Verifies session rebase assistance surfaces the final conflict detail
    /// when every retry hits a distinct conflict state until the retry budget
    /// is exhausted.
    #[tokio::test]
    async fn test_run_rebase_assist_loop_core_reports_retry_exhaustion_detail() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        for detail in [
            "CONFLICT (content): Merge conflict in src/lib.rs",
            "CONFLICT (content): Merge conflict in src/main.rs",
            "CONFLICT (content): Merge conflict in README.md",
        ] {
            mock_git_client
                .expect_list_conflicted_files()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Box::pin(async { Ok(Vec::new()) }));
            mock_git_client
                .expect_list_staged_conflict_marker_files()
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
            mock_git_client
                .expect_rebase_continue()
                .times(1)
                .in_sequence(&mut sequence)
                .returning({
                    let detail = detail.to_string();

                    move |_| {
                        let detail = detail.clone();

                        Box::pin(async move { Ok(git::RebaseStepResult::Conflict { detail }) })
                    }
                });
        }
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_assist_loop_core(
            RebaseAssistLoopInput::Session(input),
            None,
        )
        .await;

        // Assert
        let error = result.expect_err("assist loop should report the final retry conflict");
        assert_eq!(
            error,
            "Rebase still has conflicts after assistance: CONFLICT (content): Merge conflict in \
             README.md"
        );
    }

    #[tokio::test]
    async fn test_run_rebase_start_recovers_stale_rebase_state_and_retries() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "fatal: It seems that there is already a rebase-merge directory"
                            .to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_start(&input).await;

        // Assert
        assert_eq!(result, Ok(git::RebaseStepResult::Completed));
    }

    #[tokio::test]
    async fn test_run_rebase_start_reports_cleanup_failure_for_stale_rebase_state() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "fatal: It seems that there is already a rebase-merge directory"
                            .to_string(),
                    ))
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Box::pin(async { Err(GitError::OutputParse("abort failed".to_string())) })
            });
        let (_temp_dir, input) =
            build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_start(&input).await;

        // Assert
        let error = result.expect_err("cleanup failure should stop retry flow");
        assert!(
            error.contains("Cleanup with `git rebase --abort` failed: abort failed"),
            "error should include abort failure detail"
        );
    }

    /// Verifies merged-session cleanup surfaces branch deletion failures after
    /// the worktree itself has already been removed.
    #[tokio::test]
    async fn test_cleanup_merged_session_worktree_reports_delete_branch_failure() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let folder = temp_dir.path().join("session-worktree");
        let repo_root = temp_dir.path().join("repo-root");
        let source_branch = "agentty/session-123".to_string();
        let mut mock_git_client = git::MockGitClient::new();
        let mut mock_fs_client = fs::MockFsClient::new();
        mock_git_client
            .expect_remove_worktree()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_delete_branch()
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Err(GitError::OutputParse("delete failed".to_string())) })
            });
        mock_fs_client.expect_remove_dir_all().times(0);

        // Act
        let result = SessionManager::cleanup_merged_session_worktree(
            folder,
            Arc::new(mock_fs_client),
            Arc::new(mock_git_client),
            source_branch,
            Some(repo_root),
        )
        .await;

        // Assert
        assert_eq!(result, Err("delete failed".to_string()));
    }

    #[test]
    fn test_detail_message_for_uncommitted_changes_uses_sentence_lines() {
        // Arrange
        let sync_error = SyncSessionStartError::MainHasUncommittedChanges {
            default_branch: "main".to_string(),
        };

        // Act
        let detail_message = sync_error.detail_message();

        // Assert
        assert_eq!(
            detail_message,
            "Sync cannot run while `main` has uncommitted changes.\nCommit or stash changes in \
             `main`, then try again."
        );
    }

    #[tokio::test]
    async fn test_sync_main_for_project_resolves_conflicts_with_assistance() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let working_dir = temp_dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(|folder| Box::pin(async move { Some(folder) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_get_ahead_behind()
            .times(1)
            .return_once(|_| Box::pin(async { Ok((1, 2)) }));
        mock_git_client
            .expect_list_upstream_commit_titles()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(vec![
                        "Update changelog format".to_string(),
                        "Fix sync popup copy".to_string(),
                    ])
                })
            });
        mock_git_client
            .expect_get_ahead_behind()
            .times(1)
            .return_once(|_| Box::pin(async { Ok((1, 0)) }));
        mock_git_client
            .expect_list_local_commit_titles()
            .times(1)
            .returning(|_| {
                Box::pin(async { Ok(vec!["Refine sync conflict messaging".to_string()]) })
            });
        mock_git_client
            .expect_pull_rebase()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(git::PullRebaseResult::Conflict {
                        detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                    })
                })
            });
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(2)
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        mock_git_client
            .expect_stage_all()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(1)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_continue()
            .times(1)
            .returning(|_| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
        mock_git_client
            .expect_push_current_branch()
            .times(1)
            .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
        mock_git_client.expect_abort_rebase().times(0);

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        // Act
        let result = SessionManager::sync_main_for_project_with_assist_client(
            Some("main".to_string()),
            working_dir,
            test_fs_client(),
            Arc::new(mock_git_client),
            AgentModel::Gemini3FlashPreview,
            Arc::new(mock_sync_assist_client),
        )
        .await;

        // Assert
        assert_eq!(
            result,
            Ok(SyncMainOutcome {
                pulled_commit_titles: vec![
                    "Update changelog format".to_string(),
                    "Fix sync popup copy".to_string(),
                ],
                pulled_commits: Some(2),
                pushed_commit_titles: vec!["Refine sync conflict messaging".to_string()],
                pushed_commits: Some(1),
                resolved_conflict_files: vec!["src/lib.rs".to_string()],
            }),
            "sync should succeed after assistance with summary details"
        );
    }

    #[tokio::test]
    async fn test_sync_main_for_project_fails_after_max_assistance_attempts() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let working_dir = temp_dir.path().to_path_buf();
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_find_git_repo_root()
            .times(1)
            .returning(|folder| Box::pin(async move { Some(folder) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_get_ahead_behind()
            .times(1)
            .returning(|_| Box::pin(async { Ok((0, 1)) }));
        mock_git_client
            .expect_list_upstream_commit_titles()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec!["Upstream patch".to_string()]) }));
        mock_git_client
            .expect_pull_rebase()
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(git::PullRebaseResult::Conflict {
                        detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                    })
                })
            });
        mock_git_client
            .expect_list_conflicted_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        mock_git_client
            .expect_stage_all()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client.expect_rebase_continue().times(0);
        mock_git_client.expect_push_current_branch().times(0);
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        // Act
        let result = SessionManager::sync_main_for_project_with_assist_client(
            Some("main".to_string()),
            working_dir,
            test_fs_client(),
            Arc::new(mock_git_client),
            AgentModel::Gemini3FlashPreview,
            Arc::new(mock_sync_assist_client),
        )
        .await;

        // Assert
        let error = result.expect_err("sync should fail when conflicts remain unresolved");
        assert!(matches!(error, SyncSessionStartError::Other(_)));
        assert!(
            error
                .detail_message()
                .contains("Conflicts remain unresolved after maximum assistance attempts"),
            "error detail should mention unresolved conflicts"
        );
    }

    /// Verifies sync assistance merges tracked conflicts with staged conflict
    /// marker files and returns a sorted unique list.
    #[tokio::test]
    async fn test_load_sync_conflicted_files_merges_and_sorts_results() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .returning(|_| {
                Box::pin(async { Ok(vec!["src/b.rs".to_string(), "src/c.rs".to_string()]) })
            });
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Ok(vec!["src/a.rs".to_string(), "src/c.rs".to_string()]) })
            });

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(0);
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let conflicted_files = SessionManager::load_sync_conflicted_files(&input, &[]).await;

        // Assert
        assert_eq!(
            conflicted_files,
            Ok(vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ])
        );
    }

    /// Verifies sync conflict checks keep the loop in assistance mode when
    /// staged files still contain conflict markers.
    #[tokio::test]
    async fn test_stage_and_check_for_sync_conflicts_detects_remaining_markers() {
        // Arrange
        let mut mock_git_client = git::MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_stage_all()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(0);
        let temp_dir = tempdir().expect("failed to create temporary test directory");
        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let still_has_conflicts =
            SessionManager::stage_and_check_for_sync_conflicts(&input, &["src/lib.rs".to_string()])
                .await;

        // Assert
        assert_eq!(still_has_conflicts, Ok(true));
    }

    /// Verifies sync assistance aborts when the conflicted file fingerprint
    /// repeats across attempts without any file changes.
    #[tokio::test]
    async fn test_run_sync_rebase_assist_loop_aborts_for_unchanged_conflict_files() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let conflict_file = temp_dir.path().join("src/lib.rs");
        std::fs::create_dir_all(
            conflict_file
                .parent()
                .expect("conflict file should have a parent directory"),
        )
        .expect("create conflict directory");
        std::fs::write(&conflict_file, "<<<<<<< HEAD\none\n=======\ntwo\n>>>>>>>")
            .expect("write conflict file");

        let fingerprint_fs_client = create_passthrough_mock_fs_client();
        let fingerprint = SessionManager::conflicted_file_fingerprint(
            &fingerprint_fs_client,
            temp_dir.path(),
            &["src/lib.rs".to_string()],
        )
        .await;

        let mut mock_git_client = git::MockGitClient::new();
        mock_git_client
            .expect_list_conflicted_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(REBASE_ASSIST_POLICY.max_attempts)
            .returning(|_, _| Box::pin(async { Ok(vec![]) }));
        mock_git_client
            .expect_stage_all()
            .times(REBASE_ASSIST_POLICY.max_attempts - 1)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(REBASE_ASSIST_POLICY.max_attempts - 1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_sync_assist_client = MockSyncAssistClient::new();
        mock_sync_assist_client
            .expect_resolve_rebase_conflicts()
            .times(REBASE_ASSIST_POLICY.max_attempts - 1)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        let input = build_sync_rebase_input_for_test(
            temp_dir.path().to_path_buf(),
            Arc::new(mock_git_client),
            Arc::new(mock_sync_assist_client),
        );

        // Act
        let result = SessionManager::run_sync_rebase_assist_loop(input, fingerprint).await;

        // Assert
        assert_eq!(
            result,
            Err(
                "Sync rebase assistance made no progress: conflicted files did not change"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_changes_with_file_content() {
        // Arrange
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_content_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("conflict.rs");
        let files = vec!["conflict.rs".to_string()];

        // Act
        std::fs::write(&file_path, "<<<<<<< HEAD\nfoo\n=======\nbar\n>>>>>>>")
            .expect("write initial content");
        let fingerprint_before =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
        std::fs::write(
            &file_path,
            "<<<<<<< HEAD\nfoo_patched\n=======\nbar\n>>>>>>>",
        )
        .expect("write patched content");
        let fingerprint_after =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

        // Assert — partial progress changes the fingerprint
        assert_ne!(fingerprint_before, fingerprint_after);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_stable_for_unchanged_content() {
        // Arrange
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_stable_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        std::fs::write(temp_dir.join("conflict.rs"), "same content").expect("write file");
        let files = vec!["conflict.rs".to_string()];

        // Act
        let fingerprint_a =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
        let fingerprint_b =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

        // Assert — identical content produces identical fingerprint
        assert_eq!(fingerprint_a, fingerprint_b);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_order_independent() {
        // Arrange
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_order_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        std::fs::write(temp_dir.join("a.rs"), "content a").expect("write a.rs");
        std::fs::write(temp_dir.join("b.rs"), "content b").expect("write b.rs");

        // Act
        let fingerprint_ab = SessionManager::conflicted_file_fingerprint(
            &fs_client,
            &temp_dir,
            &["a.rs".to_string(), "b.rs".to_string()],
        )
        .await;
        let fingerprint_ba = SessionManager::conflicted_file_fingerprint(
            &fs_client,
            &temp_dir,
            &["b.rs".to_string(), "a.rs".to_string()],
        )
        .await;

        // Assert — order of file list does not affect the fingerprint
        assert_eq!(fingerprint_ab, fingerprint_ba);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_conflicted_file_fingerprint_missing_file_is_stable() {
        // Arrange — reference a file that does not exist on disk
        let fs_client = create_passthrough_mock_fs_client();
        let temp_dir = std::env::temp_dir().join(format!(
            "agentty_fp_missing_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let files = vec!["nonexistent.rs".to_string()];

        // Act — should not panic; missing files are silently skipped
        let fingerprint_a =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;
        let fingerprint_b =
            SessionManager::conflicted_file_fingerprint(&fs_client, &temp_dir, &files).await;

        // Assert — deterministic even when file is absent
        assert_eq!(fingerprint_a, fingerprint_b);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
