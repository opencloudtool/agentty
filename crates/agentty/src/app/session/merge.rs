//! Merge, rebase, and cleanup workflows for session branches.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;

use super::access::{SESSION_HANDLES_NOT_FOUND_ERROR, SESSION_NOT_FOUND_ERROR};
use super::{COMMIT_MESSAGE, session_branch};
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, effective_permission_mode,
    format_detail_lines, run_agent_assist,
};
use crate::app::task::TaskService;
use crate::app::{AppEvent, AppServices, ProjectManager, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::infra::db::Database;
use crate::infra::git;

const MERGE_COMMIT_MESSAGE_TIMEOUT: Duration = Duration::from_mins(2);
const REBASE_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 3,
    // Allow up to 3 consecutive identical-content observations before
    // giving up, so the agent gets a genuine second chance when partial
    // progress is made inside a file without fully clearing all markers.
    max_identical_failure_streak: 3,
};
const REBASE_ASSIST_PROMPT_TEMPLATE: &str =
    include_str!("../../../resources/rebase_assist_prompt.md");
const MERGE_COMMIT_MESSAGE_PROMPT_TEMPLATE: &str =
    include_str!("../../../resources/merge_commit_message_prompt.md");

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
    id: String,
    output: Arc<Mutex<String>>,
    permission_mode: PermissionMode,
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
    id: String,
    output: Arc<Mutex<String>>,
    permission_mode: PermissionMode,
    session_model: AgentModel,
}

struct RebaseTaskInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    permission_mode: PermissionMode,
    session_model: AgentModel,
    status: Arc<Mutex<Status>>,
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

impl SyncSessionStartError {
    /// Returns user-facing detail for non-popup sync start errors.
    pub(crate) fn detail_message(&self) -> String {
        match self {
            Self::MainHasUncommittedChanges { default_branch } => format!(
                "Sync cannot run while `{default_branch}` has uncommitted changes. Commit or \
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
        let permission_mode = session.permission_mode;
        let session_model = session.model;
        let app_event_tx = services.event_sender();

        let handles = self
            .session_handles_or_err(session_id)
            .map_err(|_| SESSION_HANDLES_NOT_FOUND_ERROR.to_string())?;
        let output = Arc::clone(&handles.output);
        let status = Arc::clone(&handles.status);

        if !TaskService::update_status(&status, &db, &app_event_tx, &id, Status::Merging).await {
            return Err("Invalid status transition to Merging".to_string());
        }

        let base_branch = match db.get_session_base_branch(&id).await {
            Ok(Some(base_branch)) => base_branch,
            Ok(None) => {
                let _ =
                    TaskService::update_status(&status, &db, &app_event_tx, &id, Status::Review)
                        .await;

                return Err("No git worktree for this session".to_string());
            }
            Err(error) => {
                let _ =
                    TaskService::update_status(&status, &db, &app_event_tx, &id, Status::Review)
                        .await;

                return Err(error);
            }
        };

        let working_dir = projects.working_dir().to_path_buf();
        let Some(repo_root) = git::find_git_repo_root(working_dir).await else {
            let _ =
                TaskService::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;

            return Err("Failed to find git repository root".to_string());
        };

        let merge_task_input = MergeTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id: id.clone(),
            output,
            permission_mode,
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
        let MergeTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id,
            output,
            permission_mode,
            repo_root,
            session_model,
            source_branch,
            status,
        } = input;

        let merge_result: Result<String, String> = async {
            // Rebase onto the base branch first to ensure the merge is clean and
            // includes all recent changes. This also handles auto-commit and
            // conflict resolution via the agent.
            let rebase_input = RebaseAssistInput {
                app_event_tx: app_event_tx.clone(),
                base_branch: base_branch.clone(),
                db: db.clone(),
                folder: folder.clone(),
                id: id.clone(),
                output: Arc::clone(&output),
                permission_mode,
                session_model,
            };
            if let Err(error) = Self::execute_rebase_workflow(rebase_input).await {
                return Err(format!("Merge failed during rebase step: {error}"));
            }

            let squash_diff = {
                let repo_root = repo_root.clone();
                let source_branch = source_branch.clone();
                let base_branch = base_branch.clone();

                git::squash_merge_diff(repo_root, source_branch, base_branch)
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

                    match tokio::time::timeout(MERGE_COMMIT_MESSAGE_TIMEOUT, generate_message).await
                    {
                        Ok(Ok(message)) => message,
                        Ok(Err(_)) | Err(_) => fallback_commit_message,
                    }
                };

                let merge_outcome = {
                    let repo_root = repo_root.clone();
                    let source_branch = source_branch.clone();
                    let base_branch = base_branch.clone();
                    let commit_message = commit_message.clone();
                    git::squash_merge(repo_root, source_branch, base_branch, commit_message).await?
                };

                (merge_outcome, Some(commit_message))
            };

            if !TaskService::update_status(&status, &db, &app_event_tx, &id, Status::Done).await {
                return Err("Invalid status transition to Done".to_string());
            }

            Self::cleanup_merged_session_worktree(
                folder.clone(),
                source_branch.clone(),
                Some(repo_root),
            )
            .await
            .map_err(|error| {
                format!("Merged successfully but failed to remove worktree: {error}")
            })?;

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
        .await;

        Self::finalize_merge_task(merge_result, &output, &db, &app_event_tx, &id, &status).await;
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
                TaskService::append_session_output(output, db, app_event_tx, id, &merge_message)
                    .await;
            }
            Err(error) => {
                let merge_error = format!("\n[Merge Error] {error}\n");
                TaskService::append_session_output(output, db, app_event_tx, id, &merge_error)
                    .await;
                let _ =
                    TaskService::update_status(status, db, app_event_tx, id, Status::Review).await;
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

        if !TaskService::update_status(&status, &db, &app_event_tx, &session.id, Status::Rebasing)
            .await
        {
            return Err("Invalid status transition to Rebasing".to_string());
        }

        let id = session.id.clone();
        let permission_mode = session.permission_mode;
        let session_model = session.model;

        let rebase_task_input = RebaseTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder: session.folder.clone(),
            id,
            output,
            permission_mode,
            session_model,
            status,
        };
        tokio::spawn(async move {
            Self::run_rebase_task(rebase_task_input).await;
        });

        Ok(())
    }

    /// Synchronizes the selected project branch with its upstream.
    ///
    /// This is a repository-level operation and does not mutate session state.
    ///
    /// # Errors
    /// Returns a [`SyncSessionStartError`] when sync cannot be started or
    /// `git pull --rebase` fails.
    pub(crate) async fn sync_main(
        &self,
        projects: &ProjectManager,
    ) -> Result<(), SyncSessionStartError> {
        let default_branch = projects.git_branch().map(str::to_string).ok_or_else(|| {
            SyncSessionStartError::Other("Active project has no git branch".to_string())
        })?;

        let working_dir = projects.working_dir().to_path_buf();
        let _repo_root = git::find_git_repo_root(working_dir.clone())
            .await
            .ok_or_else(|| {
                SyncSessionStartError::Other("Failed to find git repository root".to_string())
            })?;

        let is_default_branch_clean = git::is_worktree_clean(working_dir.clone())
            .await
            .map_err(SyncSessionStartError::Other)?;
        if !is_default_branch_clean {
            return Err(SyncSessionStartError::MainHasUncommittedChanges {
                default_branch: default_branch.clone(),
            });
        }

        let pull_result = git::pull_rebase(working_dir)
            .await
            .map_err(SyncSessionStartError::Other)?;
        if let git::PullRebaseResult::Conflict { detail } = pull_result {
            return Err(SyncSessionStartError::Other(format!(
                "Sync stopped on rebase conflicts while updating `{default_branch}`: {detail}"
            )));
        }

        Ok(())
    }

    async fn run_rebase_task(input: RebaseTaskInput) {
        let RebaseTaskInput {
            app_event_tx,
            base_branch,
            db,
            folder,
            id,
            output,
            permission_mode,
            session_model,
            status,
        } = input;

        let rebase_result: Result<String, String> = async {
            let rebase_input = RebaseAssistInput {
                app_event_tx: app_event_tx.clone(),
                base_branch: base_branch.clone(),
                db: db.clone(),
                folder: folder.clone(),
                id: id.clone(),
                output: Arc::clone(&output),
                permission_mode,
                session_model,
            };

            Self::execute_rebase_workflow(rebase_input).await
        }
        .await;

        Self::finalize_rebase_task(rebase_result, &output, &db, &app_event_tx, &id, &status).await;
    }

    async fn execute_rebase_workflow(input: RebaseAssistInput) -> Result<String, String> {
        // Auto-commit any pending changes before rebasing to avoid
        // "cannot rebase: You have unstaged changes".
        if let Err(error) = Self::commit_changes(&input.folder, true).await
            && !error.contains("Nothing to commit")
        {
            return Err(format!(
                "Failed to commit pending changes before rebase: {error}"
            ));
        }

        if let Err(error) = Self::run_rebase_assist_loop(input.clone()).await {
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
                TaskService::append_session_output(output, db, app_event_tx, id, &rebase_message)
                    .await;
            }
            Err(error) => {
                let rebase_error = format!("\n[Rebase Error] {error}\n");
                TaskService::append_session_output(output, db, app_event_tx, id, &rebase_error)
                    .await;
            }
        }

        let _ = TaskService::update_status(status, db, app_event_tx, id, Status::Review).await;
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
        let mut failure_tracker =
            FailureTracker::new(REBASE_ASSIST_POLICY.max_identical_failure_streak);

        let rebase_in_progress = Self::is_rebase_in_progress(&input).await?;
        if !rebase_in_progress {
            let initial_step = Self::run_rebase_start(&input).await?;
            if initial_step == git::RebaseStepResult::Completed {
                return Ok(());
            }
        }

        let mut previous_conflict_files: Vec<String> = vec![];

        for assist_attempt in 1..=REBASE_ASSIST_POLICY.max_attempts {
            let conflicted_files =
                Self::load_conflicted_files(&input, &previous_conflict_files).await?;
            if conflicted_files.is_empty() {
                let continue_step = Self::run_rebase_continue(&input).await?;
                match continue_step {
                    git::RebaseStepResult::Completed => {
                        return Ok(());
                    }
                    git::RebaseStepResult::Conflict { detail } => {
                        if failure_tracker.observe(&detail) {
                            Self::abort_rebase_after_assist_failure(&input.folder).await;

                            return Err(format!(
                                "Rebase assistance made no progress: repeated identical conflict \
                                 state. Last detail: {detail}"
                            ));
                        }

                        if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
                            Self::abort_rebase_after_assist_failure(&input.folder).await;

                            return Err(format!(
                                "Rebase still has conflicts after assistance: {detail}"
                            ));
                        }
                    }
                }

                continue;
            }

            let conflict_fingerprint =
                Self::conflicted_file_fingerprint(&input.folder, &conflicted_files);
            if failure_tracker.observe(&conflict_fingerprint) {
                Self::abort_rebase_after_assist_failure(&input.folder).await;

                return Err(
                    "Rebase assistance made no progress: conflicted files did not change"
                        .to_string(),
                );
            }

            Self::append_rebase_assist_header(&input, assist_attempt, &conflicted_files).await;
            Self::run_rebase_assist_agent(&input, &conflicted_files).await?;

            let still_has_conflicts =
                Self::stage_and_check_for_conflicts(&input, &conflicted_files).await?;
            previous_conflict_files = conflicted_files;
            if still_has_conflicts {
                if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
                    Self::abort_rebase_after_assist_failure(&input.folder).await;

                    return Err(
                        "Conflicts remain unresolved after maximum assistance attempts".to_string(),
                    );
                }

                continue;
            }

            let continue_step = Self::run_rebase_continue(&input).await?;
            match continue_step {
                git::RebaseStepResult::Completed => {
                    return Ok(());
                }
                git::RebaseStepResult::Conflict { detail } => {
                    if failure_tracker.observe(&detail) {
                        Self::abort_rebase_after_assist_failure(&input.folder).await;

                        return Err(format!(
                            "Rebase assistance made no progress: repeated identical conflict \
                             state. Last detail: {detail}"
                        ));
                    }

                    if assist_attempt == REBASE_ASSIST_POLICY.max_attempts {
                        Self::abort_rebase_after_assist_failure(&input.folder).await;

                        return Err(format!(
                            "Rebase still has conflicts after assistance: {detail}"
                        ));
                    }
                }
            }
        }

        Self::abort_rebase_after_assist_failure(&input.folder).await;

        Err("Failed to complete assisted rebase".to_string())
    }

    /// Returns whether the session worktree has an in-progress rebase.
    ///
    /// # Errors
    /// Returns an error if git state cannot be queried.
    async fn is_rebase_in_progress(input: &RebaseAssistInput) -> Result<bool, String> {
        let folder = input.folder.clone();
        let is_rebase_in_progress = git::is_rebase_in_progress(folder).await?;

        Ok(is_rebase_in_progress)
    }

    /// Starts the rebase step for an assisted rebase flow.
    ///
    /// # Errors
    /// Returns an error if spawning the git process fails or git returns a
    /// non-conflict failure.
    async fn run_rebase_start(input: &RebaseAssistInput) -> Result<git::RebaseStepResult, String> {
        let folder = input.folder.clone();
        let base_branch = input.base_branch.clone();
        let result = git::rebase_start(folder, base_branch).await?;

        Ok(result)
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
        let mut conflicted = git::list_conflicted_files(folder.clone()).await?;

        let staged_with_markers =
            git::list_staged_conflict_marker_files(folder, previous_conflict_files.to_vec())
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
        let prompt = Self::rebase_assist_prompt(&input.base_branch, conflicted_files);
        let mut assist_context = Self::assist_context(input);
        assist_context.permission_mode =
            Self::rebase_assist_permission_mode(assist_context.permission_mode);

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
        git::stage_all(folder).await?;

        let folder = input.folder.clone();
        if git::has_unmerged_paths(folder).await? {
            return Ok(true);
        }

        let folder = input.folder.clone();
        let staged_with_markers =
            git::list_staged_conflict_marker_files(folder, conflict_files.to_vec()).await?;

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
        let result = git::rebase_continue(folder).await?;

        Ok(result)
    }

    /// Renders the rebase-assist prompt from the markdown template.
    fn rebase_assist_prompt(base_branch: &str, conflicted_files: &[String]) -> String {
        let conflicted_files = Self::format_conflicted_file_list(conflicted_files);

        REBASE_ASSIST_PROMPT_TEMPLATE
            .replace("{base_branch}", base_branch)
            .replace("{conflicted_files}", &conflicted_files)
    }

    /// Resolves effective permission mode for rebase assistance runs.
    fn rebase_assist_permission_mode(permission_mode: PermissionMode) -> PermissionMode {
        effective_permission_mode(permission_mode)
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
            id: input.id.clone(),
            output: Arc::clone(&input.output),
            permission_mode: input.permission_mode,
            session_model: input.session_model,
        }
    }

    /// Aborts rebase after assistance fails to keep worktree state clean.
    async fn abort_rebase_after_assist_failure(session_folder: &Path) {
        let folder = session_folder.to_path_buf();
        let _ = git::abort_rebase(folder).await;
    }

    fn generate_merge_commit_message_from_diff(
        folder: &Path,
        session_model: AgentModel,
        diff: &str,
    ) -> Option<String> {
        let prompt = Self::merge_commit_message_prompt(diff);
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
        let mut command = backend.build_start_command(
            folder,
            prompt,
            session_model.as_str(),
            PermissionMode::AutoEdit,
            false,
        );
        command.stdin(Stdio::null());
        let output = command
            .output()
            .map_err(|error| format!("Failed to run merge commit message model: {error}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed = crate::infra::agent::parse_response(
            session_model.kind(),
            &stdout,
            &stderr,
        );
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

    pub(crate) fn merge_commit_message_prompt(diff: &str) -> String {
        MERGE_COMMIT_MESSAGE_PROMPT_TEMPLATE.replace("{{diff}}", diff)
    }

    pub(crate) fn fallback_merge_commit_message(
        source_branch: &str,
        target_branch: &str,
    ) -> String {
        format!("Apply session updates\n\n- Squash merge `{source_branch}` into `{target_branch}`.")
    }

    /// Removes a merged session worktree and deletes its source branch.
    ///
    /// # Errors
    /// Returns an error if worktree or branch cleanup fails.
    pub(crate) async fn cleanup_merged_session_worktree(
        folder: PathBuf,
        source_branch: String,
        repo_root: Option<PathBuf>,
    ) -> Result<(), String> {
        let repo_root = repo_root.or_else(|| Self::resolve_repo_root_from_worktree(&folder));

        git::remove_worktree(folder.clone()).await?;

        if let Some(repo_root) = repo_root {
            git::delete_branch(repo_root, source_branch).await?;
        }

        let _ = tokio::fs::remove_dir_all(&folder).await;

        Ok(())
    }

    /// Resolves a repository root path from a git worktree path.
    pub(crate) fn resolve_repo_root_from_worktree(worktree_path: &Path) -> Option<PathBuf> {
        let git_path = worktree_path.join(".git");
        if git_path.is_dir() {
            return Some(worktree_path.to_path_buf());
        }

        if !git_path.is_file() {
            return None;
        }

        let git_file = std::fs::read_to_string(git_path).ok()?;
        let git_dir_line = git_file.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir = PathBuf::from(git_dir_line.trim_start_matches("gitdir:").trim());
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            worktree_path.join(git_dir)
        };

        git_dir.parent()?.parent()?.parent().map(Path::to_path_buf)
    }

    /// Commits all changes in a session worktree using a fixed message.
    ///
    /// The first commit in a session worktree is created normally. Subsequent
    /// commits with the same fixed message amend `HEAD` so the worktree keeps
    /// a single evolving session commit.
    ///
    /// When `no_verify` is `true`, pre-commit and commit-msg hooks are
    /// skipped. Use this for defensive commits (e.g., before rebase) where the
    /// session code was already validated by hooks during the normal
    /// auto-commit flow.
    pub(crate) async fn commit_changes(folder: &Path, no_verify: bool) -> Result<String, String> {
        let folder = folder.to_path_buf();
        git::commit_all_preserving_single_commit(
            folder.clone(),
            COMMIT_MESSAGE.to_string(),
            no_verify,
        )
        .await?;

        git::head_short_hash(folder).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rebase_assist_permission_mode_plan_uses_auto_edit() {
        // Arrange
        let permission_mode = PermissionMode::Plan;

        // Act
        let effective_mode = SessionManager::rebase_assist_permission_mode(permission_mode);

        // Assert
        assert_eq!(effective_mode, PermissionMode::AutoEdit);
    }

    #[test]
    fn test_rebase_assist_prompt_includes_branch_and_files() {
        // Arrange
        let base_branch = "main";
        let conflicted_files = vec!["src/lib.rs".to_string(), "README.md".to_string()];

        // Act
        let prompt = SessionManager::rebase_assist_prompt(base_branch, &conflicted_files);

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
            id: "session-123".to_string(),
            output: Arc::new(Mutex::new(String::new())),
            permission_mode: PermissionMode::AutoEdit,
            session_model: AgentModel::Gemini3FlashPreview,
        };

        // Act
        #[allow(clippy::clone_on_copy)] // Explicit clone to test derivation
        let cloned_input = input.clone();

        // Assert
        assert_eq!(input.base_branch, cloned_input.base_branch);
        assert_eq!(input.id, cloned_input.id);
        assert_eq!(input.folder, cloned_input.folder);
        assert_eq!(input.permission_mode, cloned_input.permission_mode);
        assert_eq!(input.session_model, cloned_input.session_model);
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
