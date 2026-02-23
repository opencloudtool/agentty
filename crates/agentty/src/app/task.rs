//! App-wide background task helpers for status polling, version checks, and
//! Codex app-server turns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::app::assist::{
    AssistContext, AssistPolicy, FailureTracker, append_assist_header, format_detail_lines,
    run_agent_assist,
};
use crate::app::session::COMMIT_MESSAGE;
use crate::domain::agent::AgentModel;
use crate::domain::session::Status;
use crate::infra::codex_app_server::{CodexAppServerClient, CodexStreamEvent, CodexTurnRequest};
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
    pub(super) output: Arc<Mutex<String>>,
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
    /// On failure, this clears runtime process tracking, transitions the
    /// session back to `Review`, and then returns the original error so session
    /// workers do not leave sessions stuck in `InProgress`.
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
            output,
            prompt,
            session_output,
            session_model,
            status,
        } = input;
        let model = session_model.as_str().to_string();
        let request = CodexTurnRequest {
            folder: folder.clone(),
            model,
            prompt,
            session_output,
            session_id: id.clone(),
        };

        let (stream_tx, stream_rx) = mpsc::unbounded_channel::<CodexStreamEvent>();
        let consumer_handle = Self::spawn_stream_consumer(
            stream_rx,
            Arc::clone(&output),
            db.clone(),
            app_event_tx.clone(),
            id.clone(),
        );

        Self::set_session_progress(&app_event_tx, &id, Some("Thinking".to_string()));

        let turn_result = codex_app_server_client.run_turn(request, stream_tx).await;

        let streamed_any_content = consumer_handle.await.unwrap_or(false);
        Self::clear_session_progress(&app_event_tx, &id);

        let response = match turn_result {
            Ok(response) => response,
            Err(error) => {
                let () = codex_app_server_client.shutdown_session(id.clone()).await;
                if let Ok(mut guard) = child_pid.lock() {
                    *guard = None;
                }

                let _ = Self::update_status(&status, &db, &app_event_tx, &id, Status::Review).await;

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

        if !streamed_any_content && !response.assistant_message.trim().is_empty() {
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
        let assist_context = AssistContext {
            app_event_tx: context.app_event_tx.clone(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            id: context.id.clone(),
            output: Arc::clone(&context.output),
            session_model: context.session_model,
        };

        run_agent_assist(&assist_context, &prompt)
            .await
            .map_err(|error| format!("Commit assistance failed: {error}"))
    }

    fn auto_commit_assist_prompt(commit_error: &str) -> String {
        AUTO_COMMIT_ASSIST_PROMPT_TEMPLATE.replace("{commit_error}", commit_error.trim())
    }

    fn format_commit_error_for_display(commit_error: &str) -> String {
        format_detail_lines(commit_error)
    }

    /// Spawns a background task that consumes [`CodexStreamEvent`]s and
    /// forwards them to the session output buffer and progress indicator.
    ///
    /// Returns a join handle that resolves to `true` when at least one
    /// assistant message was streamed, or `false` otherwise.
    fn spawn_stream_consumer(
        mut stream_rx: mpsc::UnboundedReceiver<CodexStreamEvent>,
        output: Arc<Mutex<String>>,
        db: Database,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        session_id: String,
    ) -> tokio::task::JoinHandle<bool> {
        tokio::spawn(async move {
            let mut streamed_any_content = false;
            let mut active_progress: Option<String> = None;

            while let Some(event) = stream_rx.recv().await {
                match event {
                    CodexStreamEvent::AssistantMessage(message) => {
                        if let Some(previous_progress) = active_progress.take() {
                            Self::append_progress_completion(
                                &output,
                                &db,
                                &app_event_tx,
                                &session_id,
                                &previous_progress,
                            )
                            .await;
                            Self::set_session_progress(&app_event_tx, &session_id, None);
                        }

                        let formatted = format!("{}\n\n", message.trim_end());
                        Self::append_session_output(
                            &output,
                            &db,
                            &app_event_tx,
                            &session_id,
                            &formatted,
                        )
                        .await;
                        streamed_any_content = true;
                    }
                    CodexStreamEvent::ProgressUpdate(progress) => {
                        if active_progress.as_deref() != Some(&progress) {
                            if let Some(previous_progress) = active_progress.take() {
                                Self::append_progress_completion(
                                    &output,
                                    &db,
                                    &app_event_tx,
                                    &session_id,
                                    &previous_progress,
                                )
                                .await;
                            }

                            Self::set_session_progress(
                                &app_event_tx,
                                &session_id,
                                Some(progress.clone()),
                            );
                            active_progress = Some(progress);
                        }
                    }
                }
            }

            if let Some(previous_progress) = active_progress.take() {
                Self::append_progress_completion(
                    &output,
                    &db,
                    &app_event_tx,
                    &session_id,
                    &previous_progress,
                )
                .await;
            }

            streamed_any_content
        })
    }

    /// Appends a progress completion line to session output when a progress
    /// phase ends.
    async fn append_progress_completion(
        output: &Arc<Mutex<String>>,
        db: &Database,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        progress_message: &str,
    ) {
        if let Some(completion_text) = Self::progress_completion_message(progress_message) {
            let formatted = format!("{completion_text}\n");
            Self::append_session_output(output, db, app_event_tx, session_id, &formatted).await;
        }
    }

    /// Maps an active progress message to its completion text.
    fn progress_completion_message(progress_message: &str) -> Option<String> {
        let normalized = progress_message.trim().trim_end_matches('.').trim();
        if normalized.is_empty() {
            return None;
        }

        let completion_message = match normalized {
            "Searching the web" => "Web search completed".to_string(),
            "Thinking" => "Thinking completed".to_string(),
            "Running a command" => "Command completed".to_string(),
            other => format!("{other} completed"),
        };

        Some(completion_message)
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
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::Status;
    use crate::infra::codex_app_server::MockCodexAppServerClient;
    use crate::infra::db::Database;
    use crate::infra::git;

    #[tokio::test]
    async fn test_run_codex_app_server_task_error_restores_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        let session_id = "session-id";
        db.insert_session(
            session_id,
            AgentModel::Gpt53Codex.as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let mut mock_codex_client = MockCodexAppServerClient::new();
        mock_codex_client
            .expect_run_turn()
            .times(1)
            .returning(|_, _| Box::pin(async { Err("turn failed".to_string()) }));
        mock_codex_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));
        let child_pid = Arc::new(Mutex::new(Some(4242)));
        let status = Arc::new(Mutex::new(Status::InProgress));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();

        // Act
        let result = TaskService::run_codex_app_server_task(
            Arc::new(mock_codex_client),
            RunCodexAppServerTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: dir.path().to_path_buf(),
                git_client: Arc::new(git::RealGitClient),
                id: session_id.to_string(),
                output: Arc::new(Mutex::new(String::new())),
                prompt: "hello".to_string(),
                session_output: Some("history".to_string()),
                session_model: AgentModel::Gpt53Codex,
                status: Arc::clone(&status),
            },
        )
        .await;

        // Assert
        assert_eq!(result, Err("turn failed".to_string()));
        assert_eq!(
            child_pid.lock().ok().and_then(|guard| *guard),
            None,
            "child pid should be cleared on error"
        );
        assert_eq!(
            status.lock().map(|value| *value).ok(),
            Some(Status::Review),
            "status should leave InProgress on app-server errors"
        );
        let sessions = db.load_sessions().await.expect("failed to load sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, Status::Review.to_string());
    }

    #[tokio::test]
    async fn test_run_codex_app_server_task_error_keeps_review_status() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        let session_id = "session-id";
        db.insert_session(
            session_id,
            AgentModel::Gpt53Codex.as_str(),
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let mut mock_codex_client = MockCodexAppServerClient::new();
        mock_codex_client
            .expect_run_turn()
            .times(1)
            .returning(|_, _| Box::pin(async { Err("turn failed".to_string()) }));
        mock_codex_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));
        let child_pid = Arc::new(Mutex::new(Some(999)));
        let status = Arc::new(Mutex::new(Status::Review));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();

        // Act
        let result = TaskService::run_codex_app_server_task(
            Arc::new(mock_codex_client),
            RunCodexAppServerTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: dir.path().to_path_buf(),
                git_client: Arc::new(git::RealGitClient),
                id: session_id.to_string(),
                output: Arc::new(Mutex::new(String::new())),
                prompt: "hello".to_string(),
                session_output: None,
                session_model: AgentModel::Gpt53Codex,
                status: Arc::clone(&status),
            },
        )
        .await;

        // Assert
        assert_eq!(result, Err("turn failed".to_string()));
        assert_eq!(
            child_pid.lock().ok().and_then(|guard| *guard),
            None,
            "child pid should be cleared on error"
        );
        assert_eq!(
            status.lock().map(|value| *value).ok(),
            Some(Status::Review),
            "status should remain Review when already settled"
        );
        let sessions = db.load_sessions().await.expect("failed to load sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, Status::Review.to_string());
    }

    #[tokio::test]
    async fn test_stream_consumer_forwards_assistant_messages_to_output() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "stream-test",
            AgentModel::Gpt53Codex.as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();

        // Act
        let handle = TaskService::spawn_stream_consumer(
            stream_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "stream-test".to_string(),
        );
        stream_tx
            .send(CodexStreamEvent::AssistantMessage(
                "Hello world".to_string(),
            ))
            .expect("send should succeed");
        stream_tx
            .send(CodexStreamEvent::AssistantMessage(
                "Second message".to_string(),
            ))
            .expect("send should succeed");
        drop(stream_tx);
        let streamed_any = handle.await.expect("consumer task should complete");

        // Assert
        assert!(streamed_any);
        let buffer = output.lock().expect("lock output").clone();
        assert!(
            buffer.contains("Hello world"),
            "output should contain first message"
        );
        assert!(
            buffer.contains("Second message"),
            "output should contain second message"
        );
    }

    #[tokio::test]
    async fn test_stream_consumer_updates_progress_and_appends_completion() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "progress-test",
            AgentModel::Gpt53Codex.as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();

        // Act
        let handle = TaskService::spawn_stream_consumer(
            stream_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "progress-test".to_string(),
        );
        stream_tx
            .send(CodexStreamEvent::ProgressUpdate(
                "Running a command".to_string(),
            ))
            .expect("send should succeed");
        stream_tx
            .send(CodexStreamEvent::AssistantMessage("Done".to_string()))
            .expect("send should succeed");
        drop(stream_tx);
        handle.await.expect("consumer task should complete");

        // Assert
        let buffer = output.lock().expect("lock output").clone();
        assert!(
            buffer.contains("Command completed"),
            "output should contain progress completion: {buffer}"
        );
        assert!(
            buffer.contains("Done"),
            "output should contain assistant message: {buffer}"
        );
        let mut progress_events = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::SessionProgressUpdated {
                progress_message, ..
            } = event
            {
                progress_events.push(progress_message);
            }
        }
        assert!(
            progress_events.contains(&Some("Running a command".to_string())),
            "should emit progress update event"
        );
        assert!(
            progress_events.contains(&None),
            "should clear progress when assistant message arrives"
        );
    }

    #[tokio::test]
    async fn test_stream_consumer_deduplicates_repeated_progress() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "dedup-test",
            AgentModel::Gpt53Codex.as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();

        // Act
        let handle = TaskService::spawn_stream_consumer(
            stream_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "dedup-test".to_string(),
        );
        stream_tx
            .send(CodexStreamEvent::ProgressUpdate("Thinking".to_string()))
            .expect("send should succeed");
        stream_tx
            .send(CodexStreamEvent::ProgressUpdate("Thinking".to_string()))
            .expect("send should succeed");
        stream_tx
            .send(CodexStreamEvent::ProgressUpdate("Thinking".to_string()))
            .expect("send should succeed");
        drop(stream_tx);
        handle.await.expect("consumer task should complete");

        // Assert
        let mut progress_set_count = 0;
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::SessionProgressUpdated {
                progress_message: Some(_),
                ..
            } = event
            {
                progress_set_count += 1;
            }
        }
        assert_eq!(
            progress_set_count, 1,
            "repeated identical progress should emit only one set event"
        );
    }

    #[tokio::test]
    async fn test_stream_consumer_returns_false_when_no_content_streamed() {
        // Arrange
        let output = Arc::new(Mutex::new(String::new()));
        let db = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = db
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "empty-test",
            AgentModel::Gpt53Codex.as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();

        // Act
        let handle = TaskService::spawn_stream_consumer(
            stream_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "empty-test".to_string(),
        );
        drop(stream_tx);
        let streamed_any = handle.await.expect("consumer task should complete");

        // Assert
        assert!(!streamed_any);
    }

    #[test]
    fn test_progress_completion_message_maps_known_messages() {
        // Act / Assert
        assert_eq!(
            TaskService::progress_completion_message("Searching the web"),
            Some("Web search completed".to_string())
        );
        assert_eq!(
            TaskService::progress_completion_message("Thinking"),
            Some("Thinking completed".to_string())
        );
        assert_eq!(
            TaskService::progress_completion_message("Running a command"),
            Some("Command completed".to_string())
        );
        assert_eq!(
            TaskService::progress_completion_message("Working: custom"),
            Some("Working: custom completed".to_string())
        );
    }

    #[test]
    fn test_progress_completion_message_returns_none_for_empty() {
        // Act / Assert
        assert_eq!(TaskService::progress_completion_message(""), None);
        assert_eq!(TaskService::progress_completion_message("  "), None);
    }

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
