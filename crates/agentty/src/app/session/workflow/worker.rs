//! Per-session async worker orchestration for serialized command execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent as agent;
use ag_agent::{
    AgentChannel, AgentError, AgentRequestKind, OneShotClient, TurnContinuation, TurnEvent,
    TurnRequest, TurnResult, create_agent_channel,
};
use ag_forge as forge;
use ag_git::GitClient;
use ag_protocol::AgentResponse;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::merge::{
    ExistingSessionRebaseAssistClient, RebaseAssistFuture, RebaseAssistMode, RebaseCommandInput,
};
#[cfg(test)]
use super::published_branch;
use super::task::SessionTranscriptMessageAppend;
use super::{SessionTaskService, isolation, session_folder, turn};
use crate::app::branch_publish::{
    BranchPublishTaskContext, BranchPublishTaskSession, review_request_from_publish_result,
    run_branch_publish_action,
};
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError, unix_timestamp_from_system_time};
use crate::app::{AppEvent, AppServices, SessionManager};
use crate::domain::agent::AgentSelection;
use crate::domain::session::{
    PublishBranchAction, QueuedMessage, ReviewRequest, SessionId, SessionStats, Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{AppRepositories, OperationRepository, SessionOperationRow};
use crate::infra::fs::FsClient;
use crate::infra::personality::PersonalityCatalogClient;

const RESTART_FAILURE_REASON: &str = "Interrupted by app restart";
const CANCEL_BEFORE_EXECUTION_REASON: &str = "Session canceled before execution";
const CREATE_REVIEW_REQUEST_OPERATION_KIND: &str = "create_review_request";
const REBASE_OPERATION_KIND: &str = "rebase";
const SKIPPED_CREATE_REVIEW_REQUEST_REASON: &str =
    "Review-request creation was canceled or already finished before execution";

/// Shared completion slot used by programmatic review-request callers.
///
/// The runtime retains a clone while handing the command to the worker so an
/// enqueue failure can still answer the caller instead of dropping the
/// response channel with an ambiguous runtime-unavailable error.
pub(super) type ReviewRequestResponse =
    Arc<Mutex<Option<oneshot::Sender<Result<ReviewRequest, ag_session::SessionError>>>>>;

/// Per-turn data captured at enqueue time that travels alongside the channel
/// turn but is consumed only after turn completion.
///
/// Groups per-turn state that would otherwise be threaded as individual
/// parameters through the `run_channel_turn` → `apply_turn_result` →
/// `apply_successful_turn_result` call chain. Future per-turn data (retry
/// policies, model overrides, etc.) should be added here instead of widening
/// every intermediate signature.
pub(super) struct TurnMetadata {
    /// Published-upstream reference captured when the turn was queued,
    /// consumed after turn completion by the auto-push workflow.
    pub(super) published_upstream_ref: Option<String>,
    /// Forge review-thread identifiers explicitly targeted by this turn.
    pub(super) review_comment_thread_ids: Vec<String>,
    /// Agent provider and model selected for the session when the turn was
    /// queued.
    pub(super) session_agent: AgentSelection,
}

/// Single command variant serialized per session worker.
///
/// Replaces the previous four-variant enum (`Reply`, `ReplyAppServer`,
/// `StartPrompt`, `StartPromptAppServer`) with a single provider-agnostic
/// variant. The underlying channel adapter handles transport-specific details.
pub(super) enum SessionCommand {
    /// Publishes the session branch and creates or refreshes its forge review
    /// request after earlier work on this worker has completed.
    CreateReviewRequest {
        /// Session snapshot captured when the action was accepted.
        branch_publish_session: BranchPublishTaskSession,
        /// Persisted operation identifier.
        operation_id: String,
        /// Optional user-selected remote branch name.
        remote_branch_name: Option<String>,
        /// Optional programmatic caller waiting for the resulting review
        /// request.
        response: Option<ReviewRequestResponse>,
    },
    /// Runs the session branch rebase workflow through this worker so
    /// conflict-resolution prompts reuse the active provider conversation.
    Rebase {
        /// Stored base branch used to resolve the concrete rebase target.
        base_branch: String,
        /// Persisted operation identifier.
        operation_id: String,
    },
    /// Executes one agent turn with the given request kind and prompt.
    Run {
        /// Optional draft materialization executed before the first turn.
        preparation: Option<Box<super::lifecycle::SessionWorktreePreparation>>,
        /// Persisted operation identifier.
        operation_id: String,
        /// Whether this is a first-message start or a follow-up resume.
        request_kind: AgentRequestKind,
        /// Replayable transcript text captured when this turn was queued.
        replay_transcript: Option<String>,
        /// Structured user prompt payload.
        prompt: TurnPrompt,
        /// Per-turn metadata consumed during and after turn execution.
        turn_metadata: TurnMetadata,
    },
}

impl SessionCommand {
    /// Returns the persisted operation identifier for this command.
    fn operation_id(&self) -> &str {
        match self {
            Self::CreateReviewRequest { operation_id, .. }
            | Self::Rebase { operation_id, .. }
            | Self::Run { operation_id, .. } => operation_id,
        }
    }

    /// Returns the operation kind persisted in the operations table.
    fn kind(&self) -> &'static str {
        match self {
            Self::CreateReviewRequest { .. } => CREATE_REVIEW_REQUEST_OPERATION_KIND,
            Self::Rebase { .. } => REBASE_OPERATION_KIND,
            Self::Run {
                request_kind: AgentRequestKind::SessionStart,
                ..
            } => "start_prompt",
            Self::Run {
                request_kind: AgentRequestKind::SessionResume,
                ..
            } => "reply",
            Self::Run {
                request_kind: AgentRequestKind::FocusedReview,
                ..
            } => "focused_review",
            Self::Run {
                request_kind: AgentRequestKind::UtilityPrompt,
                ..
            } => "utility_prompt",
            Self::Run {
                request_kind: AgentRequestKind::AccountRead,
                ..
            } => "account_read",
        }
    }
}

/// Worker command paired with its shared queue order when it was submitted
/// behind active work.
struct ScheduledSessionCommand {
    command: SessionCommand,
    queued_order: Option<u64>,
}

impl ScheduledSessionCommand {
    /// Returns whether this command can make a questioned session runnable.
    fn can_run_while_question(&self) -> bool {
        self.queued_order.is_none() || matches!(self.command, SessionCommand::Run { .. })
    }

    /// Wraps a command that is ready to run without joining the visible queue.
    fn immediate(command: SessionCommand) -> Self {
        Self {
            command,
            queued_order: None,
        }
    }

    /// Wraps a command queued at the supplied shared submission order.
    fn queued(command: SessionCommand, queued_order: u64) -> Self {
        Self {
            command,
            queued_order: Some(queued_order),
        }
    }
}

/// Sender and shared ordering source owned by one active session worker.
#[derive(Clone)]
struct SessionWorkerHandle {
    queued_work_sequence: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<ScheduledSessionCommand>,
    wakeup: Arc<Notify>,
}

impl SessionWorkerHandle {
    /// Reserves the next submission order shared with queued chat messages.
    fn next_queued_work_order(&self) -> u64 {
        self.queued_work_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Wakes the worker so buffered work is reconsidered after an external
    /// status transition.
    fn wake(&self) {
        self.wakeup.notify_one();
    }
}

/// Next unit selected from the combined action and chat queues.
enum ScheduledSessionWork {
    Command(Box<ScheduledSessionCommand>),
    Message(QueuedMessage),
}

/// Returns whether the session has a queued or running rebase operation.
pub(super) fn has_unfinished_rebase_operation(
    operations: &[SessionOperationRow],
    session_id: &str,
) -> bool {
    operations.iter().any(|operation| {
        operation.session_id == session_id && operation.kind == REBASE_OPERATION_KIND
    })
}

/// Returns whether the session has a queued or running branch operation that
/// must run before automatic post-turn publishing.
pub(super) fn has_unfinished_branch_operation(
    operations: &[SessionOperationRow],
    session_id: &str,
) -> bool {
    operations.iter().any(|operation| {
        operation.session_id == session_id
            && matches!(
                operation.kind.as_str(),
                CREATE_REVIEW_REQUEST_OPERATION_KIND | REBASE_OPERATION_KIND
            )
    })
}

/// Shared state threaded through all worker turn executions.
pub(super) struct SessionWorkerContext {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Serializes post-turn publish ownership with queued branch operations.
    pub(super) branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Per-turn cancellation token shared with the UI through
    /// [`SessionHandles`]. The worker renews it before command preflight;
    /// the UI calls `cancel()` on the current token to
    /// interrupt a running turn.
    pub(super) cancel_token: Arc<Mutex<CancellationToken>>,
    /// Provider-agnostic agent channel for this session's worker.
    pub(super) channel: Arc<dyn AgentChannel>,
    /// Runtime accounting root; only CLI transports may use it for signaling.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) db: AppRepositories,
    pub(super) folder: PathBuf,
    pub(super) fs_client: Arc<dyn FsClient>,
    pub(super) git_client: Arc<dyn GitClient>,
    /// Workspace-only personality discovery used immediately before turns.
    pub(super) personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    /// In-memory queue of prompts staged while the session is `InProgress`.
    ///
    /// Shared with [`SessionHandles::queued_messages`]. The worker drains
    /// this queue between turns; the lifecycle pushes new entries when a
    /// user submits a chat message during a running turn.
    pub(super) queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    pub(super) session_id: SessionId,
    /// Agent provider and model selected for this session.
    pub(super) session_agent: AgentSelection,
    pub(super) status: Arc<Mutex<Status>>,
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

impl SessionWorkerContext {
    /// Returns the submission order of the next queued chat prompt.
    fn next_queued_message_order(&self) -> Option<u64> {
        self.queued_messages
            .lock()
            .ok()
            .and_then(|guard| guard.front().map(QueuedMessage::order))
    }

    /// Pops the next queued chat message for dispatch as a follow-up turn.
    fn pop_queued_message(&self) -> Option<QueuedMessage> {
        // Sync critical section (single pop, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        self.queued_messages
            .lock()
            .ok()
            .and_then(|mut guard| guard.pop_front())
    }

    /// Removes every queued prompt without dispatching it.
    fn clear_queued_messages(&self) {
        // Sync critical section (single clear, no `.await`);
        // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
        // Selection".
        if let Ok(mut guard) = self.queued_messages.lock() {
            guard.clear();
        }
    }

    /// Loads the latest published upstream reference before running a queued
    /// follow-up turn.
    ///
    /// Queued prompts are created while another turn is still running, so
    /// their auto-push metadata is resolved at drain time from persistence
    /// instead of being captured when the user submits the queued prompt.
    async fn load_published_upstream_ref(&self) -> Option<String> {
        self.db
            .sessions()
            .load_session_published_upstream_ref(&self.session_id)
            .await
            .ok()
            .flatten()
    }

    /// Returns the current shared session status.
    fn current_status(&self) -> Status {
        // Sync critical section (single read, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        self.status.lock().map_or(Status::Review, |guard| *guard)
    }
}

/// Existing-session rebase assistance backed by the worker's active channel.
#[derive(Clone)]
struct SessionWorkerRebaseAssistClient {
    /// Reducer event sender used for transient progress and output updates.
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Per-turn cancellation token shared with the UI.
    cancel_token: Arc<Mutex<CancellationToken>>,
    /// Provider channel already associated with this session.
    channel: Arc<dyn AgentChannel>,
    /// Runtime accounting root; app-server cancellation uses channel shutdown.
    child_pid: Arc<Mutex<Option<u32>>>,
    /// Repository bundle used for conversation and usage persistence.
    db: AppRepositories,
    /// Session worktree folder where the utility prompt runs.
    folder: PathBuf,
    /// Main repository checkout that must remain read-only during assist
    /// turns, or `None` when the shared repository is bare and has no main
    /// working checkout.
    main_checkout_root: Option<PathBuf>,
    /// Per-app session update versions for targeted refresh events.
    session_update_versions: SessionUpdateVersionMap,
    /// Session identifier whose provider conversation is reused.
    session_id: SessionId,
    /// Agent provider and model used for the rebase-assist utility prompt.
    session_agent: AgentSelection,
    /// Shared typed transcript snapshot mirrored to the render layer.
    transcript: Arc<Mutex<SessionTranscript>>,
}

impl SessionWorkerRebaseAssistClient {
    /// Clones the worker fields needed to run a rebase-assist utility turn.
    fn from_context(context: &SessionWorkerContext, main_checkout_root: Option<PathBuf>) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            cancel_token: Arc::clone(&context.cancel_token),
            channel: Arc::clone(&context.channel),
            child_pid: Arc::clone(&context.child_pid),
            db: context.db.clone(),
            folder: context.folder.clone(),
            main_checkout_root,
            session_update_versions: context.session_update_versions.clone(),
            session_id: context.session_id.clone(),
            session_agent: context.session_agent,
            transcript: Arc::clone(&context.transcript),
        }
    }

    /// Runs one utility prompt through the current session channel.
    ///
    /// # Errors
    /// Returns an error when the provider turn fails or conversation metadata
    /// cannot be persisted.
    async fn run_assist_turn(&self, prompt: String) -> Result<(), SessionError> {
        let turn_cancel_token = self.fresh_turn_cancel_token()?;
        let reasoning_level = turn::load_session_reasoning_level(&self.db, &self.session_id).await;
        let speed_mode = turn::load_session_speed_mode(&self.db, &self.session_id).await;
        let provider_conversation_id = self
            .db
            .sessions()
            .get_session_provider_conversation_id(&self.session_id)
            .await
            .ok()
            .flatten();
        let persisted_instruction_conversation_id = self
            .db
            .sessions()
            .get_session_instruction_conversation_id(&self.session_id)
            .await
            .ok()
            .flatten();
        let req = TurnRequest {
            continuation: TurnContinuation::provider(
                Some(turn::live_transcript_source(&self.transcript)),
                persisted_instruction_conversation_id,
                provider_conversation_id,
                None,
            ),
            folder: self.folder.clone(),
            main_checkout_root: self.main_checkout_root.clone(),
            model: self.session_agent.model().provider_model_str().to_string(),
            permission_mode: agent::PermissionMode::AutoEdit,
            personality: ag_agent::PersonalityPrompt::default(),
            prompt: TurnPrompt::from_agent_data(prompt),
            reasoning_level,
            request_kind: AgentRequestKind::UtilityPrompt,
            response_style: agent::ResponseStyle::default(),
            speed_mode,
        };
        let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let consumer = tokio::spawn(turn::consume_turn_events(
            event_rx,
            self.app_event_tx.clone(),
            self.session_id.clone(),
            Arc::clone(&self.child_pid),
        ));

        let turn_result = self
            .run_turn_with_cancellation(turn_cancel_token, req, event_tx)
            .await;
        let _ = consumer.await;
        let turn_result = turn_result.map_err(turn::session_error_from_agent_error)?;

        self.append_assist_answer(&turn_result.assistant_message)
            .await;
        self.persist_assist_turn_metadata(&turn_result).await?;

        Ok(())
    }

    /// Replaces the shared cancellation token for one rebase-assist turn.
    fn fresh_turn_cancel_token(&self) -> Result<CancellationToken, SessionError> {
        // Sync critical section (assignment + clone, no `.await`); `std::sync::Mutex`
        // is the correct choice per CLAUDE.md §"Mutex Selection".
        let mut guard = self
            .cancel_token
            .lock()
            .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))?;
        *guard = CancellationToken::new();

        Ok(guard.clone())
    }

    /// Runs the provider turn while honoring the shared cancellation token.
    async fn run_turn_with_cancellation(
        &self,
        cancel_token: CancellationToken,
        req: TurnRequest,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
    ) -> Result<TurnResult, AgentError> {
        if cancel_token.is_cancelled() {
            turn::terminate_child_process(&self.child_pid, self.session_agent.kind());
            let _ = self
                .channel
                .shutdown_session(self.session_id.to_string())
                .await;

            return Err(AgentError::InterruptedByUser(
                "[Stopped] Session interrupted by user.".to_string(),
            ));
        }

        let turn_future = self
            .channel
            .run_turn(self.session_id.to_string(), req, event_tx);
        tokio::pin!(turn_future);

        tokio::select! {
            result = &mut turn_future => result,
            () = cancel_token.cancelled() => {
                turn::terminate_child_process(&self.child_pid, self.session_agent.kind());
                let _ = self.channel.shutdown_session(self.session_id.to_string()).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), &mut turn_future).await;

                Err(AgentError::InterruptedByUser(
                    "[Stopped] Session interrupted by user.".to_string(),
                ))
            }
        }
    }

    /// Appends the utility prompt answer to the session transcript.
    async fn append_assist_answer(&self, assistant_message: &AgentResponse) {
        let answer_text = assistant_message.to_answer_display_text();
        if answer_text.trim().is_empty() {
            return;
        }

        SessionTaskService::append_session_transcript_message(
            &self.transcript,
            &self.db,
            &self.app_event_tx,
            &self.session_update_versions,
            &self.session_id,
            SessionTranscriptMessageAppend {
                kind: SessionMessageKind::AssistantAnswer,
                raw_content: &answer_text,
            },
        )
        .await;
    }

    /// Persists token usage and updated provider conversation identifiers.
    ///
    /// # Errors
    /// Returns an error when conversation identifier persistence fails.
    async fn persist_assist_turn_metadata(
        &self,
        turn_result: &TurnResult,
    ) -> Result<(), SessionError> {
        let token_usage_delta = SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: agent::SessionDiffState::Unknown,
            input_tokens: turn_result.input_tokens,
            output_tokens: turn_result.output_tokens,
        };
        if let Err(error) = self
            .db
            .sessions()
            .update_session_stats(&self.session_id, &token_usage_delta)
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                error = %error,
                "failed to persist session stats after rebase-assist turn"
            );
        }
        if let Err(error) = self
            .db
            .usage()
            .upsert_session_usage(
                &self.session_id,
                self.session_agent.model().as_str(),
                &token_usage_delta,
            )
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                model = %self.session_agent.model().as_str(),
                error = %error,
                "failed to persist session usage after rebase-assist turn"
            );
        }
        let Some(provider_conversation_id) = turn_result.provider_conversation_id.clone() else {
            return Ok(());
        };

        self.db
            .sessions()
            .update_session_provider_conversation_id(
                &self.session_id,
                Some(provider_conversation_id.clone()),
            )
            .await?;
        if agent::transport_mode(self.session_agent.kind()).uses_app_server() {
            self.db
                .sessions()
                .update_session_instruction_conversation_id(
                    &self.session_id,
                    agent::normalize_instruction_conversation_id(Some(&provider_conversation_id)),
                )
                .await?;
        }

        Ok(())
    }
}

impl ExistingSessionRebaseAssistClient for SessionWorkerRebaseAssistClient {
    fn resolve_rebase_conflicts(
        &self,
        prompt: String,
    ) -> RebaseAssistFuture<Result<(), SessionError>> {
        let assist_client = self.clone();

        Box::pin(async move { assist_client.run_assist_turn(prompt).await })
    }
}

/// Runtime snapshot required to create or reuse one session worker.
pub(super) struct SessionWorkerRuntime {
    branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
    cancel_token: Arc<Mutex<CancellationToken>>,
    child_pid: Arc<Mutex<Option<u32>>>,
    folder: PathBuf,
    personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    queued_work_sequence: Arc<AtomicU64>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Per-app session update versions shared with the main runtime.
    session_update_versions: SessionUpdateVersionMap,
    /// Agent provider and model selected for this session.
    session_agent: AgentSelection,
    session_id: SessionId,
    status: Arc<Mutex<Status>>,
    transcript: Arc<Mutex<SessionTranscript>>,
}

/// Owns per-session worker queue senders and test channel overrides.
pub(crate) struct SessionWorkerService {
    /// Channels pre-registered for specific session workers in tests.
    ///
    /// Tests populate this map before enqueueing a command so that
    /// `ensure_session_worker` uses the injected channel instead of the
    /// default factory, enabling deterministic command execution without
    /// spawning real provider processes.
    pub(in crate::app::session) test_agent_channels: HashMap<SessionId, Arc<dyn AgentChannel>>,
    workers: HashMap<SessionId, SessionWorkerHandle>,
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
    ///
    /// # Errors
    /// Returns an error when loading operations, cleaning interrupted rebases,
    /// reconciling session status, or recording interrupted operations fails.
    pub(super) async fn fail_unfinished_operations_from_previous_run_at(
        db: &AppRepositories,
        base_path: &Path,
        git_client: Arc<dyn GitClient>,
        timestamp_seconds: i64,
    ) -> Result<(), SessionError> {
        let unfinished_operations = db.operations().load_unfinished_session_operations().await?;
        Self::abort_rebase_operations_from_previous_run(
            base_path,
            git_client.as_ref(),
            &unfinished_operations,
        )
        .await?;

        let interrupted_session_ids: HashSet<String> = unfinished_operations
            .into_iter()
            .map(|operation| operation.session_id)
            .collect();

        for session_id in interrupted_session_ids {
            db.sessions()
                .update_session_status_with_timing_at(
                    &session_id,
                    &Status::Review.to_string(),
                    timestamp_seconds,
                )
                .await?;
        }

        db.operations()
            .fail_unfinished_session_operations(RESTART_FAILURE_REASON)
            .await?;

        Ok(())
    }

    /// Aborts stale git rebase state left by interrupted worker operations.
    ///
    /// Only worker-backed rebase operations are handled here because merge
    /// tasks are not yet persisted in `session_operation`.
    ///
    /// # Errors
    /// Returns an error when Git cannot inspect or abort interrupted rebase
    /// state.
    async fn abort_rebase_operations_from_previous_run(
        base_path: &Path,
        git_client: &dyn GitClient,
        unfinished_operations: &[SessionOperationRow],
    ) -> Result<(), SessionError> {
        let mut rebase_session_ids = unfinished_operations
            .iter()
            .filter(|operation| operation.kind == REBASE_OPERATION_KIND)
            .map(|operation| operation.session_id.as_str())
            .collect::<Vec<_>>();
        rebase_session_ids.sort_unstable();
        rebase_session_ids.dedup();

        for session_id in rebase_session_ids {
            let folder = session_folder(base_path, session_id);
            let is_rebase_in_progress = git_client.is_rebase_in_progress(folder.clone()).await?;
            if is_rebase_in_progress {
                git_client.abort_rebase(folder).await?;
            }
        }

        Ok(())
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
        let session_id = runtime.session_id.clone();
        let worker = self.ensure_session_worker(services, &runtime);

        self.persist_and_send_command(services, &session_id, worker, command)
            .await
    }

    /// Claims and enqueues one command under its stable operation identifier.
    ///
    /// Returns `true` when this call enqueued the command and `false` when an
    /// earlier attempt already durably accepted it.
    ///
    /// # Errors
    /// Returns an error if operation persistence fails or no worker is
    /// available.
    pub(super) async fn enqueue_session_command_idempotently(
        &mut self,
        services: &AppServices,
        runtime: SessionWorkerRuntime,
        command: SessionCommand,
    ) -> Result<bool, SessionError> {
        let session_id = runtime.session_id.clone();
        let operation_id = command.operation_id().to_string();
        let claimed = services
            .db()
            .operations()
            .claim_session_operation(&operation_id, &session_id, command.kind())
            .await?;
        if !claimed {
            return Ok(false);
        }

        let worker = self.ensure_session_worker(services, &runtime);
        self.send_persisted_command(
            services.db().operations(),
            &session_id,
            worker.sender,
            ScheduledSessionCommand::immediate(command),
        )
        .await?;

        Ok(true)
    }

    /// Persists and enqueues a command only when the session already owns a
    /// worker sender.
    ///
    /// Running-session actions use this path so a stale `InProgress` status
    /// can never create a second worker that executes concurrently with the
    /// original turn.
    ///
    /// # Errors
    /// Returns an error without persisting the operation when the active
    /// worker sender is unavailable, or when persistence or delivery fails.
    pub(super) async fn enqueue_existing_session_command(
        &mut self,
        services: &AppServices,
        session_id: &SessionId,
        command: SessionCommand,
    ) -> Result<u64, SessionError> {
        let worker = self.workers.get(session_id).cloned().ok_or_else(|| {
            SessionError::Workflow(
                "Cannot queue session action because the active session worker is unavailable"
                    .to_string(),
            )
        })?;
        let operation_id = command.operation_id().to_string();
        services
            .db()
            .operations()
            .insert_session_operation(&operation_id, session_id, command.kind())
            .await?;
        let queued_order = worker.next_queued_work_order();

        self.send_persisted_command(
            services.db().operations(),
            session_id,
            worker.sender,
            ScheduledSessionCommand::queued(command, queued_order),
        )
        .await?;

        Ok(queued_order)
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.workers.remove(session_id);
    }

    /// Wakes an existing session worker after an external state transition.
    pub(super) fn wake_session_worker(&self, session_id: &str) {
        if let Some(worker) = self.workers.get(session_id) {
            worker.wake();
        }
    }

    /// Returns an existing session worker sender or creates one lazily.
    fn ensure_session_worker(
        &mut self,
        services: &AppServices,
        runtime: &SessionWorkerRuntime,
    ) -> SessionWorkerHandle {
        if let Some(worker) = self.workers.get(&runtime.session_id) {
            return worker.clone();
        }

        // When a pre-registered channel exists, reuse it; otherwise fall back
        // to the production channel factory.
        let channel = self
            .test_agent_channels
            .remove(&runtime.session_id)
            .unwrap_or_else(|| {
                create_agent_channel(
                    runtime.session_agent.kind(),
                    services.app_server_client_override(),
                )
            });

        let context = SessionWorkerContext {
            app_event_tx: services.event_sender(),
            branch_operation_lock: Arc::clone(&runtime.branch_operation_lock),
            cancel_token: Arc::clone(&runtime.cancel_token),
            channel,
            child_pid: Arc::clone(&runtime.child_pid),
            clock: services.clock(),
            db: services.db().clone(),
            folder: runtime.folder.clone(),
            fs_client: services.fs_client(),
            git_client: services.git_client(),
            personality_catalog_client: Arc::clone(&runtime.personality_catalog_client),
            queued_messages: Arc::clone(&runtime.queued_messages),
            review_request_client: Arc::clone(&runtime.review_request_client),
            session_update_versions: Arc::clone(&runtime.session_update_versions),
            session_id: runtime.session_id.clone(),
            session_agent: runtime.session_agent,
            status: Arc::clone(&runtime.status),
            transcript: Arc::clone(&runtime.transcript),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let wakeup = Arc::new(Notify::new());
        let worker = SessionWorkerHandle {
            queued_work_sequence: Arc::clone(&runtime.queued_work_sequence),
            sender,
            wakeup: Arc::clone(&wakeup),
        };
        self.workers
            .insert(runtime.session_id.clone(), worker.clone());
        Self::spawn_session_worker(context, services.one_shot_client(), wakeup, receiver);

        worker
    }

    /// Persists one operation and sends its command to the selected worker.
    async fn persist_and_send_command(
        &mut self,
        services: &AppServices,
        session_id: &SessionId,
        worker: SessionWorkerHandle,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        let operation_id = command.operation_id().to_string();
        services
            .db()
            .operations()
            .insert_session_operation(&operation_id, session_id, command.kind())
            .await?;

        self.send_persisted_command(
            services.db().operations(),
            session_id,
            worker.sender,
            ScheduledSessionCommand::immediate(command),
        )
        .await
    }

    /// Sends one command whose operation row has already been persisted.
    async fn send_persisted_command(
        &mut self,
        operations: &dyn OperationRepository,
        session_id: &SessionId,
        sender: mpsc::UnboundedSender<ScheduledSessionCommand>,
        scheduled_command: ScheduledSessionCommand,
    ) -> Result<(), SessionError> {
        let operation_id = scheduled_command.command.operation_id().to_string();
        if sender.send(scheduled_command).is_err() {
            self.workers.remove(session_id);
            // Best-effort: operation tracking metadata is non-critical.
            let _ = operations
                .mark_session_operation_failed(&operation_id, "Session worker is not available")
                .await;

            return Err(SessionError::Workflow(
                "Session worker is not available".to_string(),
            ));
        }

        Ok(())
    }

    /// Spawns the background loop that executes queued session commands.
    ///
    /// Queued workflow actions and chat messages share one submission order,
    /// so the next displayed row is always the next work executed. Scheduling
    /// pauses while the session is in `Question` state, except for an
    /// immediate answer command that makes the session runnable again. A turn
    /// stopped by the user (`Ctrl+C`) clears queued chat so canceled work does
    /// not silently leak into the next session activity.
    fn spawn_session_worker(
        context: SessionWorkerContext,
        one_shot_client: Arc<dyn OneShotClient>,
        wakeup: Arc<Notify>,
        mut receiver: mpsc::UnboundedReceiver<ScheduledSessionCommand>,
    ) {
        tokio::spawn(async move {
            let mut pending_commands = VecDeque::new();
            loop {
                while let Ok(command) = receiver.try_recv() {
                    pending_commands.push_back(command);
                }

                let Some(work) = Self::next_scheduled_work(&context, &mut pending_commands) else {
                    tokio::select! {
                        command = receiver.recv() => {
                            let Some(command) = command else {
                                break;
                            };
                            pending_commands.push_back(command);
                        }
                        () = wakeup.notified() => {}
                    }

                    continue;
                };
                let result = match work {
                    ScheduledSessionWork::Command(command) => {
                        Self::process_session_command(&context, &one_shot_client, command.command)
                            .await
                    }
                    ScheduledSessionWork::Message(message) => {
                        Self::process_queued_message(&context, &one_shot_client, message).await
                    }
                };
                Self::clear_queued_messages_after_stop(&context, result.as_ref());
            }

            // Best-effort: session transport may already be torn down.
            let _ = context
                .channel
                .shutdown_session(context.session_id.to_string())
                .await;
            // Sync critical section (single assignment, no `.await`);
            // `std::sync::Mutex` is the correct choice per CLAUDE.md
            // §"Mutex Selection".
            if let Ok(mut guard) = context.child_pid.lock() {
                *guard = None;
            }
        });
    }

    /// Selects the oldest runnable work across workflow and chat queues.
    fn next_scheduled_work(
        context: &SessionWorkerContext,
        pending_commands: &mut VecDeque<ScheduledSessionCommand>,
    ) -> Option<ScheduledSessionWork> {
        if matches!(context.current_status(), Status::Question) {
            let runnable_index = pending_commands
                .iter()
                .position(ScheduledSessionCommand::can_run_while_question)?;

            return pending_commands
                .remove(runnable_index)
                .map(Box::new)
                .map(ScheduledSessionWork::Command);
        }
        if pending_commands
            .front()
            .is_some_and(|command| command.queued_order.is_none())
        {
            return pending_commands
                .pop_front()
                .map(Box::new)
                .map(ScheduledSessionWork::Command);
        }

        let command_order = pending_commands
            .front()
            .and_then(|command| command.queued_order);
        let message_order = context.next_queued_message_order();
        if command_order.is_some_and(|command_order| {
            message_order.is_none_or(|message_order| command_order <= message_order)
        }) {
            return pending_commands
                .pop_front()
                .map(Box::new)
                .map(ScheduledSessionWork::Command);
        }

        context
            .pop_queued_message()
            .map(ScheduledSessionWork::Message)
    }

    /// Clears pending chat messages when the work just stopped by user action.
    fn clear_queued_messages_after_stop(
        context: &SessionWorkerContext,
        result: Option<&Result<(), SessionError>>,
    ) {
        if matches!(result, Some(Err(SessionError::StoppedByUser(_)))) {
            context.clear_queued_messages();
            Self::emit_queue_session_updated(context);
        }
    }

    /// Executes one queued session command including its operation
    /// bookkeeping. Returns `None` when the command was skipped before
    /// execution (already finished or cancelled) and `Some(result)` when the
    /// turn ran.
    async fn process_session_command(
        context: &SessionWorkerContext,
        one_shot_client: &Arc<dyn OneShotClient>,
        command: SessionCommand,
    ) -> Option<Result<(), SessionError>> {
        let operation_id = command.operation_id().to_string();
        if !Self::prepare_session_command(context, &command).await {
            Self::complete_skipped_session_command(context, &command);

            return None;
        }

        if matches!(
            command,
            SessionCommand::Run {
                request_kind: AgentRequestKind::SessionStart | AgentRequestKind::SessionResume,
                ..
            }
        ) {
            let _ = context.app_event_tx.send(AppEvent::SessionTurnStarted {
                session_id: context.session_id.clone(),
            });
        }

        let result = Self::execute_session_command(context, one_shot_client, command).await;
        match &result {
            Ok(()) => {
                // Best-effort: operation tracking metadata is non-critical.
                let _ = context
                    .db
                    .operations()
                    .mark_session_operation_done(&operation_id)
                    .await;
            }
            Err(error) => {
                // Best-effort: operation tracking metadata is non-critical.
                let _ = context
                    .db
                    .operations()
                    .mark_session_operation_failed(&operation_id, &error.to_string())
                    .await;
            }
        }

        Some(result)
    }

    /// Renews turn cancellation before checking whether this operation may run.
    /// The selected token remains shared through execution, so cancellation
    /// after the final preflight check cannot be erased by turn startup.
    pub(super) async fn prepare_session_command(
        context: &SessionWorkerContext,
        command: &SessionCommand,
    ) -> bool {
        if matches!(command, SessionCommand::Run { .. })
            && let Ok(mut token) = context.cancel_token.lock()
        {
            *token = CancellationToken::new();
        }

        let operation_id = command.operation_id();
        if Self::should_skip_worker_command(context, operation_id).await {
            return false;
        }

        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .operations()
            .mark_session_operation_running(operation_id)
            .await;

        !Self::should_skip_worker_command(context, operation_id).await
    }

    /// Resolves external observers when a queued command is canceled or
    /// otherwise finishes before worker execution begins.
    fn complete_skipped_session_command(context: &SessionWorkerContext, command: &SessionCommand) {
        if matches!(command, SessionCommand::CreateReviewRequest { .. }) {
            let _ = context
                .app_event_tx
                .send(AppEvent::BranchPublishActionResolved {
                    session_id: context.session_id.clone(),
                });
        }

        if matches!(command, SessionCommand::Rebase { .. }) {
            let _ = context
                .app_event_tx
                .send(AppEvent::SessionQueuedSyncResolved {
                    session_id: context.session_id.clone(),
                });
        }

        if let SessionCommand::CreateReviewRequest {
            response: Some(response),
            ..
        } = command
            && let Ok(mut response) = response.lock()
            && let Some(response_tx) = response.take()
        {
            let _ = response_tx.send(Err(ag_session::SessionError::Operation(
                SKIPPED_CREATE_REVIEW_REQUEST_REASON.to_string(),
            )));
        }
    }

    /// Dispatches one queued chat message as a follow-up `SessionResume` turn.
    ///
    /// The turn is persisted as its own `reply` operation with a fresh
    /// identifier so cancellation, retry, and operation tracking behave the
    /// same as a normal reply.
    async fn process_queued_message(
        context: &SessionWorkerContext,
        one_shot_client: &Arc<dyn OneShotClient>,
        message: QueuedMessage,
    ) -> Option<Result<(), SessionError>> {
        let prompt = message.into_prompt();

        // Mirror the queue change into render snapshots so the inline queued
        // row disappears as soon as the follow-up turn starts. The targeted
        // event re-syncs only this session from handles.
        Self::emit_queue_session_updated(context);

        let operation_id = Uuid::new_v4().to_string();
        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .operations()
            .insert_session_operation(&operation_id, &context.session_id, "reply")
            .await;
        let published_upstream_ref = context.load_published_upstream_ref().await;
        append_drained_prompt_to_transcript(context, &prompt).await;
        let command = SessionCommand::Run {
            preparation: None,
            operation_id,
            request_kind: AgentRequestKind::SessionResume,
            replay_transcript: None,
            prompt,
            turn_metadata: TurnMetadata {
                published_upstream_ref,
                review_comment_thread_ids: Vec::new(),
                session_agent: context.session_agent,
            },
        };

        Self::process_session_command(context, one_shot_client, command).await
    }

    /// Emits a targeted [`AppEvent::SessionUpdated`] for the worker's session
    /// after the in-memory queue mutates so the reducer re-syncs the snapshot
    /// from the handles without paying for a full `RefreshSessions` reload.
    fn emit_queue_session_updated(context: &SessionWorkerContext) {
        let version = SessionTaskService::next_session_update_version(
            &context.session_update_versions,
            context.session_id.as_str(),
        );
        let _ = context.app_event_tx.send(AppEvent::SessionUpdated {
            session_id: context.session_id.clone(),
            version,
        });
    }

    /// Executes the queued command through the session's agent channel.
    pub(super) async fn execute_session_command(
        context: &SessionWorkerContext,
        one_shot_client: &Arc<dyn OneShotClient>,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        match command {
            SessionCommand::CreateReviewRequest {
                branch_publish_session,
                remote_branch_name,
                response,
                ..
            } => {
                Self::run_create_review_request_command(
                    context,
                    branch_publish_session,
                    remote_branch_name,
                    response,
                )
                .await
            }
            SessionCommand::Rebase { base_branch, .. } => {
                Self::run_rebase_command(context, Arc::clone(one_shot_client), base_branch).await
            }
            SessionCommand::Run {
                preparation,
                request_kind,
                replay_transcript,
                prompt,
                turn_metadata,
                ..
            } => {
                turn::run_channel_turn(
                    context,
                    Arc::clone(one_shot_client),
                    turn_metadata,
                    request_kind,
                    replay_transcript,
                    prompt,
                    preparation,
                )
                .await
            }
        }
    }

    /// Publishes one review request inside the serialized session worker.
    ///
    /// The live status is applied at execution time so an action accepted
    /// during `InProgress` or `Rebasing` observes the completed work's
    /// review-ready state instead of the enqueue-time snapshot.
    async fn run_create_review_request_command(
        context: &SessionWorkerContext,
        mut branch_publish_session: BranchPublishTaskSession,
        remote_branch_name: Option<String>,
        response: Option<ReviewRequestResponse>,
    ) -> Result<(), SessionError> {
        branch_publish_session.status = context.current_status();
        let _ = context
            .app_event_tx
            .send(AppEvent::BranchPublishActionStarted {
                session_id: context.session_id.clone(),
            });
        let result = run_branch_publish_action(
            PublishBranchAction::PublishPullRequest,
            BranchPublishTaskContext {
                branch_operation_lock: Arc::clone(&context.branch_operation_lock),
                session: branch_publish_session,
            },
            context.db.clone(),
            Arc::clone(&context.clock),
            Arc::clone(&context.git_client),
            Arc::clone(&context.review_request_client),
            remote_branch_name,
        )
        .await;
        let response_result = review_request_from_publish_result(&result)
            .map_err(ag_session::SessionError::Operation);
        let command_result = review_request_from_publish_result(&result)
            .map(|_| ())
            .map_err(SessionError::Workflow);
        let _ = context
            .app_event_tx
            .send(AppEvent::BranchPublishActionCompleted {
                result: Box::new(result),
                session_id: context.session_id.clone(),
            });
        if let Some(response) = response
            && let Ok(mut response) = response.lock()
            && let Some(response_tx) = response.take()
        {
            let _ = response_tx.send(response_result);
        }

        command_result
    }

    /// Runs the session rebase command inside this worker's serialized queue.
    ///
    /// The rebase task applies the `Rebasing` status at execution time so
    /// sync requested during an active turn can remain visibly queued until
    /// the worker reaches this command.
    ///
    /// # Errors
    /// Returns an error when the rebase workflow fails after appending the
    /// user-visible rebase outcome.
    async fn run_rebase_command(
        context: &SessionWorkerContext,
        one_shot_client: Arc<dyn OneShotClient>,
        base_branch: String,
    ) -> Result<(), SessionError> {
        let validation = match isolation::validate_session_worktree(
            context.fs_client.as_ref(),
            context.git_client.as_ref(),
            &context.folder,
            &context.session_id,
        )
        .await
        {
            Ok(validation) => validation,
            Err(error) => {
                Self::record_rebase_validation_failure(context, &error).await;

                return Err(error);
            }
        };
        let assist_client = Arc::new(SessionWorkerRebaseAssistClient::from_context(
            context,
            validation.main_checkout,
        ));
        SessionManager::run_rebase_command(RebaseCommandInput {
            app_event_tx: context.app_event_tx.clone(),
            assist_mode: RebaseAssistMode::ExistingSession(assist_client),
            base_branch,
            branch_operation_lock: Arc::clone(&context.branch_operation_lock),
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            fs_client: Arc::clone(&context.fs_client),
            git_client: Arc::clone(&context.git_client),
            id: context.session_id.clone(),
            one_shot_client,
            review_request_client: Arc::clone(&context.review_request_client),
            session_agent: context.session_agent,
            session_update_versions: context.session_update_versions.clone(),
            status: Arc::clone(&context.status),
            transcript: Arc::clone(&context.transcript),
        })
        .await
    }

    /// Persists one pre-rebase validation failure before resolving its queue
    /// row.
    async fn record_rebase_validation_failure(
        context: &SessionWorkerContext,
        error: &SessionError,
    ) {
        let notice = TranscriptNotice::RebaseError.format(error);
        SessionTaskService::append_workflow_notice(
            &context.transcript,
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            &notice,
        )
        .await;
        let _ = context
            .app_event_tx
            .send(AppEvent::SessionQueuedSyncResolved {
                session_id: context.session_id.clone(),
            });
    }

    /// Returns whether a queued command should be skipped before execution.
    async fn should_skip_worker_command(
        context: &SessionWorkerContext,
        operation_id: &str,
    ) -> bool {
        let operation_is_unfinished = context
            .db
            .operations()
            .is_session_operation_unfinished(operation_id)
            .await
            .unwrap_or(false);
        if !operation_is_unfinished {
            return true;
        }

        let is_cancel_requested = context
            .db
            .operations()
            .is_cancel_requested_for_operation(operation_id)
            .await
            .unwrap_or(false);
        if !is_cancel_requested {
            return false;
        }

        // Best-effort: operation tracking metadata is non-critical.
        let _ = context
            .db
            .operations()
            .mark_session_operation_canceled(operation_id, CANCEL_BEFORE_EXECUTION_REASON)
            .await;

        true
    }
}

impl SessionManager {
    /// Marks unfinished operations from previous process runs as failed.
    ///
    /// # Errors
    /// Returns an error when startup recovery cannot finish, leaving the
    /// unfinished operations available for a later retry.
    pub(crate) async fn fail_unfinished_operations_from_previous_run(
        db: AppRepositories,
        base_path: PathBuf,
        git_client: Arc<dyn GitClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<(), SessionError> {
        let timestamp_seconds = unix_timestamp_from_system_time(clock.now_system_time());

        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_path.as_path(),
            git_client,
            timestamp_seconds,
        )
        .await
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
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;

        self.worker_service_mut()
            .enqueue_session_command(services, runtime, command)
            .await
    }

    /// Claims and enqueues a command by its stable operation identifier.
    ///
    /// # Errors
    /// Returns an error if the session runtime cannot be built, operation
    /// persistence fails, or no worker is available.
    pub(super) async fn enqueue_session_command_idempotently(
        &mut self,
        services: &AppServices,
        session_id: &str,
        command: SessionCommand,
    ) -> Result<bool, SessionError> {
        let runtime = self.session_worker_runtime_or_err(services, session_id)?;

        self.worker_service_mut()
            .enqueue_session_command_idempotently(services, runtime, command)
            .await
    }

    /// Persists and queues review-request creation on one session worker.
    ///
    /// Active turn and rebase sessions must already own a worker so stale
    /// status cannot create a concurrent executor. Review-ready sessions
    /// lazily create a worker and execute the action immediately.
    ///
    /// # Errors
    /// Returns an error when operation persistence or worker delivery fails.
    pub(crate) async fn enqueue_review_request_creation(
        &mut self,
        services: &AppServices,
        branch_publish_session: BranchPublishTaskSession,
        remote_branch_name: Option<String>,
        response_tx: Option<oneshot::Sender<Result<ReviewRequest, ag_session::SessionError>>>,
    ) -> Result<Option<u64>, SessionError> {
        let session_id = branch_publish_session.id.clone();
        let status = branch_publish_session.status;
        let response = response_tx.map(|response_tx| Arc::new(Mutex::new(Some(response_tx))));
        let command = SessionCommand::CreateReviewRequest {
            branch_publish_session,
            operation_id: Uuid::new_v4().to_string(),
            remote_branch_name,
            response: response.clone(),
        };
        let result = if matches!(status, Status::InProgress | Status::Rebasing) {
            self.worker_service_mut()
                .enqueue_existing_session_command(services, &session_id, command)
                .await
                .map(Some)
        } else {
            self.enqueue_session_command(services, &session_id, command)
                .await
                .map(|()| None)
        };
        if let Err(error) = &result
            && let Some(response) = response
            && let Ok(mut response) = response.lock()
            && let Some(response_tx) = response.take()
        {
            let _ = response_tx.send(Err(ag_session::SessionError::Operation(error.to_string())));
        }

        result
    }

    /// Drops the in-memory worker sender for a session.
    pub(super) fn clear_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().clear_session_worker(session_id);
    }

    /// Wakes an existing worker so it re-evaluates buffered work against the
    /// current session status.
    pub(crate) fn wake_session_worker(&mut self, session_id: &str) {
        self.worker_service_mut().wake_session_worker(session_id);
    }

    /// Drops worker queues for touched sessions that reached terminal status.
    ///
    /// Terminal sessions (`Done`, `Canceled`) no longer execute turns, so
    /// dropping their worker sender lets the worker task exit and shut down any
    /// provider runtime process associated with that session.
    pub(crate) fn clear_terminal_session_workers(
        &mut self,
        updated_session_ids: &HashSet<SessionId>,
    ) {
        let terminal_session_ids = updated_session_ids
            .iter()
            .filter(|session_id| {
                self.state
                    .handle(session_id)
                    .and_then(|handles| handles.status.lock().ok().map(|status| *status))
                    .is_some_and(|status| matches!(status, Status::Done | Status::Canceled))
            })
            .cloned()
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
        services: &AppServices,
        session_id: &str,
    ) -> Result<SessionWorkerRuntime, SessionError> {
        let (session, handles) = self.session_and_handles_or_err(session_id)?;

        Ok(SessionWorkerRuntime {
            branch_operation_lock: Arc::clone(&handles.branch_operation_lock),
            cancel_token: Arc::clone(&handles.cancel_token),
            child_pid: Arc::clone(&handles.child_pid),
            folder: session.folder.clone(),
            personality_catalog_client: services.personality_catalog_client(),
            queued_messages: Arc::clone(&handles.queued_messages),
            queued_work_sequence: Arc::clone(&handles.queued_work_sequence),
            review_request_client: services.review_request_client(),
            session_update_versions: services.session_update_versions(),
            session_id: session.id.clone(),
            session_agent: session.agent,
            status: Arc::clone(&handles.status),
            transcript: Arc::clone(&handles.transcript),
        })
    }
}

/// Appends one drained queued prompt to the typed session transcript so it
/// renders alongside the normal reply prompt line once the queued turn starts
/// running.
async fn append_drained_prompt_to_transcript(context: &SessionWorkerContext, prompt: &TurnPrompt) {
    let prompt_transcript_text = prompt.transcript_text();

    SessionTaskService::append_session_transcript_message(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        SessionTranscriptMessageAppend {
            kind: SessionMessageKind::UserPrompt,
            raw_content: &prompt_transcript_text,
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ag_agent::{MockAgentChannel, MockOneShotClient, PermissionMode};
    use ag_git::{MockGitClient, RebaseStepResult};
    use ag_protocol::{ReviewCommentOutcome, ReviewCommentResolution, TurnPromptAttachment};
    use mockall::Sequence;
    use tempfile::tempdir;
    use tracing::instrument::WithSubscriber;

    use super::super::post_turn::{
        PostTurnContext, TurnPersonalityPersistence, apply_turn_result,
        build_assistant_message_content, status_update_after_turn_result,
    };
    use super::super::turn::{
        consume_turn_events, resolve_turn_personality, run_channel_turn,
        run_turn_with_cancellation, terminate_child_process,
    };
    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel, SpeedMode};
    use crate::domain::personality::Personality;
    use crate::domain::question::QuestionItem;
    use crate::domain::session::{PublishedBranchSyncStatus, ReviewRequest, ReviewRequestState};
    use crate::infra::db::{AppRepositories, PersistedSessionCreation, SessionTurnMetadata};
    use crate::infra::fs;
    use crate::infra::personality::{MockPersonalityCatalogClient, RealPersonalityCatalogClient};

    /// Builds one filesystem mock that treats every probed path as an
    /// existing directory.
    fn mock_fs_client_with_existing_directories() -> fs::MockFsClient {
        let mut fs_client = fs::MockFsClient::new();
        fs_client.expect_is_dir().times(0..).returning(|_| true);
        fs_client
            .expect_canonicalize()
            .times(0..)
            .returning(|path| Box::pin(async move { Ok(path) }));

        fs_client
    }

    /// Builds one git client mock that detects the `wt/sess1` worktree and
    /// resolves the given main working checkout.
    fn mock_git_client_detecting_main_repo(main_repo_root: PathBuf) -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_checkout_working_tree()
            .once()
            .returning(move |_| {
                let main_repo_root = main_repo_root.clone();

                Box::pin(async move { Ok(Some(main_repo_root)) })
            });

        mock_git_client
    }

    /// Inserts one in-progress Antigravity-backed session for worker-flow
    /// tests.
    async fn insert_in_progress_test_session(db: &AppRepositories) -> i64 {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        project_id
    }

    /// Inserts one in-progress read-only researcher for worker-flow tests.
    async fn insert_in_progress_research_session(db: &AppRepositories) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "antigravity",
                base_branch: "main",
                id: "sess1",
                is_draft: false,
                model: "gemini-3.8-flash",
                orchestration_task_id: None,
                parent_session_id: None,
                permission_mode: ag_agent::PermissionMode::AutoEdit,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                response_style: ag_agent::ResponseStyle::default(),
                role: Some("OrchestrationResearcher"),
                speed_mode: SpeedMode::Normal,
                status: "InProgress",
            })
            .await
            .expect("failed to insert research session");
    }

    /// Seeds one unfinished operation and its owning session for recovery
    /// tests.
    async fn seed_recovery_test_operation(
        db: &AppRepositories,
        status: Status,
        operation_kind: &str,
    ) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                &status.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", operation_kind)
            .await
            .expect("failed to insert session operation");
    }

    fn empty_transcript() -> Arc<Mutex<SessionTranscript>> {
        Arc::new(Mutex::new(SessionTranscript::default()))
    }

    /// Builds one user prompt referencing a single managed image attachment.
    fn turn_prompt_with_attachment(attachment_path: PathBuf) -> TurnPrompt {
        TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                local_image_path: attachment_path,
                placeholder: "[Image #1]".to_string(),
            }],
            text: "Continue [Image #1]".to_string(),
            text_source: ag_protocol::TurnPromptTextSource::UserPrompt,
        }
    }

    fn resume_command(operation_id: &str) -> SessionCommand {
        SessionCommand::Run {
            preparation: None,
            operation_id: operation_id.to_string(),
            request_kind: AgentRequestKind::SessionResume,
            replay_transcript: None,
            prompt: "Continue".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet5,
                ),
            },
        }
    }

    fn cancel_token_after_short_delay(cancel_token: Arc<Mutex<CancellationToken>>) {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_token.lock().expect("cancel token lock").cancel();
        });
    }

    fn expect_clean_main_checkout_snapshot(
        mock_git_client: &mut MockGitClient,
        main_repo_root: PathBuf,
    ) {
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_checkout_working_tree()
            .once()
            .returning(move |_| {
                let main_repo_root = main_repo_root.clone();

                Box::pin(async move { Ok(Some(main_repo_root)) })
            });
        mock_git_client
            .expect_tracked_worktree_status()
            .once()
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    }

    fn transcript_text(transcript: &Arc<Mutex<SessionTranscript>>) -> String {
        transcript
            .lock()
            .ok()
            .and_then(|transcript| transcript.replay_text())
            .unwrap_or_default()
    }

    /// Builds a title-generation boundary that verifies temporary research
    /// sessions use an isolated read-only utility request.
    fn research_title_one_shot_client() -> Arc<dyn OneShotClient> {
        let mut title_client = MockOneShotClient::new();
        title_client
            .expect_submit()
            .once()
            .withf(|request| {
                request.permission_mode == PermissionMode::ReadOnly
                    && request.request_kind == AgentRequestKind::UtilityPrompt
            })
            .returning(|_| {
                Ok(agent::OneShotSubmission {
                    response: AgentResponse::plain("Inspect architecture boundaries"),
                    stats: agent::SessionStats {
                        added_lines: 0,
                        deleted_lines: 0,
                        diff_state: agent::SessionDiffState::Unknown,
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                })
            });

        Arc::new(title_client)
    }

    /// Builds a deterministic post-turn one-shot boundary. Tests whose
    /// worktrees are clean never submit; auto-commit tests receive the
    /// canonical message they already expect.
    fn auto_commit_one_shot_client() -> Arc<dyn OneShotClient> {
        let mut one_shot_client = MockOneShotClient::new();
        one_shot_client.expect_submit().times(0..).returning(|request| {
            let answer = if request
                .prompt
                .contains("Reconcile the current review-request title")
            {
                r#"{"title":"Old title","description":"Old body\n\n- Update the linked review request body.","is_title_change_significant":false}"#
            } else {
                "Refine review metadata sync\n\n- Update the linked review request body."
            };

            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain(answer),
                stats: agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

        Arc::new(one_shot_client)
    }

    /// Applies one turn result through the narrowed post-turn dependency set
    /// cloned from a full worker context.
    async fn apply_worker_turn_result(
        context: &SessionWorkerContext,
        turn_metadata: TurnMetadata,
        turn_result: Result<TurnResult, AgentError>,
    ) -> Result<Status, SessionError> {
        let post_turn_context =
            PostTurnContext::from_worker(context, auto_commit_one_shot_client());

        apply_turn_result(
            &post_turn_context,
            turn_metadata,
            TurnPersonalityPersistence::default(),
            turn_result,
        )
        .await
    }

    #[test]
    fn test_status_update_after_turn_result_skips_stopped_by_user() {
        // Arrange
        let result = Err(SessionError::StoppedByUser(
            "[Stopped] Session interrupted by user.".to_string(),
        ));

        // Act
        let status_update = status_update_after_turn_result(&result);

        // Assert
        assert_eq!(status_update, None);
    }

    #[test]
    fn test_status_update_after_turn_result_falls_back_to_review_for_errors() {
        // Arrange
        let result = Err(SessionError::Workflow("backend failed".to_string()));

        // Act
        let status_update = status_update_after_turn_result(&result);

        // Assert
        assert_eq!(status_update, Some(Status::Review));
    }

    #[test]
    /// Ensures session command request kinds map to stable persisted
    /// operation labels.
    fn test_session_command_kind_values() {
        // Arrange
        let review_request_command = SessionCommand::CreateReviewRequest {
            branch_publish_session: BranchPublishTaskSession {
                base_branch: "main".to_string(),
                folder: PathBuf::new(),
                id: "sess1".into(),
                published_upstream_ref: None,
                review_request: None,
                status: Status::Review,
            },
            operation_id: "op-review-request".to_string(),
            remote_branch_name: None,
            response: None,
        };
        let start_command = SessionCommand::Run {
            preparation: None,
            operation_id: "op-start".to_string(),
            request_kind: AgentRequestKind::SessionStart,
            replay_transcript: None,
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet5,
                ),
            },
        };
        let resume_command = SessionCommand::Run {
            preparation: None,
            operation_id: "op-resume".to_string(),
            request_kind: AgentRequestKind::SessionResume,
            replay_transcript: None,
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet5,
                ),
            },
        };
        let account_read_command = SessionCommand::Run {
            preparation: None,
            operation_id: "op-account-read".to_string(),
            request_kind: AgentRequestKind::AccountRead,
            replay_transcript: None,
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet5,
                ),
            },
        };
        let focused_review_command = SessionCommand::Run {
            preparation: None,
            operation_id: "op-focused-review".to_string(),
            request_kind: AgentRequestKind::FocusedReview,
            replay_transcript: None,
            prompt: "prompt".into(),
            turn_metadata: TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    AgentModel::ClaudeSonnet5,
                ),
            },
        };

        // Act
        let review_request_kind = review_request_command.kind();
        let start_kind = start_command.kind();
        let resume_kind = resume_command.kind();
        let account_read_kind = account_read_command.kind();
        let focused_review_kind = focused_review_command.kind();

        // Assert
        assert_eq!(review_request_kind, "create_review_request");
        assert_eq!(start_kind, "start_prompt");
        assert_eq!(resume_kind, "reply");
        assert_eq!(account_read_kind, "account_read");
        assert_eq!(focused_review_kind, "focused_review");
    }

    #[test]
    fn test_unfinished_branch_operation_includes_review_request_creation() {
        // Arrange
        let operations = vec![SessionOperationRow {
            cancel_requested: false,
            finished_at: None,
            heartbeat_at: None,
            id: "op-review-request".to_string(),
            kind: CREATE_REVIEW_REQUEST_OPERATION_KIND.to_string(),
            last_error: None,
            queued_at: 0,
            session_id: "sess1".to_string(),
            started_at: None,
            status: "queued".to_string(),
        }];

        // Act
        let has_branch_operation = has_unfinished_branch_operation(&operations, "sess1");
        let other_session_has_branch_operation =
            has_unfinished_branch_operation(&operations, "sess2");

        // Assert
        assert!(has_branch_operation);
        assert!(!other_session_has_branch_operation);
    }

    #[tokio::test]
    async fn test_create_review_request_command_waits_for_live_review_status() {
        // Arrange
        let (mut context, _db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Done).await;
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;
        let (response_tx, response_rx) = oneshot::channel();
        let command = SessionCommand::CreateReviewRequest {
            branch_publish_session: BranchPublishTaskSession {
                base_branch: "main".to_string(),
                folder: context.folder.clone(),
                id: context.session_id.clone(),
                published_upstream_ref: None,
                review_request: None,
                status: Status::InProgress,
            },
            operation_id: "op-review-request".to_string(),
            remote_branch_name: None,
            response: Some(Arc::new(Mutex::new(Some(response_tx)))),
        };

        // Act
        let result = SessionWorkerService::execute_session_command(
            &context,
            &auto_commit_one_shot_client(),
            command,
        )
        .await;
        let response = response_rx
            .await
            .expect("review-request response should be delivered");
        let started_event = app_event_rx
            .recv()
            .await
            .expect("publish-start event should be emitted");
        let completed_event = app_event_rx
            .recv()
            .await
            .expect("publish-complete event should be emitted");

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::Workflow(message))
                if message == "Session must be in review to publish the review request."
        ));
        assert_eq!(
            response,
            Err(ag_session::SessionError::Operation(
                "Session must be in review to publish the review request.".to_string()
            ))
        );
        assert!(matches!(
            started_event,
            AppEvent::BranchPublishActionStarted { session_id } if session_id == "sess1"
        ));
        assert!(matches!(
            completed_event,
            AppEvent::BranchPublishActionCompleted { result, session_id }
                if result.is_err() && session_id == "sess1"
        ));
    }

    #[tokio::test]
    async fn test_skipped_review_request_command_answers_programmatic_caller() {
        // Arrange
        let (mut context, db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Review).await;
        db.operations()
            .insert_session_operation(
                "op-review-request",
                &context.session_id,
                CREATE_REVIEW_REQUEST_OPERATION_KIND,
            )
            .await
            .expect("review-request operation should be inserted");
        db.operations()
            .request_cancel_for_session_operations(&context.session_id)
            .await
            .expect("review-request operation should be canceled");
        let (response_tx, response_rx) = oneshot::channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;
        let command = SessionCommand::CreateReviewRequest {
            branch_publish_session: BranchPublishTaskSession {
                base_branch: "main".to_string(),
                folder: context.folder.clone(),
                id: context.session_id.clone(),
                published_upstream_ref: None,
                review_request: None,
                status: Status::Review,
            },
            operation_id: "op-review-request".to_string(),
            remote_branch_name: None,
            response: Some(Arc::new(Mutex::new(Some(response_tx)))),
        };

        // Act
        let command_result = SessionWorkerService::process_session_command(
            &context,
            &auto_commit_one_shot_client(),
            command,
        )
        .await;
        let response = response_rx
            .await
            .expect("skipped review-request response should be delivered");
        let app_event = app_event_rx
            .recv()
            .await
            .expect("skipped review-request should resolve its queued row");

        // Assert
        assert!(command_result.is_none());
        assert_eq!(
            response,
            Err(ag_session::SessionError::Operation(
                SKIPPED_CREATE_REVIEW_REQUEST_REASON.to_string()
            ))
        );
        assert!(matches!(
            app_event,
            AppEvent::BranchPublishActionResolved { session_id } if session_id == "sess1"
        ));
        assert!(app_event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_skipped_rebase_command_resolves_queued_sync() {
        // Arrange
        let (mut context, db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
        db.operations()
            .insert_session_operation("op-rebase", &context.session_id, REBASE_OPERATION_KIND)
            .await
            .expect("rebase operation should be inserted");
        db.operations()
            .request_cancel_for_session_operations(&context.session_id)
            .await
            .expect("rebase operation should be canceled");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;
        let command = SessionCommand::Rebase {
            base_branch: "main".to_string(),
            operation_id: "op-rebase".to_string(),
        };

        // Act
        let command_result = SessionWorkerService::process_session_command(
            &context,
            &auto_commit_one_shot_client(),
            command,
        )
        .await;
        let app_event = app_event_rx
            .recv()
            .await
            .expect("skipped rebase should resolve its queued row");

        // Assert
        assert!(command_result.is_none());
        assert!(matches!(
            app_event,
            AppEvent::SessionQueuedSyncResolved { session_id } if session_id == "sess1"
        ));
        assert!(app_event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_send_persisted_command_marks_failed_when_worker_receiver_is_closed() {
        // Arrange
        let database = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_test_session(&database).await;
        database
            .operations()
            .insert_session_operation("rollup-failed", "sess1", "reply")
            .await
            .expect("failed to insert operation");
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let mut worker_service = SessionWorkerService::new();

        // Act
        let result = worker_service
            .send_persisted_command(
                database.operations(),
                &SessionId::from("sess1"),
                sender,
                ScheduledSessionCommand::immediate(resume_command("rollup-failed")),
            )
            .await;
        let unfinished = database
            .operations()
            .is_session_operation_unfinished("rollup-failed")
            .await
            .expect("failed to inspect operation");

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::Workflow(error))
                if error == "Session worker is not available"
        ));
        assert!(!unfinished);
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
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
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
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        };

        // Act
        let items = agent_response.question_items();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, numbered_questions);
    }

    #[test]
    /// Ensures assistant message content prefers `answer` messages when
    /// available.
    fn test_build_assistant_message_content_prefers_answer_messages() {
        // Arrange
        let response = AgentResponse {
            answer: "Implemented the fix.".to_string(),
            questions: vec![QuestionItem::new("Need me to run tests?")],
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        };

        // Act
        let message_content = build_assistant_message_content(&response);

        // Assert
        assert_eq!(
            message_content,
            Some("Implemented the fix.\n\n".to_string())
        );
    }

    #[test]
    /// Ensures assistant message content falls back to question text when no
    /// answers are present.
    fn test_build_assistant_message_content_falls_back_to_question_text() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::new("Should I apply the patch?")],
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        };

        // Act
        let message_content = build_assistant_message_content(&response);

        // Assert
        assert_eq!(
            message_content,
            Some("Should I apply the patch?\n\n".to_string())
        );
    }

    #[test]
    /// Ensures blank protocol messages do not append empty transcript messages.
    fn test_build_assistant_message_content_returns_none_for_blank_messages() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::new("\n")],
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        };

        // Act
        let message_content = build_assistant_message_content(&response);

        // Assert
        assert_eq!(message_content, None);
    }

    #[tokio::test]
    async fn test_run_channel_turn_finalizes_invalid_permission_setup_failure() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess1", "gemini-3.8-flash", "main", "Question", project_id)
            .await
            .expect("failed to insert question session");
        db.sessions()
            .update_session_questions("sess1", r#"[{"text":"Continue?"}]"#)
            .await
            .expect("failed to persist questions");
        sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'sess1'")
            .execute(&pool)
            .await
            .expect("failed to corrupt permission mode");

        let attachment_path = crate::app::agentty_home()
            .join("tmp")
            .join("sess1")
            .join("images")
            .join("image-1.png");
        let image_directory = attachment_path
            .parent()
            .expect("attachment should have a parent")
            .to_path_buf();
        let mut fs_client = mock_fs_client_with_existing_directories();
        let expected_attachment_path = attachment_path.clone();
        fs_client
            .expect_remove_file()
            .once()
            .withf(move |path| path == &expected_attachment_path)
            .returning(|_| Box::pin(async { Ok(()) }));
        fs_client
            .expect_remove_dir()
            .once()
            .withf(move |path| path == &image_directory)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_git_client = MockGitClient::new();
        expect_clean_main_checkout_snapshot(&mut mock_git_client, base_dir.path().join("main"));
        let mut mock_channel = MockAgentChannel::new();
        mock_channel.expect_run_turn().times(0);
        let transcript = empty_transcript();
        let status = Arc::new(Mutex::new(Status::Question));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs_client),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
            status: Arc::clone(&status),
        };
        let prompt = turn_prompt_with_attachment(attachment_path);

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionResume,
            None,
            prompt,
            None,
        )
        .await;
        let persisted_session = db
            .sessions()
            .load_session("sess1")
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        let error = result.expect_err("invalid permission mode should fail the turn");
        assert!(
            error
                .to_string()
                .contains("Unknown permission mode: invalid")
        );
        assert!(transcript_text(&transcript).contains("Unknown permission mode: invalid"));
        assert_eq!(
            *status.lock().expect("status lock poisoned"),
            Status::Review
        );
        assert_eq!(persisted_session.status, "Review");
    }

    #[tokio::test]
    /// Verifies process-only events do not append transcript content.
    async fn test_consume_turn_events_ignores_pid_only_events_for_transcript_messages() {
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
            "session-1".into(),
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
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_research_session(&db).await;
        db.sessions()
            .update_session_provisional_title("sess1", "test prompt")
            .await
            .expect("failed to persist provisional research title");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .withf(|_session_id, request, _events| {
                request.permission_mode == PermissionMode::ReadOnly
                    && !request.prompt.text.contains("# Read Only Mode")
            })
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_hours(1)).await;
                    unreachable!("should be cancelled before completing")
                })
            });
        mock_channel
            .expect_shutdown_session()
            .times(1)
            .returning(|_| Box::pin(async { Ok(()) }));

        let mut mock_git_client = MockGitClient::new();
        expect_clean_main_checkout_snapshot(&mut mock_git_client, base_dir.path().join("main"));

        let cancel_token = Arc::new(Mutex::new(CancellationToken::new()));
        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::clone(&cancel_token),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        cancel_token_after_short_delay(Arc::clone(&cancel_token));

        // Act
        let result = run_channel_turn(
            &context,
            research_title_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
            None,
        )
        .await;

        // Assert
        let error_message = result.expect_err("should return an error").to_string();
        assert!(
            error_message.contains("[Stopped]"),
            "error should contain [Stopped], got: {error_message}"
        );
        let output_text = transcript_text(&transcript);
        assert!(
            output_text.contains("[Stopped]"),
            "stopped message should be appended to transcript, got: {output_text}"
        );
        assert_eq!(
            *context.status.lock().expect("status lock poisoned"),
            Status::InProgress,
            "stopped turn worker must not fall back to Review before the UI cancellation path \
             finalizes Canceled"
        );
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");
        assert_eq!(
            sessions[0].status, "InProgress",
            "stopped turn worker must not persist Review and trigger automatic focused review"
        );
        assert_eq!(
            sessions[0].title.as_deref(),
            Some("Inspect architecture boundaries")
        );
    }

    #[tokio::test]
    /// Verifies worker preflight renews a previous turn's cancelled token while
    /// preserving the persisted read-only permission for the new operation.
    async fn test_worker_proceeds_read_only_after_previous_cancellation() {
        // Arrange — pre-cancel the token to simulate a previous turn's
        // cancellation. Worker preflight renews it for this new operation.
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_permission_mode("sess1", PermissionMode::ReadOnly)
            .await
            .expect("failed to set read-only permission mode");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .once()
            .withf(|_session_id, request, _events| {
                request.permission_mode == PermissionMode::ReadOnly
                    && request.prompt.text.contains("# Read Only Mode")
            })
            .returning(|_session_id, _req, _events| {
                Box::pin(async { Ok(successful_turn_result("done")) })
            });

        let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        // Pre-cancel the token to simulate a previous turn's cancellation.
        let stale_token = CancellationToken::new();
        stale_token.cancel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(stale_token)),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        db.operations()
            .insert_session_operation("new-turn", "sess1", "start_prompt")
            .await
            .expect("new operation");
        let command = SessionCommand::Run {
            operation_id: "new-turn".to_string(),
            preparation: None,
            prompt: "test prompt".into(),
            replay_transcript: None,
            request_kind: AgentRequestKind::SessionStart,
            turn_metadata: default_turn_metadata(),
        };

        // Act
        let result = SessionWorkerService::process_session_command(
            &context,
            &auto_commit_one_shot_client(),
            command,
        )
        .await
        .expect("new operation runs");

        // Assert — turn succeeded despite the stale cancellation.
        assert!(
            result.is_ok(),
            "stale cancelled token should not cancel the new turn"
        );
    }

    #[tokio::test]
    /// Verifies a turn that dirties the main checkout records a warning while
    /// preserving the successful agent response.
    async fn test_run_channel_turn_warns_when_main_checkout_status_changes() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_test_session(&db).await;

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .once()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            questions: Vec::new(),
                            review_comment_outcomes: Vec::new(),
                            subtasks: Vec::new(),
                            verification_verdicts: Vec::new(),
                        },
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });

        let status_call_count = Arc::new(Mutex::new(0));
        let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(move |_| {
                let status_call_count = Arc::clone(&status_call_count);

                Box::pin(async move {
                    let mut call_count = status_call_count
                        .lock()
                        .expect("status call count lock poisoned");
                    *call_count += 1;
                    if *call_count == 1 {
                        Ok(String::new())
                    } else {
                        Ok(" M README.md\n".to_string())
                    }
                })
            });
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
            None,
        )
        .await;

        // Assert
        assert!(result.is_ok(), "main checkout changes should warn only");
        let output_text = transcript_text(&transcript);
        assert!(output_text.contains("[Main Checkout Warning]"));
        assert!(output_text.contains("tracked-file status changed"));
        assert!(output_text.contains("done"));
    }

    #[tokio::test]
    /// Verifies a clean post-turn tracked status completes without warning,
    /// even when pre-turn status was dirty.
    async fn test_run_channel_turn_skips_warning_when_main_checkout_is_clean_after_turn() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_test_session(&db).await;

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .once()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            questions: Vec::new(),
                            review_comment_outcomes: Vec::new(),
                            subtasks: Vec::new(),
                            verification_verdicts: Vec::new(),
                        },
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });

        let status_call_count = Arc::new(Mutex::new(0));
        let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(move |_| {
                let status_call_count = Arc::clone(&status_call_count);

                Box::pin(async move {
                    let mut call_count = status_call_count
                        .lock()
                        .expect("status call count lock poisoned");
                    *call_count += 1;
                    if *call_count == 1 {
                        Ok(" M README.md\n".to_string())
                    } else {
                        Ok(String::new())
                    }
                })
            });
        mock_git_client.expect_head_hash().times(0);
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
            None,
        )
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "clean post-turn tracked status should complete"
        );
        let output_text = transcript_text(&transcript);
        assert!(!output_text.contains("[Main Checkout Warning]"));
        assert!(output_text.contains("done"));
    }

    #[tokio::test]
    /// Verifies an unchanged pre-existing dirty tracked status completes
    /// without warning.
    async fn test_run_channel_turn_skips_warning_when_main_checkout_stays_dirty() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_test_session(&db).await;

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .once()
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            questions: Vec::new(),
                            review_comment_outcomes: Vec::new(),
                            subtasks: Vec::new(),
                            verification_verdicts: Vec::new(),
                        },
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        provider_conversation_id: None,
                    })
                })
            });

        let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(|_| Box::pin(async { Ok(" M README.md\n".to_string()) }));
        mock_git_client.expect_head_hash().times(0);
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
            None,
        )
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "unchanged dirty tracked status should complete"
        );
        let output_text = transcript_text(&transcript);
        assert!(!output_text.contains("[Main Checkout Warning]"));
        assert!(output_text.contains("done"));
    }

    #[tokio::test]
    /// Verifies a bare shared repository (no main working checkout) skips the
    /// main-checkout status snapshot: `tracked_worktree_status` is never called
    /// and the turn proceeds with `main_checkout_root` set to `None`.
    async fn test_run_channel_turn_skips_main_checkout_snapshot_for_bare_repo() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_test_session(&db).await;

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .once()
            .withf(|_session_id, request, _events| request.main_checkout_root.is_none())
            .returning(|_session_id, _req, _events| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "done".to_string(),
                            questions: Vec::new(),
                            review_comment_outcomes: Vec::new(),
                            subtasks: Vec::new(),
                            verification_verdicts: Vec::new(),
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
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_checkout_working_tree()
            .once()
            .returning(|_| Box::pin(async { Ok(None) }));
        mock_git_client.expect_tracked_worktree_status().times(0);
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
            None,
        )
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "bare shared repository turn should complete without a main-checkout snapshot"
        );
        let output_text = transcript_text(&transcript);
        assert!(!output_text.contains("[Main Checkout Warning]"));
        assert!(output_text.contains("done"));
    }

    #[tokio::test]
    async fn test_run_channel_turn_persists_failure_when_main_checkout_snapshot_fails() {
        // Arrange
        let (mut context, _db, _queue, base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
        let main_repo_root = base_dir.path().join("main");
        let mut mock_git_client = mock_git_client_detecting_main_repo(main_repo_root);
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_tracked_worktree_status()
            .once()
            .returning(|_| {
                Box::pin(async {
                    Err(ag_git::GitError::CommandFailed {
                        command: "git status".to_string(),
                        stderr: "status failed".to_string(),
                    })
                })
            });
        context.git_client = Arc::new(mock_git_client);

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_one_shot_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
            None,
        )
        .await;

        // Assert
        assert!(matches!(result, Err(SessionError::Workflow(_))));
        assert!(transcript_text(&context.transcript).contains("status failed"));
    }

    /// Builds the default turn metadata used by session worker tests that
    /// exercise the `Gemini38Flash` path without branch publication.
    fn default_turn_metadata() -> TurnMetadata {
        TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
        }
    }

    #[tokio::test]
    async fn test_resolve_turn_personality_marks_new_and_unchanged_prompts() {
        // Arrange
        let (mut context, db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
        db.sessions()
            .update_session_personality_id("sess1", Some("reviewer".to_string()))
            .await
            .expect("personality selection should persist");
        let personality = Personality {
            description: "Reviews code".to_string(),
            id: "reviewer".to_string(),
            name: "Code Reviewer".to_string(),
            prompt: "Review carefully.".to_string(),
        };
        let fingerprint = personality.fingerprint();
        let mut personality_catalog_client = MockPersonalityCatalogClient::new();
        personality_catalog_client
            .expect_resolve()
            .times(2)
            .returning(move |_, _| {
                let personality = personality.clone();
                Box::pin(async move { Some(personality) })
            });
        context.personality_catalog_client = Arc::new(personality_catalog_client);

        // Act
        let changed = resolve_turn_personality(&context).await;
        persist_test_personality_state(&db, changed.persistence.clone()).await;
        let unchanged = resolve_turn_personality(&context).await;

        // Assert
        assert_eq!(
            changed.prompt,
            ag_agent::PersonalityPrompt::active("Review carefully.".to_string(), true)
        );
        assert_eq!(
            unchanged.prompt,
            ag_agent::PersonalityPrompt::active("Review carefully.".to_string(), false)
        );
        assert_eq!(
            unchanged.persistence,
            TurnPersonalityPersistence {
                applied_personality_id: Some("reviewer".to_string()),
                applied_personality_prompt_hash: Some(fingerprint),
            }
        );
    }

    #[tokio::test]
    async fn test_resolve_turn_personality_reports_unavailable_profile_once_and_clears_prior() {
        // Arrange
        let (mut context, db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
        db.sessions()
            .update_session_personality_id("sess1", Some("missing".to_string()))
            .await
            .expect("personality selection should persist");
        persist_test_personality_state(
            &db,
            TurnPersonalityPersistence {
                applied_personality_id: Some("reviewer".to_string()),
                applied_personality_prompt_hash: Some("prior-hash".to_string()),
            },
        )
        .await;
        let mut personality_catalog_client = MockPersonalityCatalogClient::new();
        personality_catalog_client
            .expect_resolve()
            .times(2)
            .returning(|_, _| Box::pin(async { None }));
        context.personality_catalog_client = Arc::new(personality_catalog_client);

        // Act
        let first_resolution = resolve_turn_personality(&context).await;
        persist_test_personality_state(&db, first_resolution.persistence.clone()).await;
        let second_resolution = resolve_turn_personality(&context).await;
        let transcript = transcript_text(&context.transcript);

        // Assert
        assert_eq!(
            first_resolution.prompt,
            ag_agent::PersonalityPrompt::cleared(true)
        );
        assert_eq!(
            second_resolution.prompt,
            ag_agent::PersonalityPrompt::cleared(false)
        );
        assert_eq!(
            second_resolution.persistence,
            TurnPersonalityPersistence {
                applied_personality_id: Some("missing".to_string()),
                applied_personality_prompt_hash: None,
            }
        );
        assert_eq!(transcript.matches("is unavailable").count(), 1);
    }

    #[tokio::test]
    async fn test_resolve_turn_personality_clears_removed_selection() {
        // Arrange
        let (context, db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
        persist_test_personality_state(
            &db,
            TurnPersonalityPersistence {
                applied_personality_id: Some("reviewer".to_string()),
                applied_personality_prompt_hash: Some("prior-hash".to_string()),
            },
        )
        .await;

        // Act
        let resolution = resolve_turn_personality(&context).await;

        // Assert
        assert_eq!(
            resolution.prompt,
            ag_agent::PersonalityPrompt::cleared(true)
        );
        assert_eq!(
            resolution.persistence,
            TurnPersonalityPersistence::default()
        );
    }

    #[tokio::test]
    async fn test_resolve_turn_personality_defaults_when_session_state_is_unavailable() {
        // Arrange
        let (mut context, _db, _queue, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::InProgress).await;
        context.session_id = "missing-session".into();

        // Act
        let missing_session = resolve_turn_personality(&context).await;
        let (closed_db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        pool.close().await;
        context.db = closed_db;
        let query_failure = resolve_turn_personality(&context)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert_eq!(
            missing_session.prompt,
            ag_agent::PersonalityPrompt::default()
        );
        assert_eq!(
            missing_session.persistence,
            TurnPersonalityPersistence::default()
        );
        assert_eq!(query_failure.prompt, ag_agent::PersonalityPrompt::default());
        assert_eq!(
            query_failure.persistence,
            TurnPersonalityPersistence::default()
        );
    }

    /// Persists one successful personality-delivery marker for resolver tests.
    async fn persist_test_personality_state(
        db: &AppRepositories,
        personality: TurnPersonalityPersistence,
    ) {
        db.sessions()
            .persist_session_turn_metadata(
                "sess1",
                &SessionTurnMetadata {
                    applied_personality_id: personality.applied_personality_id,
                    applied_personality_prompt_hash: personality.applied_personality_prompt_hash,
                    instruction_conversation_id: None,
                    model: AgentModel::Gemini38Flash.as_str().to_string(),
                    provider_conversation_id: None,
                    questions_json: "[]".to_string(),
                    review_comment_resolutions: Vec::new(),
                    token_usage_delta: SessionStats::default(),
                },
            )
            .await
            .expect("personality application should persist");
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
        let mut unrelated_child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("start unrelated process");
        let recycled_pid = unrelated_child.id().expect("unrelated PID");
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();

        let mut mock_channel = MockAgentChannel::new();
        // `run_turn` must NOT be called — the early-exit path returns
        // before reaching the select.
        mock_channel.expect_run_turn().never();
        mock_channel
            .expect_shutdown_session()
            .times(2)
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(Some(recycled_pid))),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await.expect("db should open"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-preturn".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        let req = TurnRequest {
            continuation: TurnContinuation::fresh(),
            folder: context.folder.clone(),
            main_checkout_root: None,
            model: "gemini-3.8-flash".to_string(),
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality: ag_agent::PersonalityPrompt::default(),
            prompt: "test".into(),
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::SessionStart,
            response_style: ag_agent::ResponseStyle::default(),
            speed_mode: crate::domain::agent::SpeedMode::default(),
        };

        // Act — pass the pre-cancelled token directly.
        let result = run_turn_with_cancellation(
            &context,
            cancel_token.clone(),
            req.clone(),
            mpsc::unbounded_channel().0,
        )
        .await;

        *context.child_pid.lock().expect("PID slot") = Some(recycled_pid);
        let assist = SessionWorkerRebaseAssistClient::from_context(&context, None);
        let assist_result = assist
            .run_turn_with_cancellation(cancel_token, req, mpsc::unbounded_channel().0)
            .await;

        // Assert — a retained runtime's recycled PID never authorizes a signal.
        assert!(
            assist_result
                .expect_err("assist canceled")
                .to_string()
                .contains("[Stopped]")
        );
        assert!(context.child_pid.lock().expect("PID slot").is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), unrelated_child.wait())
                .await
                .is_err()
        );
        unrelated_child
            .kill()
            .await
            .expect("clean up owned process");
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
        for rebase_assist in [false, true] {
            // Arrange — mock channel whose `run_turn` never resolves and
            // whose `shutdown_session` completes immediately (simulating a
            // channel that ignores the shutdown request).
            let mut unrelated_child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .expect("start unrelated process");
            let recycled_pid = unrelated_child.id().expect("unrelated PID");
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
                branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
                cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
                channel: Arc::new(mock_channel),
                child_pid: Arc::new(Mutex::new(Some(recycled_pid))),
                clock: Arc::new(crate::infra::clock::RealClock),
                db: AppRepositories::in_memory().await.expect("db should open"),
                folder: std::env::temp_dir(),
                fs_client: Arc::new(fs::MockFsClient::new()),
                git_client: Arc::new(MockGitClient::new()),
                transcript: empty_transcript(),
                personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
                queued_messages: Arc::new(Mutex::new(VecDeque::new())),
                review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

                session_update_versions: Arc::default(),
                session_id: "sess-timeout".into(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Antigravity,
                    AgentModel::Gemini38Flash,
                ),
                status: Arc::new(Mutex::new(Status::InProgress)),
            };

            let req = TurnRequest {
                continuation: TurnContinuation::fresh(),
                folder: context.folder.clone(),
                main_checkout_root: None,
                model: "gemini-3.8-flash".to_string(),
                permission_mode: ag_agent::PermissionMode::AutoEdit,
                personality: ag_agent::PersonalityPrompt::default(),
                prompt: "test".into(),
                reasoning_level: ReasoningLevel::default(),
                request_kind: AgentRequestKind::SessionStart,
                response_style: ag_agent::ResponseStyle::default(),
                speed_mode: crate::domain::agent::SpeedMode::default(),
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
            let result = if rebase_assist {
                SessionWorkerRebaseAssistClient::from_context(&context, None)
                    .run_turn_with_cancellation(cancel_token, req, mpsc::unbounded_channel().0)
                    .await
            } else {
                run_turn_with_cancellation(&context, cancel_token, req, mpsc::unbounded_channel().0)
                    .await
            };

            // Assert — cancellation cannot signal a retained runtime's recycled PID.
            assert!(
                unrelated_child
                    .try_wait()
                    .expect("poll unrelated process")
                    .is_none()
            );
            unrelated_child
                .kill()
                .await
                .expect("clean up owned process");
            let error_message = result.expect_err("should return an error").to_string();
            assert!(
                error_message.contains("[Stopped]"),
                "error should contain [Stopped], got: {error_message}"
            );
        }
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
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(Some(child_pid))),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await.expect("db should open"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-term".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeHaiku4520251001,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        terminate_child_process(&context.child_pid, context.session_agent.kind());

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
    /// PID is stored for a CLI channel.
    async fn test_terminate_child_process_noop_when_no_pid() {
        // Arrange
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await.expect("db should open"),
            folder: std::env::temp_dir(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess-nopid".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeHaiku4520251001,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act — should not panic or error.
        terminate_child_process(&context.child_pid, context.session_agent.kind());

        // Assert — PID slot remains None.
        assert!(
            context.child_pid.lock().expect("child_pid lock").is_none(),
            "PID slot should still be None"
        );
    }

    #[tokio::test]
    /// Verifies thought deltas update the loader state without appending
    /// transcript messages.
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
        consume_turn_events(event_rx, app_event_tx, "session-1".into(), child_pid).await;

        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            events,
            vec![
                AppEvent::SessionProgressUpdated {
                    progress_message: Some("Inspecting files".to_string()),
                    session_id: "session-1".into(),
                },
                AppEvent::SessionProgressUpdated {
                    progress_message: None,
                    session_id: "session-1".into(),
                },
            ]
        );
    }

    #[tokio::test]
    /// Verifies ready thought-delta bursts enqueue only the latest progress
    /// update before the final clear event.
    async fn test_consume_turn_events_coalesces_ready_thought_delta_bursts() {
        // Arrange
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let child_pid = Arc::new(Mutex::new(None));

        for thought in ["first", "second", "third"] {
            event_tx
                .send(TurnEvent::ThoughtDelta(thought.to_string()))
                .expect("failed to send thought delta");
        }
        drop(event_tx);

        // Act
        consume_turn_events(event_rx, app_event_tx, "session-1".into(), child_pid).await;

        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            events,
            vec![
                AppEvent::SessionProgressUpdated {
                    progress_message: Some("third".to_string()),
                    session_id: "session-1".into(),
                },
                AppEvent::SessionProgressUpdated {
                    progress_message: None,
                    session_id: "session-1".into(),
                },
            ]
        );
    }

    #[tokio::test]
    /// Verifies completed turns keep a linked open PR/MR title and
    /// description aligned with the latest session commit message.
    async fn test_apply_turn_result_syncs_linked_review_request_metadata_after_commit() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let folder = base_dir.path().join("sess1");
        let commit_message =
            "Refine review metadata sync\n\n- Update the linked review request body.";
        let mut sequence = Sequence::new();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder,
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(auto_commit_git_client(commit_message, &mut sequence)),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(review_metadata_sync_client(
                base_dir.path(),
                &mut sequence,
            )),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = successful_turn_result("Implemented the change.");

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Antigravity,
                    crate::domain::agent::AgentModel::Gemini38Flash,
                ),
            },
            Ok(turn_result),
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
        let review_request = db
            .reviews()
            .load_session_review_request("sess1")
            .await
            .expect("failed to load review request")
            .expect("review request should remain linked");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            sync_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Succeeded,
            ]
        );
        assert_eq!(review_request.title, "Old title");
    }

    #[tokio::test]
    /// Verifies failed post-turn auto-push skips linked PR/MR metadata sync.
    async fn test_apply_turn_result_skips_review_request_metadata_sync_when_auto_push_fails() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let commit_message =
            "Refine review metadata sync\n\n- Update the linked review request body.";
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(auto_commit_git_client_with_push_failure(commit_message)),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                review_comment_thread_ids: Vec::new(),
                session_agent: AgentSelection::new(
                    crate::domain::agent::AgentKind::Antigravity,
                    crate::domain::agent::AgentModel::Gemini38Flash,
                ),
            },
            Ok(successful_turn_result("Implemented the change.")),
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
        let review_request = db
            .reviews()
            .load_session_review_request("sess1")
            .await
            .expect("failed to load review request")
            .expect("review request should remain linked");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(
            sync_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Failed,
            ]
        );
        assert_eq!(review_request.title, "Old title");
    }

    /// Inserts an in-progress session linked to an open GitHub review request.
    async fn insert_in_progress_session_with_review_request(db: &AppRepositories) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.reviews()
            .update_session_review_request("sess1", Some(linked_github_review_request()))
            .await
            .expect("failed to persist review request");
    }

    /// Returns one linked GitHub review request fixture for metadata sync
    /// tests.
    fn linked_github_review_request() -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: 100,
            summary: forge::ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: forge::ForgeKind::GitHub,
                source_branch: "wt/session-id".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Draft".to_string()),
                target_branch: "main".to_string(),
                title: "Old title".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        }
    }

    /// Expects the branch-publish safety preflight used by published-branch
    /// auto-push.
    fn expect_safe_auto_push_state(mock_git_client: &mut MockGitClient) {
        mock_git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(None) }));
        mock_git_client
            .expect_detect_git_info()
            .once()
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    }

    /// Expects one successful advisory pre-commit readiness check.
    fn expect_pre_commit_hook_ready(mock_git_client: &mut MockGitClient) {
        mock_git_client
            .expect_check_pre_commit_hook_ready()
            .once()
            .returning(|_| Box::pin(async { Ok(()) }));
    }

    /// Proves a later successful push performs no review-thread effects.
    async fn assert_later_push_skips_review_operations(context: &SessionWorkerContext) {
        let mut git_client = MockGitClient::new();
        expect_safe_auto_push_state(&mut git_client);
        git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        git_client.expect_repo_url().never();
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        published_branch::run_published_branch_auto_push(
            published_branch::PublishedBranchAutoPushInput {
                app_event_tx,
                db: context.db.clone(),
                folder: context.folder.clone(),
                git_client: Arc::new(git_client),
                published_upstream_ref: "origin/wt/session-id".to_string(),
                review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
                review_request_metadata_sync: None,
                session_id: context.session_id.clone(),
                session_update_versions: context.session_update_versions.clone(),
                sync_operation_id: "later-push".to_string(),
                transcript: Arc::clone(&context.transcript),
            },
        )
        .await;
        let event = app_event_rx
            .recv()
            .await
            .expect("later push should report completion");

        assert!(matches!(
            event,
            AppEvent::PublishedBranchSyncUpdated {
                sync_status: PublishedBranchSyncStatus::Succeeded,
                ..
            }
        ));
    }

    /// Pushes a descendant commit that has reverted the reported fix.
    async fn push_descendant_that_reverted_fix(context: &SessionWorkerContext) {
        let mut git_client = MockGitClient::new();
        expect_safe_auto_push_state(&mut git_client);
        git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        git_client
            .expect_get_ref_ahead_behind()
            .once()
            .withf(|_, left_ref, right_ref| left_ref == "HEAD" && right_ref == "fix-commit")
            .returning(|_, _, _| Box::pin(async { Ok((1, 0)) }));
        git_client.expect_repo_url().never();

        published_branch::run_published_branch_auto_push(
            published_branch::PublishedBranchAutoPushInput {
                app_event_tx: context.app_event_tx.clone(),
                db: context.db.clone(),
                folder: context.folder.clone(),
                git_client: Arc::new(git_client),
                published_upstream_ref: "origin/wt/session-id".to_string(),
                review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
                review_request_metadata_sync: None,
                session_id: context.session_id.clone(),
                session_update_versions: context.session_update_versions.clone(),
                sync_operation_id: "push-after-revert".to_string(),
                transcript: Arc::clone(&context.transcript),
            },
        )
        .await;
    }

    /// Returns one git client mock through a successful dirty-worktree commit.
    fn dirty_auto_commit_git_client(commit_message: &str) -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
        expect_pre_commit_hook_ready(&mut mock_git_client);
        mock_git_client
            .expect_is_worktree_clean()
            .once()
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_diff()
            .once()
            .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
        mock_git_client
            .expect_has_commits_since()
            .once()
            .returning(|_, _| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_commit_message()
            .once()
            .returning({
                let commit_message = commit_message.to_string();

                move |_| {
                    let commit_message = commit_message.clone();

                    Box::pin(async move { Ok(Some(commit_message)) })
                }
            });
        mock_git_client
            .expect_commit_all_preserving_single_commit()
            .once()
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_head_short_hash()
            .once()
            .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));

        mock_git_client
    }

    /// Returns one git client mock that produces a successful auto-commit
    /// outcome.
    fn auto_commit_git_client(commit_message: &str, sequence: &mut Sequence) -> MockGitClient {
        let mut mock_git_client = dirty_auto_commit_git_client(commit_message);
        expect_safe_auto_push_state(&mut mock_git_client);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
            })
            .in_sequence(sequence)
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        mock_git_client
            .expect_repo_url()
            .once()
            .in_sequence(sequence)
            .returning(|_| {
                Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
            });

        mock_git_client
    }

    /// Returns one git client that proves the branch push precedes forge
    /// review-thread mutations.
    fn review_resolution_git_client(sequence: &mut Sequence) -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .once()
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_head_hash()
            .once()
            .in_sequence(sequence)
            .returning(|_| Box::pin(async { Ok("commit-1".to_string()) }));
        expect_safe_auto_push_state(&mut mock_git_client);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .in_sequence(sequence)
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        mock_git_client
            .expect_get_ref_ahead_behind()
            .once()
            .in_sequence(sequence)
            .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
        mock_git_client
            .expect_repo_url()
            .once()
            .in_sequence(sequence)
            .returning(|_| {
                Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
            });

        mock_git_client
    }

    /// Returns one forge client that replies to and resolves the expected
    /// review thread after the sequenced push.
    fn review_resolution_client(
        folder: PathBuf,
        sequence: &mut Sequence,
    ) -> forge::MockReviewRequestClient {
        let mut review_request_client = forge::MockReviewRequestClient::new();
        review_request_client
            .expect_detect_remote()
            .once()
            .in_sequence(sequence)
            .returning(|_| Ok(github_forge_remote()));
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .once()
            .in_sequence(sequence)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(forge::ReviewCommentSnapshot {
                        pr_level_comments: Vec::new(),
                        threads: vec![forge::ReviewCommentThread {
                            anchor_side: forge::ReviewCommentAnchorSide::New,
                            comments: Vec::new(),
                            id: "thread-42".to_string(),
                            is_outdated: Some(false),
                            is_resolved: false,
                            line: Some(1),
                            path: "src/lib.rs".to_string(),
                            start_line: None,
                        }],
                    })
                })
            });
        review_request_client
            .expect_reply_to_thread()
            .once()
            .in_sequence(sequence)
            .withf(move |remote, display_id, thread_id, body| {
                remote.command_working_directory.as_deref() == Some(folder.as_path())
                    && display_id == "#42"
                    && thread_id == "thread-42"
                    && body.starts_with(
                        "Added the missing validation.\n\n<!-- agentty review resolution:",
                    )
                    && body.ends_with(" -->")
            })
            .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
        review_request_client
            .expect_resolve_thread()
            .once()
            .in_sequence(sequence)
            .withf(|_, display_id, thread_id| display_id == "#42" && thread_id == "thread-42")
            .returning(|_, _, _| Box::pin(async { Ok(()) }));

        review_request_client
    }

    /// Returns one git client mock that commits successfully but fails the
    /// follow-up auto-push.
    fn auto_commit_git_client_with_push_failure(commit_message: &str) -> MockGitClient {
        let mut mock_git_client = dirty_auto_commit_git_client(commit_message);
        expect_safe_auto_push_state(&mut mock_git_client);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
            })
            .returning(|_, _| {
                Box::pin(async {
                    Err(ag_git::GitError::CommandFailed {
                        command: "git push origin wt/session-id".to_string(),
                        stderr: "fatal: remote rejected the push".to_string(),
                    })
                })
            });

        mock_git_client
    }

    /// Returns one review-request client mock that expects the latest commit
    /// message metadata.
    fn review_metadata_sync_client(
        base_dir: &std::path::Path,
        sequence: &mut Sequence,
    ) -> forge::MockReviewRequestClient {
        let folder = base_dir.join("sess1");
        let mut mock_review_request_client = forge::MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .once()
            .in_sequence(sequence)
            .returning(|_| Ok(github_forge_remote()));
        mock_review_request_client
            .expect_review_request_metadata()
            .once()
            .in_sequence(sequence)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(forge::ReviewRequestMetadata {
                        body: "Old body".to_string(),
                        title: "Old title".to_string(),
                    })
                })
            });
        mock_review_request_client
            .expect_sync_review_request_metadata()
            .once()
            .in_sequence(sequence)
            .withf(move |remote, display_id, input| {
                remote.command_working_directory.as_deref() == Some(folder.as_path())
                    && display_id == "#42"
                    && input.title.as_ref().is_some_and(|title| {
                        title.current == "Old title" && title.desired == "Old title"
                    })
                    && input.body.as_ref().is_some_and(|body| {
                        body.current == "Old body"
                            && body.desired
                                == "Old body\n\n- Update the linked review request body."
                    })
            })
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(forge::ReviewRequestSummary {
                        display_id: "#42".to_string(),
                        forge_kind: forge::ForgeKind::GitHub,
                        source_branch: "wt/session-id".to_string(),
                        state: ReviewRequestState::Open,
                        status_summary: Some("Draft".to_string()),
                        target_branch: "main".to_string(),
                        title: "Old title".to_string(),
                        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                    })
                })
            });

        mock_review_request_client
    }

    /// Returns one GitHub forge remote fixture for worker metadata sync tests.
    fn github_forge_remote() -> forge::ForgeRemote {
        forge::ForgeRemote {
            command_working_directory: None,
            forge_kind: forge::ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Returns one successful turn result with the provided answer text.
    fn successful_turn_result(answer: &str) -> TurnResult {
        TurnResult {
            assistant_message: AgentResponse {
                answer: answer.to_string(),
                questions: Vec::new(),
                review_comment_outcomes: Vec::new(),
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        }
    }

    /// Returns one completed turn that reports a fixed review thread.
    fn fixed_review_turn_result() -> TurnResult {
        TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: vec![ReviewCommentOutcome {
                    reply: "Added the missing validation.".to_string(),
                    resolution: ReviewCommentResolution::Fixed,
                    thread_id: "thread-42".to_string(),
                }],
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        }
    }

    #[tokio::test]
    /// Verifies completed turns auto-push already-published session branches
    /// in the background and report sync progress through app events.
    async fn test_apply_turn_result_starts_background_push_for_published_branch() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        expect_safe_auto_push_state(&mut mock_git_client);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .withf(|folder, remote_branch_name| {
                folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
            })
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(successful_turn_result("Implemented the change."));

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: Vec::new(),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
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
                    ..
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
        assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
        assert_eq!(sync_events[0].0, "sess1");
        assert_eq!(sync_events[1].0, "sess1");
        assert_eq!(sync_events[0].1, sync_events[1].1);
    }

    #[tokio::test]
    /// Verifies fixed, allowlisted outcomes are replied to and resolved only
    /// after the completed turn reaches the published branch.
    async fn test_apply_turn_result_resolves_fixed_review_threads_after_push() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let folder = base_dir.path().join("sess1");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut sequence = Sequence::new();
        let mock_git_client = review_resolution_git_client(&mut sequence);
        let review_request_client = review_resolution_client(folder.clone(), &mut sequence);
        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(review_request_client),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = fixed_review_turn_result();

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                review_comment_thread_ids: vec!["thread-42".to_string()],
                session_agent,
            },
            Ok(turn_result),
        )
        .await
        .expect("turn result should succeed");
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut completed = false;
            while !completed {
                let event = app_event_rx.recv().await.expect("missing app event");
                completed = matches!(
                    event,
                    AppEvent::PublishedBranchSyncUpdated {
                        sync_status: PublishedBranchSyncStatus::Succeeded,
                        ..
                    }
                );
            }
        })
        .await
        .expect("timed out waiting for completed branch sync");
        let notice = transcript
            .lock()
            .expect("transcript lock should be available")
            .messages()
            .last()
            .expect("resolution notice should be appended")
            .content
            .clone();
        let unfinished_operations = context
            .db
            .reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("failed to load completed review-comment operations");

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(unfinished_operations, Vec::new());
        assert_eq!(
            notice.trim(),
            "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
        );
    }

    #[tokio::test]
    async fn test_failed_push_discards_review_fix_undone_by_descendant() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut git_client = auto_commit_git_client_with_push_failure("Fix the review comment");
        git_client
            .expect_head_hash()
            .once()
            .returning(|_| Box::pin(async { Ok("fix-commit".to_string()) }));
        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = fixed_review_turn_result();

        // Act
        apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                review_comment_thread_ids: vec!["thread-42".to_string()],
                session_agent,
            },
            Ok(turn_result),
        )
        .await
        .expect("turn result should succeed");
        let first_push_events = tokio::time::timeout(Duration::from_secs(1), async {
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
        .expect("timed out waiting for failed push");
        let pending_operations = db
            .reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("failed to load pending review operation");
        push_descendant_that_reverted_fix(&context).await;
        let remaining_operations = db
            .reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("failed to load discarded review operation");
        let notice = transcript
            .lock()
            .expect("transcript lock should be available")
            .messages()
            .last()
            .expect("stale-operation notice should be appended")
            .content
            .clone();

        // Assert
        assert_eq!(
            first_push_events,
            vec![
                PublishedBranchSyncStatus::InProgress,
                PublishedBranchSyncStatus::Failed,
            ]
        );
        assert_eq!(pending_operations.len(), 1);
        assert_eq!(
            pending_operations[0].commit_hash.as_deref(),
            Some("fix-commit")
        );
        assert_eq!(remaining_operations, Vec::new());
        assert_eq!(
            notice.trim(),
            "[Review Comments Warning] Discarded 1 saved review thread update(s) because the \
             pushed branch tip no longer exactly matches the reported fix commit. Reopen review \
             comments to retry."
        );
    }

    #[tokio::test]
    async fn test_commit_binding_failure_retains_review_operation_for_fresh_retry() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let (app_event_tx, _) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut git_client = dirty_auto_commit_git_client("Fix the review comment");
        git_client.expect_head_hash().once().returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::OutputParse(
                    "commit binding interrupted".to_string(),
                ))
            })
        });
        git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let error = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                review_comment_thread_ids: vec!["thread-42".to_string()],
                session_agent,
            },
            Ok(fixed_review_turn_result()),
        )
        .await
        .expect_err("commit binding should fail");
        let pending_operations = db
            .reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("failed to load binding-pending review operation");

        // Assert
        assert!(error.to_string().contains("commit binding interrupted"));
        assert_eq!(pending_operations.len(), 1);
        assert!(pending_operations[0].commit_hash.is_none());
    }

    #[tokio::test]
    async fn test_commit_failure_discards_review_operations_before_later_push() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut git_client = MockGitClient::new();
        git_client.expect_is_worktree_clean().once().returning(|_| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("commit failed".to_string())) })
        });
        git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let mut review_request_client = forge::MockReviewRequestClient::new();
        review_request_client.expect_detect_remote().never();
        review_request_client
            .expect_fetch_review_comment_snapshot()
            .never();
        review_request_client.expect_reply_to_thread().never();
        review_request_client.expect_resolve_thread().never();
        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(review_request_client),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: vec![ReviewCommentOutcome {
                    reply: "Added the missing validation.".to_string(),
                    resolution: ReviewCommentResolution::Fixed,
                    thread_id: "thread-42".to_string(),
                }],
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        };

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: Some("origin/wt/session-id".to_string()),
                review_comment_thread_ids: vec!["thread-42".to_string()],
                session_agent,
            },
            Ok(turn_result),
        )
        .await
        .expect("turn result should succeed");
        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
        let transcript_text = transcript
            .lock()
            .expect("transcript lock should be available")
            .replay_text()
            .expect("commit failure notices should be persisted");
        let unfinished_operations = context
            .db
            .reviews()
            .load_session_review_comment_resolutions("sess1")
            .await
            .expect("failed to load discarded review-comment operation");

        // Act
        assert_later_push_skips_review_operations(&context).await;

        // Assert
        assert_eq!(status, Status::Review);
        assert_eq!(unfinished_operations, Vec::new());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }))
        );
        assert!(transcript_text.contains("[Commit Error] commit failed"));
        assert!(transcript_text.contains(
            "could not commit the review-comment changes, so it did not push the branch"
        ));
    }

    #[tokio::test]
    async fn test_apply_turn_result_rejects_incomplete_review_comment_outcome_batch() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        insert_in_progress_session_with_review_request(&db).await;
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut git_client = MockGitClient::new();
        git_client
            .expect_is_worktree_clean()
            .once()
            .returning(|_| Box::pin(async { Ok(true) }));
        git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented one change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: vec![ReviewCommentOutcome {
                    reply: "Added the first validation.".to_string(),
                    resolution: ReviewCommentResolution::Fixed,
                    thread_id: "thread-1".to_string(),
                }],
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        };

        // Act
        let status = apply_worker_turn_result(
            &context,
            TurnMetadata {
                published_upstream_ref: None,
                review_comment_thread_ids: vec!["thread-1".to_string(), "thread-2".to_string()],
                session_agent,
            },
            Ok(turn_result),
        )
        .await
        .expect("turn result should succeed");
        let transcript_text = transcript
            .lock()
            .expect("transcript lock should be available")
            .replay_text()
            .expect("validation warning should be persisted");

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            transcript_text
                .contains("exactly one valid outcome for 1 of 2 selected review thread(s)")
        );
        assert!(transcript_text.contains("No review replies were posted or threads resolved"));
    }

    #[tokio::test]
    /// Verifies completed turns leave auto-push idle while queued follow-up
    /// messages are waiting to run.
    async fn test_apply_turn_result_skips_background_push_while_messages_are_queued() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::from([queued_message(
                0,
                "queued follow-up",
            )]))),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: Vec::new(),
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: Vec::new(),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let mut emitted_sync_event = false;
        while let Ok(event) = app_event_rx.try_recv() {
            if matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }) {
                emitted_sync_event = true;
            }
        }

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            !emitted_sync_event,
            "queued follow-up messages should suppress post-turn auto-push events"
        );
    }

    #[tokio::test]
    /// Verifies completed turns leave auto-push idle while queued sync will
    /// publish the branch after rebasing.
    async fn test_apply_turn_result_skips_background_push_while_sync_is_queued() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("queued-sync", "sess1", "rebase")
            .await
            .expect("failed to insert queued sync operation");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: Vec::new(),
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: Vec::new(),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let mut emitted_sync_event = false;
        while let Ok(event) = app_event_rx.try_recv() {
            if matches!(event, AppEvent::PublishedBranchSyncUpdated { .. }) {
                emitted_sync_event = true;
            }
        }

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            !emitted_sync_event,
            "queued sync should suppress post-turn auto-push events"
        );
    }

    #[tokio::test]
    /// Verifies failed background auto-push attempts emit one durable notice
    /// for atomic reducer promotion.
    async fn test_apply_turn_result_reports_background_push_failures() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let session_agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash);
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        expect_safe_auto_push_state(&mut mock_git_client);
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .once()
            .returning(|_, _| {
                Box::pin(async {
                    Err(ag_git::GitError::CommandFailed {
                        command: "git push origin wt/session-id".to_string(),
                        stderr:
                            "fatal: could not read username for 'https://github.com/openai/agentty': terminal prompts disabled"
                                .to_string(),
                    })
                })
            });
        let transcript = empty_transcript();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().join("sess1"),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent,
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(successful_turn_result("Implemented the change."));

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: Some("origin/wt/session-id".to_string()),
            review_comment_thread_ids: Vec::new(),
            session_agent,
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let sync_events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut sync_events = Vec::new();
            while sync_events.len() < 2 {
                let event = app_event_rx.recv().await.expect("missing app event");
                if let AppEvent::PublishedBranchSyncUpdated {
                    persistent_notice,
                    sync_status,
                    ..
                } = event
                {
                    sync_events.push((sync_status, persistent_notice));
                }
            }

            sync_events
        })
        .await
        .expect("timed out waiting for sync events");

        // Assert
        assert_eq!(status, Status::Review);
        assert!(matches!(
            sync_events.as_slice(),
            [
                (PublishedBranchSyncStatus::InProgress, None),
                (PublishedBranchSyncStatus::Failed, Some(_))
            ]
        ));
        let failure_notice = sync_events[1]
            .1
            .as_deref()
            .expect("failed sync should promote one durable notice");
        assert!(failure_notice.contains("[Branch Push Error]"));
        assert!(failure_notice.contains("gh auth login"));
    }

    #[tokio::test]
    /// Verifies failed turn-metadata persistence forces a refresh and skips
    /// reducer projection emission.
    async fn test_apply_turn_result_refreshes_when_turn_metadata_persistence_fails() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .delete_session("sess1")
            .await
            .expect("failed to delete session");
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let context = SessionWorkerContext {
            app_event_tx,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: Vec::new(),
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 2,
            output_tokens: 3,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
        };
        let error = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect_err("turn result should fail when metadata persistence fails");
        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
        let output = transcript_text(&context.transcript);

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
    /// Verifies turn persistence appends only the protocol answer to assistant
    /// transcript messages.
    async fn test_apply_turn_result_persists_only_assistant_answer() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
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
        let transcript = Arc::new(Mutex::new(crate::test_support::assistant_transcript(
            "Hey! How can I help you today?",
        )));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: Arc::clone(&transcript),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Hey! How can I help you today?".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: Vec::new(),
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: None,
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let output = transcript_text(&transcript);

        // Assert
        assert_eq!(status, Status::Review);
        assert!(
            output.starts_with(
                "Hey! How can I help you today?\n\nHey! How can I help you today?\n\n"
            )
        );
        assert!(!output.contains("[Commit] No changes to commit."));
        assert!(!output.contains("## Change Summary"));
    }

    #[tokio::test]
    /// Persists the current app-server instruction bootstrap marker after a
    /// successful turn so later follow-ups can reuse the compact reminder.
    async fn test_apply_turn_result_persists_instruction_conversation_id_for_app_server_turns() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess1", "gpt-5.6-sol", "main", "InProgress", project_id)
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(mock_git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };
        let turn_result = Ok(TurnResult {
            assistant_message: AgentResponse {
                answer: "Implemented the change.".to_string(),
                questions: Vec::new(),
                review_comment_outcomes: Vec::new(),
                subtasks: Vec::new(),
                verification_verdicts: Vec::new(),
            },
            context_reset: true,
            input_tokens: 0,
            output_tokens: 0,
            provider_conversation_id: Some("thread-123".to_string()),
        });

        // Act
        let turn_metadata = TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                AgentModel::Gpt56Sol,
            ),
        };
        let status = apply_worker_turn_result(&context, turn_metadata, turn_result)
            .await
            .expect("turn result should succeed");
        let instruction_conversation_id = db
            .sessions()
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

    /// Test harness for existing-session rebase assistance worker coverage.
    struct RebaseAssistWorkerHarness {
        context: SessionWorkerContext,
        db: AppRepositories,
        status: Arc<Mutex<Status>>,
    }

    /// Writes one conflict-marked file used by rebase-assist worker tests.
    fn write_rebase_conflict_file(base_dir: &std::path::Path) {
        let conflict_file = base_dir.join("src/lib.rs");
        std::fs::create_dir_all(
            conflict_file
                .parent()
                .expect("conflict file should have a parent"),
        )
        .expect("failed to create conflict directory");
        std::fs::write(
            conflict_file,
            concat!(
                "<<",
                "<<<< HEAD\nours\n",
                "===",
                "====\ntheirs\n",
                ">>",
                ">>>>> incoming\n"
            ),
        )
        .expect("failed to write conflict file");
    }

    /// Seeds one rebasing session with existing provider conversation ids.
    async fn seed_existing_session_rebase_metadata(
        db: &AppRepositories,
        parent_session_id: Option<&str>,
    ) {
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        if let Some(parent_session_id) = parent_session_id {
            db.sessions()
                .insert_session(
                    parent_session_id,
                    "gpt-5.6-sol",
                    "main",
                    "Review",
                    project_id,
                )
                .await
                .expect("failed to insert parent session");
            db.sessions()
                .insert_stacked_draft_session(
                    "sess1",
                    "gpt-5.6-sol",
                    "main",
                    "Rebasing",
                    parent_session_id,
                    project_id,
                )
                .await
                .expect("failed to insert stacked session");
        } else {
            db.sessions()
                .insert_session("sess1", "gpt-5.6-sol", "main", "Rebasing", project_id)
                .await
                .expect("failed to insert session");
        }
        db.sessions()
            .update_session_provider_conversation_id("sess1", Some("thread-before".to_string()))
            .await
            .expect("failed to seed provider conversation id");
        db.sessions()
            .update_session_instruction_conversation_id(
                "sess1",
                Some("instruction-before".to_string()),
            )
            .await
            .expect("failed to seed instruction conversation id");
    }

    /// Builds the mock channel expected for one existing-session rebase turn.
    fn mock_existing_session_rebase_channel(main_checkout_root: PathBuf) -> MockAgentChannel {
        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .times(1)
            .withf(move |session_id, request, _| {
                session_id == "sess1"
                    && request.request_kind == AgentRequestKind::UtilityPrompt
                    && request.permission_mode == PermissionMode::AutoEdit
                    && request.main_checkout_root.as_ref() == Some(&main_checkout_root)
                    && request.continuation.provider_conversation_id() == Some("thread-before")
                    && request.continuation.persisted_instruction_conversation_id()
                        == Some("instruction-before")
                    && request
                        .prompt
                        .text
                        .contains("Resolve conflicts in only these files")
                    && request.prompt.text.contains("src/lib.rs")
            })
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(TurnResult {
                        assistant_message: AgentResponse {
                            answer: "Resolved conflicts inside existing session.".to_string(),
                            questions: Vec::new(),
                            review_comment_outcomes: Vec::new(),
                            subtasks: Vec::new(),
                            verification_verdicts: Vec::new(),
                        },
                        context_reset: false,
                        input_tokens: 11,
                        output_tokens: 7,
                        provider_conversation_id: Some("thread-after".to_string()),
                    })
                })
            });

        mock_channel
    }

    /// Builds the git mock expected for one assisted rebase conflict.
    fn mock_successful_conflict_rebase_git_client(main_checkout_root: PathBuf) -> MockGitClient {
        let mut mock_git_client = MockGitClient::new();
        let mut sequence = Sequence::new();
        mock_git_client
            .expect_detect_git_info()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_checkout_working_tree()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let main_checkout_root = main_checkout_root.clone();
                Box::pin(async move { Ok(Some(main_checkout_root)) })
            });
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_is_rebase_in_progress()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_rebase_start()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, target| {
                assert_eq!(target, "main");
                Box::pin(async {
                    Ok(RebaseStepResult::Conflict {
                        detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                    })
                })
            });
        mock_git_client
            .expect_list_conflicted_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, paths| {
                assert_eq!(paths, [] as [std::string::String; 0]);
                Box::pin(async { Ok(Vec::new()) })
            });
        mock_git_client
            .expect_stage_all()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_has_unmerged_paths()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(false) }));
        mock_git_client
            .expect_list_staged_conflict_marker_files()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, paths| {
                assert_eq!(paths, vec!["src/lib.rs".to_string()]);
                Box::pin(async { Ok(Vec::new()) })
            });
        mock_git_client
            .expect_run_pre_commit_hook()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(()) }));
        mock_git_client
            .expect_rebase_continue()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Box::pin(async { Ok(RebaseStepResult::Completed) }));

        mock_git_client
    }

    /// Extends the successful rebase mock with a controllable stack-metadata
    /// boundary and a terminal review-request push failure.
    fn blocking_stack_metadata_git_client(
        main_checkout_root: PathBuf,
        metadata_persistence_started: Arc<tokio::sync::Notify>,
        release_metadata_persistence: Arc<tokio::sync::Notify>,
    ) -> MockGitClient {
        let mut git_client = mock_successful_conflict_rebase_git_client(main_checkout_root);
        git_client
            .expect_ref_hash()
            .once()
            .withf(|_, reference| reference == "main")
            .returning(move |_, _| {
                let metadata_persistence_started = Arc::clone(&metadata_persistence_started);
                let release_metadata_persistence = Arc::clone(&release_metadata_persistence);

                Box::pin(async move {
                    metadata_persistence_started.notify_one();
                    release_metadata_persistence.notified().await;

                    Ok("parent-tip".to_string())
                })
            });
        git_client
            .expect_in_progress_operation()
            .once()
            .returning(|_| Box::pin(async { Ok(Some(ag_git::InProgressGitOperation::Rebase)) }));

        git_client
    }

    /// Builds the review-request command queued behind the controlled rebase.
    fn queued_review_request_command(folder: PathBuf) -> SessionCommand {
        SessionCommand::CreateReviewRequest {
            branch_publish_session: BranchPublishTaskSession {
                base_branch: "main".to_string(),
                folder,
                id: "sess1".into(),
                published_upstream_ref: None,
                review_request: None,
                status: Status::Rebasing,
            },
            operation_id: "op-review-request".to_string(),
            remote_branch_name: None,
            response: None,
        }
    }

    /// Builds one worker harness for session rebase command tests.
    fn rebase_assist_worker_harness(
        base_dir: PathBuf,
        db: AppRepositories,
        build_git_client: impl FnOnce(PathBuf) -> MockGitClient,
    ) -> RebaseAssistWorkerHarness {
        let main_checkout_root = base_dir.join("main-checkout");
        std::fs::create_dir_all(&main_checkout_root).expect("failed to create main checkout");
        // The worker canonicalizes the resolved main checkout root through the
        // real filesystem client, so the mocks must expect the canonical path
        // (on macOS `/var/...` resolves to `/private/var/...`).
        let expected_main_checkout_root = main_checkout_root
            .canonicalize()
            .expect("failed to canonicalize main checkout");

        let status = Arc::new(Mutex::new(Status::Rebasing));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_existing_session_rebase_channel(
                expected_main_checkout_root,
            )),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir,
            fs_client: Arc::new(fs::RealFsClient),
            git_client: Arc::new(build_git_client(main_checkout_root)),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                AgentModel::Gpt56Sol,
            ),
            status: Arc::clone(&status),
        };

        RebaseAssistWorkerHarness {
            context,
            db,
            status,
        }
    }

    #[tokio::test]
    /// Verifies session rebase conflict assistance runs through the existing
    /// session channel, preserving provider conversation identifiers while
    /// Agentty owns staging and `git rebase --continue`.
    async fn test_run_rebase_command_uses_existing_session_channel_for_conflicts() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        write_rebase_conflict_file(base_dir.path());
        let db = AppRepositories::in_memory().await.expect("db should open");
        seed_existing_session_rebase_metadata(&db, None).await;
        db.sessions()
            .update_session_permission_mode("sess1", PermissionMode::ReadOnly)
            .await
            .expect("failed to set chat permission mode");
        let harness = rebase_assist_worker_harness(
            base_dir.path().to_path_buf(),
            db,
            mock_successful_conflict_rebase_git_client,
        );

        // Act
        SessionWorkerService::run_rebase_command(
            &harness.context,
            auto_commit_one_shot_client(),
            "main".to_string(),
        )
        .await
        .expect("rebase command should complete");
        let provider_conversation_id = harness
            .db
            .sessions()
            .get_session_provider_conversation_id("sess1")
            .await
            .expect("failed to load provider conversation id");
        let instruction_conversation_id = harness
            .db
            .sessions()
            .get_session_instruction_conversation_id("sess1")
            .await
            .expect("failed to load instruction conversation id");
        let output_text = transcript_text(&harness.context.transcript);
        let final_status = *harness.status.lock().expect("status lock");

        // Assert
        assert_eq!(provider_conversation_id.as_deref(), Some("thread-after"));
        assert_eq!(
            instruction_conversation_id,
            agent::normalize_instruction_conversation_id(Some("thread-after"))
        );
        assert_eq!(final_status, Status::Review);
        assert!(output_text.contains("[Sync Assist] Attempt 1/3. Resolving conflicts in:"));
        assert!(output_text.contains("- src/lib.rs"));
        assert!(output_text.contains("Resolved conflicts inside existing session."));
        assert!(output_text.contains("[Sync] Successfully synced wt/sess1 onto main"));
    }

    #[tokio::test]
    /// Verifies a review request queued behind rebase cannot begin after the
    /// raw Git command but before metadata persistence and finalization end.
    async fn test_queued_review_request_waits_for_full_rebase_finalization() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        write_rebase_conflict_file(base_dir.path());
        let db = AppRepositories::in_memory().await.expect("db should open");
        seed_existing_session_rebase_metadata(&db, Some("parent-session")).await;
        db.operations()
            .insert_session_operation("op-rebase", "sess1", REBASE_OPERATION_KIND)
            .await
            .expect("failed to insert rebase operation");
        db.operations()
            .insert_session_operation(
                "op-review-request",
                "sess1",
                CREATE_REVIEW_REQUEST_OPERATION_KIND,
            )
            .await
            .expect("failed to insert review-request operation");

        let metadata_persistence_started = Arc::new(tokio::sync::Notify::new());
        let release_metadata_persistence = Arc::new(tokio::sync::Notify::new());
        let mut harness =
            rebase_assist_worker_harness(base_dir.path().to_path_buf(), db.clone(), {
                let metadata_persistence_started = Arc::clone(&metadata_persistence_started);
                let release_metadata_persistence = Arc::clone(&release_metadata_persistence);

                move |main_checkout_root| {
                    blocking_stack_metadata_git_client(
                        main_checkout_root,
                        metadata_persistence_started,
                        release_metadata_persistence,
                    )
                }
            });
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        harness.context.app_event_tx = app_event_tx;
        let transcript = Arc::clone(&harness.context.transcript);
        let status = Arc::clone(&harness.status);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        SessionWorkerService::spawn_session_worker(
            harness.context,
            auto_commit_one_shot_client(),
            Arc::new(Notify::new()),
            command_rx,
        );
        command_tx
            .send(ScheduledSessionCommand::immediate(SessionCommand::Rebase {
                base_branch: "main".to_string(),
                operation_id: "op-rebase".to_string(),
            }))
            .expect("failed to queue rebase");
        command_tx
            .send(ScheduledSessionCommand::queued(
                queued_review_request_command(base_dir.path().to_path_buf()),
                0,
            ))
            .expect("failed to queue review request");

        // Act
        tokio::time::timeout(
            Duration::from_secs(1),
            metadata_persistence_started.notified(),
        )
        .await
        .expect("rebase should reach metadata persistence");
        let events_before_release =
            std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
        release_metadata_persistence.notify_one();
        let publish_started = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let event = app_event_rx.recv().await.expect("missing app event");
                if matches!(event, AppEvent::BranchPublishActionStarted { .. }) {
                    break;
                }
            }
        })
        .await;

        // Assert
        assert!(
            events_before_release
                .iter()
                .all(|event| !matches!(event, AppEvent::BranchPublishActionStarted { .. }))
        );
        assert_eq!(*status.lock().expect("status lock"), Status::Review);
        assert!(
            transcript_text(&transcript).contains("[Sync] Successfully synced wt/sess1 onto main")
        );
        publish_started.expect("review request should start after rebase finalization");
        assert_eq!(
            db.sessions()
                .get_session_stack_base_commit_hash("sess1")
                .await
                .expect("failed to load stack-base hash")
                .as_deref(),
            Some("parent-tip")
        );
    }

    #[tokio::test]
    async fn test_queued_rebase_validation_failure_persists_error_before_resolving_row() {
        // Arrange
        let mut context = queue_helper_context(Arc::new(Mutex::new(VecDeque::new()))).await;
        context.session_id = "sess1".into();
        context.folder = PathBuf::from("missing-session-worktree");
        *context.status.lock().expect("status lock") = Status::Review;
        insert_in_progress_test_session(&context.db).await;
        let mut fs_client = fs::MockFsClient::new();
        fs_client.expect_is_dir().once().return_const(false);
        context.fs_client = Arc::new(fs_client);
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;

        // Act
        let error = SessionWorkerService::run_rebase_command(
            &context,
            auto_commit_one_shot_client(),
            "main".to_string(),
        )
        .await
        .expect_err("missing worktree should reject queued sync");
        let persisted_messages = context
            .db
            .sessions()
            .load_session_messages("sess1")
            .await
            .expect("failed to load persisted session messages");
        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

        // Assert
        assert!(error.to_string().contains("Session isolation violation"));
        assert!(transcript_text(&context.transcript).contains("[Sync Error]"));
        assert_eq!(persisted_messages.len(), 1);
        assert_eq!(persisted_messages[0].kind, "workflow_notice");
        assert!(persisted_messages[0].content.contains("[Sync Error]"));
        assert!(matches!(
            events.as_slice(),
            [
                AppEvent::SessionUpdated { session_id, .. },
                AppEvent::SessionQueuedSyncResolved {
                    session_id: resolved_session_id,
                },
            ] if session_id == "sess1" && resolved_session_id == "sess1"
        ));
        assert_eq!(*context.status.lock().expect("status lock"), Status::Review);
    }

    #[tokio::test]
    /// Verifies recovery stops immediately when unfinished operations cannot
    /// be loaded from storage.
    async fn test_fail_unfinished_operations_from_previous_run_returns_load_error() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        pool.close().await;
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_is_rebase_in_progress().times(0);
        mock_git_client.expect_abort_rebase().times(0);

        // Act
        let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await;

        // Assert
        assert!(matches!(result, Err(SessionError::Db(_))));
    }

    #[tokio::test]
    /// Verifies recovery leaves the operation unfinished when stale rebase
    /// cleanup fails.
    async fn test_fail_unfinished_operations_from_previous_run_returns_rebase_cleanup_error() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        seed_recovery_test_operation(&db, Status::Rebasing, REBASE_OPERATION_KIND).await;
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_rebase_in_progress()
            .once()
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client.expect_abort_rebase().once().returning(|_| {
            Box::pin(async { Err(ag_git::GitError::OutputParse("abort failed".to_string())) })
        });

        // Act
        let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await;
        let operation_is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(matches!(result, Err(SessionError::Git(_))));
        assert!(operation_is_unfinished);
    }

    #[tokio::test]
    /// Verifies recovery stops before interrupting operations when session
    /// status reconciliation fails.
    async fn test_fail_unfinished_operations_from_previous_run_returns_session_reconciliation_error()
     {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
        sqlx::query(
            "CREATE TRIGGER fail_recovery_session_status BEFORE UPDATE OF status ON session BEGIN \
             SELECT RAISE(FAIL, 'session status failed'); END",
        )
        .execute(&pool)
        .await
        .expect("failed to create session status trigger");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_is_rebase_in_progress().times(0);
        mock_git_client.expect_abort_rebase().times(0);

        // Act
        let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await;
        let operation_is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(matches!(result, Err(SessionError::Db(_))));
        assert!(operation_is_unfinished);
    }

    #[tokio::test]
    /// Verifies recovery returns an operation-interruption failure after
    /// session reconciliation rather than admitting normal work.
    async fn test_fail_unfinished_operations_from_previous_run_returns_operation_update_error() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
        sqlx::query(
            "CREATE TRIGGER fail_recovery_operation_update BEFORE UPDATE OF status ON \
             session_operation BEGIN SELECT RAISE(FAIL, 'operation update failed'); END",
        )
        .execute(&pool)
        .await
        .expect("failed to create operation update trigger");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_is_rebase_in_progress().times(0);
        mock_git_client.expect_abort_rebase().times(0);

        // Act
        let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await;
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");
        let operation_is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(matches!(result, Err(SessionError::Db(_))));
        assert_eq!(sessions[0].status, "Review");
        assert!(operation_is_unfinished);
    }

    #[tokio::test]
    /// Verifies a later startup successfully retries recovery after an
    /// earlier operation-interruption failure.
    async fn test_fail_unfinished_operations_from_previous_run_retries_after_failure() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
        sqlx::query(
            "CREATE TRIGGER fail_recovery_retry BEFORE UPDATE OF status ON session_operation \
             BEGIN SELECT RAISE(FAIL, 'operation update failed'); END",
        )
        .execute(&pool)
        .await
        .expect("failed to create retry trigger");
        let mut failed_recovery_git_client = MockGitClient::new();
        failed_recovery_git_client
            .expect_is_rebase_in_progress()
            .times(0);
        failed_recovery_git_client.expect_abort_rebase().times(0);

        // Act
        let failed_recovery =
            SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
                &db,
                base_dir.path(),
                Arc::new(failed_recovery_git_client),
                300,
            )
            .await;
        sqlx::query("DROP TRIGGER fail_recovery_retry")
            .execute(&pool)
            .await
            .expect("failed to remove retry trigger");
        let mut successful_recovery_git_client = MockGitClient::new();
        successful_recovery_git_client
            .expect_is_rebase_in_progress()
            .times(0);
        successful_recovery_git_client
            .expect_abort_rebase()
            .times(0);
        let successful_recovery =
            SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
                &db,
                base_dir.path(),
                Arc::new(successful_recovery_git_client),
                301,
            )
            .await;
        let operation_is_unfinished = db
            .operations()
            .is_session_operation_unfinished("op-1")
            .await
            .expect("failed to check operation status");

        // Assert
        assert!(matches!(failed_recovery, Err(SessionError::Db(_))));
        assert!(successful_recovery.is_ok());
        assert!(!operation_is_unfinished);
    }

    #[tokio::test]
    /// Verifies restart recovery marks unfinished operations failed and
    /// restores affected sessions to `Review`.
    async fn test_fail_unfinished_operations_from_previous_run_restores_session_review_status() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.sessions()
            .update_session_status_with_timing_at("sess1", "InProgress", 0)
            .await
            .expect("failed to open in-progress timing window");
        db.operations()
            .insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_is_rebase_in_progress().times(0);
        mock_git_client.expect_abort_rebase().times(0);

        // Act
        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await
        .expect("restart recovery should complete");
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");
        let operation_is_unfinished = db
            .operations()
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
    /// Verifies restart recovery aborts stale rebase metadata for interrupted
    /// rebase operations before restoring review state.
    async fn test_fail_unfinished_operations_from_previous_run_aborts_interrupted_rebase() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session("sess1", "gemini-3.8-flash", "main", "Rebasing", project_id)
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", "rebase")
            .await
            .expect("failed to insert session operation");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_is_rebase_in_progress()
            .once()
            .withf(|repo_path| repo_path.ends_with("sess1"))
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_abort_rebase()
            .once()
            .withf(|repo_path| repo_path.ends_with("sess1"))
            .returning(|_| Box::pin(async { Ok(()) }));

        // Act
        SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
            &db,
            base_dir.path(),
            Arc::new(mock_git_client),
            300,
        )
        .await
        .expect("restart recovery should complete");
        let sessions = db
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions");

        // Assert
        assert_eq!(sessions[0].status, "Review");
    }

    #[tokio::test]
    /// Verifies unfinished operations remain executable when cancel has not
    /// been requested.
    async fn test_should_skip_worker_command_without_cancel_request() {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .operations()
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
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        db.operations()
            .insert_session_operation("op-1", "sess1", "reply")
            .await
            .expect("failed to insert session operation");
        db.operations()
            .request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        };

        // Act
        let should_skip = SessionWorkerService::should_skip_worker_command(&context, "op-1").await;
        let is_unfinished = db
            .operations()
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
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        // Old operation that gets cancelled.
        db.operations()
            .insert_session_operation("op-old", "sess1", "reply")
            .await
            .expect("failed to insert old operation");
        db.operations()
            .mark_session_operation_running("op-old")
            .await
            .expect("failed to mark old operation running");
        db.operations()
            .request_cancel_for_session_operations("sess1")
            .await
            .expect("failed to request cancel");

        // New operation created after the cancel request — its
        // `cancel_requested` defaults to 0.
        db.operations()
            .insert_session_operation("op-new", "sess1", "reply")
            .await
            .expect("failed to insert new operation");

        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));

        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(mock_channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),

            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
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

    /// Builds one [`SessionWorkerContext`] backed by the supplied
    /// [`MockAgentChannel`], a fresh in-memory database, and the queued
    /// prompt list. The session row is pre-inserted as `InProgress` so the
    /// worker reaches drainage without first transitioning status.
    fn queued_message(order: u64, text: &str) -> QueuedMessage {
        QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
    }

    async fn queue_test_context(
        channel: MockAgentChannel,
        queued_messages: VecDeque<QueuedMessage>,
        status: Status,
    ) -> (
        SessionWorkerContext,
        AppRepositories,
        Arc<Mutex<VecDeque<QueuedMessage>>>,
        tempfile::TempDir,
    ) {
        // Arrange
        let base_dir = tempdir().expect("failed to create temp dir");
        let db = AppRepositories::in_memory().await.expect("db should open");
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert");
        db.sessions()
            .insert_session(
                "sess1",
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");

        let mut mock_git_client = MockGitClient::new();
        let main_repo_root = base_dir.path().join("main");
        mock_git_client
            .expect_detect_git_info()
            .times(0..)
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_checkout_working_tree()
            .times(0..)
            .returning({
                let main_repo_root = main_repo_root.clone();

                move |_| {
                    let main_repo_root = main_repo_root.clone();
                    Box::pin(async move { Ok(Some(main_repo_root)) })
                }
            });
        mock_git_client
            .expect_main_repo_root()
            .times(0..)
            .returning(move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(main_repo_root) })
            });
        mock_git_client
            .expect_tracked_worktree_status()
            .times(0..)
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .returning(|_| Box::pin(async { Ok(true) }));

        let queue_handle = Arc::new(Mutex::new(queued_messages));
        let context = SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(channel),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: base_dir.path().to_path_buf(),
            fs_client: Arc::new(mock_fs_client_with_existing_directories()),
            git_client: Arc::new(mock_git_client),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: Arc::clone(&queue_handle),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess1".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(status)),
        };

        (context, db, queue_handle, base_dir)
    }

    /// Builds one [`SessionWorkerContext`] whose only meaningful state is the
    /// shared `queued_messages` mutex; every other field is wired with a stub
    /// value because these tests only exercise the queue helpers.
    async fn queue_helper_context(
        queue: Arc<Mutex<VecDeque<QueuedMessage>>>,
    ) -> SessionWorkerContext {
        SessionWorkerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            channel: Arc::new(MockAgentChannel::new()),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: AppRepositories::in_memory().await.expect("db should open"),
            folder: PathBuf::new(),
            fs_client: Arc::new(fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            transcript: empty_transcript(),
            personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
            queued_messages: queue,
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "sess".into(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                AgentModel::Gemini38Flash,
            ),
            status: Arc::new(Mutex::new(Status::InProgress)),
        }
    }

    #[tokio::test]
    async fn test_pop_queued_message_returns_messages_in_submission_order() {
        // Arrange
        let queue = Arc::new(Mutex::new(VecDeque::from([
            queued_message(0, "first"),
            queued_message(1, "second"),
        ])));
        let context = queue_helper_context(Arc::clone(&queue)).await;

        // Act
        let first_pop = context.pop_queued_message();
        let second_pop = context.pop_queued_message();
        let empty_pop = context.pop_queued_message();

        // Assert
        assert_eq!(first_pop.expect("first prompt").transcript_text(), "first");
        assert_eq!(
            second_pop.expect("second prompt").transcript_text(),
            "second"
        );
        assert!(empty_pop.is_none());
        assert!(queue.lock().expect("queue lock").is_empty());
    }

    #[tokio::test]
    async fn test_clear_queued_messages_drops_all_pending_prompts() {
        // Arrange
        let queue = Arc::new(Mutex::new(VecDeque::from([
            queued_message(0, "alpha"),
            queued_message(1, "beta"),
        ])));
        let context = queue_helper_context(Arc::clone(&queue)).await;

        // Act
        context.clear_queued_messages();

        // Assert
        assert!(queue.lock().expect("queue lock").is_empty());
    }

    #[tokio::test]
    async fn test_clear_queued_messages_updates_shared_queue_state() {
        // Arrange
        let queue = Arc::new(Mutex::new(VecDeque::from([queued_message(
            0,
            "queued reply",
        )])));
        let context = queue_helper_context(Arc::clone(&queue)).await;

        // Act
        let has_queued_before_clear = !queue.lock().expect("queue lock").is_empty();
        context.clear_queued_messages();
        let has_queued_after_clear = !queue.lock().expect("queue lock").is_empty();

        // Assert
        assert!(has_queued_before_clear);
        assert!(!has_queued_after_clear);
    }

    #[tokio::test]
    async fn test_next_scheduled_work_follows_shared_submission_order() {
        // Arrange
        let queued = VecDeque::from([
            queued_message(0, "queued first"),
            queued_message(1, "queued second"),
        ]);
        let (context, _db, queue_handle, _base_dir) =
            queue_test_context(MockAgentChannel::new(), queued, Status::InProgress).await;
        let mut pending_commands = VecDeque::from([ScheduledSessionCommand::queued(
            SessionCommand::Rebase {
                base_branch: "main".to_string(),
                operation_id: "queued-rebase".to_string(),
            },
            2,
        )]);

        // Act
        let first_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
        let second_work =
            SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
        let third_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
        queue_handle
            .lock()
            .expect("queue lock")
            .push_back(queued_message(4, "queued last"));
        pending_commands.push_back(ScheduledSessionCommand::queued(
            SessionCommand::Rebase {
                base_branch: "main".to_string(),
                operation_id: "older-rebase".to_string(),
            },
            3,
        ));
        let fourth_work =
            SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
        pending_commands.push_front(ScheduledSessionCommand::immediate(resume_command(
            "immediate-reply",
        )));
        let fifth_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
        let sixth_work = SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);

        // Assert
        assert!(matches!(
            first_work,
            Some(ScheduledSessionWork::Message(message))
                if message.transcript_text() == "queued first"
        ));
        assert!(matches!(
            second_work,
            Some(ScheduledSessionWork::Message(message))
                if message.transcript_text() == "queued second"
        ));
        assert!(matches!(
            third_work,
            Some(ScheduledSessionWork::Command(command))
                if command.queued_order == Some(2)
                    && matches!(
                        &command.command,
                        SessionCommand::Rebase { operation_id, .. }
                            if operation_id == "queued-rebase"
                    )
        ));
        assert!(matches!(
            fourth_work,
            Some(ScheduledSessionWork::Command(command))
                if command.queued_order == Some(3)
                    && matches!(
                        &command.command,
                        SessionCommand::Rebase { operation_id, .. }
                            if operation_id == "older-rebase"
                    )
        ));
        assert!(matches!(
            fifth_work,
            Some(ScheduledSessionWork::Command(command))
                if command.queued_order.is_none()
                    && matches!(
                        &command.command,
                        SessionCommand::Run { operation_id, .. }
                            if operation_id == "immediate-reply"
                    )
        ));
        assert!(matches!(
            sixth_work,
            Some(ScheduledSessionWork::Message(message))
                if message.transcript_text() == "queued last"
        ));
        assert!(queue_handle.lock().expect("queue lock").is_empty());
    }

    #[tokio::test]
    async fn test_next_scheduled_work_pauses_queued_work_for_question() {
        // Arrange
        let queued = VecDeque::from([queued_message(1, "queued reply")]);
        let (context, _db, queue_handle, _base_dir) =
            queue_test_context(MockAgentChannel::new(), queued, Status::Question).await;
        let mut pending_commands = VecDeque::from([ScheduledSessionCommand::queued(
            SessionCommand::Rebase {
                base_branch: "main".to_string(),
                operation_id: "queued-rebase".to_string(),
            },
            0,
        )]);

        // Act
        let paused_work =
            SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);
        pending_commands.push_back(ScheduledSessionCommand::queued(
            resume_command("question-answer"),
            2,
        ));
        let answer_work =
            SessionWorkerService::next_scheduled_work(&context, &mut pending_commands);

        // Assert
        assert!(paused_work.is_none());
        assert!(matches!(
            answer_work,
            Some(ScheduledSessionWork::Command(command))
                if command.queued_order == Some(2)
                    && matches!(
                        &command.command,
                        SessionCommand::Run { operation_id, .. }
                            if operation_id == "question-answer"
                    )
        ));
        assert_eq!(pending_commands.len(), 1);
        assert_eq!(queue_handle.lock().expect("queue lock").len(), 1);
    }

    #[tokio::test]
    async fn test_worker_wakeup_resumes_buffered_action_after_question_cancel() {
        // Arrange
        let (mut context, _db, _queue_handle, _base_dir) =
            queue_test_context(MockAgentChannel::new(), VecDeque::new(), Status::Question).await;
        let status = Arc::clone(&context.status);
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let wakeup = Arc::new(Notify::new());
        let mut worker_service = SessionWorkerService::new();
        worker_service.workers.insert(
            SessionId::from("sess1"),
            SessionWorkerHandle {
                queued_work_sequence: Arc::new(AtomicU64::new(0)),
                sender: command_tx.clone(),
                wakeup: Arc::clone(&wakeup),
            },
        );
        SessionWorkerService::spawn_session_worker(
            context,
            auto_commit_one_shot_client(),
            Arc::clone(&wakeup),
            command_rx,
        );
        command_tx
            .send(ScheduledSessionCommand::queued(
                SessionCommand::Rebase {
                    base_branch: "main".to_string(),
                    operation_id: "already-resolved-rebase".to_string(),
                },
                0,
            ))
            .expect("failed to queue rebase");
        tokio::task::yield_now().await;

        // Act
        *status.lock().expect("status lock") = Status::Review;
        worker_service.wake_session_worker("sess1");
        let resolved_event =
            tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv()).await;

        // Assert
        assert!(matches!(
            resolved_event,
            Ok(Some(AppEvent::SessionQueuedSyncResolved { session_id }))
                if session_id == "sess1"
        ));
    }

    #[tokio::test]
    /// Verifies the last queued follow-up turn reloads the persisted
    /// published branch and starts auto-push after the queue has drained.
    async fn test_process_queued_message_auto_pushes_after_last_published_branch_follow_up() {
        // Arrange
        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .times(1)
            .withf(|session_id, request, _events| {
                session_id == "sess1" && request.prompt.text == "queued reply"
            })
            .returning(|_, _, _| Box::pin(async { Ok(successful_turn_result("Queued done.")) }));
        let queued = VecDeque::from([queued_message(0, "queued reply")]);
        let (mut context, db, queue_handle, base_dir) =
            queue_test_context(mock_channel, queued, Status::InProgress).await;
        db.sessions()
            .update_session_published_upstream_ref(
                "sess1",
                Some("origin/wt/session-id".to_string()),
            )
            .await
            .expect("failed to persist published upstream ref");

        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;

        let mut mock_git_client = MockGitClient::new();
        let main_repo_root = base_dir.path().join("main");
        mock_git_client
            .expect_detect_git_info()
            .times(2)
            .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
        mock_git_client
            .expect_main_checkout_working_tree()
            .times(1)
            .returning({
                let main_repo_root = main_repo_root.clone();

                move |_| {
                    let main_repo_root = main_repo_root.clone();
                    Box::pin(async move { Ok(Some(main_repo_root)) })
                }
            });
        mock_git_client
            .expect_tracked_worktree_status()
            .times(2)
            .returning(|_| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_is_worktree_clean()
            .times(1)
            .returning(|_| Box::pin(async { Ok(true) }));
        mock_git_client
            .expect_in_progress_operation()
            .times(1)
            .returning(|_| Box::pin(async { Ok(None) }));
        mock_git_client
            .expect_diff()
            .returning(|_, _| Box::pin(async { Ok(String::new()) }));
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .times(1)
            .withf(|_folder, remote_branch_name| remote_branch_name == "wt/session-id")
            .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
        context.git_client = Arc::new(mock_git_client);

        // Act
        let one_shot_client = auto_commit_one_shot_client();
        let message = context
            .pop_queued_message()
            .expect("queued message should be available");
        let turn_result =
            SessionWorkerService::process_queued_message(&context, &one_shot_client, message).await;
        let (turn_started_session_id, sync_events) =
            tokio::time::timeout(Duration::from_secs(1), async {
                let mut sync_events = Vec::new();
                let mut turn_started_session_id = None;
                while sync_events.len() < 2 || turn_started_session_id.is_none() {
                    let event = app_event_rx.recv().await.expect("missing app event");
                    match event {
                        AppEvent::SessionTurnStarted { session_id } => {
                            turn_started_session_id = Some(session_id);
                        }
                        AppEvent::PublishedBranchSyncUpdated {
                            session_id,
                            sync_operation_id,
                            sync_status,
                            ..
                        } => sync_events.push((session_id, sync_operation_id, sync_status)),
                        _ => {}
                    }
                }

                (turn_started_session_id, sync_events)
            })
            .await
            .expect("timed out waiting for sync events");

        // Assert
        assert!(matches!(turn_result, Some(Ok(()))));
        assert!(queue_handle.lock().expect("queue lock").is_empty());
        assert_eq!(turn_started_session_id.as_deref(), Some("sess1"));
        assert_eq!(sync_events[0].2, PublishedBranchSyncStatus::InProgress);
        assert_eq!(sync_events[1].2, PublishedBranchSyncStatus::Succeeded);
        assert_eq!(sync_events[0].0, "sess1");
        assert_eq!(sync_events[1].0, "sess1");
        assert_eq!(sync_events[0].1, sync_events[1].1);
    }

    #[tokio::test]
    /// Verifies that the scheduler clears every queued prompt once the
    /// running queued turn returns `StoppedByUser`, matching the `Ctrl+C`
    /// expectation that cancellation drops pending follow-ups together with
    /// the active turn.
    async fn test_process_queued_message_clears_queue_when_user_stops_running_turn() {
        // Arrange
        let mut mock_channel = MockAgentChannel::new();
        mock_channel
            .expect_run_turn()
            .times(1)
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(AgentError::InterruptedByUser(
                        "[Stopped] Session interrupted by user.".to_string(),
                    ))
                })
            });
        mock_channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        let queued = VecDeque::from([
            queued_message(0, "queued first"),
            queued_message(1, "queued second"),
        ]);
        let (context, _db, queue_handle, _base_dir) =
            queue_test_context(mock_channel, queued, Status::InProgress).await;

        // Act
        let one_shot_client = auto_commit_one_shot_client();
        let message = context
            .pop_queued_message()
            .expect("queued message should be available");
        let turn_result =
            SessionWorkerService::process_queued_message(&context, &one_shot_client, message).await;
        SessionWorkerService::clear_queued_messages_after_stop(&context, turn_result.as_ref());

        // Assert — first prompt was dispatched, the stopped result propagated,
        // and the remaining queued prompt was cleared.
        assert!(matches!(
            turn_result,
            Some(Err(SessionError::StoppedByUser(_)))
        ));
        let queue = queue_handle.lock().expect("queue lock");
        assert!(queue.is_empty(), "queue should be cleared on Ctrl+C");
    }
}
