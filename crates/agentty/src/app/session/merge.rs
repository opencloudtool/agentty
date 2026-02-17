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
use crate::app::task::TaskService;
use crate::app::title::TitleService;
use crate::app::{AppEvent, AppServices, PrManager, ProjectManager, SessionManager};
use crate::db::Database;
use crate::git;
use crate::model::{PermissionMode, Status};

const MERGE_COMMIT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(8);

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

        let session_folder = session.folder.clone();
        {
            let base_branch = base_branch.clone();
            tokio::task::spawn_blocking(move || git::rebase(&session_folder, &base_branch))
                .await
                .map_err(|error| format!("Join error: {error}"))?
                .map_err(|error| format!("Failed to rebase: {error}"))?;
        }

        let source_branch = session_branch(&session.id);

        Ok(format!(
            "Successfully rebased {source_branch} onto {base_branch}"
        ))
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
