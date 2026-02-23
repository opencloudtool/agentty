//! App-wide background task helpers for status polling, version checks, and
//! Codex app-server turns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, effective_permission_mode,
    format_detail_lines, run_agent_assist,
};
use crate::app::session::COMMIT_MESSAGE;
use crate::domain::agent::AgentModel;
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::infra::codex_app_server::{CodexAppServerClient, CodexTurnRequest};
use crate::infra::db::Database;
use crate::infra::git::GitClient;

const AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE: &str =
    include_str!("../../resources/auto_commit_assist_prompt.md");
const AUTO_COMMIT_ASSIST_POLICY: AssistPolicy = AssistPolicy {
    max_attempts: 10,
    max_identical_failure_streak: 3,
};
/// Poll interval for account-level Codex usage limits snapshots.
#[cfg(not(test))]
const CODEX_USAGE_LIMITS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Stateless helpers for app-scoped background pollers and Codex app-server
/// session execution.
pub(super) struct TaskService;

/// Inputs needed to execute one Codex app-server turn.
pub(super) struct RunCodexAppServerTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) db: Database,
    pub(super) folder: PathBuf,
    pub(super) git_client: Arc<dyn GitClient>,
    pub(super) id: String,
    pub(super) is_initial_plan_prompt: bool,
    pub(super) output: Arc<Mutex<String>>,
    pub(super) permission_mode: PermissionMode,
    pub(super) prompt: String,
    pub(super) session_output: Option<String>,
    pub(super) session_model: AgentModel,
    pub(super) status: Arc<Mutex<Status>>,
}

impl TaskService {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    ///
    /// The task emits [`AppEvent::GitStatusUpdated`] snapshots instead of
    /// mutating app state directly.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        cancel: Arc<AtomicBool>,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
    ) {
        let dir = working_dir.to_path_buf();
        tokio::spawn(async move {
            let repo_root = git_client
                .find_git_repo_root(dir.clone())
                .await
                .unwrap_or(dir);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }

                {
                    let root = repo_root.clone();
                    let _ = git_client.fetch_remote(root).await;
                }

                let status = {
                    let root = repo_root.clone();
                    git_client.get_ahead_behind(root).await.ok()
                };
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = app_event_tx.send(AppEvent::GitStatusUpdated { status });
                for _ in 0..30 {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }

    /// Spawns a background loop that periodically refreshes Codex usage
    /// limits.
    ///
    /// The task emits [`AppEvent::CodexUsageLimitsUpdated`] snapshots instead
    /// of mutating app state directly.
    ///
    /// In tests, this function is a no-op so test runs stay deterministic and
    /// offline.
    pub(super) fn spawn_codex_usage_limits_task(app_event_tx: &mpsc::UnboundedSender<AppEvent>) {
        #[cfg(test)]
        {
            let _ = app_event_tx;
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let mut refresh_tick = tokio::time::interval(CODEX_USAGE_LIMITS_REFRESH_INTERVAL);
            refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            refresh_tick.tick().await;

            loop {
                let codex_usage_limits =
                    crate::app::SessionManager::load_codex_usage_limits().await;
                let _ = app_event_tx.send(AppEvent::CodexUsageLimitsUpdated { codex_usage_limits });

                refresh_tick.tick().await;
            }
        });
    }

    /// Spawns a one-shot background check for newer `agentty` versions on
    /// npmjs.
    ///
    /// The task emits [`AppEvent::VersionAvailabilityUpdated`] with
    /// `Some("vX.Y.Z")` only when a newer version is detected.
    ///
    /// In tests, it emits an immediate `None` update instead of spawning the
    /// network check so test runs stay deterministic and offline.
    pub(super) fn spawn_version_check_task(app_event_tx: &mpsc::UnboundedSender<AppEvent>) {
        #[cfg(test)]
        {
            let _ = app_event_tx.send(AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            });
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let latest_available_version =
                crate::version::latest_npm_version_tag()
                    .await
                    .filter(|latest_version| {
                        crate::version::is_newer_than_current_version(
                            env!("CARGO_PKG_VERSION"),
                            latest_version,
                        )
                    });

            let _ = app_event_tx.send(AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            });
        });
    }

    /// Executes one Codex app-server turn for a session.
    ///
    /// # Errors
    /// Returns an error when app-server turn execution fails.
    pub(super) async fn run_codex_app_server_task(
        codex_app_server_client: Arc<dyn CodexAppServerClient>,
        input: RunCodexAppServerTaskInput,
    ) -> Result<(), String> {
        let RunCodexAppServerTaskInput {
            app_event_tx,
            child_pid,
            db,
            folder,
            git_client,
            id,
            is_initial_plan_prompt,
            output,
            permission_mode,
            prompt,
            session_output,
            session_model,
            status,
        } = input;
        let prompt = permission_mode.apply_to_prompt(&prompt, is_initial_plan_prompt);
        let model = session_model.as_str().to_string();
        let request = CodexTurnRequest {
            folder: folder.clone(),
            model,
            permission_mode,
            prompt: prompt.into_owned(),
            session_output,
            session_id: id.clone(),
        };

        Self::set_session_progress(&app_event_tx, &id, Some("Thinking".to_string()));

        let turn_result = codex_app_server_client.run_turn(request).await;
        Self::clear_session_progress(&app_event_tx, &id);

        let response = match turn_result {
            Ok(response) => response,
            Err(error) => {
                let () = codex_app_server_client.shutdown_session(id.clone()).await;
                if let Ok(mut guard) = child_pid.lock() {
                    *guard = None;
                }

                return Err(error);
            }
        };

        if let Ok(mut guard) = child_pid.lock() {
            *guard = response.pid;
        }

        if response.context_reset {
            let context_reset_message = "\n[Codex app-server] Reconnected with a new thread; \
                                         previous model context was reset.\n";
            Self::append_session_output(&output, &db, &app_event_tx, &id, context_reset_message)
                .await;
        }

        if !response.assistant_message.trim().is_empty() {
            let message = format!("{}\n\n", response.assistant_message.trim_end());
            Self::append_session_output(&output, &db, &app_event_tx, &id, &message).await;
        }

        let stats = crate::domain::session::SessionStats {
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
        };
        let _ = db.update_session_stats(&id, &stats).await;
        let _ = db
            .upsert_session_usage(&id, session_model.as_str(), &stats)
            .await;
        Self::handle_auto_commit(AssistContext {
            app_event_tx: app_event_tx.clone(),
            db: db.clone(),
            folder,
            git_client: Arc::clone(&git_client),
            id: id.clone(),
            output: Arc::clone(&output),
            permission_mode,
            session_model,
        })
        .await;

        let _ = Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;

        Ok(())
    }

    /// Applies a status transition to memory and database when valid.
    ///
    /// This emits [`AppEvent::SessionUpdated`] for targeted snapshot sync and
    /// emits [`AppEvent::RefreshSessions`] for transitions that require full
    /// list reload.
    pub(super) async fn update_status(
        status: &Mutex<Status>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        new: Status,
    ) -> bool {
        let should_update = if let Ok(mut current) = status.lock() {
            if (*current).can_transition_to(new) {
                *current = new;
                true
            } else {
                false
            }
        } else {
            false
        };
        if !should_update {
            return false;
        }
        let _ = db.update_session_status(id, &new.to_string()).await;
        let session_id = id.to_string();
        let _ = app_event_tx.send(AppEvent::SessionUpdated { session_id });
        if Self::status_requires_full_refresh(new) {
            let _ = app_event_tx.send(AppEvent::RefreshSessions);
        }

        true
    }

    /// Appends output to the in-memory handle buffer and database.
    pub(super) async fn append_session_output(
        output: &Arc<Mutex<String>>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        message: &str,
    ) {
        if let Ok(mut buffer) = output.lock() {
            buffer.push_str(message);
        }
        let _ = db.append_session_output(id, message).await;
        let _ = app_event_tx.send(AppEvent::SessionUpdated {
            session_id: id.to_string(),
        });
    }

    async fn handle_auto_commit(context: AssistContext) {
        match Self::commit_changes_with_assist(&context).await {
            Ok(Some(hash)) => {
                let message = format!("\n[Commit] committed with hash `{hash}`\n");
                Self::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.id,
                    &message,
                )
                .await;
            }
            Ok(None) => {}
            Err(commit_error) => {
                let message = format!("\n[Commit Error] {commit_error}\n");
                Self::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.id,
                    &message,
                )
                .await;
            }
        }
    }

    async fn commit_changes_with_assist(context: &AssistContext) -> Result<Option<String>, String> {
        let mut failure_tracker =
            FailureTracker::new(AUTO_COMMIT_ASSIST_POLICY.max_identical_failure_streak);
        // Test repos do not install hooks deterministically; skip hook
        // execution in tests to keep auto-commit behavior stable.
        let skip_verify_hooks = cfg!(test);

        for assist_attempt in 1..=AUTO_COMMIT_ASSIST_POLICY.max_attempts + 1 {
            match Self::commit_changes_with_git_client(context, skip_verify_hooks).await {
                Ok(commit_hash) => {
                    return Ok(Some(commit_hash));
                }
                Err(commit_error) if commit_error.contains("Nothing to commit") => {
                    return Ok(None);
                }
                Err(commit_error) => {
                    // Keep test execution deterministic and offline by skipping
                    // model-assisted commit retries.
                    if cfg!(test) {
                        return Err(commit_error);
                    }

                    if failure_tracker.observe(&commit_error) {
                        return Err(format!(
                            "Auto-commit assistance made no progress: repeated identical commit \
                             failure. Last error: {commit_error}"
                        ));
                    }

                    if assist_attempt > AUTO_COMMIT_ASSIST_POLICY.max_attempts {
                        return Err(commit_error);
                    }

                    Self::append_commit_assist_header(context, assist_attempt, &commit_error).await;
                    Self::run_commit_assist_for_error(context, &commit_error).await?;
                }
            }
        }

        Err("Failed to auto-commit after assistance attempts".to_string())
    }

    /// Commits all worktree changes and returns the current `HEAD` short hash.
    ///
    /// Pass `no_verify` to skip commit hooks (used in tests for deterministic
    /// execution without pre-commit setup).
    ///
    /// # Errors
    /// Returns an error if staging/commit fails or `HEAD` cannot be resolved.
    async fn commit_changes_with_git_client(
        context: &AssistContext,
        no_verify: bool,
    ) -> Result<String, String> {
        let folder = context.folder.clone();
        context
            .git_client
            .commit_all_preserving_single_commit(
                folder.clone(),
                COMMIT_MESSAGE.to_string(),
                no_verify,
            )
            .await?;

        context.git_client.head_short_hash(folder).await
    }

    async fn append_commit_assist_header(
        context: &AssistContext,
        assist_attempt: usize,
        commit_error: &str,
    ) {
        let formatted_error = Self::format_commit_error_for_display(commit_error);
        append_assist_header(
            context,
            "Commit",
            assist_attempt,
            AUTO_COMMIT_ASSIST_POLICY.max_attempts,
            "Resolving auto-commit failure:",
            &formatted_error,
        )
        .await;
    }

    async fn run_commit_assist_for_error(
        context: &AssistContext,
        commit_error: &str,
    ) -> Result<(), String> {
        let prompt = Self::auto_commit_assist_prompt(commit_error);
        let effective_permission_mode =
            Self::auto_commit_assist_permission_mode(context.permission_mode);
        let assist_context = AssistContext {
            app_event_tx: context.app_event_tx.clone(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            id: context.id.clone(),
            output: Arc::clone(&context.output),
            permission_mode: effective_permission_mode,
            session_model: context.session_model,
        };

        run_agent_assist(&assist_context, &prompt)
            .await
            .map_err(|error| format!("Commit assistance failed: {error}"))
    }

    fn auto_commit_assist_prompt(commit_error: &str) -> String {
        AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE.replace("{commit_error}", commit_error.trim())
    }

    fn auto_commit_assist_permission_mode(permission_mode: PermissionMode) -> PermissionMode {
        effective_permission_mode(permission_mode)
    }

    fn format_commit_error_for_display(commit_error: &str) -> String {
        format_detail_lines(commit_error)
    }

    fn clear_session_progress(app_event_tx: &mpsc::UnboundedSender<AppEvent>, id: &str) {
        Self::set_session_progress(app_event_tx, id, None);
    }

    fn set_session_progress(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        id: &str,
        progress_message: Option<String>,
    ) {
        let _ = app_event_tx.send(AppEvent::SessionProgressUpdated {
            progress_message,
            session_id: id.to_string(),
        });
    }

    fn status_requires_full_refresh(status: Status) -> bool {
        matches!(
            status,
            Status::InProgress | Status::Review | Status::Merging | Status::Done | Status::Canceled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_codex_usage_limits_task_is_noop_in_tests() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_codex_usage_limits_task(&app_event_tx);

        // Assert
        assert!(app_event_rx.try_recv().is_err());
    }
}
