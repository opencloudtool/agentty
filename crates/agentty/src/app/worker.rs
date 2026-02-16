//! Per-session async worker orchestration for serialized command execution.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::agent::AgentKind;
use crate::app::task::RunSessionTaskInput;
use crate::app::{App, AppEvent};
use crate::db::Database;
use crate::model::{PermissionMode, Status};

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";

/// Command variants serialized per session worker.
pub(super) enum SessionCommand {
    Reply {
        agent: AgentKind,
        command: Command,
        operation_id: String,
        permission_mode: PermissionMode,
    },
    StartPrompt {
        agent: AgentKind,
        command: Command,
        operation_id: String,
        permission_mode: PermissionMode,
    },
}

impl SessionCommand {
    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::Reply { operation_id, .. } | Self::StartPrompt { operation_id, .. } => {
                operation_id
            }
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::Reply { .. } => "reply",
            Self::StartPrompt { .. } => "start_prompt",
        }
    }
}

struct SessionWorkerContext {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    commit_count: Arc<Mutex<i64>>,
    db: Database,
    folder: PathBuf,
    output: Arc<Mutex<String>>,
    session_id: String,
    status: Arc<Mutex<Status>>,
}

impl App {
    /// Marks unfinished operations from previous process runs as failed.
    pub(super) async fn fail_unfinished_operations_from_previous_run(db: &Database) {
        let interrupted_session_ids: HashSet<String> = db
            .load_unfinished_session_operations()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|operation| operation.session_id)
            .collect();

        for session_id in interrupted_session_ids {
            let _ = db
                .update_session_status(&session_id, &Status::Review.to_string())
                .await;
        }

        let _ = db
            .fail_unfinished_session_operations(RESTART_FAILURE_REASON)
            .await;
    }

    /// Persists and enqueues a command on the per-session worker queue.
    ///
    /// # Errors
    /// Returns an error if operation persistence fails or no worker is
    /// available.
    pub(super) async fn enqueue_session_command(
        &mut self,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<(), String> {
        let operation_id = command.operation_id().to_string();
        self.db
            .insert_session_operation(&operation_id, session_id, command.kind())
            .await?;

        let sender = self.ensure_session_worker(session_id)?;
        if sender.send(command).is_err() {
            let _ = self
                .db
                .mark_session_operation_failed(&operation_id, "Session worker is not available")
                .await;

            return Err("Session worker is not available".to_string());
        }

        Ok(())
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.session_workers.remove(session_id);
    }

    /// Returns an existing session worker sender or creates one lazily.
    ///
    /// # Errors
    /// Returns an error when the session cannot be found.
    fn ensure_session_worker(
        &mut self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedSender<SessionCommand>, String> {
        if let Some(sender) = self.session_workers.get(session_id) {
            return Ok(sender.clone());
        }

        let session = self
            .session_state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let handles = self
            .session_state
            .handles
            .get(session_id)
            .ok_or_else(|| "Session handles not found".to_string())?;
        let context = SessionWorkerContext {
            app_event_tx: self.app_event_sender(),
            commit_count: Arc::clone(&handles.commit_count),
            db: self.db.clone(),
            folder: session.folder.clone(),
            output: Arc::clone(&handles.output),
            session_id: session.id.clone(),
            status: Arc::clone(&handles.status),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        self.session_workers
            .insert(session_id.to_string(), sender.clone());
        Self::spawn_session_worker(context, receiver);

        Ok(sender)
    }

    /// Spawns the background loop that executes queued session commands.
    fn spawn_session_worker(
        context: SessionWorkerContext,
        mut receiver: mpsc::UnboundedReceiver<SessionCommand>,
    ) {
        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                let operation_id = command.operation_id().to_string();
                if Self::should_skip_worker_command(&context, &operation_id).await {
                    continue;
                }
                let _ = context
                    .db
                    .mark_session_operation_running(&operation_id)
                    .await;
                if Self::should_skip_worker_command(&context, &operation_id).await {
                    continue;
                }

                let result = match command {
                    SessionCommand::StartPrompt {
                        agent,
                        command,
                        permission_mode,
                        ..
                    } => {
                        App::run_session_task(RunSessionTaskInput {
                            agent,
                            app_event_tx: context.app_event_tx.clone(),
                            cmd: command,
                            commit_count: Arc::clone(&context.commit_count),
                            db: context.db.clone(),
                            folder: context.folder.clone(),
                            id: context.session_id.clone(),
                            output: Arc::clone(&context.output),
                            permission_mode,
                            status: Arc::clone(&context.status),
                        })
                        .await
                    }
                    SessionCommand::Reply {
                        agent,
                        command,
                        permission_mode,
                        ..
                    } => {
                        let _ = App::update_status(
                            &context.status,
                            &context.db,
                            &context.app_event_tx,
                            &context.session_id,
                            Status::InProgress,
                        )
                        .await;

                        App::run_session_task(RunSessionTaskInput {
                            agent,
                            app_event_tx: context.app_event_tx.clone(),
                            cmd: command,
                            commit_count: Arc::clone(&context.commit_count),
                            db: context.db.clone(),
                            folder: context.folder.clone(),
                            id: context.session_id.clone(),
                            output: Arc::clone(&context.output),
                            permission_mode,
                            status: Arc::clone(&context.status),
                        })
                        .await
                    }
                };

                match result {
                    Ok(()) => {
                        let _ = context.db.mark_session_operation_done(&operation_id).await;
                    }
                    Err(error) => {
                        let _ = context
                            .db
                            .mark_session_operation_failed(&operation_id, &error)
                            .await;
                    }
                }
            }
        });
    }

    /// Returns whether a queued command should be skipped before execution.
    async fn should_skip_worker_command(
        context: &SessionWorkerContext,
        operation_id: &str,
    ) -> bool {
        let operation_is_unfinished = context
            .db
            .is_session_operation_unfinished(operation_id)
            .await
            .unwrap_or(false);
        if !operation_is_unfinished {
            return true;
        }

        let is_cancel_requested = context
            .db
            .is_cancel_requested_for_session_operations(&context.session_id)
            .await
            .unwrap_or(false);
        if !is_cancel_requested {
            return false;
        }

        let _ = context
            .db
            .mark_session_operation_canceled(operation_id, CANCEL_BEFORE_EXECUTION_REASON)
            .await;

        true
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_session_command_kind_values() {
        // Arrange
        let reply_command = SessionCommand::Reply {
            agent: AgentKind::Gemini,
            command: Command::new("echo"),
            operation_id: "a".to_string(),
            permission_mode: PermissionMode::AutoEdit,
        };
        let start_prompt_command = SessionCommand::StartPrompt {
            agent: AgentKind::Gemini,
            command: Command::new("echo"),
            operation_id: "b".to_string(),
            permission_mode: PermissionMode::AutoEdit,
        };

        // Act
        let reply_kind = reply_command.kind();
        let start_prompt_kind = start_prompt_command.kind();

        // Assert
        assert_eq!(reply_kind, "reply");
        assert_eq!(start_prompt_kind, "start_prompt");
    }

    #[tokio::test]
    async fn test_fail_unfinished_operations_from_previous_run_restores_session_review_status() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        db.insert_session(
            "sess1",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");

        // Act
        App::fail_unfinished_operations_from_previous_run(&db).await;
        let sessions = db.load_sessions().await.expect("failed to load sessions");
        let operation_is_unfinished = db
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, "Review");
        assert!(!operation_is_unfinished);
    }

    #[tokio::test]
    async fn test_should_skip_worker_command_when_cancel_is_requested() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert");
        db.insert_session(
            "sess1",
            "gemini",
            "gemini-3-flash-preview",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
        db.insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");
        db.request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            commit_count: Arc::new(Mutex::new(0)),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = App::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(should_skip);
        assert!(!is_unfinished);
    }
}
