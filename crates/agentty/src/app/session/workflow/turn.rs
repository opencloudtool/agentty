//! Channel turn execution for session workers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent as agent;
use ag_agent::{
    AgentError, AgentRequestKind, LiveTranscript, OneShotClient, PersonalityPrompt,
    TurnContinuation, TurnEvent, TurnRequest, TurnResult,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::lifecycle::SessionTitleGenerationTaskInput;
use super::worker::{SessionWorkerContext, TurnMetadata};
use super::{SessionTaskService, StatusTransition, isolation, post_turn};
use crate::app::session::SessionError;
use crate::app::{AppEvent, SessionManager, setting};
use crate::domain::agent::{AgentKind, AgentSelection, ReasoningLevel, ResponseStyle};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{SessionId, SessionRole, Status};
use crate::domain::session_message::SessionTranscript;
use crate::domain::setting::SettingName;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptTextSource};
use crate::infra::db::AppRepositories;
use crate::infra::process;

/// Maximum ready turn events folded into one progress app-event emission.
///
/// The cap prevents a chatty provider stream from turning coalescing itself
/// into an unbounded drain. Events beyond this budget remain queued for the
/// consumer's next await/try-receive cycle.
const TURN_EVENT_PROGRESS_COALESCE_BUDGET: usize = 64;
/// Agent-facing behavior appended to ordinary read-only chat turns.
const READ_ONLY_CHAT_PROMPT: &str = include_str!("../../template/read_only_chat_prompt.md");

/// Live transcript source exposed to provider transports for replay.
#[derive(Debug)]
struct LiveSessionTranscript {
    transcript: Arc<Mutex<SessionTranscript>>,
}

impl LiveTranscript for LiveSessionTranscript {
    fn replay_text(&self) -> Option<String> {
        self.transcript
            .lock()
            .ok()
            .and_then(|transcript| transcript.replay_text())
    }
}

/// Builds a replay source backed by the session's typed transcript handle.
pub(super) fn live_transcript_source(
    transcript: &Arc<Mutex<SessionTranscript>>,
) -> Arc<dyn LiveTranscript> {
    Arc::new(LiveSessionTranscript {
        transcript: Arc::clone(transcript),
    })
}

/// Main-checkout tracked-file status captured before one provider turn.
struct MainCheckoutSnapshot {
    main_repo_root: PathBuf,
    tracked_status_output: String,
}

impl MainCheckoutSnapshot {
    /// Captures the main repository checkout status before a provider turn.
    ///
    /// Returns `None` when the shared repository is bare and therefore has no
    /// main working checkout to snapshot; the session worktree is still
    /// validated in that case.
    ///
    /// # Errors
    /// Returns a workflow error when the session folder is not a valid linked
    /// worktree or the main-checkout tracked status cannot be read.
    async fn capture(context: &SessionWorkerContext) -> Result<Option<Self>, SessionError> {
        let validation = isolation::validate_session_worktree(
            context.fs_client.as_ref(),
            context.git_client.as_ref(),
            &context.folder,
            &context.session_id,
        )
        .await?;
        let Some(main_repo_root) = validation.main_checkout else {
            return Ok(None);
        };
        let tracked_status_output = context
            .git_client
            .tracked_worktree_status(main_repo_root.clone())
            .await
            .map_err(|error| Self::status_error(&error))?;

        Ok(Some(Self {
            main_repo_root,
            tracked_status_output,
        }))
    }

    /// Builds a warning when the main checkout tracked-file status changed and
    /// remains dirty after a provider turn.
    ///
    /// # Errors
    /// Returns a workflow error when main-checkout status cannot be read after
    /// the provider turn.
    async fn dirty_warning(
        &self,
        context: &SessionWorkerContext,
    ) -> Result<Option<String>, SessionError> {
        let current_status = context
            .git_client
            .tracked_worktree_status(self.main_repo_root.clone())
            .await
            .map_err(|error| Self::status_error(&error))?;
        if current_status != self.tracked_status_output && !current_status.trim().is_empty() {
            return Ok(Some(TranscriptNotice::MainCheckoutWarning.format(
                "The main checkout's tracked-file status changed during this turn. Continuing \
                 this session; merge and sync actions still require a clean main checkout.",
            )));
        }

        Ok(None)
    }

    /// Converts main-checkout tracked status failures into workflow errors.
    fn status_error(error: &ag_git::GitError) -> SessionError {
        SessionError::Workflow(format!(
            "Session isolation violation: failed to inspect main checkout tracked status: {error}"
        ))
    }
}

/// Executes one agent turn through the session channel and applies all
/// post-turn effects (stats, auto-commit, size refresh, status update).
///
/// When `request_kind` is [`AgentRequestKind::SessionResume`], the session
/// is first transitioned to `InProgress` (start turns set `InProgress` in
/// the lifecycle before enqueueing). Start and resume turns schedule detached
/// title generation while the current title remains provisional. Progress
/// events update the UI indicator; `PidUpdate` events update the shared PID
/// slot used for accounting and CLI cancellation. If the turn fails, the
/// error is appended to session output before transitioning to `Review`;
/// user-stopped turns skip that fallback so the UI cancellation path can
/// finalize `Canceled`.
///
/// A fresh [`CancellationToken`] is swapped into the shared mutex at
/// the top of this function so stale cancellations from previous
/// turns cannot affect new work. A `Ctrl+c` arriving during setup
/// cancels the new token, which is detected by the early-exit check
/// in [`run_turn_with_cancellation`].
pub(super) async fn run_channel_turn(
    context: &SessionWorkerContext,
    one_shot_client: Arc<dyn OneShotClient>,
    turn_metadata: TurnMetadata,
    request_kind: AgentRequestKind,
    replay_transcript: Option<String>,
    prompt: TurnPrompt,
) -> Result<(), SessionError> {
    // Discard stale cancellations, then keep the new token for this turn.
    let turn_cancel_token = fresh_turn_cancel_token(context)?;

    prepare_resume_turn(context, &request_kind).await;

    let post_turn_context =
        post_turn::PostTurnContext::from_worker(context, Arc::clone(&one_shot_client));
    let main_checkout_snapshot = match MainCheckoutSnapshot::capture(context).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return finalize_turn_setup_failure(
                context,
                &post_turn_context,
                turn_metadata,
                &prompt,
                &error,
            )
            .await;
        }
    };

    let session_project_id = load_session_project_id(&context.db, &context.session_id).await;
    let permission_mode = match load_session_permission_mode(&context.db, &context.session_id).await
    {
        Ok(permission_mode) => permission_mode,
        Err(error) => {
            return finalize_turn_setup_failure(
                context,
                &post_turn_context,
                turn_metadata,
                &prompt,
                &error,
            )
            .await;
        }
    };
    let reasoning_level = load_session_reasoning_level(&context.db, &context.session_id).await;
    let response_style = load_session_response_style(&context.db, &context.session_id).await;
    let speed_mode = load_session_speed_mode(&context.db, &context.session_id).await;
    let continuation = load_turn_continuation(context, replay_transcript).await;
    let ResolvedTurnPersonality {
        persistence: personality_persistence,
        prompt: personality,
    } = resolve_turn_personality(context).await;

    let agent_prompt = prepare_agent_prompt(context, prompt.clone(), permission_mode).await;
    let req = TurnRequest {
        continuation,
        folder: context.folder.clone(),
        main_checkout_root: main_checkout_snapshot
            .as_ref()
            .map(|snapshot| snapshot.main_repo_root.clone()),
        model: turn_metadata
            .session_agent
            .model()
            .provider_model_str()
            .to_string(),
        permission_mode,
        personality,
        prompt: agent_prompt,
        reasoning_level,
        request_kind: request_kind.clone(),
        response_style,
        speed_mode,
    };

    let (event_tx, event_rx) = mpsc::unbounded_channel::<TurnEvent>();
    let consumer = tokio::spawn(consume_turn_events(
        event_rx,
        context.app_event_tx.clone(),
        context.session_id.clone(),
        Arc::clone(&context.child_pid),
    ));

    spawn_turn_title_generation(
        context,
        Arc::clone(&one_shot_client),
        session_project_id,
        &prompt.text,
        turn_metadata.session_agent,
    )
    .await;

    let turn_result = run_turn_with_cancellation(context, turn_cancel_token, req, event_tx).await;
    SessionManager::cleanup_prompt_attachment_paths(
        context.fs_client.clone(),
        prompt.local_image_paths().cloned().collect(),
    )
    .await;

    let _ = consumer.await;

    let turn_result =
        add_main_checkout_warning(context, main_checkout_snapshot.as_ref(), turn_result).await;
    let finalizer_context = post_turn::TurnFinalizerContext::from_worker(context);
    let result = post_turn::apply_turn_result(
        &post_turn_context,
        turn_metadata,
        personality_persistence,
        turn_result,
    )
    .await;
    post_turn::finalize_channel_turn(&finalizer_context, &result).await;

    result.map(|_| ())
}

/// Applies role-specific controller and read-only chat instructions.
async fn prepare_agent_prompt(
    context: &SessionWorkerContext,
    prompt: TurnPrompt,
    permission_mode: PermissionMode,
) -> TurnPrompt {
    let session_role = load_session_role(&context.db, &context.session_id).await;
    let agent_prompt =
        crate::app::orchestration::controller_prompt(&context.db, &context.session_id, prompt)
            .await;

    apply_read_only_chat_prompt(agent_prompt, permission_mode, session_role)
}

/// Adds mode-switch guidance only to user-owned read-only chat sessions.
fn apply_read_only_chat_prompt(
    prompt: TurnPrompt,
    permission_mode: PermissionMode,
    session_role: SessionRole,
) -> TurnPrompt {
    if permission_mode != PermissionMode::ReadOnly || session_role != SessionRole::Worker {
        return prompt;
    }

    let agent_prompt = prompt.agent_text();

    TurnPrompt {
        attachments: prompt.attachments,
        text: format!("{READ_ONLY_CHAT_PROMPT}\n\n{agent_prompt}"),
        text_source: TurnPromptTextSource::AgentData,
    }
}

/// Cleans up and reports a failure that occurs after turn setup has started
/// but before the provider request can run.
async fn finalize_turn_setup_failure(
    context: &SessionWorkerContext,
    post_turn_context: &post_turn::PostTurnContext,
    turn_metadata: TurnMetadata,
    prompt: &TurnPrompt,
    error: &SessionError,
) -> Result<(), SessionError> {
    SessionManager::cleanup_prompt_attachment_paths(
        context.fs_client.clone(),
        prompt.local_image_paths().cloned().collect(),
    )
    .await;
    let finalizer_context = post_turn::TurnFinalizerContext::from_worker(context);
    let result = post_turn::apply_turn_result(
        post_turn_context,
        turn_metadata,
        post_turn::TurnPersonalityPersistence::default(),
        Err(AgentError::Backend(error.to_string())),
    )
    .await;
    post_turn::finalize_channel_turn(&finalizer_context, &result).await;

    result.map(|_| ())
}

/// Loads the durable provider context needed to continue one session turn.
async fn load_turn_continuation(
    context: &SessionWorkerContext,
    replay_transcript: Option<String>,
) -> TurnContinuation {
    let provider_conversation_id = context
        .db
        .sessions()
        .get_session_provider_conversation_id(&context.session_id)
        .await
        .ok()
        .flatten();
    let instruction_conversation_id = context
        .db
        .sessions()
        .get_session_instruction_conversation_id(&context.session_id)
        .await
        .ok()
        .flatten();

    TurnContinuation::provider(
        Some(live_transcript_source(&context.transcript)),
        instruction_conversation_id,
        provider_conversation_id,
        replay_transcript,
    )
}

/// Returns the provider permission mode enforced for one persisted session.
///
/// # Errors
/// Returns an error when the session cannot be loaded or its persisted role
/// or permission mode is invalid.
pub(super) async fn load_session_permission_mode(
    db: &AppRepositories,
    session_id: &str,
) -> Result<PermissionMode, SessionError> {
    let row = db
        .sessions()
        .load_session(session_id)
        .await?
        .ok_or(SessionError::NotFound)?;
    let role = row
        .role
        .as_deref()
        .map(str::parse::<SessionRole>)
        .transpose()
        .map_err(|reason| crate::infra::db::DbError::InvalidData {
            entity: "session role",
            reason,
        })?
        .unwrap_or_default();
    if role == SessionRole::OrchestrationResearcher {
        return Ok(PermissionMode::ReadOnly);
    }

    row.permission_mode
        .parse()
        .map_err(|reason| crate::infra::db::DbError::InvalidData {
            entity: "session permission mode",
            reason,
        })
        .map_err(SessionError::from)
}

/// Returns the persisted role controlling one session's execution policy.
pub(super) async fn load_session_role(db: &AppRepositories, session_id: &str) -> SessionRole {
    db.sessions()
        .load_session(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.role)
        .and_then(|role| role.parse::<SessionRole>().ok())
        .unwrap_or_default()
}

/// Personality prompt plus the state persisted after a successful turn.
pub(super) struct ResolvedTurnPersonality {
    pub(super) persistence: post_turn::TurnPersonalityPersistence,
    pub(super) prompt: PersonalityPrompt,
}

/// Resolves the selected personality from the session worktree immediately
/// before one provider request is built.
pub(super) async fn resolve_turn_personality(
    context: &SessionWorkerContext,
) -> ResolvedTurnPersonality {
    let state = match context
        .db
        .sessions()
        .load_session_personality_state(&context.session_id)
        .await
    {
        Ok(Some(state)) => state,
        Ok(None) => {
            return ResolvedTurnPersonality {
                persistence: post_turn::TurnPersonalityPersistence::default(),
                prompt: PersonalityPrompt::default(),
            };
        }
        Err(error) => {
            warn!(
                session_id = %context.session_id,
                %error,
                "failed to load session personality state"
            );

            return ResolvedTurnPersonality {
                persistence: post_turn::TurnPersonalityPersistence::default(),
                prompt: PersonalityPrompt::default(),
            };
        }
    };
    let Some(selected_id) = state.personality_id else {
        return ResolvedTurnPersonality {
            persistence: post_turn::TurnPersonalityPersistence::default(),
            prompt: PersonalityPrompt::cleared(state.applied_personality_prompt_hash.is_some()),
        };
    };
    let personality = context
        .personality_catalog_client
        .resolve(context.folder.clone(), selected_id.clone())
        .await;
    let Some(personality) = personality else {
        let should_report_fallback = state.applied_personality_id.as_deref()
            != Some(selected_id.as_str())
            || state.applied_personality_prompt_hash.is_some();
        if should_report_fallback {
            let notice = TranscriptNotice::Personality.format(format!(
                "Selected personality `{selected_id}` is unavailable in this session worktree; \
                 continuing without it."
            ));
            SessionTaskService::append_workflow_notice(
                &context.transcript,
                &context.db,
                &context.app_event_tx,
                &context.session_update_versions,
                &context.session_id,
                &notice,
            )
            .await;
        }

        return ResolvedTurnPersonality {
            persistence: post_turn::TurnPersonalityPersistence {
                applied_personality_id: Some(selected_id),
                applied_personality_prompt_hash: None,
            },
            prompt: PersonalityPrompt::cleared(state.applied_personality_prompt_hash.is_some()),
        };
    };
    let fingerprint = personality.fingerprint();
    let changed = state.applied_personality_id.as_deref() != Some(personality.id.as_str())
        || state.applied_personality_prompt_hash.as_deref() != Some(fingerprint.as_str());

    ResolvedTurnPersonality {
        persistence: post_turn::TurnPersonalityPersistence {
            applied_personality_id: Some(personality.id),
            applied_personality_prompt_hash: Some(fingerprint),
        },
        prompt: PersonalityPrompt::active(personality.prompt, changed),
    }
}

/// Applies best-effort state cleanup before a resume turn starts.
async fn prepare_resume_turn(context: &SessionWorkerContext, request_kind: &AgentRequestKind) {
    if !matches!(request_kind, AgentRequestKind::SessionResume) {
        return;
    }

    let _ = context
        .db
        .sessions()
        .update_session_questions(&context.session_id, "")
        .await;

    let status_transition = StatusTransition::from_parts(
        context.app_event_tx.clone(),
        Arc::clone(&context.clock),
        context.db.clone(),
        context.session_id.clone(),
        Arc::clone(&context.session_update_versions),
        Arc::clone(&context.status),
    );
    let _ = status_transition.apply(Status::InProgress).await;
}

/// Runs one agent turn with cancellation support.
///
/// Races `run_turn` against the per-turn [`CancellationToken`]. When the
/// token is cancelled (`Ctrl+c`), `SIGTERM` is sent only to a CLI child
/// process (if any) via [`terminate_child_process`], the channel is shut
/// down gracefully through `shutdown_session`, and the function waits for
/// the `run_turn` future to resolve (with a timeout) so the subprocess is
/// not orphaned. App-server PIDs are accounting metadata, never signal
/// targets: their runtime owners handle cancellation through
/// `shutdown_session`, including when a retained PID has been recycled.
///
/// Each turn receives its own fresh token, created at the start of
/// [`run_channel_turn`]. This eliminates the stale-permit problem that
/// required the previous `Notify` + `AtomicBool` flag-check pattern.
pub(super) async fn run_turn_with_cancellation(
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
        terminate_child_process(&context.child_pid, context.session_agent.kind());
        let _ = context
            .channel
            .shutdown_session(context.session_id.to_string())
            .await;

        return Err(AgentError::InterruptedByUser(
            "[Stopped] Session interrupted by user.".to_string(),
        ));
    }

    let turn_future = context
        .channel
        .run_turn(context.session_id.to_string(), req, event_tx);
    tokio::pin!(turn_future);

    tokio::select! {
        result = &mut turn_future => result,
        () = cancel_token.cancelled() => {
            // Only CLI PIDs are signal targets. App-server runtimes are
            // stopped by their owner below, never through sampled PIDs.
            terminate_child_process(&context.child_pid, context.session_agent.kind());

            // Graceful shutdown: close stdin, wait for exit, kill if
            // needed.
            let _ = context
                .channel
                .shutdown_session(context.session_id.to_string())
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

            Err(AgentError::InterruptedByUser(
                "[Stopped] Session interrupted by user.".to_string(),
            ))
        }
    }
}

/// Converts channel-layer turn failures into session workflow errors.
pub(super) fn session_error_from_agent_error(error: AgentError) -> SessionError {
    match error {
        AgentError::InterruptedByUser(message) => SessionError::StoppedByUser(message),
        other => SessionError::Workflow(other.to_string()),
    }
}

/// Loads the project identifier associated with one persisted session.
pub(super) async fn load_session_project_id(db: &AppRepositories, session_id: &str) -> Option<i64> {
    db.sessions()
        .load_session_project_id(session_id)
        .await
        .ok()
        .flatten()
}

/// Loads the effective reasoning level for one session context.
pub(super) async fn load_session_reasoning_level(
    db: &AppRepositories,
    session_id: &str,
) -> ReasoningLevel {
    db.sessions()
        .load_session_reasoning_level(session_id)
        .await
        .unwrap_or_default()
}

/// Loads the response style for one session context.
pub(super) async fn load_session_response_style(
    db: &AppRepositories,
    session_id: &str,
) -> ResponseStyle {
    db.sessions()
        .load_session_response_style(session_id)
        .await
        .unwrap_or_default()
}

/// Loads the response-speed preference for one session context.
pub(super) async fn load_session_speed_mode(
    db: &AppRepositories,
    session_id: &str,
) -> crate::domain::agent::SpeedMode {
    db.sessions()
        .load_session_speed_mode(session_id)
        .await
        .unwrap_or_default()
}

/// Consumes [`TurnEvent`]s from `event_rx` and applies their side effects.
///
/// - [`TurnEvent::ThoughtDelta`]: coalesces immediately ready thought bursts
///   and updates the transient thinking loader text with the latest message.
/// - [`TurnEvent::PidUpdate`]: writes the new PID into `child_pid`.
/// - [`TurnEvent::Completed`] / [`TurnEvent::Failed`]: reserved; ignored here
///   because completion is signalled by `run_turn`'s return value.
pub(super) async fn consume_turn_events(
    mut event_rx: mpsc::UnboundedReceiver<TurnEvent>,
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    session_id: SessionId,
    child_pid: Arc<Mutex<Option<u32>>>,
) {
    let mut active_progress: Option<String> = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TurnEvent::ThoughtDelta(thought) => {
                let Some(thought) = normalize_thinking_stream_text(&thought) else {
                    continue;
                };
                let thought = coalesce_ready_turn_progress_events(
                    &mut event_rx,
                    child_pid.as_ref(),
                    thought,
                    &session_id,
                );
                if active_progress.as_deref() == Some(thought.as_str()) {
                    continue;
                }

                active_progress = Some(thought.clone());
                SessionTaskService::set_session_progress(&app_event_tx, &session_id, Some(thought));
            }
            TurnEvent::PidUpdate(pid) => {
                set_child_pid(child_pid.as_ref(), pid);
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

/// Coalesces immediately ready turn progress events before app-event enqueue.
///
/// PID updates are still applied as they are encountered, while repeated
/// thought deltas collapse to the newest normalized message. Completion events
/// remain ignored here because turn completion is handled by the channel
/// result.
fn coalesce_ready_turn_progress_events(
    event_rx: &mut mpsc::UnboundedReceiver<TurnEvent>,
    child_pid: &Mutex<Option<u32>>,
    initial_thought: String,
    session_id: &SessionId,
) -> String {
    let mut latest_thought = initial_thought;
    let mut coalesced_events = 0;

    for _ in 1..TURN_EVENT_PROGRESS_COALESCE_BUDGET {
        let Ok(event) = event_rx.try_recv() else {
            break;
        };

        coalesced_events += 1;
        match event {
            TurnEvent::ThoughtDelta(thought) => {
                if let Some(thought) = normalize_thinking_stream_text(&thought) {
                    latest_thought = thought;
                }
            }
            TurnEvent::PidUpdate(pid) => set_child_pid(child_pid, pid),
            TurnEvent::Completed { .. } | TurnEvent::Failed(_) => {}
        }
    }

    let remaining_events = event_rx.len();
    if remaining_events > 0 {
        debug!(
            budget = TURN_EVENT_PROGRESS_COALESCE_BUDGET,
            coalesced_events,
            remaining_events,
            session_id = session_id.as_str(),
            "turn event progress coalesce budget exhausted with queued events remaining"
        );
    }

    latest_thought
}

/// Records the latest child process id observed from a provider turn stream.
fn set_child_pid(child_pid: &Mutex<Option<u32>>, pid: Option<u32>) {
    // Sync critical section (single assignment, no `.await`);
    // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex Selection".
    if let Ok(mut guard) = child_pid.lock() {
        *guard = pid;
    }
}

/// Converts post-turn main-checkout tracked-file changes into transcript
/// warnings while preserving the successful provider result.
///
/// When no main checkout was captured (bare shared repository), the provider
/// result passes through unchanged because there is no main checkout to guard.
async fn add_main_checkout_warning(
    context: &SessionWorkerContext,
    main_checkout_snapshot: Option<&MainCheckoutSnapshot>,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<TurnResult, AgentError> {
    let result = turn_result?;
    let Some(main_checkout_snapshot) = main_checkout_snapshot else {
        return Ok(result);
    };
    match main_checkout_snapshot.dirty_warning(context).await {
        Ok(Some(warning)) => {
            append_main_checkout_warning(context, warning).await;

            Ok(result)
        }
        Ok(None) => Ok(result),
        Err(error) => Err(AgentError::Backend(error.to_string())),
    }
}

/// Clears tracked accounting and sends `SIGTERM` only for a CLI transport.
/// App-server PIDs may outlive a turn and must never authorize a signal.
///
/// Best-effort: the PID slot may be `None` before a runtime starts, or the
/// process may have already exited. Both cases are silently ignored.
pub(super) fn terminate_child_process(child_pid: &Mutex<Option<u32>>, kind: AgentKind) {
    // Sync critical section (the guard is dropped at the end of the chain
    // expression, before any `.await`); `std::sync::Mutex` is the correct
    // choice per CLAUDE.md §"Mutex Selection".
    let active_pid = child_pid
        .lock()
        .ok()
        .and_then(|mut child_pid| child_pid.take());

    if !agent::transport_mode(kind).uses_app_server()
        && let Some(pid) = active_pid
    {
        process::send_terminate_signal(pid);
    }
}

/// Replaces the shared cancellation token for a new turn and returns the
/// token used by the running channel future.
fn fresh_turn_cancel_token(
    context: &SessionWorkerContext,
) -> Result<CancellationToken, SessionError> {
    // Sync critical section (assignment + clone, no `.await`); `std::sync::Mutex`
    // is the correct choice per CLAUDE.md §"Mutex Selection".
    let mut guard = context
        .cancel_token
        .lock()
        .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))?;
    *guard = CancellationToken::new();

    Ok(guard.clone())
}

/// Appends one main-checkout warning to the live and persisted transcript.
async fn append_main_checkout_warning(context: &SessionWorkerContext, warning: String) {
    SessionTaskService::append_workflow_notice(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &warning,
    )
    .await;
}

/// Spawns read-only title generation while a session still has its
/// provisional title.
async fn spawn_turn_title_generation(
    context: &SessionWorkerContext,
    one_shot_client: Arc<dyn OneShotClient>,
    session_project_id: Option<i64>,
    prompt: &str,
    session_agent: AgentSelection,
) {
    let title_agent = setting::load_default_fast_agent_selection_from_repositories(
        &context.db,
        session_project_id,
        session_agent,
        AgentKind::ALL,
    )
    .await;
    let title_reasoning_level = load_title_reasoning_level(&context.db, session_project_id).await;
    let title_speed_mode = setting::load_project_speed_mode_setting(
        &context.db,
        session_project_id,
        SettingName::DefaultFastSpeedMode,
    )
    .await;
    let title_speed_mode = if title_agent.kind().supports_speed_mode() {
        title_speed_mode
    } else {
        crate::domain::agent::SpeedMode::Normal
    };
    let title_agent = title_agent.compatible_with_speed_mode(title_speed_mode);

    let _title_generation_task =
        SessionManager::spawn_session_title_generation_task(SessionTitleGenerationTaskInput {
            app_event_tx: context.app_event_tx.clone(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            latest_request: prompt.to_string(),
            one_shot_client,
            requires_provisional_title: true,
            reasoning_level: title_reasoning_level,
            session_agent: title_agent,
            session_id: context.session_id.clone(),
            speed_mode: title_speed_mode,
            tracked_generation: None,
        })
        .await;
}

/// Loads the Fast-role reasoning default used by detached title generation.
async fn load_title_reasoning_level(
    db: &AppRepositories,
    session_project_id: Option<i64>,
) -> ReasoningLevel {
    let Some(project_id) = session_project_id else {
        return ReasoningLevel::default();
    };

    db.settings()
        .load_project_reasoning_level(project_id, SettingName::DefaultFastReasoningLevel)
        .await
        .unwrap_or_default()
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
    use super::*;
    use crate::infra::db::{DbError, PersistedSessionCreation};

    #[test]
    fn read_only_chat_prompt_redirects_write_access_requests_to_mode_shortcut() {
        // Arrange, Act
        let prompt = READ_ONLY_CHAT_PROMPT;

        // Assert
        assert!(prompt.contains("Do not ask a clarification question requesting write access"));
        assert!(prompt.contains("switching the session to `Auto Edit` with `Shift+Tab`"));
    }

    #[tokio::test]
    async fn persisted_research_role_selects_read_only_permission_mode() {
        // Arrange
        let repositories = AppRepositories::in_memory().await.expect("db should open");
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        repositories
            .sessions()
            .insert_session("worker", "gpt-5.6-sol", "main", "InProgress", project_id)
            .await
            .expect("failed to insert worker session");
        repositories
            .sessions()
            .insert_session(
                "read-only-worker",
                "gpt-5.6-sol",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert read-only worker session");
        repositories
            .sessions()
            .update_session_permission_mode("read-only-worker", PermissionMode::ReadOnly)
            .await
            .expect("failed to set read-only permission mode");
        repositories
            .sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id: "researcher",
                is_draft: false,
                model: "gpt-5.6-sol",
                orchestration_task_id: None,
                parent_session_id: None,
                permission_mode: PermissionMode::AutoEdit,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                response_style: ag_agent::ResponseStyle::default(),
                role: Some("OrchestrationResearcher"),
                speed_mode: crate::domain::agent::SpeedMode::Normal,
                status: "InProgress",
            })
            .await
            .expect("failed to insert research session");

        // Act
        let worker_mode = load_session_permission_mode(&repositories, "worker")
            .await
            .expect("worker mode should load");
        let read_only_worker_mode = load_session_permission_mode(&repositories, "read-only-worker")
            .await
            .expect("read-only worker mode should load");
        let research_mode = load_session_permission_mode(&repositories, "researcher")
            .await
            .expect("research mode should load");
        let missing_error = load_session_permission_mode(&repositories, "missing")
            .await
            .expect_err("missing session should fail");

        // Assert
        assert_eq!(worker_mode, PermissionMode::AutoEdit);
        assert_eq!(read_only_worker_mode, PermissionMode::ReadOnly);
        assert_eq!(research_mode, PermissionMode::ReadOnly);
        assert!(matches!(missing_error, SessionError::NotFound));
    }

    #[tokio::test]
    async fn permission_mode_load_propagates_query_errors() {
        // Arrange
        let (repositories, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        pool.close().await;

        // Act
        let result = load_session_permission_mode(&repositories, "worker").await;

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::Db(DbError::Query(sqlx::Error::PoolClosed)))
        ));
    }

    #[tokio::test]
    async fn response_style_load_returns_persisted_value_and_defaults_missing_sessions() {
        // Arrange
        let repositories = AppRepositories::in_memory().await.expect("db should open");
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        repositories
            .sessions()
            .insert_session("worker", "gpt-5.6-sol", "main", "InProgress", project_id)
            .await
            .expect("failed to insert worker session");
        repositories
            .sessions()
            .update_session_response_style("worker", ResponseStyle::Concise)
            .await
            .expect("failed to persist response style");

        // Act
        let persisted_style = load_session_response_style(&repositories, "worker").await;
        let missing_style = load_session_response_style(&repositories, "missing").await;

        // Assert
        assert_eq!(persisted_style, ResponseStyle::Concise);
        assert_eq!(missing_style, ResponseStyle::Balanced);
    }

    #[tokio::test]
    async fn permission_mode_load_rejects_invalid_persisted_values() {
        // Arrange
        let (repositories, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("db should open");
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        repositories
            .sessions()
            .insert_session("worker", "gpt-5.6-sol", "main", "InProgress", project_id)
            .await
            .expect("failed to insert worker session");
        sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'worker'")
            .execute(&pool)
            .await
            .expect("failed to corrupt permission mode");

        // Act
        let permission_result = load_session_permission_mode(&repositories, "worker").await;
        sqlx::query(
            "UPDATE session SET permission_mode = 'auto_edit', role = 'invalid' WHERE id = \
             'worker'",
        )
        .execute(&pool)
        .await
        .expect("failed to corrupt session role");
        let role_result = load_session_permission_mode(&repositories, "worker").await;

        // Assert
        assert!(matches!(
            permission_result,
            Err(SessionError::Db(DbError::InvalidData {
                entity: "session permission mode",
                ..
            }))
        ));
        assert!(matches!(
            role_result,
            Err(SessionError::Db(DbError::InvalidData {
                entity: "session role",
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn title_reasoning_level_defaults_without_project() {
        // Arrange
        let repositories = AppRepositories::in_memory().await.expect("db should open");

        // Act
        let reasoning_level = load_title_reasoning_level(&repositories, None).await;

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::High);
    }
}
