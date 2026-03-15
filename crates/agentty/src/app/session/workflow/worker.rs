//! Per-session async worker orchestration for serialized command execution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use serde_json;
use tokio::sync::mpsc;

use super::SessionTaskService;
use crate::app::assist::AssistContext;
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{SessionStats, Status};
use crate::domain::setting::SettingName;
use crate::infra::agent;
use crate::infra::channel::{
    AgentChannel, AgentError, TurnEvent, TurnMode, TurnPrompt, TurnRequest, TurnResult,
    create_agent_channel,
};
use crate::infra::db::Database;
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";

/// Single command variant serialized per session worker.
///
/// Replaces the previous four-variant enum (`Reply`, `ReplyAppServer`,
/// `StartPrompt`, `StartPromptAppServer`) with a single provider-agnostic
/// variant. The underlying channel adapter handles transport-specific details.
pub(super) enum SessionCommand {
    /// Executes one agent turn with the given mode and prompt.
    Run {
        /// Persisted operation identifier.
        operation_id: String,
        /// Whether this is a first-message start or a follow-up resume.
        mode: TurnMode,
        /// Structured user prompt payload.
        prompt: TurnPrompt,
        /// Session model used for stats and post-turn operations.
        session_model: AgentModel,
    },
}

impl SessionCommand {
    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::Run { operation_id, .. } => operation_id,
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::Run {
                mode: TurnMode::Start,
                ..
            } => "start_prompt",
            Self::Run {
                mode: TurnMode::Resume { .. },
                ..
            } => "reply",
        }
    }
}

/// Shared state threaded through all worker turn executions.
struct SessionWorkerContext {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Provider-agnostic agent channel for this session's worker.
    channel: Arc<dyn AgentChannel>,
    child_pid: Arc<Mutex<Option<u32>>>,
    db: Database,
    folder: PathBuf,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    output: Arc<Mutex<String>>,
    session_id: String,
    status: Arc<Mutex<Status>>,
}

/// Runtime snapshot required to create or reuse one session worker.
pub(super) struct SessionWorkerRuntime {
    child_pid: Arc<Mutex<Option<u32>>>,
    folder: PathBuf,
    output: Arc<Mutex<String>>,
    session_id: String,
    session_model: AgentModel,
    status: Arc<Mutex<Status>>,
}

/// Owns per-session worker queue senders and test channel overrides.
pub(crate) struct SessionWorkerService {
    /// Channels pre-registered for specific session workers in tests.
    ///
    /// Tests populate this map before enqueueing a command so that
    /// `ensure_session_worker` uses the injected channel instead of the
    /// default factory, enabling deterministic command execution without
    /// spawning real provider processes.
    pub(in crate::app::session) test_agent_channels: HashMap<String, Arc<dyn AgentChannel>>,
    workers: HashMap<String, mpsc::UnboundedSender<SessionCommand>>,
}

impl SessionWorkerService {
    /// Creates an empty worker service with no active session workers.
    pub(in crate::app::session) fn new() -> Self {
        Self {
            test_agent_channels: HashMap::new(),
            workers: HashMap::new(),
        }
    }

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
        services: &AppServices,
        runtime: SessionWorkerRuntime,
        command: SessionCommand,
    ) -> Result<(), String> {
        let operation_id = command.operation_id().to_string();
        let session_id = runtime.session_id.clone();
        services
            .db()
            .insert_session_operation(&operation_id, &session_id, command.kind())
            .await?;

        let sender = self.ensure_session_worker(services, &runtime);
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

    /// Drops worker queues for sessions no longer present in the active list.
    pub(super) fn retain_active_workers(&mut self, active_session_ids: &HashSet<String>) {
        self.workers
            .retain(|session_id, _| active_session_ids.contains(session_id));
    }

    /// Returns an existing session worker sender or creates one lazily.
    fn ensure_session_worker(
        &mut self,
        services: &AppServices,
        runtime: &SessionWorkerRuntime,
    ) -> mpsc::UnboundedSender<SessionCommand> {
        if let Some(sender) = self.workers.get(&runtime.session_id) {
            return sender.clone();
        }

        // When a pre-registered channel exists, reuse it; otherwise fall back
        // to the production channel factory.
        let channel = self
            .test_agent_channels
            .remove(&runtime.session_id)
            .unwrap_or_else(|| {
                create_agent_channel(runtime.session_model.kind(), services.app_server_client())
            });

        let context = SessionWorkerContext {
            app_event_tx: services.event_sender(),
            channel,
            child_pid: Arc::clone(&runtime.child_pid),
            db: services.db().clone(),
            folder: runtime.folder.clone(),
            fs_client: services.fs_client(),
            git_client: services.git_client(),
            output: Arc::clone(&runtime.output),
            session_id: runtime.session_id.clone(),
            status: Arc::clone(&runtime.status),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        self.workers
            .insert(runtime.session_id.clone(), sender.clone());
        Self::spawn_session_worker(context, receiver);

        sender
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

            let _ = context
                .channel
                .shutdown_session(context.session_id.clone())
                .await;
            if let Ok(mut guard) = context.child_pid.lock() {
                *guard = None;
            }
        });
    }

    /// Executes the queued command through the session's agent channel.
    async fn execute_session_command(
        context: &SessionWorkerContext,
        command: SessionCommand,
    ) -> Result<(), String> {
        let SessionCommand::Run {
            mode,
            prompt,
            session_model,
            ..
        } = command;

        Self::run_channel_turn(context, mode, prompt, session_model).await
    }

    /// Executes one agent turn through the session channel and applies all
    /// post-turn effects (stats, auto-commit, size refresh, status update).
    ///
    /// When `mode` is [`TurnMode::Resume`], the session is first transitioned
    /// to `InProgress` (start turns set `InProgress` in the lifecycle before
    /// enqueueing). Start turns schedule detached title generation immediately
    /// before the main turn request runs. Progress events update the UI
    /// indicator; `PidUpdate` events update the shared PID slot used for
    /// cancellation. If the turn fails, the error is appended to session output
    /// before transitioning to `Review`.
    async fn run_channel_turn(
        context: &SessionWorkerContext,
        mode: TurnMode,
        prompt: TurnPrompt,
        session_model: AgentModel,
    ) -> Result<(), String> {
        if matches!(mode, TurnMode::Resume { .. }) {
            let _ = context
                .db
                .update_session_questions(&context.session_id, "")
                .await;

            let _ = SessionTaskService::update_status(
                &context.status,
                &context.db,
                &context.app_event_tx,
                &context.session_id,
                Status::InProgress,
            )
            .await;
        }

        let session_project_id = load_session_project_id(&context.db, &context.session_id).await;
        let reasoning_level = load_project_reasoning_level(&context.db, session_project_id).await;
        let provider_conversation_id = context
            .db
            .get_session_provider_conversation_id(&context.session_id)
            .await
            .ok()
            .flatten();

        let req = TurnRequest {
            folder: context.folder.clone(),
            live_session_output: Some(Arc::clone(&context.output)),
            model: session_model.as_str().to_string(),
            mode: mode.clone(),
            prompt: prompt.clone(),
            protocol_profile: agent::ProtocolRequestProfile::SessionTurn,
            provider_conversation_id,
            reasoning_level,
        };

        let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();

        let consumer = tokio::spawn(consume_turn_events(
            event_rx,
            Arc::clone(&context.output),
            context.db.clone(),
            context.app_event_tx.clone(),
            context.session_id.clone(),
            Arc::clone(&context.child_pid),
        ));

        SessionTaskService::set_session_progress(
            &context.app_event_tx,
            &context.session_id,
            Some("Thinking".to_string()),
        );

        spawn_start_turn_title_generation(
            context,
            session_project_id,
            &mode,
            &prompt.text,
            session_model,
        )
        .await;

        let turn_result = context
            .channel
            .run_turn(context.session_id.clone(), req, event_tx)
            .await;
        SessionManager::cleanup_prompt_attachment_paths(
            context.fs_client.clone(),
            prompt.local_image_paths().cloned().collect(),
        )
        .await;

        let streamed_assistant_content = consumer.await.unwrap_or(false);
        SessionTaskService::clear_session_progress(&context.app_event_tx, &context.session_id);

        let result = apply_turn_result(
            context,
            session_model,
            turn_result,
            streamed_assistant_content,
        )
        .await;

        if let Some(session_size) = SessionTaskService::refresh_persisted_session_size(
            &context.db,
            context.git_client.as_ref(),
            &context.session_id,
            &context.folder,
        )
        .await
        {
            let _ = context.app_event_tx.send(AppEvent::SessionSizeUpdated {
                session_id: context.session_id.clone(),
                session_size,
            });
        }

        let target_status = match &result {
            Ok(status) => *status,
            Err(_) => Status::Review,
        };

        let _ = SessionTaskService::update_status(
            &context.status,
            &context.db,
            &context.app_event_tx,
            &context.session_id,
            target_status,
        )
        .await;

        result.map(|_| ())
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

impl SessionManager {
    /// Marks unfinished operations from previous process runs as failed.
    pub(crate) async fn fail_unfinished_operations_from_previous_run(db: &Database) {
        SessionWorkerService::fail_unfinished_operations_from_previous_run(db).await;
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
        let runtime = self.session_worker_runtime_or_err(session_id)?;

        self.worker_service_mut()
            .enqueue_session_command(services, runtime, command)
            .await
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().clear_session_worker(session_id);
    }

    /// Drops worker queues for touched sessions that reached terminal status.
    ///
    /// Terminal sessions (`Done`, `Canceled`) no longer execute turns, so
    /// dropping their worker sender lets the worker task exit and shut down any
    /// provider runtime process associated with that session.
    pub(crate) fn clear_terminal_session_workers(&mut self, updated_session_ids: &HashSet<String>) {
        let terminal_session_ids = updated_session_ids
            .iter()
            .filter_map(|session_id| {
                self.sessions
                    .iter()
                    .find(|session| session.id == *session_id)
                    .and_then(|session| {
                        matches!(session.status, Status::Done | Status::Canceled)
                            .then(|| session.id.clone())
                    })
            })
            .collect::<Vec<_>>();

        for session_id in terminal_session_ids {
            self.clear_session_worker(&session_id);
        }
    }

    /// Builds worker-runtime data for one session.
    ///
    /// # Errors
    /// Returns an error when the session or runtime handles are missing.
    fn session_worker_runtime_or_err(
        &self,
        session_id: &str,
    ) -> Result<SessionWorkerRuntime, String> {
        let (session, handles) = self.session_and_handles_or_err(session_id)?;

        Ok(SessionWorkerRuntime {
            child_pid: Arc::clone(&handles.child_pid),
            folder: session.folder.clone(),
            output: Arc::clone(&handles.output),
            session_id: session.id.clone(),
            session_model: session.model,
            status: Arc::clone(&handles.status),
        })
    }
}

/// Applies the turn result: appends the final response, persists the raw
/// summary payload, updates stats, and runs auto-commit. Returns `Ok(Status)`
/// on success or `Err(description)` on turn failure after appending the error
/// to session output.
///
/// The final parsed response appends non-empty protocol `answer` text only
/// when no assistant stream chunks were already appended for this turn. This
/// avoids duplicate assistant output while preserving streamed answers.
///
/// When no `answer` messages exist, worker output falls back to joined
/// `question` text so clarification prompts remain visible while thought-only
/// responses are not persisted as final transcript output.
///
/// Structured response summaries are stored separately from transcript output
/// so the UI can render them without parsing answer markdown from `answer`
/// messages.
///
/// `question` messages are persisted to the session row and trigger
/// `Status::Question`; all responses are emitted through
/// `AppEvent::AgentResponseReceived` for reducer-level routing.
async fn apply_turn_result(
    context: &SessionWorkerContext,
    session_model: AgentModel,
    turn_result: Result<TurnResult, AgentError>,
    streamed_assistant_content: bool,
) -> Result<Status, String> {
    match turn_result {
        Ok(result) => {
            let TurnResult {
                assistant_message,
                input_tokens,
                output_tokens,
                provider_conversation_id,
                ..
            } = result;

            if let Some(message) =
                build_assistant_transcript_output(&assistant_message, streamed_assistant_content)
            {
                SessionTaskService::append_session_output(
                    &context.output,
                    &context.db,
                    &context.app_event_tx,
                    &context.session_id,
                    message.as_str(),
                )
                .await;
            }

            let summary_markdown = assistant_message
                .summary
                .as_ref()
                .and_then(|summary| serde_json::to_string(summary).ok())
                .unwrap_or_default();
            let _ = context
                .db
                .update_session_summary(&context.session_id, &summary_markdown)
                .await;

            let question_items = assistant_message.question_items();
            let target_status = if question_items.is_empty() {
                let _ = context
                    .db
                    .update_session_questions(&context.session_id, "")
                    .await;

                Status::Review
            } else {
                if let Ok(questions_json) = serde_json::to_string(&question_items) {
                    let _ = context
                        .db
                        .update_session_questions(&context.session_id, &questions_json)
                        .await;
                }

                Status::Question
            };
            let _ = context.app_event_tx.send(AppEvent::AgentResponseReceived {
                response: assistant_message,
                session_id: context.session_id.clone(),
            });

            let stats = SessionStats {
                input_tokens,
                output_tokens,
            };
            let _ = context
                .db
                .update_session_stats(&context.session_id, &stats)
                .await;
            let _ = context
                .db
                .upsert_session_usage(&context.session_id, session_model.as_str(), &stats)
                .await;
            let _ = context
                .db
                .update_session_provider_conversation_id(
                    &context.session_id,
                    provider_conversation_id.as_deref(),
                )
                .await;

            SessionTaskService::handle_auto_commit(AssistContext {
                app_event_tx: context.app_event_tx.clone(),
                child_pid: Arc::clone(&context.child_pid),
                db: context.db.clone(),
                folder: context.folder.clone(),
                git_client: Arc::clone(&context.git_client),
                id: context.session_id.clone(),
                output: Arc::clone(&context.output),
                session_model,
            })
            .await;

            Ok(target_status)
        }
        Err(error) => {
            let message = format!("\n{}\n", error.0.trim());
            SessionTaskService::append_session_output(
                &context.output,
                &context.db,
                &context.app_event_tx,
                &context.session_id,
                &message,
            )
            .await;

            Err(error.0)
        }
    }
}

/// Spawns first-turn session title generation from the initial user prompt.
async fn spawn_start_turn_title_generation(
    context: &SessionWorkerContext,
    session_project_id: Option<i64>,
    mode: &TurnMode,
    prompt: &str,
    session_model: AgentModel,
) {
    if !matches!(mode, TurnMode::Start) {
        return;
    }

    let title_model = load_project_model_setting(
        &context.db,
        session_project_id,
        SettingName::DefaultFastModel,
    )
    .await
    .unwrap_or(session_model);

    SessionManager::spawn_session_title_generation_task(
        context.app_event_tx.clone(),
        context.db.clone(),
        &context.session_id,
        &context.folder,
        prompt,
        title_model,
    );
}

/// Loads the project identifier associated with one persisted session.
async fn load_session_project_id(db: &Database, session_id: &str) -> Option<i64> {
    db.load_session_project_id(session_id).await.ok().flatten()
}

/// Loads the project-scoped reasoning level for one session context.
async fn load_project_reasoning_level(db: &Database, project_id: Option<i64>) -> ReasoningLevel {
    let Some(project_id) = project_id else {
        return ReasoningLevel::default();
    };

    db.load_project_reasoning_level(project_id)
        .await
        .unwrap_or_default()
}

/// Loads one project-scoped model setting and parses it into an [`AgentModel`].
async fn load_project_model_setting(
    db: &Database,
    project_id: Option<i64>,
    setting_name: SettingName,
) -> Option<AgentModel> {
    let project_id = project_id?;

    db.get_project_setting(project_id, setting_name.as_str())
        .await
        .ok()
        .flatten()
        .and_then(|setting_value| AgentModel::from_str(&setting_value).ok())
}

/// Builds the persisted transcript chunk for one parsed assistant response.
///
/// Prefers joined `answer` messages so normal chat output stays concise.
///
/// When `streamed_assistant_content` is `true`, non-empty `answer` text is not
/// appended again to avoid duplicate output in the transcript.
///
/// Falls back to joined `question` text when no answers are present so
/// clarification prompts stay visible while thought-only responses are not
/// persisted as final transcript output.
fn build_assistant_transcript_output(
    assistant_message: &agent::AgentResponse,
    streamed_assistant_content: bool,
) -> Option<String> {
    let answer_text = assistant_message.to_answer_display_text();
    if !answer_text.trim().is_empty() {
        if streamed_assistant_content {
            return None;
        }

        return Some(format!("{}\n\n", answer_text.trim_end()));
    }

    let question_text = assistant_message
        .question_items()
        .into_iter()
        .filter_map(|question_item| {
            let trimmed_question = question_item.text.trim();
            if trimmed_question.is_empty() {
                return None;
            }

            Some(trimmed_question.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if question_text.is_empty() {
        return None;
    }

    Some(format!("{question_text}\n\n"))
}

/// Consumes [`TurnEvent`]s from `event_rx` and applies their side effects.
///
/// - [`TurnEvent::AssistantDelta`]: appends text to the session output buffer
///   and emits [`AppEvent::OutputAppended`].
/// - [`TurnEvent::ThoughtDelta`]: updates the live progress indicator.
/// - [`TurnEvent::Progress`]: updates the live progress indicator.
/// - [`TurnEvent::PidUpdate`]: writes the new PID into `child_pid`.
/// - [`TurnEvent::Completed`] / [`TurnEvent::Failed`]: reserved; ignored here
///   because completion is signalled by `run_turn`'s return value.
///
/// Returns `true` when at least one non-empty [`TurnEvent::AssistantDelta`]
/// was appended to transcript output.
async fn consume_turn_events(
    mut event_rx: mpsc::UnboundedReceiver<TurnEvent>,
    output: Arc<Mutex<String>>,
    db: Database,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    session_id: String,
    child_pid: Arc<Mutex<Option<u32>>>,
) -> bool {
    let mut streamed_assistant_content = false;
    let mut active_progress: Option<String> = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TurnEvent::AssistantDelta(text) => {
                if text.trim().is_empty() {
                    continue;
                }

                if active_progress.take().is_some() {
                    SessionTaskService::set_session_progress(&app_event_tx, &session_id, None);
                }

                SessionTaskService::append_session_output(
                    &output,
                    &db,
                    &app_event_tx,
                    &session_id,
                    &text,
                )
                .await;
                streamed_assistant_content = true;
            }
            TurnEvent::ThoughtDelta(thought) => {
                let Some(thought) = normalize_thinking_stream_text(&thought) else {
                    continue;
                };
                if active_progress.as_deref() == Some(thought.as_str()) {
                    continue;
                }

                active_progress = Some(thought.clone());
                SessionTaskService::set_session_progress(
                    &app_event_tx,
                    &session_id,
                    Some(thought.clone()),
                );
            }
            TurnEvent::Progress(progress) => {
                let Some(progress) = normalize_thinking_stream_text(&progress) else {
                    continue;
                };
                if active_progress.as_deref() == Some(progress.as_str()) {
                    continue;
                }

                active_progress = Some(progress.clone());
                SessionTaskService::set_session_progress(
                    &app_event_tx,
                    &session_id,
                    Some(progress.clone()),
                );
            }
            TurnEvent::PidUpdate(pid) => {
                if let Ok(mut guard) = child_pid.lock() {
                    *guard = pid;
                }
            }
            TurnEvent::Completed { .. } | TurnEvent::Failed(_) => {
                // Completion is signalled by run_turn's return value; these
                // variants are reserved for future use and ignored here.
            }
        }
    }

    if active_progress.take().is_some() {
        SessionTaskService::set_session_progress(&app_event_tx, &session_id, None);
    }

    streamed_assistant_content
}

/// Returns one normalized thinking/progress text line.
///
/// The worker stores this normalized form in progress state so repeated
/// updates can be de-duplicated reliably.
fn normalize_thinking_stream_text(text: &str) -> Option<String> {
    let trimmed_text = text.trim();
    if trimmed_text.is_empty() {
        return None;
    }

    Some(trimmed_text.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;
    use crate::infra::agent::AgentResponse;
    use crate::infra::agent::protocol::AgentResponseMessage;
    use crate::infra::channel::MockAgentChannel;
    use crate::infra::db::Database;
    use crate::infra::fs;
    use crate::infra::git::MockGitClient;

    #[test]
    /// Ensures `Start` mode maps to `start_prompt` and `Resume` maps to
    /// `reply` in persisted operation labels.
    fn test_session_command_kind_values() {
        // Arrange
        let start_command = SessionCommand::Run {
            operation_id: "op-start".to_string(),
            mode: TurnMode::Start,
            prompt: "prompt".into(),
            session_model: AgentModel::ClaudeSonnet46,
        };
        let resume_command = SessionCommand::Run {
            operation_id: "op-resume".to_string(),
            mode: TurnMode::Resume {
                session_output: None,
            },
            prompt: "prompt".into(),
            session_model: AgentModel::ClaudeSonnet46,
        };

        // Act
        let start_kind = start_command.kind();
        let resume_kind = resume_command.kind();

        // Assert
        assert_eq!(start_kind, "start_prompt");
        assert_eq!(resume_kind, "reply");
    }

    #[test]
    fn test_agent_response_questions_returns_only_question_messages() {
        // Arrange
        let agent_response = AgentResponse {
            messages: vec![
                AgentResponseMessage::answer("Implemented the feature."),
                AgentResponseMessage::question("Need a target branch?"),
                AgentResponseMessage::question("Need migration notes?"),
            ],
            summary: None,
        };

        // Act
        let items = agent_response.question_items();

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "Need a target branch?");
        assert_eq!(items[1].text, "Need migration notes?");
    }

    #[test]
    fn test_agent_response_questions_preserves_ordered_list_as_single_question_text() {
        // Arrange
        let numbered_questions =
            "1) Is this repository intentionally incomplete (docs-only), or should it include the \
             referenced dotfiles tree (for\nexample `.config/` and `lua/`)?\n2) Should I propose \
             and apply a docs-only cleanup now (aligning setup steps to the current files), or \
             keep docs\nas-is and treat missing files as a known gap?\n3) Do you want keyd \
             instructions rewritten to the safer `/etc/keyd/default.conf` path with existence \
             checks and\nrollback notes?";
        let agent_response = AgentResponse {
            messages: vec![AgentResponseMessage::question(numbered_questions)],
            summary: None,
        };

        // Act
        let items = agent_response.question_items();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, numbered_questions);
    }

    #[test]
    /// Ensures transcript output prefers `answer` messages when available.
    fn test_build_assistant_transcript_output_prefers_answer_messages() {
        // Arrange
        let response = AgentResponse {
            messages: vec![
                AgentResponseMessage::answer("Implemented the fix."),
                AgentResponseMessage::question("Need me to run tests?"),
            ],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response, false);

        // Assert
        assert_eq!(
            transcript_output,
            Some("Implemented the fix.\n\n".to_string())
        );
    }

    #[test]
    /// Ensures transcript output suppresses final answer append when assistant
    /// chunks already streamed during the turn.
    fn test_build_assistant_transcript_output_skips_answer_when_assistant_streamed() {
        // Arrange
        let response = AgentResponse {
            messages: vec![AgentResponseMessage::answer("Implemented the fix.")],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response, true);

        // Assert
        assert_eq!(transcript_output, None);
    }

    #[test]
    /// Ensures transcript output falls back to question text when no answers
    /// are present.
    fn test_build_assistant_transcript_output_falls_back_to_question_text() {
        // Arrange
        let response = AgentResponse {
            messages: vec![AgentResponseMessage::question("Should I apply the patch?")],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response, false);

        // Assert
        assert_eq!(
            transcript_output,
            Some("Should I apply the patch?\n\n".to_string())
        );
    }

    #[test]
    /// Ensures blank protocol messages do not append empty transcript output.
    fn test_build_assistant_transcript_output_returns_none_for_blank_messages() {
        // Arrange
        let response = AgentResponse {
            messages: vec![
                AgentResponseMessage::answer(""),
                AgentResponseMessage::question("\n"),
            ],
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response, false);

        // Assert
        assert_eq!(transcript_output, None);
    }

    #[tokio::test]
    /// Verifies streamed thought/progress updates stay in the live progress
    /// badge and are not appended to transcript output.
    async fn test_consume_turn_events_keeps_streamed_thinking_updates_out_of_transcript() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let output = Arc::new(Mutex::new(String::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::ThoughtDelta(
                "Reviewing changed files".to_string(),
            ))
            .expect("failed to send thought delta");
        event_tx
            .send(TurnEvent::Progress("Running tests".to_string()))
            .expect("failed to send progress update");
        drop(event_tx);

        // Act
        let streamed_assistant_content = consume_turn_events(
            event_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "session-1".to_string(),
            child_pid,
        )
        .await;

        let mut progress_messages = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::SessionProgressUpdated {
                progress_message, ..
            } = event
            {
                progress_messages.push(progress_message);
            }
        }

        // Assert
        assert!(!streamed_assistant_content);
        assert_eq!(output.lock().expect("output lock poisoned").as_str(), "");
        assert_eq!(
            progress_messages,
            vec![
                Some("Reviewing changed files".to_string()),
                Some("Running tests".to_string()),
                None,
            ]
        );
    }

    #[tokio::test]
    /// Verifies duplicate streamed thinking/progress updates are suppressed for
    /// progress events and never append transcript output.
    async fn test_consume_turn_events_suppresses_duplicate_thinking_updates() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let output = Arc::new(Mutex::new(String::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::Progress("Running tests".to_string()))
            .expect("failed to send first progress update");
        event_tx
            .send(TurnEvent::Progress("Running tests".to_string()))
            .expect("failed to send duplicate progress update");
        event_tx
            .send(TurnEvent::ThoughtDelta("Running tests".to_string()))
            .expect("failed to send duplicate thought update");
        event_tx
            .send(TurnEvent::ThoughtDelta("Summarizing results".to_string()))
            .expect("failed to send new thought update");
        drop(event_tx);

        // Act
        let _ = consume_turn_events(
            event_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "session-1".to_string(),
            child_pid,
        )
        .await;

        let mut progress_messages = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::SessionProgressUpdated {
                progress_message, ..
            } = event
            {
                progress_messages.push(progress_message);
            }
        }

        // Assert
        assert_eq!(output.lock().expect("output lock poisoned").as_str(), "");
        assert_eq!(
            progress_messages,
            vec![
                Some("Running tests".to_string()),
                Some("Summarizing results".to_string()),
                None,
            ]
        );
    }

    #[tokio::test]
    /// Verifies generic badge progress updates stay in progress state only and
    /// do not append transcript output.
    async fn test_consume_turn_events_keeps_badge_updates_out_of_transcript() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let output = Arc::new(Mutex::new(String::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::ThoughtDelta("Thinking".to_string()))
            .expect("failed to send thought update");
        event_tx
            .send(TurnEvent::Progress("Phase: plan".to_string()))
            .expect("failed to send phase progress update");
        event_tx
            .send(TurnEvent::Progress("Using tool: Cargo.toml".to_string()))
            .expect("failed to send tool progress update");
        drop(event_tx);

        // Act
        let streamed_any_content = consume_turn_events(
            event_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "session-1".to_string(),
            child_pid,
        )
        .await;

        let mut progress_messages = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::SessionProgressUpdated {
                progress_message, ..
            } = event
            {
                progress_messages.push(progress_message);
            }
        }

        // Assert
        assert!(!streamed_any_content);
        assert_eq!(output.lock().expect("output lock poisoned").as_str(), "");
        assert_eq!(
            progress_messages,
            vec![
                Some("Thinking".to_string()),
                Some("Phase: plan".to_string()),
                Some("Using tool: Cargo.toml".to_string()),
                None,
            ]
        );
    }

    #[tokio::test]
    /// Verifies assistant stream chunks mark the turn as streamed-assistant
    /// output.
    async fn test_consume_turn_events_returns_true_for_assistant_delta() {
        // Arrange
        let db = Database::open_in_memory().await.expect("failed to open db");
        let output = Arc::new(Mutex::new(String::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::AssistantDelta("Partial answer".to_string()))
            .expect("failed to send assistant delta");
        drop(event_tx);

        // Act
        let streamed_assistant_content = consume_turn_events(
            event_rx,
            Arc::clone(&output),
            db,
            app_event_tx,
            "session-1".to_string(),
            child_pid,
        )
        .await;

        // Assert
        assert!(streamed_assistant_content);
        assert_eq!(
            output.lock().expect("output lock poisoned").as_str(),
            "Partial answer"
        );
    }

    #[tokio::test]
    /// Verifies restart recovery marks unfinished operations failed and
    /// restores affected sessions to `Review`.
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
    /// Verifies unfinished operations remain executable when cancel has not
    /// been requested.
    async fn test_should_skip_worker_command_without_cancel_request() {
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

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(!should_skip);
        assert!(is_unfinished);
    }

    #[tokio::test]
    /// Verifies cancel requests skip queued operations before execution and
    /// mark them canceled.
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

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(should_skip);
        assert!(!is_unfinished);
    }
}
