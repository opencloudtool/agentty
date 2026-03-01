//! Merge, rebase, and cleanup workflows for session branches.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use askama::Template;
use serde::Deserialize;
use tokio::sync::mpsc;

use super::access::{SESSION_HANDLES_NOT_FOUND_ERROR, SESSION_NOT_FOUND_ERROR};
use super::{COMMIT_MESSAGE, session_branch};
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::session::SessionTaskService;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::session::Status;
use crate::infra::db::Database;
use crate::infra::git::{self, GitClient};

const MERGE_COMMIT_MESSAGE_TIMEOUT: Duration = Duration::from_mins(2);
const REBASE_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 3,
    // Allow up to 3 consecutive identical-content observations before
    // giving up, so the agent gets a genuine second chance when partial
    // progress is made inside a file without fully clearing all markers.
    max_identical_failure_streak: 3,
};

/// Askama view model for rendering rebase conflict-assistance prompts.
#[derive(Template)]
#[template(path = "rebase_assist_prompt.md", escape = "none")]
struct RebaseAssistPromptTemplate<'a> {
    base_branch: &'a str,
    conflicted_files: &'a str,
}

/// Askama view model for rendering squash commit-message generation prompts.
#[derive(Template)]
#[template(path = "merge_commit_message_prompt.md", escape = "none")]
struct MergeCommitMessagePromptTemplate<'a> {
    diff: &'a str,
}

/// Boxed async result used by sync conflict assistance boundary methods.
type SyncAssistFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Deserialize)]
/// Parsed response schema used when generating merge commit messages via model.
pub(crate) struct ModelMergeCommitMessageResponse {
    /// Optional commit body text.
    pub(crate) description: String,
    /// One-line commit title.
    pub(crate) title: String,
}

struct MergeTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
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
    db: Database,
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    id: String,
    output: Arc<Mutex<String>>,
    session_model: AgentModel,
}

struct RebaseTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
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
    /// Runs one sync conflict assistance command in a blocking worker.
    ///
    /// # Errors
    /// Returns an error when spawning, joining, or executing the agent command
    /// fails.
    async fn run_assist_command(
        folder: PathBuf,
        prompt: String,
        session_model: AgentModel,
    ) -> Result<(), String> {
        tokio::task::spawn_blocking(move || {
            let backend = crate::infra::agent::create_backend(session_model.kind());
            let mut command = backend
                .build_command(crate::infra::agent::BuildCommandRequest {
                    folder: &folder,
                    mode: crate::infra::agent::AgentCommandMode::Start { prompt: &prompt },
                    model: session_model.as_str(),
                })
                .map_err(|error| {
                    format!("Failed to build sync rebase assist model command: {error}")
                })?;
            command.stdin(Stdio::null());

            let output = command
                .output()
                .map_err(|error| format!("Failed to run sync rebase assist model: {error}"))?;
            if output.status.success() {
                return Ok(());
            }

            let detail = Self::assist_output_detail(&output.stdout, &output.stderr);

            Err(format!(
                "Sync rebase assistance failed with exit code {}: {detail}",
                output
                    .status
                    .code()
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            ))
        })
        .await
        .map_err(|error| format!("Failed to join sync rebase assist task: {error}"))?
    }

    /// Formats stdout/stderr text for sync-assist failure messages.
    fn assist_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
        let trimmed_stdout = String::from_utf8_lossy(stdout).trim().to_string();
        let trimmed_stderr = String::from_utf8_lossy(stderr).trim().to_string();
        if !trimmed_stderr.is_empty() && !trimmed_stdout.is_empty() {
            return format!("stderr: {trimmed_stderr}; stdout: {trimmed_stdout}");
        }

        if !trimmed_stderr.is_empty() {
            return format!("stderr: {trimmed_stderr}");
        }

        if !trimmed_stdout.is_empty() {
            return format!("stdout: {trimmed_stdout}");
        }

        "no stdout or stderr output".to_string()
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
        let session = self
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
        let git_client = self.git_client();

        let handles = self
            .session_handles_or_err(session_id)
            .map_err(|_| SESSION_HANDLES_NOT_FOUND_ERROR.to_string())?;
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

                return Err(error);
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
            db,
            folder,
            git_client,
            id: id.clone(),
            output,
            repo_root,
            session_model,
            source_branch: session_branch(&id),
            status,
        };
        tokio::spawn(async move {
            Self::run_merge_task(merge_task_input).await;
        });

        Ok(())
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
    /// Returns an error when the rebase step fails, squash-merge git commands
    /// fail, status transitions are invalid, or worktree cleanup fails.
    async fn execute_merge_workflow(input: MergeTaskInput) -> Result<String, String> {
        let MergeTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            git_client,
            id,
            output,
            repo_root,
            session_model,
            source_branch,
            status,
        } = input;

        // Rebase onto the base branch first to ensure the merge is clean and
        // includes all recent changes. This also handles auto-commit and
        // conflict resolution via the agent.
        let rebase_input = RebaseAssistInput {
            app_event_tx: app_event_tx.clone(),
            base_branch: base_branch.clone(),
            db: db.clone(),
            folder: folder.clone(),
            git_client: Arc::clone(&git_client),
            id: id.clone(),
            output: Arc::clone(&output),
            session_model,
        };
        if let Err(error) = Self::execute_rebase_workflow(rebase_input).await {
            return Err(format!("Merge failed during rebase step: {error}"));
        }

        let squash_diff = {
            let repo_root = repo_root.clone();
            let source_branch = source_branch.clone();
            let base_branch = base_branch.clone();

            git_client
                .squash_merge_diff(repo_root, source_branch, base_branch)
                .await
                .map_err(|error| format!("Failed to inspect merge diff: {error}"))?
        };

        let (merge_outcome, commit_message) = if squash_diff.trim().is_empty() {
            (git::SquashMergeOutcome::AlreadyPresentInTarget, None)
        } else {
            let fallback_commit_message =
                Self::fallback_merge_commit_message(&source_branch, &base_branch);
            let commit_message = {
                let folder = folder.clone();
                let squash_diff = squash_diff.clone();
                let fallback_commit_message_for_task = fallback_commit_message.clone();
                let generate_message = tokio::task::spawn_blocking(move || {
                    Self::generate_merge_commit_message_from_diff(
                        &folder,
                        session_model,
                        &squash_diff,
                    )
                    .unwrap_or(fallback_commit_message_for_task)
                });

                match tokio::time::timeout(MERGE_COMMIT_MESSAGE_TIMEOUT, generate_message).await {
                    Ok(Ok(message)) => message,
                    Ok(Err(_)) | Err(_) => fallback_commit_message,
                }
            };

            let merge_outcome = {
                let repo_root = repo_root.clone();
                let source_branch = source_branch.clone();
                let base_branch = base_branch.clone();
                let commit_message = commit_message.clone();
                git_client
                    .squash_merge(repo_root, source_branch, base_branch, commit_message)
                    .await?
            };

            (merge_outcome, Some(commit_message))
        };

        if !SessionTaskService::update_status(&status, &db, &app_event_tx, &id, Status::Done).await
        {
            return Err("Invalid status transition to Done".to_string());
        }

        Self::cleanup_merged_session_worktree(
            folder.clone(),
            Arc::clone(&git_client),
            source_branch.clone(),
            Some(repo_root),
        )
        .await
        .map_err(|error| format!("Merged successfully but failed to remove worktree: {error}"))?;

        if let Some(commit_message) = commit_message {
            Self::update_session_title_and_summary_from_commit_message(
                &db,
                &id,
                &commit_message,
                &app_event_tx,
            )
            .await;
        }

        Ok(Self::merge_success_message(
            &source_branch,
            &base_branch,
            merge_outcome,
        ))
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
        let session = self
            .session_or_err(session_id)
            .map_err(|_| SESSION_NOT_FOUND_ERROR.to_string())?;
        if session.status != Status::Review {
            return Err("Session must be in review status".to_string());
        }

        let base_branch = services
            .db()
            .get_session_base_branch(&session.id)
            .await?
            .ok_or_else(|| "No git worktree for this session".to_string())?;

        let handles = self
            .session_handles_or_err(session_id)
            .map_err(|_| SESSION_HANDLES_NOT_FOUND_ERROR.to_string())?;
        let output = Arc::clone(&handles.output);

        let status = Arc::clone(&handles.status);
        let db = services.db().clone();
        let app_event_tx = services.event_sender();
        let git_client = self.git_client();

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
            db,
            folder: session.folder.clone(),
            git_client,
            id,
            output,
            session_model,
            status,
        };
        tokio::spawn(async move {
            Self::run_rebase_task(rebase_task_input).await;
        });

        Ok(())
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
        let sync_assist_client: Arc<dyn SyncAssistClient> = Arc::new(RealSyncAssistClient);

        Self::sync_main_for_project_with_assist_client(
            default_branch,
            working_dir,
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
            .map_err(SyncSessionStartError::Other)?;
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
            .map_err(SyncSessionStartError::Other)?;
        let mut resolved_conflict_files = Vec::new();
        if let git::PullRebaseResult::Conflict { detail } = pull_result {
            let sync_rebase_input = SyncRebaseAssistInput {
                base_branch: default_branch.clone(),
                folder: working_dir.clone(),
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
            .map_err(SyncSessionStartError::Other)?;

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
            .await?;

        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
            .await?;
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
        input.git_client.stage_all(folder).await?;

        let folder = input.folder.clone();
        if input.git_client.has_unmerged_paths(folder).await? {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, conflict_files.to_vec())
            .await?;

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
        let result = input.git_client.rebase_continue(folder).await?;

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
            db,
            folder,
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
                db: db.clone(),
                folder: folder.clone(),
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
    async fn execute_rebase_workflow(input: RebaseAssistInput) -> Result<String, String> {
        // Auto-commit any pending changes before rebasing to avoid
        // "cannot rebase: You have unstaged changes".
        if let Err(error) =
            Self::commit_changes_with_git_client(input.git_client.as_ref(), &input.folder, true)
                .await
            && !error.contains("Nothing to commit")
        {
            return Err(format!(
                "Failed to commit pending changes before rebase: {error}"
            ));
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

    /// Updates persisted session title and summary from the final squash commit
    /// message.
    pub(crate) async fn update_session_title_and_summary_from_commit_message(
        db: &Database,
        session_id: &str,
        commit_message: &str,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
    ) {
        let (title, summary) = Self::session_title_and_summary_from_commit_message(commit_message);

        let _ = db.update_session_title(session_id, &title).await;

        let _ = db.update_session_summary(session_id, &summary).await;

        let _ = app_event_tx.send(AppEvent::RefreshSessions);
    }

    fn session_title_and_summary_from_commit_message(commit_message: &str) -> (String, String) {
        let trimmed_message = commit_message.trim();
        if trimmed_message.is_empty() {
            let fallback_message = "Apply session updates".to_string();

            return (fallback_message.clone(), fallback_message);
        }

        let title = trimmed_message
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("Apply session updates")
            .to_string();
        let summary = trimmed_message.to_string();

        (title, summary)
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

                let conflict_fingerprint =
                    Self::conflicted_file_fingerprint(assist_input.folder(), &conflicted_files);
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
        let is_rebase_in_progress = input.git_client.is_rebase_in_progress(folder).await?;

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
                if !Self::is_stale_rebase_state_error(&error) {
                    return Err(error);
                }

                Self::recover_from_stale_rebase_start_error(input, &error).await?;

                input.git_client.rebase_start(folder, base_branch).await
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
            .await?;

        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
            .await?;
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
        input.git_client.stage_all(folder).await?;

        let folder = input.folder.clone();
        if input.git_client.has_unmerged_paths(folder).await? {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers = input
            .git_client
            .list_staged_conflict_marker_files(folder, conflict_files.to_vec())
            .await?;

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
        let result = input.git_client.rebase_continue(folder).await?;

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
    fn conflicted_file_fingerprint(folder: &Path, conflicted_files: &[String]) -> String {
        let mut sorted_files = conflicted_files.to_vec();
        sorted_files.sort_unstable();

        let mut hasher = DefaultHasher::new();
        for file in &sorted_files {
            file.hash(&mut hasher);
            let file_path = folder.join(file);
            if let Ok(content) = std::fs::read(&file_path) {
                content.hash(&mut hasher);
            }
        }

        format!("{:016x}", hasher.finish())
    }

    fn assist_context(input: &RebaseAssistInput) -> AssistContext {
        AssistContext {
            app_event_tx: input.app_event_tx.clone(),
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

    fn generate_merge_commit_message_from_diff(
        folder: &Path,
        session_model: AgentModel,
        diff: &str,
    ) -> Option<String> {
        let prompt = Self::merge_commit_message_prompt(diff).ok()?;
        let model_response =
            Self::generate_merge_commit_message_with_model(folder, session_model, &prompt).ok()?;
        let parsed_response = Self::parse_merge_commit_message_response(&model_response)?;
        let message = if parsed_response.description.is_empty() {
            parsed_response.title
        } else {
            format!(
                "{}\n\n{}",
                parsed_response.title, parsed_response.description
            )
        };

        Some(message)
    }

    fn generate_merge_commit_message_with_model(
        folder: &Path,
        session_model: AgentModel,
        prompt: &str,
    ) -> Result<String, String> {
        let backend = crate::infra::agent::create_backend(session_model.kind());
        let mut command = backend
            .build_command(crate::infra::agent::BuildCommandRequest {
                folder,
                mode: crate::infra::agent::AgentCommandMode::Start { prompt },
                model: session_model.as_str(),
            })
            .map_err(|error| format!("Failed to build merge commit message command: {error}"))?;
        command.stdin(Stdio::null());
        let output = command
            .output()
            .map_err(|error| format!("Failed to run merge commit message model: {error}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed = crate::infra::agent::parse_response(session_model.kind(), &stdout, &stderr);
        let content = parsed.content.trim().to_string();

        if content.is_empty() {
            let stderr_text = stderr.trim();
            if stderr_text.is_empty() {
                return Err("Merge commit message model returned empty output".to_string());
            }

            return Err(format!(
                "Merge commit message model returned empty output: {stderr_text}"
            ));
        }

        Ok(content)
    }

    pub(crate) fn parse_merge_commit_message_response(
        content: &str,
    ) -> Option<ModelMergeCommitMessageResponse> {
        serde_json::from_str(content.trim()).ok().or_else(|| {
            let json_start = content.find('{')?;
            let json_end = content.rfind('}')?;
            let json = &content[json_start..=json_end];
            serde_json::from_str(json).ok()
        })
    }

    /// Renders the merge commit-message prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error if Askama template rendering fails.
    pub(crate) fn merge_commit_message_prompt(diff: &str) -> Result<String, String> {
        let template = MergeCommitMessagePromptTemplate { diff };

        template
            .render()
            .map_err(|error| format!("Failed to render `merge_commit_message_prompt.md`: {error}"))
    }

    pub(crate) fn fallback_merge_commit_message(
        source_branch: &str,
        target_branch: &str,
    ) -> String {
        format!("Apply session updates\n\n- Squash merge `{source_branch}` into `{target_branch}`.")
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
        git_client: Arc<dyn GitClient>,
        source_branch: String,
        repo_root: Option<PathBuf>,
    ) -> Result<(), String> {
        let repo_root = match repo_root {
            Some(repo_root) => Some(repo_root),
            None => git_client.main_repo_root(folder.clone()).await.ok(),
        };

        git_client.remove_worktree(folder.clone()).await?;

        if let Some(repo_root) = repo_root {
            git_client.delete_branch(repo_root, source_branch).await?;
        }

        let _ = tokio::fs::remove_dir_all(&folder).await;

        Ok(())
    }

    /// Commits all changes using a caller-provided git client.
    ///
    /// This test-only helper keeps high-level session tests isolated from
    /// shelling out to real git commands.
    #[cfg(test)]
    pub(crate) async fn commit_changes_with_client_for_test(
        git_client: &dyn GitClient,
        folder: &Path,
        no_verify: bool,
    ) -> Result<String, String> {
        Self::commit_changes_with_git_client(git_client, folder, no_verify).await
    }

    /// Commits all changes in a session worktree using the provided git client.
    ///
    /// This helper exists so high-level merge/rebase tests can mock external
    /// git command behavior while preserving production commit semantics.
    async fn commit_changes_with_git_client(
        git_client: &dyn GitClient,
        folder: &Path,
        no_verify: bool,
    ) -> Result<String, String> {
        let folder = folder.to_path_buf();
        git_client
            .commit_all_preserving_single_commit(
                folder.clone(),
                COMMIT_MESSAGE.to_string(),
                no_verify,
            )
            .await?;

        git_client.head_short_hash(folder).await
    }
}

#[cfg(test)]
mod tests {
    use mockall::Sequence;

    use super::*;

    /// Builds rebase assistance input with the provided git client for unit
    /// tests.
    async fn build_rebase_assist_input_for_test(
        git_client: Arc<dyn GitClient>,
    ) -> RebaseAssistInput {
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let db = Database::open_in_memory().await.expect("failed to open db");

        RebaseAssistInput {
            app_event_tx,
            base_branch: "main".to_string(),
            db,
            folder: PathBuf::from("/tmp/rebase-start-test"),
            git_client,
            id: "session-123".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            session_model: AgentModel::Gemini3FlashPreview,
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
    fn test_session_title_and_summary_from_commit_message() {
        // Arrange
        let commit_message = "Refine merge flow\n\n- Update title handling";

        // Act
        let (title, summary) =
            SessionManager::session_title_and_summary_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Refine merge flow");
        assert_eq!(summary, "Refine merge flow\n\n- Update title handling");
    }

    #[test]
    fn test_session_title_and_summary_from_commit_message_skips_blank_prefix() {
        // Arrange
        let commit_message = "\n\nRefine merge flow\n\n- Update title handling";

        // Act
        let (title, summary) =
            SessionManager::session_title_and_summary_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Refine merge flow");
        assert_eq!(summary, "Refine merge flow\n\n- Update title handling");
    }

    #[test]
    fn test_session_title_and_summary_from_commit_message_empty_uses_fallback() {
        // Arrange
        let commit_message = "  \n";

        // Act
        let (title, summary) =
            SessionManager::session_title_and_summary_from_commit_message(commit_message);

        // Assert
        assert_eq!(title, "Apply session updates");
        assert_eq!(summary, "Apply session updates");
    }

    #[tokio::test]
    async fn test_rebase_assist_input_clone() {
        // Arrange
        let (tx, _rx) = mpsc::unbounded_channel();
        let db = Database::open_in_memory().await.expect("failed to open db");
        let input = RebaseAssistInput {
            app_event_tx: tx,
            base_branch: "main".to_string(),
            db,
            folder: PathBuf::from("/tmp/test"),
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
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Err("state query failed".to_string()) }));
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let input = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

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
            .returning(|_| Box::pin(async { Err("failed to list conflicts".to_string()) }));
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        let input = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

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
                    Err(
                        "fatal: It seems that there is already a rebase-merge directory"
                            .to_string(),
                    )
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
        let input = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

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
                    Err(
                        "fatal: It seems that there is already a rebase-merge directory"
                            .to_string(),
                    )
                })
            });
        mock_git_client
            .expect_abort_rebase()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Err("abort failed".to_string()) }));
        let input = build_rebase_assist_input_for_test(Arc::new(mock_git_client)).await;

        // Act
        let result = SessionManager::run_rebase_start(&input).await;

        // Assert
        let error = result.expect_err("cleanup failure should stop retry flow");
        assert!(
            error.contains("Cleanup with `git rebase --abort` failed: abort failed"),
            "error should include abort failure detail"
        );
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
        let working_dir = PathBuf::from("/tmp/sync-main-assist-success");
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
            .returning(|_| Box::pin(async { Ok(()) }));
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
        let working_dir = PathBuf::from("/tmp/sync-main-assist-fail");
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

    #[test]
    fn test_conflicted_file_fingerprint_changes_with_file_content() {
        // Arrange
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
        let fingerprint_before = SessionManager::conflicted_file_fingerprint(&temp_dir, &files);
        std::fs::write(
            &file_path,
            "<<<<<<< HEAD\nfoo_patched\n=======\nbar\n>>>>>>>",
        )
        .expect("write patched content");
        let fingerprint_after = SessionManager::conflicted_file_fingerprint(&temp_dir, &files);

        // Assert — partial progress changes the fingerprint
        assert_ne!(fingerprint_before, fingerprint_after);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_conflicted_file_fingerprint_stable_for_unchanged_content() {
        // Arrange
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
        let fingerprint_a = SessionManager::conflicted_file_fingerprint(&temp_dir, &files);
        let fingerprint_b = SessionManager::conflicted_file_fingerprint(&temp_dir, &files);

        // Assert — identical content produces identical fingerprint
        assert_eq!(fingerprint_a, fingerprint_b);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_conflicted_file_fingerprint_order_independent() {
        // Arrange
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
            &temp_dir,
            &["a.rs".to_string(), "b.rs".to_string()],
        );
        let fingerprint_ba = SessionManager::conflicted_file_fingerprint(
            &temp_dir,
            &["b.rs".to_string(), "a.rs".to_string()],
        );

        // Assert — order of file list does not affect the fingerprint
        assert_eq!(fingerprint_ab, fingerprint_ba);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_conflicted_file_fingerprint_missing_file_is_stable() {
        // Arrange — reference a file that does not exist on disk
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
        let fingerprint_a = SessionManager::conflicted_file_fingerprint(&temp_dir, &files);
        let fingerprint_b = SessionManager::conflicted_file_fingerprint(&temp_dir, &files);

        // Assert — deterministic even when file is absent
        assert_eq!(fingerprint_a, fingerprint_b);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
