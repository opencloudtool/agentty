//! App-wide background task helpers for status polling, version checks, and
//! app-server turns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::app::assist::AssistContext;
use crate::app::{AppEvent, SessionTaskService};
use crate::domain::agent::AgentModel;
use crate::domain::session::Status;
use crate::infra::app_server::{AppServerClient, AppServerStreamEvent, AppServerTurnRequest};
use crate::infra::db::Database;
use crate::infra::git::GitClient;

/// Poll interval for account-level Codex usage limits snapshots.
#[cfg(not(test))]
const CODEX_USAGE_LIMITS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Stateless helpers for app-scoped background pollers and app-server
/// session execution.
pub(super) struct TaskService;

/// Inputs needed to execute one app-server turn.
pub(super) struct RunAppServerTaskInput {
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

    /// Executes one app-server turn for a session.
    ///
    /// On failure, this clears runtime process tracking, transitions the
    /// session back to `Review`, and then returns the original error so session
    /// workers do not leave sessions stuck in `InProgress`.
    ///
    /// # Errors
    /// Returns an error when app-server turn execution fails.
    pub(super) async fn run_app_server_task(
        app_server_client: Arc<dyn AppServerClient>,
        input: RunAppServerTaskInput,
    ) -> Result<(), String> {
        let RunAppServerTaskInput {
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
        let request = AppServerTurnRequest {
            live_session_output: Some(Arc::clone(&output)),
            folder: folder.clone(),
            model,
            prompt,
            session_id: id.clone(),
            session_output,
        };

        let (stream_tx, stream_rx) = mpsc::unbounded_channel::<AppServerStreamEvent>();
        let consumer_handle = Self::spawn_stream_consumer(
            stream_rx,
            Arc::clone(&output),
            db.clone(),
            app_event_tx.clone(),
            id.clone(),
        );

        SessionTaskService::set_session_progress(&app_event_tx, &id, Some("Thinking".to_string()));

        let turn_result = app_server_client.run_turn(request, stream_tx).await;

        let streamed_any_content = consumer_handle.await.unwrap_or(false);
        SessionTaskService::clear_session_progress(&app_event_tx, &id);

        let response = match turn_result {
            Ok(response) => response,
            Err(error) => {
                let () = app_server_client.shutdown_session(id.clone()).await;
                if let Ok(mut guard) = child_pid.lock() {
                    *guard = None;
                }

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

        if let Ok(mut guard) = child_pid.lock() {
            *guard = response.pid;
        }

        if response.context_reset {
            let context_reset_message = "\n[App-server] Reconnected with a new session context; \
                                         previous model context was reset.\n";
            SessionTaskService::append_session_output(
                &output,
                &db,
                &app_event_tx,
                &id,
                context_reset_message,
            )
            .await;
        }

        if !streamed_any_content && !response.assistant_message.trim().is_empty() {
            let message = format!("{}\n\n", response.assistant_message.trim_end());
            SessionTaskService::append_session_output(&output, &db, &app_event_tx, &id, &message)
                .await;
        }

        let stats = crate::domain::session::SessionStats {
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
        };
        let _ = db.update_session_stats(&id, &stats).await;
        let _ = db
            .upsert_session_usage(&id, session_model.as_str(), &stats)
            .await;
        {
            let folder = folder.clone();
            SessionTaskService::handle_auto_commit(AssistContext {
                app_event_tx: app_event_tx.clone(),
                db: db.clone(),
                folder,
                git_client: Arc::clone(&git_client),
                id: id.clone(),
                output: Arc::clone(&output),
                session_model,
            })
            .await;
        }

        SessionTaskService::refresh_persisted_session_size(&db, git_client.as_ref(), &id, &folder)
            .await;
        let _ = SessionTaskService::update_status(&status, &db, &app_event_tx, &id, Status::Review)
            .await;

        Ok(())
    }

    /// Spawns a background task that consumes [`AppServerStreamEvent`]s and
    /// forwards them to the session output buffer and progress indicator.
    ///
    /// Returns a join handle that resolves to `true` when at least one
    /// non-empty assistant message was streamed, or `false` otherwise.
    fn spawn_stream_consumer(
        mut stream_rx: mpsc::UnboundedReceiver<AppServerStreamEvent>,
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
                    AppServerStreamEvent::AssistantMessage { is_delta, message } => {
                        let trimmed_message = message.trim_end();
                        if trimmed_message.trim().is_empty() {
                            continue;
                        }

                        if active_progress.take().is_some() {
                            SessionTaskService::set_session_progress(
                                &app_event_tx,
                                &session_id,
                                None,
                            );
                        }

                        let formatted = if is_delta {
                            message
                        } else {
                            format!("{trimmed_message}\n\n")
                        };
                        SessionTaskService::append_session_output(
                            &output,
                            &db,
                            &app_event_tx,
                            &session_id,
                            &formatted,
                        )
                        .await;
                        streamed_any_content = true;
                    }
                    AppServerStreamEvent::ProgressUpdate(progress) => {
                        if active_progress.as_deref() != Some(&progress) {
                            active_progress = Some(progress.clone());

                            SessionTaskService::set_session_progress(
                                &app_event_tx,
                                &session_id,
                                Some(progress.clone()),
                            );
                        }
                    }
                }
            }

            if active_progress.take().is_some() {
                SessionTaskService::set_session_progress(&app_event_tx, &session_id, None);
            }

            streamed_any_content
        })
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
    use crate::infra::app_server::{AppServerTurnResponse, MockAppServerClient};
    use crate::infra::db::Database;
    use crate::infra::git::MockGitClient;

    /// Collects all currently buffered app events from a receiver.
    fn collect_app_events(app_event_rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> Vec<AppEvent> {
        let mut observed_events = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            observed_events.push(event);
        }

        observed_events
    }

    /// Returns `true` when a `SessionUpdated` event exists for `session_id`.
    fn has_session_updated_event(observed_events: &[AppEvent], session_id: &str) -> bool {
        observed_events.iter().any(|event| {
            matches!(
                event,
                AppEvent::SessionUpdated { session_id: event_session_id }
                if event_session_id == session_id
            )
        })
    }

    /// Returns `true` when a `RefreshSessions` event was emitted.
    fn has_refresh_sessions_event(observed_events: &[AppEvent]) -> bool {
        observed_events
            .iter()
            .any(|event| matches!(event, AppEvent::RefreshSessions))
    }

    /// Clones the current session output buffer from shared state.
    fn output_snapshot(output: &Arc<Mutex<String>>) -> String {
        output
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    /// Ensures app-server turn failures clear runtime process state and
    /// restore `Review` from `InProgress`.
    async fn test_run_app_server_task_error_restores_review_status() {
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
        let mut mock_app_server_client = MockAppServerClient::new();
        mock_app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|_, _| Box::pin(async { Err("turn failed".to_string()) }));
        mock_app_server_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));
        let child_pid = Arc::new(Mutex::new(Some(4242)));
        let status = Arc::new(Mutex::new(Status::InProgress));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();

        // Act
        let result = TaskService::run_app_server_task(
            Arc::new(mock_app_server_client),
            RunAppServerTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: dir.path().to_path_buf(),
                git_client: Arc::new(MockGitClient::new()),
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
    /// Ensures app-server turn failures keep settled `Review` sessions in
    /// `Review`.
    async fn test_run_app_server_task_error_keeps_review_status() {
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
        let mut mock_app_server_client = MockAppServerClient::new();
        mock_app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|_, _| Box::pin(async { Err("turn failed".to_string()) }));
        mock_app_server_client
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async {}));
        let child_pid = Arc::new(Mutex::new(Some(999)));
        let status = Arc::new(Mutex::new(Status::Review));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();

        // Act
        let result = TaskService::run_app_server_task(
            Arc::new(mock_app_server_client),
            RunAppServerTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: dir.path().to_path_buf(),
                git_client: Arc::new(MockGitClient::new()),
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
    /// Verifies the app-server success path streams output, stores the child
    /// pid, and emits session refresh events without real provider processes.
    async fn test_run_app_server_task_success_streams_output_and_updates_status() {
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
        let mut mock_app_server_client = MockAppServerClient::new();
        mock_app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|_, stream_tx| {
                let _ =
                    stream_tx.send(AppServerStreamEvent::ProgressUpdate("Thinking".to_string()));
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    is_delta: false,
                    message: "streamed assistant output".to_string(),
                });
                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: "fallback message".to_string(),
                        context_reset: false,
                        input_tokens: 9,
                        output_tokens: 7,
                        pid: Some(5150),
                    })
                })
            });
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_diff()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Err("Nothing to commit".to_string()) }));
        mock_git_client
            .expect_diff()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        let child_pid = Arc::new(Mutex::new(None));
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new(Status::InProgress));
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        // Act
        let result = TaskService::run_app_server_task(
            Arc::new(mock_app_server_client),
            RunAppServerTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db: db.clone(),
                folder: dir.path().join("worktree"),
                git_client: Arc::new(mock_git_client),
                id: session_id.to_string(),
                output: Arc::clone(&output),
                prompt: "hello".to_string(),
                session_output: Some("history".to_string()),
                session_model: AgentModel::Gpt53Codex,
                status: Arc::clone(&status),
            },
        )
        .await;
        let observed_events = collect_app_events(&mut app_event_rx);

        // Assert
        assert!(result.is_ok());
        assert_eq!(
            child_pid.lock().ok().and_then(|guard| *guard),
            Some(5150),
            "child pid should be set from app-server response"
        );
        assert_eq!(
            status.lock().map(|value| *value).ok(),
            Some(Status::Review),
            "status should return to Review after successful turn"
        );
        let output_text = output_snapshot(&output);
        assert!(output_text.contains("streamed assistant output"));
        assert!(!output_text.contains("fallback message"));
        let sessions = db.load_sessions().await.expect("failed to load sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, Status::Review.to_string());
        assert_eq!(sessions[0].input_tokens, 9);
        assert_eq!(sessions[0].output_tokens, 7);
        assert!(
            has_session_updated_event(&observed_events, session_id),
            "expected at least one SessionUpdated event"
        );
        assert!(
            has_refresh_sessions_event(&observed_events),
            "expected refresh notification after status transition"
        );
    }

    #[tokio::test]
    /// Ensures streaming assistant chunks are appended to output buffers.
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
            .send(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "Hello world".to_string(),
            })
            .expect("send should succeed");
        stream_tx
            .send(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "Second message".to_string(),
            })
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
    /// Ensures streamed delta chunks are concatenated inline without synthetic
    /// paragraph spacing between chunks.
    async fn test_stream_consumer_concatenates_delta_chunks_inline() {
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
            "delta-test",
            AgentModel::Gemini3FlashPreview.as_str(),
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
            "delta-test".to_string(),
        );
        stream_tx
            .send(AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: "The current weather in Kazan, Russia is **-9C (16".to_string(),
            })
            .expect("send should succeed");
        stream_tx
            .send(AppServerStreamEvent::AssistantMessage {
                is_delta: true,
                message: "F)** with moderate snow.".to_string(),
            })
            .expect("send should succeed");
        drop(stream_tx);
        let streamed_any = handle.await.expect("consumer task should complete");

        // Assert
        assert!(streamed_any);
        let buffer = output.lock().expect("lock output").clone();
        assert!(buffer.contains("16F)** with moderate snow."));
        assert!(!buffer.contains("16\n\nF)**"));
    }

    #[tokio::test]
    /// Verifies whitespace-only assistant chunks are ignored so fallback output
    /// can still be emitted by the completed turn response.
    async fn test_run_app_server_task_uses_fallback_after_whitespace_only_streamed_chunk() {
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
            AgentModel::Gemini3FlashPreview.as_str(),
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        let mut mock_app_server_client = MockAppServerClient::new();
        mock_app_server_client
            .expect_run_turn()
            .times(1)
            .returning(|_, stream_tx| {
                let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                    is_delta: false,
                    message: "\n".to_string(),
                });
                Box::pin(async {
                    Ok(AppServerTurnResponse {
                        assistant_message: "fallback message".to_string(),
                        context_reset: false,
                        input_tokens: 5,
                        output_tokens: 6,
                        pid: Some(5151),
                    })
                })
            });
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .times(1)
            .returning(|_, _, _| Box::pin(async { Err("Nothing to commit".to_string()) }));
        mock_git_client
            .expect_diff()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        let child_pid = Arc::new(Mutex::new(None));
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new(Status::InProgress));
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();

        // Act
        let result = TaskService::run_app_server_task(
            Arc::new(mock_app_server_client),
            RunAppServerTaskInput {
                app_event_tx,
                child_pid: Arc::clone(&child_pid),
                db,
                folder: dir.path().to_path_buf(),
                git_client: Arc::new(mock_git_client),
                id: session_id.to_string(),
                output: Arc::clone(&output),
                prompt: "hello".to_string(),
                session_output: Some("history".to_string()),
                session_model: AgentModel::Gemini3FlashPreview,
                status: Arc::clone(&status),
            },
        )
        .await;

        // Assert
        assert!(result.is_ok());
        let output_text = output_snapshot(&output);
        assert!(
            output_text.contains("fallback message"),
            "fallback output should be appended when streamed chunks are whitespace-only"
        );
        assert!(output_text.contains("[Commit] No changes to commit."));
        assert_eq!(
            status.lock().map(|value| *value).ok(),
            Some(Status::Review),
            "status should return to Review after successful turn"
        );
    }

    #[tokio::test]
    /// Verifies streaming progress lines update UI state without leaking
    /// synthetic completion messages into session output.
    async fn test_stream_consumer_updates_progress_without_completion_lines() {
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
            .send(AppServerStreamEvent::ProgressUpdate(
                "Running a command".to_string(),
            ))
            .expect("send should succeed");
        stream_tx
            .send(AppServerStreamEvent::AssistantMessage {
                is_delta: false,
                message: "Done".to_string(),
            })
            .expect("send should succeed");
        drop(stream_tx);
        handle.await.expect("consumer task should complete");

        // Assert
        let buffer = output.lock().expect("lock output").clone();
        assert!(
            !buffer.contains("Command completed"),
            "output should not contain progress completion lines: {buffer}"
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
    /// Verifies identical repeated progress updates are collapsed to one
    /// state-change event.
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
            .send(AppServerStreamEvent::ProgressUpdate("Thinking".to_string()))
            .expect("send should succeed");
        stream_tx
            .send(AppServerStreamEvent::ProgressUpdate("Thinking".to_string()))
            .expect("send should succeed");
        stream_tx
            .send(AppServerStreamEvent::ProgressUpdate("Thinking".to_string()))
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
    /// Ensures empty streams report no assistant content for fallback output
    /// behavior.
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
    /// Verifies Codex usage refresh is disabled in test builds.
    fn test_spawn_codex_usage_limits_task_is_noop_in_tests() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_codex_usage_limits_task(&app_event_tx);

        // Assert
        assert!(app_event_rx.try_recv().is_err());
    }
}
