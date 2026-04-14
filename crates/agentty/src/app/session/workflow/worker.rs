//! Per-session async worker orchestration for serialized command execution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::SessionTaskService;
use crate::app::assist::AssistContext;
use crate::app::session::{
    Clock, SessionError, TurnAppliedState, remote_branch_name_from_upstream_ref,
    unix_timestamp_from_system_time,
};
use crate::app::{AppEvent, AppServices, SessionManager, branch_publish};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::session::{
    PublishedBranchSyncStatus, SessionFollowUpTask, SessionStats, Status,
};
use crate::domain::setting::SettingName;
use crate::infra::channel::{
    AgentChannel, AgentError, AgentRequestKind, TurnEvent, TurnPrompt, TurnRequest, TurnResult,
    create_agent_channel,
};
use crate::infra::db::{Database, SessionTurnMetadata};
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;
use crate::infra::{agent, process};

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
        /// Persisted published-upstream reference captured when the turn was
        /// queued, if this session already tracks a remote review branch.
        published_upstream_ref: Option<String>,
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
    /// Per-turn cancellation token shared with the UI through
    /// [`SessionHandles`]. The worker swaps in a fresh token at the start
    /// of each turn; the UI calls `cancel()` on the current token to
    /// interrupt a running turn.
    cancel_token: Arc<Mutex<CancellationToken>>,
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

/// Applies one successful turn result to persistence and returns the
/// corresponding reducer projection.
struct TurnPersistence<'a> {
    context: &'a SessionWorkerContext,
    session_model: AgentModel,
}

/// Runtime snapshot required to create or reuse one session worker.
pub(super) struct SessionWorkerRuntime {
    cancel_token: Arc<Mutex<CancellationToken>>,
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
            cancel_token: Arc::clone(&runtime.cancel_token),
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
            published_upstream_ref,
            request_kind,
            prompt,
            session_model,
            ..
        } = command;

        Self::run_channel_turn(
            context,
            published_upstream_ref,
            request_kind,
            prompt,
            session_model,
        )
        .await
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
    ///
    /// A fresh [`CancellationToken`] is swapped into the shared mutex at
    /// the top of this function so stale cancellations from previous
    /// turns cannot affect new work. A `Ctrl+c` arriving during setup
    /// cancels the new token, which is detected by the early-exit check
    /// in [`run_turn_with_cancellation`].
    async fn run_channel_turn(
        context: &SessionWorkerContext,
        published_upstream_ref: Option<String>,
        request_kind: AgentRequestKind,
        prompt: TurnPrompt,
        session_model: AgentModel,
    ) -> Result<(), SessionError> {
        // Swap in a fresh token so stale cancellations from previous
        // turns are discarded. The cloned token is passed to
        // `run_turn_with_cancellation` for the duration of this turn.
        let turn_cancel_token = {
            let mut guard = context
                .cancel_token
                .lock()
                .expect("cancel token lock poisoned");
            *guard = CancellationToken::new();
            guard.clone()
        };

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
        let reasoning_level =
            load_session_reasoning_level(&context.db, &context.session_id, session_project_id)
                .await;
        let provider_conversation_id = context
            .db
            .get_session_provider_conversation_id(&context.session_id)
            .await
            .ok()
            .flatten();
        let persisted_instruction_conversation_id = context
            .db
            .get_session_instruction_conversation_id(&context.session_id)
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
            persisted_instruction_conversation_id,
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

        let turn_result =
            run_turn_with_cancellation(context, turn_cancel_token, req, event_tx).await;
        SessionManager::cleanup_prompt_attachment_paths(
            context.fs_client.clone(),
            prompt.local_image_paths().cloned().collect(),
        )
        .await;

        let _ = consumer.await;

        let result =
            apply_turn_result(context, published_upstream_ref, session_model, turn_result).await;

        if let Some((session_size, added_lines, deleted_lines)) =
            SessionTaskService::refresh_persisted_session_diff_stats(
                &context.db,
                context.git_client.as_ref(),
                &context.session_id,
                &context.folder,
            )
            .await
        {
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = context.app_event_tx.send(AppEvent::SessionSizeUpdated {
                added_lines,
                deleted_lines,
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
            .is_cancel_requested_for_operation(operation_id)
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
            cancel_token: Arc::clone(&handles.cancel_token),
            child_pid: Arc::clone(&handles.child_pid),
            folder: session.folder.clone(),
            output: Arc::clone(&handles.output),
            session_id: session.id.clone(),
            session_model: session.model,
            status: Arc::clone(&handles.status),
        })
    }
}

impl TurnPersistence<'_> {
    /// Persists one completed turn and returns the reducer projection derived
    /// from the canonical stored values.
    async fn apply(
        &self,
        assistant_message: &agent::AgentResponse,
        input_tokens: u64,
        output_tokens: u64,
        provider_conversation_id: Option<&str>,
    ) -> Result<TurnAppliedState, SessionError> {
        let summary = persisted_session_summary_payload(assistant_message);
        let questions = assistant_message.question_items();
        let questions_json = if questions.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&questions).unwrap_or_default()
        };
        let follow_up_tasks = turn_applied_follow_up_tasks(assistant_message);
        let persisted_follow_up_text = follow_up_tasks
            .iter()
            .map(|follow_up_task| follow_up_task.text.clone())
            .collect::<Vec<_>>();
        let token_usage_delta = SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            input_tokens,
            output_tokens,
        };
        let instruction_conversation_id =
            if agent::transport_mode(self.session_model.kind()).uses_app_server() {
                agent::normalize_instruction_conversation_id(provider_conversation_id)
            } else {
                None
            };
        self.context
            .db
            .persist_session_turn_metadata(
                &self.context.session_id,
                &SessionTurnMetadata {
                    follow_up_tasks: &persisted_follow_up_text,
                    instruction_conversation_id: instruction_conversation_id.as_deref(),
                    model: self.session_model.as_str(),
                    provider_conversation_id,
                    questions_json: &questions_json,
                    summary: &summary,
                    token_usage_delta: &token_usage_delta,
                },
            )
            .await?;

        Ok(TurnAppliedState {
            follow_up_tasks,
            questions,
            summary: (!summary.is_empty()).then_some(summary),
            token_usage_delta,
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
/// Runs one agent turn with cancellation support.
///
/// Races `run_turn` against the per-turn [`CancellationToken`]. When the
/// token is cancelled (`Ctrl+c`), `SIGTERM` is sent to the active child
/// process (if any) via [`terminate_child_process`], the channel is shut
/// down gracefully through `shutdown_session`, and the function waits for
/// the `run_turn` future to resolve (with a timeout) so the subprocess is
/// not orphaned. Sending `SIGTERM` from inside the cancellation branch
/// (rather than from the UI) eliminates the stale-PID / PID-reuse risk:
/// `run_turn` has not returned yet, so the PID slot still belongs to the
/// active child.
///
/// Each turn receives its own fresh token, created at the start of
/// [`run_channel_turn`]. This eliminates the stale-permit problem that
/// required the previous `Notify` + `AtomicBool` flag-check pattern.
async fn run_turn_with_cancellation(
    context: &SessionWorkerContext,
    cancel_token: CancellationToken,
    req: TurnRequest,
    event_tx: mpsc::UnboundedSender<TurnEvent>,
) -> Result<TurnResult, AgentError> {
    // Honour a cancel that arrived during pre-turn setup, before the
    // select had a chance to observe it. The token was freshly created
    // at the top of `run_channel_turn`, so a cancelled state here is a
    // real `Ctrl+c`, not a stale leftover.
    if cancel_token.is_cancelled() {
        terminate_child_process(context);
        let _ = context
            .channel
            .shutdown_session(context.session_id.clone())
            .await;

        return Err(AgentError::Backend(
            "[Stopped] Session interrupted by user.".to_string(),
        ));
    }

    let turn_future = context
        .channel
        .run_turn(context.session_id.clone(), req, event_tx);
    tokio::pin!(turn_future);

    tokio::select! {
        result = &mut turn_future => result,
        () = cancel_token.cancelled() => {
            // Send SIGTERM to the child process while it is guaranteed
            // alive (run_turn has not returned yet). This is safe from
            // PID-reuse because the PID slot is only cleared after
            // run_turn completes. App-server channels ignore the signal
            // because their PID slot is always None.
            terminate_child_process(context);

            // Graceful shutdown: close stdin, wait for exit, kill if
            // needed.
            let _ = context
                .channel
                .shutdown_session(context.session_id.clone())
                .await;

            // Wait for the turn future to resolve so the subprocess is
            // not orphaned. CLI channels return a signal-killed error
            // once the child exits; app-server channels complete once
            // their runtime stops. A timeout guards against indefinite
            // blocking if the channel does not shut down promptly.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                &mut turn_future,
            )
            .await;

            Err(AgentError::Backend(
                "[Stopped] Session interrupted by user.".to_string(),
            ))
        }
    }
}

/// Sends `SIGTERM` to the active child process tracked in
/// `context.child_pid`, if any.
///
/// Best-effort: the PID slot may be `None` (app-server channels never
/// publish a PID) or the process may have already exited. Both cases are
/// silently ignored.
fn terminate_child_process(context: &SessionWorkerContext) {
    let active_pid = context
        .child_pid
        .lock()
        .expect("child_pid lock poisoned")
        .take();

    if let Some(pid) = active_pid {
        process::send_terminate_signal(pid);
    }
}

/// joined question text so clarification prompts remain visible while
/// thought-only responses are not persisted as final transcript output.
///
/// The raw agent `summary` payload is stored only in the session row. The
/// reducer receives a matching [`TurnAppliedState`] projection so the active UI
/// can render the same summary and follow-up metadata without embedding a
/// second markdown copy into `session.output`. If canonical metadata
/// persistence fails, the worker appends a recovery error, triggers
/// `RefreshSessions`, and skips reducer projection emission.
async fn apply_turn_result(
    context: &SessionWorkerContext,
    published_upstream_ref: Option<String>,
    session_model: AgentModel,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<Status, SessionError> {
    match turn_result {
        Ok(result) => {
            apply_successful_turn_result(context, published_upstream_ref, session_model, result)
                .await
        }
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

/// Persists the successful turn payload, emits the reducer projection, and
/// runs the auto-commit workflow with the project's fast-model default before
/// returning the next session status.
async fn apply_successful_turn_result(
    context: &SessionWorkerContext,
    published_upstream_ref: Option<String>,
    session_model: AgentModel,
    result: TurnResult,
) -> Result<Status, SessionError> {
    let TurnResult {
        assistant_message,
        context_reset: _,
        input_tokens,
        output_tokens,
        provider_conversation_id,
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
    let turn_applied_state = match (TurnPersistence {
        context,
        session_model,
    }
    .apply(
        &assistant_message,
        input_tokens,
        output_tokens,
        provider_conversation_id.as_deref(),
    )
    .await)
    {
        Ok(turn_applied_state) => turn_applied_state,
        Err(error) => {
            handle_turn_persistence_failure(context, &error).await;

            return Err(error);
        }
    };
    let target_status = if turn_applied_state.questions.is_empty() {
        Status::Review
    } else {
        Status::Question
    };
    // Fire-and-forget: receiver may be dropped during shutdown.
    let _ = context.app_event_tx.send(AppEvent::AgentResponseReceived {
        session_id: context.session_id.clone(),
        turn_applied_state,
    });
    let auto_commit_model = SessionTaskService::load_auto_commit_model_setting(
        &context.db,
        &context.session_id,
        session_model,
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
        session_model: auto_commit_model,
    })
    .await;
    start_published_branch_auto_push(context, published_upstream_ref);

    Ok(target_status)
}

/// Starts one detached auto-push task for a session that already tracks a
/// published upstream branch.
fn start_published_branch_auto_push(
    context: &SessionWorkerContext,
    published_upstream_ref: Option<String>,
) {
    let Some(published_upstream_ref) = published_upstream_ref else {
        return;
    };

    let sync_operation_id = Uuid::new_v4().to_string();
    let session_id = context.session_id.clone();
    let app_event_tx = context.app_event_tx.clone();
    let db = context.db.clone();
    let folder = context.folder.clone();
    let git_client = Arc::clone(&context.git_client);
    let output = Arc::clone(&context.output);

    let _ = app_event_tx.send(AppEvent::PublishedBranchSyncUpdated {
        session_id: session_id.clone(),
        sync_operation_id: sync_operation_id.clone(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    });

    tokio::spawn(async move {
        run_published_branch_auto_push(
            app_event_tx,
            db,
            folder,
            git_client,
            output,
            session_id,
            sync_operation_id,
            published_upstream_ref,
        )
        .await;
    });
}

/// Runs one detached auto-push for a previously published session branch and
/// reports its state through the app event pipeline.
async fn run_published_branch_auto_push(
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    db: Database,
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    output: Arc<Mutex<String>>,
    session_id: String,
    sync_operation_id: String,
    published_upstream_ref: String,
) {
    let remote_branch_name = remote_branch_name_from_upstream_ref(&published_upstream_ref);
    let push_result = branch_publish::push_session_branch_to_remote(
        &db,
        folder,
        git_client,
        &session_id,
        Some(remote_branch_name.as_str()),
    )
    .await;

    match push_result {
        Ok(_) => {
            let _ = app_event_tx.send(AppEvent::PublishedBranchSyncUpdated {
                session_id,
                sync_operation_id,
                sync_status: PublishedBranchSyncStatus::Idle,
            });
        }
        Err(failure) => {
            let failure = branch_publish::branch_push_failure(
                crate::domain::session::PublishBranchAction::Push,
                &failure.message,
            );
            let message = format!("\n[Branch Push Error] {}\n", failure.message);
            SessionTaskService::append_session_output(
                &output,
                &db,
                &app_event_tx,
                &session_id,
                &message,
            )
            .await;

            let _ = app_event_tx.send(AppEvent::PublishedBranchSyncUpdated {
                session_id,
                sync_operation_id,
                sync_status: PublishedBranchSyncStatus::Failed,
            });
        }
    }
}

/// Reconciles a failed turn-metadata write by surfacing the error and forcing
/// the next UI reload to prefer durable state.
async fn handle_turn_persistence_failure(context: &SessionWorkerContext, error: &SessionError) {
    let message =
        format!("\n[Turn Metadata Error] Failed to persist completed turn metadata: {error}\n");
    SessionTaskService::append_session_output(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.session_id,
        &message,
    )
    .await;

    let _ = context.app_event_tx.send(AppEvent::RefreshSessions);
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

    let _title_generation_task = SessionManager::spawn_session_title_generation_task(
        context.app_event_tx.clone(),
        context.db.clone(),
        &context.session_id,
        &context.folder,
        prompt,
        title_model,
        None,
    );
}

/// Loads the project identifier associated with one persisted session.
async fn load_session_project_id(db: &Database, session_id: &str) -> Option<i64> {
    db.load_session_project_id(session_id).await.ok().flatten()
}

/// Loads the effective reasoning level for one session context.
async fn load_session_reasoning_level(
    db: &Database,
    session_id: &str,
    project_id: Option<i64>,
) -> ReasoningLevel {
    if let Ok(Some(reasoning_level)) = db.load_session_reasoning_level_override(session_id).await {
        return reasoning_level;
    }

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

    db.get_project_setting(project_id, setting_name)
        .await
        .ok()
        .flatten()
        .and_then(|setting_value| AgentModel::from_str(&setting_value).ok())
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

/// Builds the reducer-facing follow-up-task projection for one assistant
/// response.
fn turn_applied_follow_up_tasks(
    assistant_message: &agent::AgentResponse,
) -> Vec<SessionFollowUpTask> {
    assistant_message
        .follow_up_task_items()
        .into_iter()
        .enumerate()
        .map(|(index, text)| SessionFollowUpTask {
            id: i64::try_from(index).unwrap_or(i64::MAX),
            launched_session_id: None,
            position: index,
            text,
        })
        .collect()
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
            published_upstream_ref: None,
            request_kind: AgentRequestKind::SessionStart,
            prompt: "prompt".into(),
            session_model: AgentModel::ClaudeSonnet46,
        };
        let resume_command = SessionCommand::Run {
            operation_id: "op-resume".to_string(),
            published_upstream_ref: None,
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
    /// Verifies the worker's `select!` cancellation path gracefully stops a
    /// running turn through `shutdown_session` and returns the `[Stopped]`
    /// error text when the cancel token is cancelled during `run_channel_turn`.
    async fn test_run_channel_turn_returns_stopped_when_cancel_token_fires() {
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

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    // Simulate a long-running app-server turn that never
                    // completes on its own.
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    unreachable!("should be cancelled before completing")
                })
            });
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));

        let cancel_token = Arc::new(Mutex::new(CancellationToken::new()));
        let output = Arc::new(Mutex::new(String::new()));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::clone(&cancel_token),
            channel: Arc::new(mock_channel),
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

        // Cancel the token shortly after the turn starts.
        let token_handle = Arc::clone(&cancel_token);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token_handle.lock().expect("cancel token lock").cancel();
        });

        // Act
        let result = SessionWorkerService::run_channel_turn(
            &context,
            None,
            AgentRequestKind::SessionStart,
            "test prompt".into(),
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
        let output_text = output.lock().expect("output lock").clone();
        assert!(
            output_text.contains("[Stopped]"),
            "stopped message should be appended to output, got: {output_text}"
        );
    }

    #[tokio::test]
    /// Verifies that a previous turn's cancelled token does not affect the
    /// next turn. Each turn swaps in a fresh `CancellationToken`, so stale
    /// cancellations are structurally impossible.
    async fn test_run_channel_turn_proceeds_after_previous_cancellation() {
        // Arrange — pre-cancel the token to simulate a previous turn's
        // cancellation. `run_channel_turn` swaps in a fresh token so the
        // stale cancellation is discarded.
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

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            follow_up_tasks: Vec::new(),
                            questions: Vec::new(),
                            summary: None,
                        },
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        // Pre-cancel the token to simulate a previous turn's cancellation.
        let stale_token = CancellationToken::new();
        stale_token.cancel();

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(stale_token)),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db,
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act — the turn should complete normally because
        // `run_channel_turn` swaps in a fresh token.
        let result = SessionWorkerService::run_channel_turn(
            &context,
            None,
            AgentRequestKind::SessionStart,
            "test prompt".into(),
            AgentModel::Gemini3FlashPreview,
        )
        .await;

        // Assert — turn succeeded despite the stale cancellation.
        assert!(
            result.is_ok(),
            "stale cancelled token should not cancel the new turn"
        );
    }

    #[tokio::test]
    /// Verifies that a cancel arriving during the pre-turn setup window
    /// (between the token swap in `run_channel_turn` and the entry into
    /// `run_turn_with_cancellation`) is honoured immediately. The token is
    /// already cancelled before `run_turn_with_cancellation` starts, so
    /// `run_turn` must never be called.
    async fn test_run_turn_with_cancellation_honours_pre_turn_cancel() {
        // Arrange — create a pre-cancelled token, simulating a Ctrl+c
        // that arrived during pre-turn setup.
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();

        let mut mock_channel = MockAgentChannel::new();
        // `run_turn` must NOT be called — the early-exit path returns
        // before reaching the select.
        mock_channel.expect_run_turn().never();
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db: Database::open_in_memory().await.expect("failed to open db"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess-preturn".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        let req = TurnRequest {
            folder: context.folder.clone(),
            live_session_output: None,
            model: "gemini-3-flash-preview".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "test".into(),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
        };

        // Act — pass the pre-cancelled token directly.
        let result =
            run_turn_with_cancellation(&context, cancel_token, req, mpsc::unbounded_channel().0)
                .await;

        // Assert — should return [Stopped] without ever calling run_turn.
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
    }

    #[tokio::test]
    /// Verifies that `run_turn_with_cancellation` returns `[Stopped]` even
    /// when `run_turn` does not resolve after `shutdown_session`. The
    /// 5-second timeout guard ensures the cancellation branch does not
    /// block indefinitely.
    async fn test_run_turn_with_cancellation_returns_stopped_after_drain_timeout() {
        // Arrange — mock channel whose `run_turn` never resolves and
        // whose `shutdown_session` completes immediately (simulating a
        // channel that ignores the shutdown request).
        let cancel_token = CancellationToken::new();

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    // Never resolves — simulates a stuck channel.
                    std::future::pending::<Result<TurnResult, AgentError>>().await
                })
            });
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db: Database::open_in_memory().await.expect("failed to open db"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess-timeout".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        let req = TurnRequest {
            folder: context.folder.clone(),
            live_session_output: None,
            model: "gemini-3-flash-preview".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            prompt: "test".into(),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
        };

        // Spawn a task that cancels the token after a small delay so the
        // select branch fires mid-turn (not before the pre-check).
        let token_for_cancel = cancel_token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token_for_cancel.cancel();
        });

        // Act — the drain timeout (5 seconds) runs with real wall-clock
        // delay. This test validates that the function does not block
        // indefinitely when `run_turn` never resolves.
        let result =
            run_turn_with_cancellation(&context, cancel_token, req, mpsc::unbounded_channel().0)
                .await;

        // Assert — returns [Stopped] despite `run_turn` never resolving.
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
    }

    #[tokio::test]
    /// Verifies that `terminate_child_process` sends `SIGTERM` to the
    /// child process tracked in the context's PID slot, killing it.
    async fn test_terminate_child_process_sends_sigterm_to_active_child() {
        // Arrange — spawn a long-running child and store its PID in the
        // context.
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep");
        let child_pid = child.id().expect("child has no pid");

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(Some(child_pid))),
            clock: Arc::new(crate::app::session::RealClock),
            db: Database::open_in_memory().await.expect("failed to open db"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess-term".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        terminate_child_process(&context);

        // Assert — the child should have been terminated by SIGTERM.
        let exit_status = child.wait().await.expect("failed to wait on child");
        assert!(
            !exit_status.success(),
            "child should have been killed by SIGTERM"
        );
        // PID slot should be cleared after termination.
        assert!(
            context.child_pid.lock().expect("child_pid lock").is_none(),
            "PID slot should be cleared after termination"
        );
    }

    #[tokio::test]
    /// Verifies that `terminate_child_process` is a no-op when no child
    /// PID is stored (app-server channels never set a PID).
    async fn test_terminate_child_process_noop_when_no_pid() {
        // Arrange
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db: Database::open_in_memory().await.expect("failed to open db"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess-nopid".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act — should not panic or error.
        terminate_child_process(&context);

        // Assert — PID slot remains None.
        assert!(
            context.child_pid.lock().expect("child_pid lock").is_none(),
            "PID slot should still be None"
        );
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
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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
        let status =
            apply_turn_result(&context, None, AgentModel::Gemini3FlashPreview, turn_result)
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
        assert!(output.starts_with("Implemented the change.\n\n"));
        assert!(output.contains("[Commit] No changes to commit."));
        assert!(!output.contains("## Change Summary"));
        assert!(!output.contains("Document the worker summary flow."));
    }

    #[tokio::test]
    /// Verifies completed turns auto-push already-published session branches
    /// in the background and report sync progress through app events.
    async fn test_apply_turn_result_starts_background_push_for_published_branch() {
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
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "agentty/session-id"
            })
            .returning(|_, _| Box::pin(async { Ok("origin/agentty/session-id".to_string()) }));
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
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
                follow_up_tasks: Vec::new(),
                summary: None,
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let status = apply_turn_result(
            &context,
            Some("origin/agentty/session-id".to_string()),
            AgentModel::Gemini3FlashPreview,
            turn_result,
        )
        .await
        .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated {
                    session_id,
                    sync_operation_id,
                    sync_status,
                } = event
                {
                    sync_events.push((session_id, sync_operation_id, sync_status));
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
        assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Idle);
        assert_eq!(sync_events[0].0, "sess1");
        assert_eq!(sync_events[1].0, "sess1");
        assert_eq!(sync_events[0].1, sync_events[1].1);
    }

    #[tokio::test]
    /// Verifies failed background auto-push attempts append a visible error
    /// and keep the session marked as failed for the latest sync attempt.
    async fn test_apply_turn_result_reports_background_push_failures() {
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
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .returning(|_, _| {
                Box::pin(async {
                    Err(crate::infra::git::GitError::CommandFailed {
                        command: "git push origin agentty/session-id".to_string(),
                        stderr:
                            "fatal: could not read username for 'https://github.com/openai/agentty': terminal prompts disabled"
                                .to_string(),
                    })
                })
            });
        let output = Arc::new(Mutex::new(String::new()));
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db,
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            output: Arc::clone(&output),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                follow_up_tasks: Vec::new(),
                summary: None,
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let status = apply_turn_result(
            &context,
            Some("origin/agentty/session-id".to_string()),
            AgentModel::Gemini3FlashPreview,
            turn_result,
        )
        .await
        .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated { sync_status, .. } = event {
                    sync_events.push(sync_status);
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");
        let output = output.lock().expect("output lock poisoned");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            sync_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Failed,
            ]
        );
        assert!(output.contains("[Branch Push Error]"));
        assert!(output.contains("gh auth login"));
    }

    #[tokio::test]
    /// Verifies failed turn-metadata persistence forces a refresh and skips
    /// reducer projection emission.
    async fn test_apply_turn_result_refreshes_when_turn_metadata_persistence_fails() {
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
        db.delete_session("sess1")
            .await
            .expect("failed to delete session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::app::session::RealClock),
            db,
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            output: Arc::new(Mutex::new(String::new())),
            session_id: "sess1".to_string(),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                follow_up_tasks: vec!["Document the failure path.".to_string()],
                summary: Some(AgentResponseSummary {
                    turn: "- Attempted the update.".to_string(),
                    session: "- Session state should not project without persistence.".to_string(),
                }),
            },
            context_reset: false,
            input_tokens: 2,
            output_tokens: 3,
            provider_conversation_id: None,
        });

        // Act
        let error = apply_turn_result(&context, None, AgentModel::Gemini3FlashPreview, turn_result)
            .await
            .expect_err("turn result should fail when metadata persistence fails");
        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
        let output = context.output.lock().expect("output lock poisoned");

        // Assert
        assert!(
            error
                .to_string()
                .contains("no rows returned by a query that expected to return at least one row")
        );
        assert!(output.contains("Implemented the change."));
        assert!(
            output.contains("[Turn Metadata Error] Failed to persist completed turn metadata:")
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AppEvent::RefreshSessions))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AppEvent::AgentResponseReceived { .. }))
        );
    }

    #[tokio::test]
    /// Verifies persisted assistant text stays unchanged when summaries are
    /// stored only in structured session metadata.
    async fn test_apply_turn_result_keeps_summary_out_of_transcript_output() {
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
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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
        let status =
            apply_turn_result(&context, None, AgentModel::Gemini3FlashPreview, turn_result)
                .await
                .expect("turn result should succeed");
        let output = output.lock().expect("output lock poisoned");

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            output.starts_with("Hey! How can I help you today?Hey! How can I help you today?\n\n")
        );
        assert!(output.contains("[Commit] No changes to commit."));
        assert!(!output.contains("## Change Summary"));
    }

    #[tokio::test]
    /// Persists the current app-server instruction bootstrap marker after a
    /// successful turn so later follow-ups can reuse the compact reminder.
    async fn test_apply_turn_result_persists_instruction_conversation_id_for_app_server_turns() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = Database::open_in_memory().await.expect("failed to open db");
        let project_id = db
            .upsert_project("/tmp/project", Some("main"))
            .await
            .expect("failed to upsert project");
        db.insert_session("sess1", "gpt-5.4", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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
                follow_up_tasks: Vec::new(),
                summary: None,
            },
            context_reset: true,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: Some("thread-123".to_string()),
        });

        // Act
        let status = apply_turn_result(&context, None, AgentModel::Gpt54, turn_result)
            .await
            .expect("turn result should succeed");
        let instruction_conversation_id = db
            .get_session_instruction_conversation_id("sess1")
            .await
            .expect("failed to load instruction conversation id");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            instruction_conversation_id,
            agent::normalize_instruction_conversation_id(Some("thread-123"))
        );
    }

    #[test]
    /// Builds reducer-facing follow-up tasks with stable UI positions and
    /// placeholder in-memory identifiers.
    fn test_turn_applied_follow_up_tasks_preserve_order() {
        // Arrange
        let assistant_message = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            follow_up_tasks: vec![
                "Document the flow.".to_string(),
                "Add a regression test.".to_string(),
            ],
            summary: None,
        };

        // Act
        let follow_up_tasks = turn_applied_follow_up_tasks(&assistant_message);

        // Assert
        assert_eq!(
            follow_up_tasks
                .iter()
                .map(|follow_up_task| (follow_up_task.position, follow_up_task.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "Document the flow."), (1, "Add a regression test.")]
        );
        assert!(
            follow_up_tasks
                .iter()
                .all(|follow_up_task| follow_up_task.launched_session_id.is_none())
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
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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

    #[tokio::test]
    /// Verifies a new operation created after a session-level cancel request
    /// is not skipped. The operation-scoped check ensures stale cancel flags
    /// on older operations do not block newly enqueued work.
    async fn test_should_skip_worker_command_allows_new_operation_after_cancel() {
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

        // Old operation that gets cancelled.
        db.insert_session_operation("op-old", "sess1", "reply")
            .await
            .expect("failed to insert old operation");
        db.mark_session_operation_running("op-old")
            .await
            .expect("failed to mark old operation running");
        db.request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");

        // New operation created after the cancel request — its
        // `cancel_requested` defaults to 0.
        db.insert_session_operation("op-new", "sess1", "reply")
            .await
            .expect("failed to insert new operation");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
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

        // Act — the new operation should proceed despite the old
        // cancelled operation still being in 'running' state.
        let should_skip =
            SessionWorkerService::should_skip_worker_command(&context, "op-new").await;

        // Assert
        assert!(
            !should_skip,
            "new operation should not be skipped by stale cancel on older operation"
        );
    }
}
