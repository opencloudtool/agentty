//! Merge, rebase, and cleanup workflows for session branches.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;

use super::access::{SESSION_HANDLES_NOT_FOUND_ERROR, SESSION_NOT_FOUND_ERROR};
use super::{COMMIT_MESSAGE, session_branch};
use crate::agent::AgentKind;
use crate::app::task::{RunAgentAssistTaskInput, TaskService};
use crate::app::title::TitleService;
use crate::app::{AppEvent, AppServices, PrManager, ProjectManager, SessionManager};
use crate::db::Database;
use crate::git;
use crate::model::{PermissionMode, Status};

const MERGE_COMMIT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(8);
const REBASE_ASSIST_MAX_ATTEMPTS: usize = 3;
const REBASE_ASSIST_PROMPT_TEMPLATE: &str =
    include_str!("../../../resources/rebase_assist_prompt.md");

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
    pr_creation_in_flight: Arc<Mutex<HashSet<String>>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    repo_root: PathBuf,
    session_agent: AgentKind,
    session_model: String,
    source_branch: String,
    status: Arc<Mutex<Status>>,
}

struct RebaseAssistInput {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_branch: String,
    db: Database,
    folder: PathBuf,
    id: String,
    output: Arc<Mutex<String>>,
    permission_mode: PermissionMode,
    session_agent: AgentKind,
    session_model: String,
}

impl SessionManager {
    /// Starts a squash merge for a reviewed session branch in the background.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for merge, required git
    /// metadata is missing, or the status transition to `Merging` fails.
    pub async fn merge_session(
        &self,
        session_id: &str,
        projects: &ProjectManager,
        prs: &PrManager,
        services: &AppServices,
    ) -> Result<(), String> {
        let session = self
            .session_or_err(session_id)
            .map_err(|_| SESSION_NOT_FOUND_ERROR.to_string())?;
        if session.status != Status::Review {
            return Err("Session must be in review status".to_string());
        }

        let db = services.db().clone();
        let folder = session.folder.clone();
        let id = session.id.clone();
        let (session_agent, session_model) = TitleService::resolve_session_agent_and_model(session);
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

        let Some(repo_root) = git::find_git_repo_root(projects.working_dir()) else {
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
            pr_creation_in_flight: prs.pr_creation_in_flight(),
            pr_poll_cancel: prs.pr_poll_cancel(),
            repo_root,
            session_agent,
            session_model: session_model.as_str().to_string(),
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
            pr_creation_in_flight,
            pr_poll_cancel,
            repo_root,
            session_agent,
            session_model,
            source_branch,
            status,
        } = input;

        let merge_result: Result<String, String> = async {
            let squash_diff = {
                let repo_root = repo_root.clone();
                let source_branch = source_branch.clone();
                let base_branch = base_branch.clone();

                tokio::task::spawn_blocking(move || {
                    git::squash_merge_diff(&repo_root, &source_branch, &base_branch)
                })
                .await
                .map_err(|error| format!("Join error: {error}"))?
                .map_err(|error| format!("Failed to inspect merge diff: {error}"))?
            };

            let fallback_commit_message =
                Self::fallback_merge_commit_message(&source_branch, &base_branch);
            let commit_message = {
                let folder = folder.clone();
                let session_model = session_model.clone();
                let squash_diff = squash_diff.clone();
                let fallback_commit_message_for_task = fallback_commit_message.clone();
                let generate_message = tokio::task::spawn_blocking(move || {
                    Self::generate_merge_commit_message_from_diff(
                        &folder,
                        session_agent,
                        &session_model,
                        &squash_diff,
                    )
                    .unwrap_or(fallback_commit_message_for_task)
                });

                match tokio::time::timeout(MERGE_COMMIT_MESSAGE_TIMEOUT, generate_message).await {
                    Ok(Ok(message)) => message,
                    Ok(Err(_)) | Err(_) => fallback_commit_message,
                }
            };

            {
                let repo_root = repo_root.clone();
                let source_branch = source_branch.clone();
                let base_branch = base_branch.clone();
                let commit_message = commit_message.clone();
                tokio::task::spawn_blocking(move || {
                    git::squash_merge(&repo_root, &source_branch, &base_branch, &commit_message)
                })
                .await
                .map_err(|error| format!("Join error: {error}"))?
                .map_err(|error| format!("Failed to merge: {error}"))?;
            }

            if !TaskService::update_status(&status, &db, &app_event_tx, &id, Status::Done).await {
                return Err("Invalid status transition to Done".to_string());
            }

            Self::cancel_pr_polling_state(&pr_poll_cancel, &id);
            Self::clear_pr_creation_in_flight_state(&pr_creation_in_flight, &id);
            Self::cleanup_merged_session_worktree(
                folder.clone(),
                source_branch.clone(),
                Some(repo_root),
            )
            .await
            .map_err(|error| {
                format!("Merged successfully but failed to remove worktree: {error}")
            })?;

            Ok(format!(
                "Successfully merged {source_branch} into {base_branch}"
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

    /// Rebases a reviewed session branch onto its base branch.
    ///
    /// # Errors
    /// Returns an error if the session is invalid for rebase, required git
    /// metadata is missing, or the rebase command fails.
    pub async fn rebase_session(
        &self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<String, String> {
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

        let session_folder = session.folder.clone();
        let session_id = session.id.clone();
        let permission_mode = session.permission_mode;
        let (session_agent, session_model) = TitleService::resolve_session_agent_and_model(session);

        let rebase_input = RebaseAssistInput {
            app_event_tx: services.event_sender(),
            base_branch: base_branch.clone(),
            db: services.db().clone(),
            folder: session_folder,
            id: session_id,
            output,
            permission_mode,
            session_agent,
            session_model: session_model.as_str().to_string(),
        };
        let rebase_result = Self::run_rebase_assist_loop(rebase_input).await;
        if let Err(error) = rebase_result {
            return Err(format!("Failed to rebase: {error}"));
        }

        let source_branch = session_branch(&session.id);

        Ok(format!(
            "Successfully rebased {source_branch} onto {base_branch}"
        ))
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

        for assist_attempt in 1..=REBASE_ASSIST_MAX_ATTEMPTS {
            let conflicted_files = Self::load_conflicted_files(&input).await?;
            if conflicted_files.is_empty() {
                let continue_step = Self::run_rebase_continue(&input).await?;
                match continue_step {
                    git::RebaseStepResult::Completed => {
                        return Ok(());
                    }
                    git::RebaseStepResult::Conflict { detail } => {
                        if assist_attempt == REBASE_ASSIST_MAX_ATTEMPTS {
                            Self::abort_rebase_after_assist_failure(&input.folder).await;

                            return Err(format!(
                                "Rebase still has conflicts after assistance: {detail}"
                            ));
                        }
                    }
                }

                continue;
            }

            Self::append_rebase_assist_header(&input, assist_attempt, &conflicted_files).await;
            Self::run_rebase_assist_agent(&input, &conflicted_files).await?;

            let has_unmerged_paths = Self::stage_and_check_unmerged_paths(&input).await?;
            if has_unmerged_paths {
                if assist_attempt == REBASE_ASSIST_MAX_ATTEMPTS {
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
                    if assist_attempt == REBASE_ASSIST_MAX_ATTEMPTS {
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
        let is_rebase_in_progress =
            tokio::task::spawn_blocking(move || git::is_rebase_in_progress(&folder))
                .await
                .map_err(|error| format!("Join error: {error}"))??;

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
        let result = tokio::task::spawn_blocking(move || git::rebase_start(&folder, &base_branch))
            .await
            .map_err(|error| format!("Join error: {error}"))??;

        Ok(result)
    }

    /// Loads currently conflicted files from the worktree.
    ///
    /// # Errors
    /// Returns an error if conflicted files cannot be queried via git.
    async fn load_conflicted_files(input: &RebaseAssistInput) -> Result<Vec<String>, String> {
        let folder = input.folder.clone();
        let conflicted_files =
            tokio::task::spawn_blocking(move || git::list_conflicted_files(&folder))
                .await
                .map_err(|error| format!("Join error: {error}"))??;

        Ok(conflicted_files)
    }

    /// Appends an informational header for one rebase assistance attempt.
    async fn append_rebase_assist_header(
        input: &RebaseAssistInput,
        assist_attempt: usize,
        conflicted_files: &[String],
    ) {
        let conflict_summary = Self::format_conflicted_file_list(conflicted_files);
        let assist_header = format!(
            "\n[Rebase Assist] Attempt {assist_attempt}/{REBASE_ASSIST_MAX_ATTEMPTS}. Resolving \
             conflicts in:\n{conflict_summary}\n"
        );
        TaskService::append_session_output(
            &input.output,
            &input.db,
            &input.app_event_tx,
            &input.id,
            &assist_header,
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
        let effective_permission_mode = Self::rebase_assist_permission_mode(input.permission_mode);
        let backend = input.session_agent.create_backend();
        let command = backend.build_resume_command(
            &input.folder,
            &prompt,
            &input.session_model,
            effective_permission_mode,
        );
        TaskService::run_agent_assist_task(RunAgentAssistTaskInput {
            agent: input.session_agent,
            app_event_tx: input.app_event_tx.clone(),
            cmd: command,
            db: input.db.clone(),
            id: input.id.clone(),
            output: Arc::clone(&input.output),
            permission_mode: effective_permission_mode,
        })
        .await
    }

    /// Stages worktree edits and checks whether unresolved paths remain.
    ///
    /// # Errors
    /// Returns an error when staging or unresolved-path checks fail.
    async fn stage_and_check_unmerged_paths(input: &RebaseAssistInput) -> Result<bool, String> {
        let folder = input.folder.clone();
        tokio::task::spawn_blocking(move || git::stage_all(&folder))
            .await
            .map_err(|error| format!("Join error: {error}"))??;

        let folder = input.folder.clone();
        let has_unmerged_paths =
            tokio::task::spawn_blocking(move || git::has_unmerged_paths(&folder))
                .await
                .map_err(|error| format!("Join error: {error}"))??;

        Ok(has_unmerged_paths)
    }

    /// Continues an in-progress rebase after conflict edits are applied.
    ///
    /// # Errors
    /// Returns an error if git reports a non-conflict failure.
    async fn run_rebase_continue(
        input: &RebaseAssistInput,
    ) -> Result<git::RebaseStepResult, String> {
        let folder = input.folder.clone();
        let result = tokio::task::spawn_blocking(move || git::rebase_continue(&folder))
            .await
            .map_err(|error| format!("Join error: {error}"))??;

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
        if permission_mode == PermissionMode::Plan {
            return PermissionMode::AutoEdit;
        }

        permission_mode
    }

    /// Formats conflicted file paths as a bullet list for prompt rendering.
    fn format_conflicted_file_list(conflicted_files: &[String]) -> String {
        conflicted_files
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Aborts rebase after assistance fails to keep worktree state clean.
    async fn abort_rebase_after_assist_failure(session_folder: &Path) {
        let folder = session_folder.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || git::abort_rebase(&folder)).await;
    }

    fn cancel_pr_polling_state(
        pr_poll_cancel: &Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
        id: &str,
    ) {
        if let Ok(mut polling) = pr_poll_cancel.lock()
            && let Some(cancel) = polling.remove(id)
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn clear_pr_creation_in_flight_state(
        pr_creation_in_flight: &Arc<Mutex<HashSet<String>>>,
        id: &str,
    ) {
        if let Ok(mut in_flight) = pr_creation_in_flight.lock() {
            in_flight.remove(id);
        }
    }

    fn generate_merge_commit_message_from_diff(
        folder: &Path,
        session_agent: AgentKind,
        session_model: &str,
        diff: &str,
    ) -> Option<String> {
        let prompt = Self::merge_commit_message_prompt(diff);
        let model_response = Self::generate_merge_commit_message_with_model(
            folder,
            session_agent,
            session_model,
            &prompt,
        )
        .ok()?;
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
        agent: AgentKind,
        model: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let backend = agent.create_backend();
        let mut command =
            backend.build_start_command(folder, prompt, model, PermissionMode::AutoEdit);
        command.stdin(Stdio::null());
        let output = command
            .output()
            .map_err(|error| format!("Failed to run merge commit message model: {error}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed = agent.parse_response(&stdout, &stderr, PermissionMode::AutoEdit);
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
        format!(
            "Generate a git squash commit message using only the diff below.\nReturn strict JSON \
             with exactly two keys: `title` and `description`.\nUse repository default commit \
             format unless explicit user instructions in the diff request a different \
             format.\nRules:\n- `title` must be one line, concise, and in present simple \
             tense.\n- Do not use Conventional Commit prefixes like `feat:` or `fix:`.\n- \
             `description` is commit body text and may be an empty string when no body is \
             needed.\n- If `description` is not empty, write in present simple tense and use `-` \
             bullets when listing multiple points.\n- Include `Co-Authored-By: \
             [Agentty](https://github.com/opencloudtool/agentty)` at the end of the final \
             message.\n- Use only the diff content.\n- Do not wrap the JSON in markdown \
             fences.\n\nDiff:\n{diff}"
        )
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
        tokio::task::spawn_blocking(move || {
            let repo_root = repo_root.or_else(|| Self::resolve_repo_root_from_worktree(&folder));

            git::remove_worktree(&folder)?;

            if let Some(repo_root) = repo_root {
                git::delete_branch(&repo_root, &source_branch)?;
            }

            let _ = std::fs::remove_dir_all(&folder);

            Ok(())
        })
        .await
        .map_err(|error| format!("Join error: {error}"))?
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
    pub(crate) async fn commit_changes(
        folder: &Path,
        db: &Database,
        session_id: &str,
        commit_count: &Arc<Mutex<i64>>,
    ) -> Result<String, String> {
        let folder = folder.to_path_buf();
        let commit_hash = tokio::task::spawn_blocking(move || {
            git::commit_all(&folder, COMMIT_MESSAGE)?;

            git::head_short_hash(&folder)
        })
        .await
        .map_err(|error| format!("Join error: {error}"))?
        .map_err(|error| format!("Failed to commit: {error}"))?;

        if let Ok(mut count) = commit_count.lock() {
            *count += 1;
        }
        let _ = db.increment_session_commit_count(session_id).await;

        Ok(commit_hash)
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
}
