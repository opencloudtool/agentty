//! Per-session async worker orchestration for serialized command execution.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::app::task::{RunSessionTaskInput, TaskService};
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::domain::agent::AgentModel;
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::infra::codex_app_server::CodexAppServerClient;
use crate::infra::db::Database;
use crate::infra::git::GitClient;

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";

/// Command variants serialized per session worker.
pub(super) enum SessionCommand {
    ReplyCodexAppServer {
        operation_id: String,
        permission_mode: PermissionMode,
        prompt: String,
        session_output: Option<String>,
        session_model: AgentModel,
    },
    Reply {
        command: Command,
        operation_id: String,
        permission_mode: PermissionMode,
        session_model: AgentModel,
    },
    StartPromptCodexAppServer {
        operation_id: String,
        permission_mode: PermissionMode,
        prompt: String,
        session_model: AgentModel,
    },
    StartPrompt {
        command: Command,
        operation_id: String,
        permission_mode: PermissionMode,
        session_model: AgentModel,
    },
}

impl SessionCommand {
    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::Reply { operation_id, .. }
            | Self::ReplyCodexAppServer { operation_id, .. }
            | Self::StartPrompt { operation_id, .. }
            | Self::StartPromptCodexAppServer { operation_id, .. } => operation_id,
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::Reply { .. } | Self::ReplyCodexAppServer { .. } => "reply",
            Self::StartPrompt { .. } | Self::StartPromptCodexAppServer { .. } => "start_prompt",
        }
    }
}

struct SessionWorkerContext {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    child_pid: Arc<Mutex<Option<u32>>>,
    codex_app_server_client: Arc<dyn CodexAppServerClient>,
    db: Database,
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    output: Arc<Mutex<String>>,
    session_id: String,
    status: Arc<Mutex<Status>>,
}

impl SessionManager {
    /// Marks unfinished operations from previous process runs as failed.
    pub(crate) async fn fail_unfinished_operations_from_previous_run(db: &Database) {
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
        services: &AppServices,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<(), String> {
        let operation_id = command.operation_id().to_string();
        services
            .db()
            .insert_session_operation(&operation_id, session_id, command.kind())
            .await?;

        let sender = self.ensure_session_worker(services, session_id)?;
        if sender.send(command).is_err() {
            let _ = services
                .db()
                .mark_session_operation_failed(&operation_id, "Session worker is not available")
                .await;

            return Err("Session worker is not available".to_string());
        }

        Ok(())
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.workers.remove(session_id);
    }

    /// Returns an existing session worker sender or creates one lazily.
    ///
    /// # Errors
    /// Returns an error when the session cannot be found.
    fn ensure_session_worker(
        &mut self,
        services: &AppServices,
        session_id: &str,
    ) -> Result<mpsc::UnboundedSender<SessionCommand>, String> {
        if let Some(sender) = self.workers.get(session_id) {
            return Ok(sender.clone());
        }

        let (session, handles) = self.session_and_handles_or_err(session_id)?;
        let context = SessionWorkerContext {
            app_event_tx: services.event_sender(),
            child_pid: Arc::clone(&handles.child_pid),
            codex_app_server_client: services.codex_app_server_client(),
            db: services.db().clone(),
            folder: session.folder.clone(),
            git_client: services.git_client(),
            output: Arc::clone(&handles.output),
            session_id: session.id.clone(),
            status: Arc::clone(&handles.status),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        self.workers.insert(session_id.to_string(), sender.clone());
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

                let result = Self::execute_session_command(&context, command).await;

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

            context
                .codex_app_server_client
                .shutdown_session(context.session_id.clone())
                .await;
            if let Ok(mut guard) = context.child_pid.lock() {
                *guard = None;
            }
        });
    }

    /// Executes one queued command with the transport required by its variant.
    async fn execute_session_command(
        context: &SessionWorkerContext,
        command: SessionCommand,
    ) -> Result<(), String> {
        match command {
            SessionCommand::StartPrompt {
                command,
                permission_mode,
                session_model,
                ..
            } => {
                Self::run_standard_session_command(
                    context,
                    command,
                    permission_mode,
                    session_model,
                    false,
                )
                .await
            }
            SessionCommand::StartPromptCodexAppServer {
                permission_mode,
                prompt,
                session_model,
                ..
            } => {
                Self::run_codex_app_server_session_command(
                    context,
                    permission_mode,
                    prompt,
                    None,
                    session_model,
                    false,
                )
                .await
            }
            SessionCommand::Reply {
                command,
                permission_mode,
                session_model,
                ..
            } => {
                Self::run_standard_session_command(
                    context,
                    command,
                    permission_mode,
                    session_model,
                    true,
                )
                .await
            }
            SessionCommand::ReplyCodexAppServer {
                permission_mode,
                prompt,
                session_output,
                session_model,
                ..
            } => {
                Self::run_codex_app_server_session_command(
                    context,
                    permission_mode,
                    prompt,
                    session_output,
                    session_model,
                    true,
                )
                .await
            }
        }
    }

    /// Executes one non-Codex-app-server command using the existing CLI path.
    async fn run_standard_session_command(
        context: &SessionWorkerContext,
        command: Command,
        permission_mode: PermissionMode,
        session_model: AgentModel,
        update_in_progress_status: bool,
    ) -> Result<(), String> {
        if update_in_progress_status {
            let _ = TaskService::update_status(
                &context.status,
                &context.db,
                &context.app_event_tx,
                &context.session_id,
                Status::InProgress,
            )
            .await;
        }

        TaskService::run_session_task(RunSessionTaskInput {
            app_event_tx: context.app_event_tx.clone(),
            child_pid: Arc::clone(&context.child_pid),
            cmd: command,
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            id: context.session_id.clone(),
            output: Arc::clone(&context.output),
            permission_mode,
            session_model,
            status: Arc::clone(&context.status),
        })
        .await
    }

    /// Executes one Codex app-server turn for a queued session command.
    async fn run_codex_app_server_session_command(
        context: &SessionWorkerContext,
        permission_mode: PermissionMode,
        prompt: String,
        session_output: Option<String>,
        session_model: AgentModel,
        update_in_progress_status: bool,
    ) -> Result<(), String> {
        if update_in_progress_status {
            let _ = TaskService::update_status(
                &context.status,
                &context.db,
                &context.app_event_tx,
                &context.session_id,
                Status::InProgress,
            )
            .await;
        }

        // Start commands keep `Status::InProgress` set by the caller, while
        // reply commands re-enter `InProgress` here.
        let is_initial_plan_prompt = !update_in_progress_status;
        TaskService::run_codex_app_server_task(
            context.codex_app_server_client.clone(),
            crate::app::task::RunCodexAppServerTaskInput {
                app_event_tx: context.app_event_tx.clone(),
                child_pid: Arc::clone(&context.child_pid),
                db: context.db.clone(),
                folder: context.folder.clone(),
                git_client: Arc::clone(&context.git_client),
                id: context.session_id.clone(),
                is_initial_plan_prompt,
                output: Arc::clone(&context.output),
                permission_mode,
                prompt,
                session_output,
                session_model,
                status: Arc::clone(&context.status),
            },
        )
        .await
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
        let reply_app_server_command = SessionCommand::ReplyCodexAppServer {
            operation_id: "a-app-server".to_string(),
            permission_mode: PermissionMode::AutoEdit,
            prompt: "prompt".to_string(),
            session_output: None,
            session_model: AgentModel::Gpt53Codex,
        };
        let reply_command = SessionCommand::Reply {
            command: Command::new("echo"),
            operation_id: "a".to_string(),
            permission_mode: PermissionMode::AutoEdit,
            session_model: AgentModel::Gemini3FlashPreview,
        };
        let start_prompt_app_server_command = SessionCommand::StartPromptCodexAppServer {
            operation_id: "b-app-server".to_string(),
            permission_mode: PermissionMode::AutoEdit,
            prompt: "prompt".to_string(),
            session_model: AgentModel::Gpt53Codex,
        };
        let start_prompt_command = SessionCommand::StartPrompt {
            command: Command::new("echo"),
            operation_id: "b".to_string(),
            permission_mode: PermissionMode::AutoEdit,
            session_model: AgentModel::Gemini3FlashPreview,
        };

        // Act
        let reply_app_server_kind = reply_app_server_command.kind();
        let reply_kind = reply_command.kind();
        let start_prompt_app_server_kind = start_prompt_app_server_command.kind();
        let start_prompt_kind = start_prompt_command.kind();

        // Assert
        assert_eq!(reply_app_server_kind, "reply");
        assert_eq!(reply_kind, "reply");
        assert_eq!(start_prompt_app_server_kind, "start_prompt");
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
        SessionManager::fail_unfinished_operations_from_previous_run(&db).await;
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
            child_pid: Arc::new(Mutex::new(None)),
            codex_app_server_client: Arc::new(
                crate::infra::codex_app_server::RealCodexAppServerClient::new(),
            ),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            git_client: Arc::new(crate::infra::git::RealGitClient),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionManager::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(should_skip);
        assert!(!is_unfinished);
    }
}
