//! Per-session async worker orchestration for serialized command execution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use serde_json;
use tokio::sync::mpsc;

use super::SessionTaskService;
use crate::app::assist::AssistContext;
use crate::app::session::{Clock, SessionError, unix_timestamp_from_system_time};
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{SessionStats, Status};
use crate::domain::setting::SettingName;
use crate::infra::agent;
use crate::infra::channel::{
    AgentChannel, AgentError, AgentRequestKind, TurnEvent, TurnPrompt, TurnRequest, TurnResult,
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
    /// Executes one agent turn with the given request kind and prompt.
    Run {
        /// Persisted operation identifier.
        operation_id: String,
        /// Whether this is a first-message start or a follow-up resume.
        request_kind: AgentRequestKind,
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
                request_kind: AgentRequestKind::SessionStart,
                ..
            } => "start_prompt",
            Self::Run {
                request_kind: AgentRequestKind::SessionResume { .. },
                ..
            } => "reply",
            Self::Run {
                request_kind: AgentRequestKind::UtilityPrompt,
                ..
            } => "utility_prompt",
        }
    }
}

/// Shared state threaded through all worker turn executions.
struct SessionWorkerContext {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Provider-agnostic agent channel for this session's worker.
    channel: Arc<dyn AgentChannel>,
    child_pid: Arc<Mutex<Option<u32>>>,
    clock: Arc<dyn Clock>,
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

    /// Marks unfinished operations from previous process runs as failed and
    /// closes any open active-work timing window at `timestamp_seconds`.
    pub(super) async fn fail_unfinished_operations_from_previous_run_at(
        db: &Database,
        timestamp_seconds: i64,
    ) {
        let interrupted_session_ids: HashSet<String> = db
            .load_unfinished_session_operations()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|operation| operation.session_id)
            .collect();

        for session_id in interrupted_session_ids {
            // Best-effort: status persistence failure is non-critical.
            let _ = db
                .update_session_status_with_timing_at(
                    &session_id,
                    &Status::Review.to_string(),
                    timestamp_seconds,
                )
                .await;
        }

        // Best-effort: operation tracking metadata is non-critical.
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
    ) -> Result<(), SessionError> {
        let operation_id = command.operation_id().to_string();
        let session_id = runtime.session_id.clone();
        services
            .db()
            .insert_session_operation(&operation_id, &session_id, command.kind())
            .await?;

        let sender = self.ensure_session_worker(services, &runtime);
        if sender.send(command).is_err() {
            // Best-effort: operation tracking metadata is non-critical.
            let _ = services
                .db()
                .mark_session_operation_failed(&operation_id, "Session worker is not available")
                .await;

            return Err(SessionError::Workflow(
                "Session worker is not available".to_string(),
            ));
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
                create_agent_channel(
                    runtime.session_model.kind(),
                    services.app_server_client_override(),
                )
            });

        let context = SessionWorkerContext {
            app_event_tx: services.event_sender(),
            channel,
            child_pid: Arc::clone(&runtime.child_pid),
            clock: services.clock(),
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
                // Best-effort: operation tracking metadata is non-critical.
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
                        // Best-effort: operation tracking metadata is non-critical.
                        let _ = context.db.mark_session_operation_done(&operation_id).await;
                    }
                    Err(error) => {
                        // Best-effort: operation tracking metadata is non-critical.
                        let _ = context
                            .db
                            .mark_session_operation_failed(&operation_id, &error.to_string())
                            .await;
                    }
                }
            }

            // Best-effort: session transport may already be torn down.
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
    ) -> Result<(), SessionError> {
        let SessionCommand::Run {
            request_kind,
            prompt,
            session_model,
            ..
        } = command;

        Self::run_channel_turn(context, request_kind, prompt, session_model).await
    }

    /// Executes one agent turn through the session channel and applies all
    /// post-turn effects (stats, auto-commit, size refresh, status update).
    ///
    /// When `request_kind` is [`AgentRequestKind::SessionResume`], the session
    /// is first transitioned to `InProgress` (start turns set `InProgress` in
    /// the lifecycle before enqueueing). Start turns schedule detached title
    /// generation immediately before the main turn request runs. Progress
    /// events update the UI indicator; `PidUpdate` events update the shared PID
    /// slot used for cancellation. If the turn fails, the error is appended to
    /// session output before transitioning to `Review`.
    async fn run_channel_turn(
        context: &SessionWorkerContext,
        request_kind: AgentRequestKind,
        prompt: TurnPrompt,
        session_model: AgentModel,
    ) -> Result<(), SessionError> {
        if matches!(request_kind, AgentRequestKind::SessionResume { .. }) {
            // Best-effort: questions persistence failure is non-critical.
            let _ = context
                .db
                .update_session_questions(&context.session_id, "")
                .await;

            // Best-effort: status transition failure is non-critical.
            let _ = SessionTaskService::update_status(
                &context.status,
                context.clock.as_ref(),
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
            request_kind: request_kind.clone(),
            prompt: prompt.clone(),
            provider_conversation_id,
            reasoning_level,
        };

        let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();

        let consumer = tokio::spawn(consume_turn_events(
            event_rx,
            context.app_event_tx.clone(),
            context.session_id.clone(),
            Arc::clone(&context.child_pid),
        ));

        spawn_start_turn_title_generation(
            context,
            session_project_id,
            &request_kind,
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

        let _ = consumer.await;

        let result = apply_turn_result(context, session_model, turn_result).await;

        if let Some(session_size) = SessionTaskService::refresh_persisted_session_size(
            &context.db,
            context.git_client.as_ref(),
            &context.session_id,
            &context.folder,
        )
        .await
        {
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = context.app_event_tx.send(AppEvent::SessionSizeUpdated {
                session_id: context.session_id.clone(),
                session_size,
            });
        }

        let target_status = match &result {
            Ok(status) => *status,
            Err(_) => Status::Review,
        };

        // Best-effort: status transition failure is non-critical.
        let _ = SessionTaskService::update_status(
            &context.status,
            context.clock.as_ref(),
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

        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .mark_session_operation_canceled(operation_id, CANCEL_BEFORE_EXECUTION_REASON)
            .await;

        true
    }
}

impl SessionManager {
    /// Marks unfinished operations from previous process runs as failed.
    pub(crate) async fn fail_unfinished_operations_from_previous_run(
        db: Database,
        clock: Arc<dyn Clock>,
    ) {
        let timestamp_seconds = unix_timestamp_from_system_time(clock.now_system_time());

        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            timestamp_seconds,
        )
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
    ) -> Result<(), SessionError> {
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
    ) -> Result<SessionWorkerRuntime, SessionError> {
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

/// Applies the turn result: appends the final response, persists follow-up
/// metadata, updates stats, and runs auto-commit. Returns `Ok(Status)` on
/// success or `Err(description)` on turn failure after appending the error to
/// session output.
///
/// The final parsed response appends non-empty protocol `answer` text once the
/// turn completes. When no `answer` text exists, worker output falls back to
/// joined question text so clarification prompts remain visible while
/// thought-only responses are not persisted as final transcript output.
///
/// The raw agent `summary` payload is persisted here so refresh-driven UI
/// rendering can reload it from the database and show separate `Current Turn`
/// and `Session Changes` blocks. When the session later reaches `Done`, the
/// merge path rewrites the persisted value into markdown with `# Summary` and
/// `# Commit` sections.
///
/// Clarification questions are persisted to the session row, follow-up tasks
/// are replaced through their dedicated table, and question responses trigger
/// `Status::Question`; all responses are emitted through
/// `AppEvent::AgentResponseReceived` for reducer-level routing.
async fn apply_turn_result(
    context: &SessionWorkerContext,
    session_model: AgentModel,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<Status, SessionError> {
    match turn_result {
        Ok(result) => apply_successful_turn_result(context, session_model, result).await,
        Err(error) => {
            let error_text = error.to_string();
            let message = format!("\n{}\n", error_text.trim());
            SessionTaskService::append_session_output(
                &context.output,
                &context.db,
                &context.app_event_tx,
                &context.session_id,
                &message,
            )
            .await;

            Err(SessionError::Workflow(error_text))
        }
    }
}

/// Persists the successful turn payload, emits reducer events, and runs the
/// auto-commit workflow before returning the next session status.
async fn apply_successful_turn_result(
    context: &SessionWorkerContext,
    session_model: AgentModel,
    result: TurnResult,
) -> Result<Status, SessionError> {
    let TurnResult {
        assistant_message,
        input_tokens,
        output_tokens,
        provider_conversation_id,
        ..
    } = result;

    if let Some(message) = build_assistant_transcript_output(&assistant_message) {
        SessionTaskService::append_session_output(
            &context.output,
            &context.db,
            &context.app_event_tx,
            &context.session_id,
            message.as_str(),
        )
        .await;
    }

    let summary_text = persisted_session_summary_payload(&assistant_message);
    // Best-effort: summary persistence failure is non-critical.
    let _ = context
        .db
        .update_session_summary(&context.session_id, &summary_text)
        .await;
    let follow_up_tasks = assistant_message.follow_up_task_items();
    // Best-effort: follow-up-task persistence failure is non-critical.
    let _ = context
        .db
        .replace_session_follow_up_tasks(&context.session_id, &follow_up_tasks)
        .await;

    let summary_prefix = summary_transcript_prefix(&context.output);
    if let Some(summary_output) =
        build_summary_transcript_output(&assistant_message, &summary_prefix)
    {
        SessionTaskService::append_session_output(
            &context.output,
            &context.db,
            &context.app_event_tx,
            &context.session_id,
            &summary_output,
        )
        .await;
    }

    // Fire-and-forget: receiver may be dropped during shutdown.
    let _ = context.app_event_tx.send(AppEvent::RefreshSessions);

    let question_items = assistant_message.question_items();
    let target_status = if question_items.is_empty() {
        // Best-effort: questions persistence failure is non-critical.
        let _ = context
            .db
            .update_session_questions(&context.session_id, "")
            .await;

        Status::Review
    } else {
        if let Ok(questions_json) = serde_json::to_string(&question_items) {
            // Best-effort: questions persistence failure is non-critical.
            let _ = context
                .db
                .update_session_questions(&context.session_id, &questions_json)
                .await;
        }

        Status::Question
    };
    // Fire-and-forget: receiver may be dropped during shutdown.
    let _ = context.app_event_tx.send(AppEvent::AgentResponseReceived {
        response: assistant_message,
        session_id: context.session_id.clone(),
    });

    let stats = SessionStats {
        input_tokens,
        output_tokens,
    };
    // Best-effort: stats persistence failure is non-critical.
    let _ = context
        .db
        .update_session_stats(&context.session_id, &stats)
        .await;
    // Best-effort: usage persistence failure is non-critical.
    let _ = context
        .db
        .upsert_session_usage(&context.session_id, session_model.as_str(), &stats)
        .await;
    // Best-effort: provider ID persistence failure is non-critical.
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

/// Spawns first-turn session title generation from the initial user prompt.
async fn spawn_start_turn_title_generation(
    context: &SessionWorkerContext,
    session_project_id: Option<i64>,
    request_kind: &AgentRequestKind,
    prompt: &str,
    session_model: AgentModel,
) {
    if !matches!(request_kind, AgentRequestKind::SessionStart) {
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

/// Returns the spacer that should precede the summary transcript block.
///
/// Summary markdown should stay visually separated from any previously
/// persisted assistant text. When the current transcript already ends with a
/// blank line, no extra spacing is needed.
fn summary_transcript_prefix(output: &Arc<Mutex<String>>) -> String {
    let Ok(output) = output.lock() else {
        return String::new();
    };

    if output.is_empty() || output.ends_with("\n\n") {
        return String::new();
    }

    if output.ends_with('\n') {
        return "\n".to_string();
    }

    "\n\n".to_string()
}

/// Builds the persisted transcript chunk for one parsed assistant response.
///
/// Prefers the top-level `answer` text so normal chat output stays concise.
/// Falls back to joined question text when no answer is present so
/// clarification prompts stay visible while thought-only responses are not
/// persisted as final transcript output.
fn build_assistant_transcript_output(assistant_message: &agent::AgentResponse) -> Option<String> {
    let answer_text = assistant_message.to_answer_display_text();
    if !answer_text.trim().is_empty() {
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

/// Serializes one assistant summary payload for session persistence.
///
/// Review-mode rendering uses the raw JSON object so it can display separate
/// `Current Turn` and `Session Changes` sections without reparsing answer
/// markdown.
fn persisted_session_summary_payload(assistant_message: &agent::AgentResponse) -> String {
    assistant_message
        .summary
        .as_ref()
        .and_then(|summary| serde_json::to_string(summary).ok())
        .unwrap_or_default()
}

/// Formats one assistant summary payload as a markdown transcript block for
/// inline output display.
///
/// Returns `None` only when no summary struct is present on the response.
/// Empty fields fall back to `"No changes"` so sections stay visually
/// consistent even when both fields are empty. `summary_prefix` is prepended
/// when the existing transcript needs extra spacing before the summary block.
fn build_summary_transcript_output(
    assistant_message: &agent::AgentResponse,
    summary_prefix: &str,
) -> Option<String> {
    let summary = assistant_message.summary.as_ref()?;
    let turn = summary.turn.trim();
    let session = summary.session.trim();

    let turn_text = if turn.is_empty() { "No changes" } else { turn };
    let session_text = if session.is_empty() {
        "No changes"
    } else {
        session
    };

    Some(format!(
        "{summary_prefix}## Change Summary\n### Current Turn\n{turn_text}\n\n### Session \
         Changes\n{session_text}\n\n"
    ))
}

/// Consumes [`TurnEvent`]s from `event_rx` and applies their side effects.
///
/// - [`TurnEvent::ThoughtDelta`]: updates the transient thinking loader text.
/// - [`TurnEvent::PidUpdate`]: writes the new PID into `child_pid`.
/// - [`TurnEvent::Completed`] / [`TurnEvent::Failed`]: reserved; ignored here
///   because completion is signalled by `run_turn`'s return value.
async fn consume_turn_events(
    mut event_rx: mpsc::UnboundedReceiver<TurnEvent>,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    session_id: String,
    child_pid: Arc<Mutex<Option<u32>>>,
) {
    let mut active_progress: Option<String> = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TurnEvent::ThoughtDelta(thought) => {
                let Some(thought) = normalize_thinking_stream_text(&thought) else {
                    continue;
                };
                if active_progress.as_deref() == Some(thought.as_str()) {
                    continue;
                }

                active_progress = Some(thought.clone());
                SessionTaskService::set_session_progress(&app_event_tx, &session_id, Some(thought));
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
        SessionTaskService::clear_session_progress(&app_event_tx, &session_id);
    }
}

/// Returns one normalized thinking text line.
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
    use crate::infra::agent::protocol::{AgentResponseSummary, QuestionItem};
    use crate::infra::channel::MockAgentChannel;
    use crate::infra::db::Database;
    use crate::infra::fs;
    use crate::infra::git::MockGitClient;

    #[test]
    /// Ensures session start requests map to `start_prompt` and session
    /// resume requests map to `reply` in persisted operation labels.
    fn test_session_command_kind_values() {
        // Arrange
        let start_command = SessionCommand::Run {
            operation_id: "op-start".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "prompt".into(),
            session_model: AgentModel::ClaudeSonnet46,
        };
        let resume_command = SessionCommand::Run {
            operation_id: "op-resume".to_string(),
            request_kind: AgentRequestKind::SessionResume {
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
            answer: "Implemented the feature.".to_string(),
            questions: vec![
                QuestionItem::new("Need a target branch?"),
                QuestionItem::new("Need migration notes?"),
            ],
            follow_up_tasks: Vec::new(),
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
            answer: String::new(),
            questions: vec![QuestionItem::new(numbered_questions)],
            follow_up_tasks: Vec::new(),
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
            answer: "Implemented the fix.".to_string(),
            questions: vec![QuestionItem::new("Need me to run tests?")],
            follow_up_tasks: Vec::new(),
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response);

        // Assert
        assert_eq!(
            transcript_output,
            Some("Implemented the fix.\n\n".to_string())
        );
    }

    #[test]
    /// Ensures transcript output falls back to question text when no answers
    /// are present.
    fn test_build_assistant_transcript_output_falls_back_to_question_text() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::new("Should I apply the patch?")],
            follow_up_tasks: Vec::new(),
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response);

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
            answer: String::new(),
            questions: vec![QuestionItem::new("\n")],
            follow_up_tasks: Vec::new(),
            summary: None,
        };

        // Act
        let transcript_output = build_assistant_transcript_output(&response);

        // Assert
        assert_eq!(transcript_output, None);
    }

    #[test]
    /// Ensures persisted summaries keep the raw turn/session payload for
    /// review-mode rendering.
    fn test_persisted_session_summary_payload_serializes_structured_summary() {
        // Arrange
        let response = AgentResponse {
            answer: "Implemented the fix.".to_string(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: "Updated the greeting flow.".to_string(),
                session: "Session now greets users on startup.".to_string(),
            }),
        };

        // Act
        let persisted_summary = persisted_session_summary_payload(&response);

        // Assert
        let summary = serde_json::from_str::<AgentResponseSummary>(&persisted_summary)
            .expect("summary should deserialize");

        assert_eq!(
            summary,
            AgentResponseSummary {
                session: "Session now greets users on startup.".to_string(),
                turn: "Updated the greeting flow.".to_string(),
            }
        );
    }

    #[tokio::test]
    /// Verifies non-output events do not append transcript content.
    async fn test_consume_turn_events_ignores_pid_only_events_for_transcript_output() {
        // Arrange
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::PidUpdate(Some(4242)))
            .expect("failed to send pid update");
        drop(event_tx);

        // Act
        consume_turn_events(
            event_rx,
            app_event_tx,
            "session-1".to_string(),
            Arc::clone(&child_pid),
        )
        .await;

        // Assert
        assert_eq!(*child_pid.lock().expect("pid lock poisoned"), Some(4242));
        assert!(app_event_rx.try_recv().is_err());
    }

    #[tokio::test]
    /// Verifies thought deltas update the loader state without appending
    /// transcript output.
    async fn test_consume_turn_events_routes_thought_delta_to_progress_state_only() {
        // Arrange
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        event_tx
            .send(TurnEvent::ThoughtDelta("Inspecting files".to_string()))
            .expect("failed to send thought delta");
        drop(event_tx);

        // Act
        consume_turn_events(event_rx, app_event_tx, "session-1".to_string(), child_pid).await;

        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            events,
            vec![
                AppEvent::SessionProgressUpdated {
                    progress_message: Some("Inspecting files".to_string()),
                    session_id: "session-1".to_string(),
                },
                AppEvent::SessionProgressUpdated {
                    progress_message: None,
                    session_id: "session-1".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    /// Verifies turn summaries are persisted to the database when the agent
    /// returns them.
    async fn test_apply_turn_result_persists_summary_to_database() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
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

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                follow_up_tasks: vec![
                    "Document the worker summary flow.".to_string(),
                    "Add a follow-up rendering test.".to_string(),
                ],
                summary: Some(AgentResponseSummary {
                    turn: "- Updated the worker flow.".to_string(),
                    session: "- Active review now reloads summary from persistence.".to_string(),
                }),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let status = apply_turn_result(&context, AgentModel::Gemini3FlashPreview, turn_result)
            .await
            .expect("turn result should succeed");
        let sessions = db.load_sessions().await.expect("failed to load sessions");

        // Assert
        assert_eq!(status, Status::Review);
        let summary = sessions[0].summary.as_deref().map(|raw| {
            serde_json::from_str::<AgentResponseSummary>(raw)
                .expect("stored summary should deserialize")
        });
        assert_eq!(
            summary,
            Some(AgentResponseSummary {
                session: "- Active review now reloads summary from persistence.".to_string(),
                turn: "- Updated the worker flow.".to_string(),
            })
        );
        let follow_up_tasks = db
            .load_session_follow_up_tasks()
            .await
            .expect("failed to load follow-up tasks");
        assert_eq!(
            follow_up_tasks
                .into_iter()
                .filter(|task| task.session_id == "sess1")
                .map(|task| task.text)
                .collect::<Vec<_>>(),
            vec![
                "Document the worker summary flow.".to_string(),
                "Add a follow-up rendering test.".to_string()
            ]
        );
        let output = context.output.lock().expect("output lock poisoned");
        assert!(output.contains("## Change Summary"));
        assert!(output.contains("### Current Turn"));
        assert!(output.contains("- Updated the worker flow."));
        assert!(output.contains("### Session Changes"));
        assert!(output.contains("- Active review now reloads summary from persistence."));
        assert!(!output.contains("Document the worker summary flow."));
    }

    #[tokio::test]
    /// Verifies persisted assistant text is separated from the appended
    /// summary block even when prior output ended without a trailing newline.
    async fn test_apply_turn_result_separates_summary_from_streamed_output() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
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

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let output = Arc::new(Mutex::new("Hey! How can I help you today?".to_string()));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db,
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::clone(&output),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Hey! How can I help you today?".to_string(),
                questions: Vec::new(),
                follow_up_tasks: Vec::new(),
                summary: Some(AgentResponseSummary {
                    turn: "No changes".to_string(),
                    session: "No changes".to_string(),
                }),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let status = apply_turn_result(&context, AgentModel::Gemini3FlashPreview, turn_result)
            .await
            .expect("turn result should succeed");
        let output = output.lock().expect("output lock poisoned");

        // Assert
        assert_eq!(status, Status::Review);
        assert!(output.contains("Hey! How can I help you today?\n\n## Change Summary"));
    }

    #[test]
    /// Formats a populated summary payload as a markdown transcript block.
    fn test_build_summary_transcript_output_returns_formatted_markdown() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: "- Added feature X.".to_string(),
                session: "- Feature X now live on branch.".to_string(),
            }),
        };

        // Act
        let output = build_summary_transcript_output(&assistant_message, "");

        // Assert
        assert_eq!(
            output,
            Some(
                "## Change Summary\n### Current Turn\n- Added feature X.\n\n### Session \
                 Changes\n- Feature X now live on branch.\n\n"
                    .to_string()
            )
        );
    }

    #[test]
    /// Returns `None` when the assistant response has no summary.
    fn test_build_summary_transcript_output_returns_none_without_summary() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: "Done.".to_string(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: None,
        };

        // Act
        let output = build_summary_transcript_output(&assistant_message, "");

        // Assert
        assert_eq!(output, None);
    }

    #[test]
    /// Falls back to "No changes" for both fields when the summary struct is
    /// present but empty.
    fn test_build_summary_transcript_output_falls_back_for_both_empty_fields() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: String::new(),
                session: String::new(),
            }),
        };

        // Act
        let output = build_summary_transcript_output(&assistant_message, "");

        // Assert
        let text = output.expect("should return some output when summary struct is present");
        assert!(text.contains("## Change Summary"));
        assert_eq!(text.matches("No changes").count(), 2);
    }

    #[test]
    /// Falls back to "No changes" for an empty `turn` field.
    fn test_build_summary_transcript_output_falls_back_for_empty_turn() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: String::new(),
                session: "- Branch has ongoing changes.".to_string(),
            }),
        };

        // Act
        let output = build_summary_transcript_output(&assistant_message, "");

        // Assert
        let text = output.expect("should return some output");
        assert!(text.contains("No changes"));
        assert!(text.contains("- Branch has ongoing changes."));
    }

    #[test]
    /// Falls back to "No changes" for an empty `session` field.
    fn test_build_summary_transcript_output_falls_back_for_empty_session() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: "- Fixed the bug.".to_string(),
                session: String::new(),
            }),
        };

        // Act
        let output = build_summary_transcript_output(&assistant_message, "");

        // Assert
        let text = output.expect("should return some output");
        assert!(text.contains("- Fixed the bug."));
        assert!(text.contains("No changes"));
    }

    #[test]
    /// Includes the requested leading spacer before the summary heading.
    fn test_build_summary_transcript_output_includes_requested_prefix() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            follow_up_tasks: Vec::new(),
            summary: Some(AgentResponseSummary {
                turn: "- Fixed the bug.".to_string(),
                session: "- Session summary stays readable.".to_string(),
            }),
        };

        // Act
        let output = build_summary_transcript_output(&assistant_message, "\n\n");

        // Assert
        assert_eq!(
            output,
            Some(
                "\n\n## Change Summary\n### Current Turn\n- Fixed the bug.\n\n### Session \
                 Changes\n- Session summary stays readable.\n\n"
                    .to_string()
            )
        );
    }

    #[test]
    /// Returns only the spacing needed to keep the summary block separated
    /// from existing transcript output.
    fn test_summary_transcript_prefix_matches_existing_spacing() {
        // Arrange
        let no_spacing_needed = Arc::new(Mutex::new("answer\n\n".to_string()));
        let single_newline = Arc::new(Mutex::new("answer\n".to_string()));
        let no_newline = Arc::new(Mutex::new("answer".to_string()));

        // Act
        let no_spacing_prefix = summary_transcript_prefix(&no_spacing_needed);
        let single_newline_prefix = summary_transcript_prefix(&single_newline);
        let no_newline_prefix = summary_transcript_prefix(&no_newline);

        // Assert
        assert_eq!(no_spacing_prefix, "");
        assert_eq!(single_newline_prefix, "\n");
        assert_eq!(no_newline_prefix, "\n\n");
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
        db.update_session_status_with_timing_at("sess1", "InProgress", 0)
            .await
            .expect("failed to open in-progress timing window");
        db.insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");

        // Act
        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(&db, 300).await;
        let sessions = db.load_sessions().await.expect("failed to load sessions");
        let operation_is_unfinished = db
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, "Review");
        assert_eq!(sessions[0].in_progress_started_at, None);
        assert_eq!(sessions[0].in_progress_total_seconds, 300);
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
            clock: Arc::new(crate::app::session::RealClock),
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
            clock: Arc::new(crate::app::session::RealClock),
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
