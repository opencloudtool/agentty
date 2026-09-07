//! Shared app-server restart and retry orchestration.

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};

use super::contract::{
    AppServerFuture, AppServerTurnRequest, AppServerTurnResponse, BorrowedAppServerFuture,
};
use super::error::AppServerError;
use super::prompt::{
    instruction_delivery_mode_for_runtime, read_latest_replay_transcript, turn_prompt_for_runtime,
};
use super::registry::{ActiveAppServerTurn, AppServerSessionRegistry};
use crate::agent::replay::ReplayContext;

/// Callbacks for inspecting runtime state during turn execution.
///
/// Bundles the query functions that [`run_turn_with_restart_retry`] uses to
/// check whether the runtime matches the current request, whether it restored
/// provider-native context, and to extract identifiers.
pub(crate) struct RuntimeInspector<Runtime> {
    /// Returns `true` when the existing runtime is compatible with the request.
    pub(crate) matches_request: fn(&Runtime, &AppServerTurnRequest) -> bool,
    /// Returns the OS process id of the runtime, when available.
    pub(crate) pid: fn(&Runtime) -> Option<u32>,
    /// Returns the provider-native conversation id, when available.
    pub(crate) provider_conversation_id: fn(&Runtime) -> Option<String>,
    /// Whether successful runtimes remain resident between session turns.
    pub(crate) retain_runtime_after_turn: bool,
    /// Returns `true` when the runtime bootstrapped by restoring prior context.
    pub(crate) restored_context: fn(&Runtime) -> bool,
}

/// Runs one app-server turn with restart-and-retry semantics.
///
/// Runtime lifecycle details (`start`, per-turn execution, and shutdown) are
/// injected by the provider. The function keeps a session-scoped runtime in
/// `sessions`, invalidates it when request shape changes, and retries once
/// after restarting the runtime when the first attempt fails.
///
/// Prompt transcript replay is only applied when a newly started runtime does
/// not expose restored provider-native context for the session.
///
/// `schema_instruction_mode` is selected by the provider client so bootstrap
/// prompts include the full JSON Schema only for transports that need
/// prompt-side schema guidance.
///
/// # Errors
/// Returns an error when runtime startup/execution fails, retry fails, or the
/// session registry lock is unavailable.
pub(crate) async fn run_turn_with_restart_retry<Runtime, StartRuntime, RunTurn, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    request: AppServerTurnRequest,
    inspector: RuntimeInspector<Runtime>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    mut start_runtime: StartRuntime,
    mut run_turn_with_runtime: RunTurn,
    mut shutdown_runtime: ShutdownRuntime,
) -> Result<AppServerTurnResponse, AppServerError>
where
    StartRuntime: FnMut(&AppServerTurnRequest) -> AppServerFuture<Result<Runtime, AppServerError>>,
    RunTurn: for<'scope> FnMut(
        &'scope mut Runtime,
        &'scope TurnPrompt,
    ) -> BorrowedAppServerFuture<
        'scope,
        Result<(String, u64, u64), AppServerError>,
    >,
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let session_id = request.session_id.clone();
    let active_turn = sessions.register_active_turn(&session_id)?;
    let session_runtime =
        take_compatible_session_runtime(sessions, &request, &inspector, &mut shutdown_runtime)
            .await?;

    let had_existing_runtime = session_runtime.is_some();
    let mut session_runtime = match session_runtime {
        Some(existing_runtime) => existing_runtime,
        None => start_runtime(&request).await?,
    };
    let first_replays = needs_replay(had_existing_runtime, &request, &inspector, &session_runtime);
    let first_provider_conversation_id = (inspector.provider_conversation_id)(&session_runtime);
    let first_attempt = {
        let (first_prompt, _first_replay) = build_attempt_prompt(
            &request,
            first_replays,
            first_provider_conversation_id.as_deref(),
            schema_instruction_mode,
            &mut shutdown_runtime,
            &mut session_runtime,
        )
        .await?;

        run_cancellable_turn_attempt(
            &active_turn,
            &mut session_runtime,
            &first_prompt,
            &mut run_turn_with_runtime,
            &mut shutdown_runtime,
        )
        .await
    };
    if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
        return complete_successful_runtime_response(
            sessions,
            session_id,
            session_runtime,
            first_replays,
            (assistant_message, input_tokens, output_tokens),
            &inspector,
            &mut shutdown_runtime,
        )
        .await;
    }

    let first_error = first_attempt
        .err()
        .unwrap_or_else(|| AppServerError::Provider("App-server turn failed".to_string()));
    if matches!(first_error, AppServerError::InterruptedByUser(_)) {
        return Err(first_error);
    }

    shutdown_runtime(&mut session_runtime).await;
    let mut restarted = start_runtime(&request).await?;
    let retry_replays = needs_replay(false, &request, &inspector, &restarted);
    let retry_provider_conversation_id = (inspector.provider_conversation_id)(&restarted);
    let (retry_prompt, _retry_replay) = build_attempt_prompt(
        &request,
        retry_replays,
        retry_provider_conversation_id.as_deref(),
        schema_instruction_mode,
        &mut shutdown_runtime,
        &mut restarted,
    )
    .await?;
    match run_cancellable_turn_attempt(
        &active_turn,
        &mut restarted,
        &retry_prompt,
        &mut run_turn_with_runtime,
        &mut shutdown_runtime,
    )
    .await
    {
        Ok(attempt_output) => {
            complete_successful_runtime_response(
                sessions,
                session_id,
                restarted,
                retry_replays,
                attempt_output,
                &inspector,
                &mut shutdown_runtime,
            )
            .await
        }
        Err(retry_error) => {
            if matches!(retry_error, AppServerError::InterruptedByUser(_)) {
                return Err(retry_error);
            }

            shutdown_runtime(&mut restarted).await;

            Err(AppServerError::RetryExhausted {
                provider: sessions.provider_name(),
                first_error: first_error.to_string(),
                retry_error: retry_error.to_string(),
            })
        }
    }
}

/// Takes the idle runtime for a request and shuts it down when it no longer
/// matches the requested model or provider context.
async fn take_compatible_session_runtime<Runtime, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    request: &AppServerTurnRequest,
    inspector: &RuntimeInspector<Runtime>,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<Option<Runtime>, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let mut session_runtime = sessions.take_session(&request.session_id)?;

    if session_runtime
        .as_ref()
        .is_some_and(|runtime| !(inspector.matches_request)(runtime, request))
    {
        if let Some(runtime) = session_runtime.as_mut() {
            shutdown_runtime(runtime).await;
        }

        session_runtime = None;
    }

    Ok(session_runtime)
}

/// Completes a successful turn, either retaining or shutting down its runtime,
/// and builds the normalized app-server response.
///
/// If the registry cannot accept the runtime, this shuts the runtime down
/// before returning the lock error so app-server child processes do not leak.
async fn complete_successful_runtime_response<Runtime, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    session_id: String,
    mut session_runtime: Runtime,
    context_reset: bool,
    attempt_output: (String, u64, u64),
    inspector: &RuntimeInspector<Runtime>,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<AppServerTurnResponse, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let (assistant_message, input_tokens, output_tokens) = attempt_output;
    let provider_conversation_id = (inspector.provider_conversation_id)(&session_runtime);

    if !inspector.retain_runtime_after_turn {
        shutdown_runtime(&mut session_runtime).await;

        return Ok(AppServerTurnResponse {
            assistant_message,
            context_reset,
            input_tokens,
            output_tokens,
            pid: None,
            provider_conversation_id,
        });
    }

    let pid = (inspector.pid)(&session_runtime);
    if let Err((error, mut leaked)) = sessions.store_session_or_recover(session_id, session_runtime)
    {
        shutdown_runtime(&mut leaked).await;

        return Err(error);
    }

    Ok(AppServerTurnResponse {
        assistant_message,
        context_reset,
        input_tokens,
        output_tokens,
        pid,
        provider_conversation_id,
    })
}

/// Runs one provider runtime attempt while watching the session-scoped
/// app-server cancellation token.
///
/// `run_turn_with_restart_retry()` temporarily owns the runtime while a turn is
/// in flight, so provider `shutdown_session()` cannot remove it from the idle
/// registry. This helper gives that shutdown path a token to fire; when it
/// fires, the running turn future is dropped, the runtime is shut down through
/// the provider lifecycle hook, and the attempt returns a user-interruption
/// error instead of retrying.
async fn run_cancellable_turn_attempt<Runtime, RunTurn, ShutdownRuntime>(
    active_turn: &ActiveAppServerTurn,
    runtime: &mut Runtime,
    prompt: &TurnPrompt,
    run_turn_with_runtime: &mut RunTurn,
    shutdown_runtime: &mut ShutdownRuntime,
) -> Result<(String, u64, u64), AppServerError>
where
    RunTurn: for<'scope> FnMut(
        &'scope mut Runtime,
        &'scope TurnPrompt,
    ) -> BorrowedAppServerFuture<
        'scope,
        Result<(String, u64, u64), AppServerError>,
    >,
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let cancellation_token = active_turn.token();
    if cancellation_token.is_cancelled() {
        shutdown_runtime(runtime).await;

        return Err(interrupted_by_user_error());
    }

    let turn_outcome = {
        let turn_future = run_turn_with_runtime(runtime, prompt);
        tokio::pin!(turn_future);
        tokio::select! {
            result = &mut turn_future => TurnAttemptOutcome::Completed(result),
            () = cancellation_token.cancelled() => TurnAttemptOutcome::Interrupted,
        }
    };

    match turn_outcome {
        TurnAttemptOutcome::Completed(result) => result,
        TurnAttemptOutcome::Interrupted => {
            shutdown_runtime(runtime).await;

            Err(interrupted_by_user_error())
        }
    }
}

/// Result of racing one app-server turn against cancellation.
enum TurnAttemptOutcome {
    /// The provider turn completed before cancellation fired.
    Completed(Result<(String, u64, u64), AppServerError>),
    /// The session cancellation token fired first.
    Interrupted,
}

/// Builds the app-server interruption error shared by initial and retry turns.
fn interrupted_by_user_error() -> AppServerError {
    AppServerError::InterruptedByUser("[Stopped] Session interrupted by user.".to_string())
}

/// Returns `true` when the attempt should replay prior transcript as
/// context for the runtime.
fn needs_replay<Runtime>(
    had_existing_runtime: bool,
    request: &AppServerTurnRequest,
    inspector: &RuntimeInspector<Runtime>,
    runtime: &Runtime,
) -> bool {
    !had_existing_runtime
        && read_latest_replay_transcript(request)
            .as_deref()
            .is_some_and(|replay_transcript| !replay_transcript.trim().is_empty())
        && !(inspector.restored_context)(runtime)
}

/// Prepares the prompt for one turn attempt, shutting down the runtime on
/// failure.
async fn build_attempt_prompt<Runtime, ShutdownRuntime>(
    request: &AppServerTurnRequest,
    replays_context: bool,
    current_provider_conversation_id: Option<&str>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    shutdown_runtime: &mut ShutdownRuntime,
    runtime: &mut Runtime,
) -> Result<(TurnPrompt, ReplayContext), AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let replay_transcript = read_latest_replay_transcript(request);
    let replay = match ReplayContext::prepare(request.folder.clone(), replay_transcript).await {
        Ok(replay) => replay,
        Err(error) => {
            shutdown_runtime(runtime).await;

            return Err(AppServerError::PromptRender(error.to_string()));
        }
    };
    let instruction_delivery_mode = instruction_delivery_mode_for_runtime(
        request,
        current_provider_conversation_id,
        replays_context,
    );

    let mut prompt = request.prompt.clone();
    if !replays_context && let Some(reference) = &replay.reference {
        prompt.text = format!("{reference}\n\n{}", prompt.agent_text());
        prompt.text_source = ag_protocol::TurnPromptTextSource::AgentData;
    }

    match turn_prompt_for_runtime(
        &prompt,
        &request.request_kind,
        replay.text.as_deref(),
        instruction_delivery_mode,
        &request.personality,
        schema_instruction_mode,
        &request.folder,
    ) {
        Ok(prompt) => Ok((prompt, replay)),
        Err(error) => {
            shutdown_runtime(runtime).await;

            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};

    use super::*;
    use crate::agent::InstructionDeliveryMode;
    use crate::channel::{AgentRequestKind, LiveTranscript};
    use crate::model::agent::ReasoningLevel;

    #[derive(Debug)]
    struct TestRuntime {
        model: String,
    }

    impl TestRuntime {
        fn shutdown(&mut self) -> BorrowedAppServerFuture<'_, ()> {
            Box::pin(async move {
                self.model = "stopped".into();
            })
        }
    }

    #[derive(Debug)]
    struct TestLiveTranscript {
        text: String,
    }

    impl LiveTranscript for TestLiveTranscript {
        fn replay_text(&self) -> Option<String> {
            Some(self.text.clone())
        }
    }

    fn live_transcript(text: &str) -> Arc<dyn LiveTranscript> {
        Arc::new(TestLiveTranscript {
            text: text.to_string(),
        })
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn session_resume_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionResume
    }

    #[tokio::test]
    async fn replay_attempt_owns_archive_and_stops_runtime_on_archive_error() {
        // Arrange
        let folder = tempfile::tempdir().expect("workspace");
        let mut shutdown = TestRuntime::shutdown;
        let mut runtime = TestRuntime {
            model: "model-a".into(),
        };
        let mut request = AppServerTurnRequest {
            folder: folder.path().to_owned(),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".into(),
            permission_mode: crate::model::permission::PermissionMode::ReadOnly,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Continue".into(),
            request_kind: AgentRequestKind::SessionResume,
            replay_transcript: Some("history".repeat(8192)),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "replay-test".into(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };

        // Act
        let (prompt, archive) = build_attempt_prompt(
            &request,
            true,
            None,
            ProtocolSchemaInstructionMode::TransportSchema,
            &mut shutdown,
            &mut runtime,
        )
        .await
        .expect("archive");
        let live_files = std::fs::read_dir(folder.path()).expect("files").count();
        drop(archive);
        let (native_prompt, native_archive) = build_attempt_prompt(
            &request,
            false,
            Some("thread"),
            ProtocolSchemaInstructionMode::TransportSchema,
            &mut shutdown,
            &mut runtime,
        )
        .await
        .expect("native archive");
        drop(native_archive);
        let remaining_files = std::fs::read_dir(folder.path()).expect("files").count();
        request.folder = folder.path().join("missing");
        let failure = build_attempt_prompt(
            &request,
            true,
            None,
            ProtocolSchemaInstructionMode::TransportSchema,
            &mut shutdown,
            &mut runtime,
        )
        .await;

        // Assert
        assert!(prompt.contains("Session checkpoint"));
        assert!(native_prompt.contains("Earlier temporary history paths have expired"));
        assert!(!native_prompt.contains("Session checkpoint"));

        assert_eq!(live_files, 1);
        assert_eq!(remaining_files, 0);
        assert!(matches!(failure, Err(AppServerError::PromptRender(_))));
        assert_eq!(runtime.model, "stopped");
    }

    #[test]
    fn take_session_returns_stored_runtime() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        sessions
            .store_session_or_recover(
                "session-1".to_string(),
                TestRuntime {
                    model: "model-a".to_string(),
                },
            )
            .expect("store should succeed");

        // Act
        let session = sessions
            .take_session("session-1")
            .expect("take should succeed");

        // Assert
        assert_eq!(
            session.map(|runtime| runtime.model),
            Some("model-a".to_string())
        );
    }

    #[test]
    fn turn_prompt_for_runtime_adds_repo_root_path_instructions_without_context_reset() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &session_start_request_kind(),
            Some("prior context"),
            InstructionDeliveryMode::BootstrapFull,
            &crate::channel::PersonalityPrompt::default(),
            ProtocolSchemaInstructionMode::PromptSchema,
            std::path::Path::new("/tmp/agentty-wt/session-1"),
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(!turn_prompt.contains("summary"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &session_resume_request_kind(),
            Some("assistant: proposed plan"),
            InstructionDeliveryMode::BootstrapWithReplay,
            &crate::channel::PersonalityPrompt::default(),
            ProtocolSchemaInstructionMode::PromptSchema,
            std::path::Path::new("/tmp/agentty-wt/session-1"),
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.contains("Continue from the supplied session context"));
        assert!(
            turn_prompt
                .contains(r"\<session_transcript> assistant: proposed plan \</session_transcript>")
        );
        assert!(turn_prompt.contains(r"\<user_prompt> Implement feature \</user_prompt>"));
    }

    #[test]
    fn turn_prompt_for_runtime_uses_shared_protocol_wrapper_for_utility_prompts() {
        // Arrange
        let prompt = "Generate title";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &AgentRequestKind::UtilityPrompt,
            None,
            InstructionDeliveryMode::BootstrapFull,
            &crate::channel::PersonalityPrompt::default(),
            ProtocolSchemaInstructionMode::PromptSchema,
            std::path::Path::new("/tmp/agentty-wt/session-1"),
        )
        .expect("turn prompt should render");

        // Assert
        assert!(!turn_prompt.contains("summary"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn read_latest_replay_transcript_prefers_live_buffer_over_snapshot() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: Some(live_transcript("live content from stream")),
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some("queued snapshot".to_string()),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };

        // Act
        let output = read_latest_replay_transcript(&request);

        // Assert
        assert_eq!(output, Some("live content from stream".to_string()));
    }

    #[test]
    fn read_latest_replay_transcript_falls_back_to_snapshot_when_live_buffer_is_empty() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: Some(live_transcript("")),
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some("queued snapshot".to_string()),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };

        // Act
        let output = read_latest_replay_transcript(&request);

        // Assert
        assert_eq!(output, Some("queued snapshot".to_string()));
    }

    #[test]
    fn read_latest_replay_transcript_falls_back_to_snapshot_when_no_live_buffer() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some("queued snapshot".to_string()),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };

        // Act
        let output = read_latest_replay_transcript(&request);

        // Assert
        assert_eq!(output, Some("queued snapshot".to_string()));
    }

    #[test]
    fn read_latest_replay_transcript_returns_none_when_both_are_absent() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_start_request_kind(),
            replay_transcript: None,
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };

        // Act
        let output = read_latest_replay_transcript(&request);

        // Assert
        assert_eq!(output, None);
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_uses_live_output_on_retry() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: Some(live_transcript("streamed before crash")),
            main_checkout_root: Some(PathBuf::from("/tmp/project")),
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some("queued snapshot".to_string()),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let captured_retry_prompt = Arc::new(Mutex::new(String::new()));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| None,
                retain_runtime_after_turn: true,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let run_count = Arc::new(AtomicUsize::new(0));
                let captured_retry_prompt = Arc::clone(&captured_retry_prompt);
                move |_runtime: &mut TestRuntime, prompt: &TurnPrompt| {
                    let attempt = run_count.fetch_add(1, Ordering::SeqCst);
                    let prompt = prompt.to_string();
                    let captured_retry_prompt = Arc::clone(&captured_retry_prompt);

                    Box::pin(async move {
                        if attempt == 0 {
                            return Err(AppServerError::Provider("first failure".to_string()));
                        }

                        if let Ok(mut guard) = captured_retry_prompt.lock() {
                            *guard = prompt;
                        }

                        Ok(("done".to_string(), 7, 3))
                    })
                }
            },
            |_runtime: &mut TestRuntime| Box::pin(async {}),
        )
        .await
        .expect("retry should succeed");

        // Assert
        assert!(response.context_reset);
        assert_eq!(response.provider_conversation_id, None);
        let retry_prompt = captured_retry_prompt
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        assert!(
            retry_prompt.contains("streamed before crash"),
            "retry prompt should contain live transcript, not queued snapshot"
        );
        assert!(
            !retry_prompt.contains("queued snapshot"),
            "retry prompt should use live transcript instead of queued snapshot"
        );
    }

    #[tokio::test]
    async fn successful_turn_shuts_down_runtime_when_retention_is_disabled() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_start_request_kind(),
            replay_transcript: None,
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let shutdown_count = Arc::new(AtomicUsize::new(0));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| Some("gemini-session".to_string()),
                retain_runtime_after_turn: false,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            |_runtime, _prompt| Box::pin(async { Ok(("done".to_string(), 7, 3)) }),
            {
                let shutdown_count = Arc::clone(&shutdown_count);
                move |_runtime| {
                    let shutdown_count = Arc::clone(&shutdown_count);

                    Box::pin(async move {
                        shutdown_count.fetch_add(1, Ordering::SeqCst);
                    })
                }
            },
        )
        .await
        .expect("turn should succeed");
        let stored_runtime = sessions
            .take_session("session-1")
            .expect("session registry should remain available");

        // Assert
        assert_eq!(response.assistant_message, "done");
        assert_eq!(
            response.provider_conversation_id.as_deref(),
            Some("gemini-session")
        );
        assert_eq!(response.pid, None);
        assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
        assert!(stored_runtime.is_none());
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_restarts_once_after_first_failure() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let folder = tempfile::tempdir().expect("workspace");
        let history = "previous transcript".repeat(4096);
        let archives = Arc::new(Mutex::new(Vec::new()));
        let request = AppServerTurnRequest {
            folder: folder.path().to_owned(),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some(history.clone()),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let start_count = Arc::new(AtomicUsize::new(0));
        let run_count = Arc::new(AtomicUsize::new(0));
        let shutdown_count = Arc::new(AtomicUsize::new(0));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| None,
                retain_runtime_after_turn: true,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            {
                let start_count = Arc::clone(&start_count);
                move |request: &AppServerTurnRequest| {
                    let start_count = Arc::clone(&start_count);
                    let model = request.model.clone();
                    assert!(
                        std::fs::read_dir(&request.folder)
                            .expect("archives")
                            .next()
                            .is_none()
                    );

                    Box::pin(async move {
                        start_count.fetch_add(1, Ordering::SeqCst);

                        Ok(TestRuntime { model })
                    })
                }
            },
            {
                let run_count = Arc::clone(&run_count);
                let folder = folder.path().to_owned();
                let archives = Arc::clone(&archives);
                move |_runtime, prompt| {
                    let entries = std::fs::read_dir(&folder)
                        .expect("archives")
                        .map(|entry| entry.expect("archive").path())
                        .collect::<Vec<_>>();
                    assert_eq!(entries.len(), 1, "only the current attempt archive exists");
                    assert_eq!(
                        std::fs::read_to_string(entries[0].join("history.md")).expect("history"),
                        history
                    );
                    assert!(prompt.contains("Session checkpoint"));
                    archives.lock().expect("archives").push(entries[0].clone());
                    let attempt = run_count.fetch_add(1, Ordering::SeqCst);

                    Box::pin(async move {
                        if attempt == 0 {
                            return Err(AppServerError::Provider("first failure".to_string()));
                        }

                        Ok(("done".to_string(), 7, 3))
                    })
                }
            },
            {
                let shutdown_count = Arc::clone(&shutdown_count);
                move |_runtime| {
                    let shutdown_count = Arc::clone(&shutdown_count);

                    Box::pin(async move {
                        shutdown_count.fetch_add(1, Ordering::SeqCst);
                    })
                }
            },
        )
        .await
        .expect("retry should succeed");

        // Assert
        assert_eq!(response.assistant_message, "done");
        assert!(response.context_reset);
        assert_eq!((response.input_tokens, response.output_tokens), (7, 3));
        assert_eq!(response.pid, Some(42));
        assert_eq!(response.provider_conversation_id, None);
        assert_eq!(start_count.load(Ordering::SeqCst), 2);
        assert_eq!(run_count.load(Ordering::SeqCst), 2);
        assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
        let archives = archives.lock().expect("archives");
        assert_eq!(archives.len(), 2);
        assert_ne!(archives[0], archives[1]);
        assert!(archives.iter().all(|path| !path.exists()));
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_shutdown_signal_interrupts_in_flight_runtime() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some("previous transcript".to_string()),
            provider_conversation_id: None,
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let run_count = Arc::new(AtomicUsize::new(0));
        let shutdown_count = Arc::new(AtomicUsize::new(0));

        // Act
        let result = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(42),
                provider_conversation_id: |_runtime| None,
                retain_runtime_after_turn: true,
                restored_context: |_runtime| false,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let run_count = Arc::clone(&run_count);
                let sessions = sessions.clone();
                move |_runtime, _prompt| {
                    let run_count = Arc::clone(&run_count);
                    let sessions = sessions.clone();

                    Box::pin(async move {
                        run_count.fetch_add(1, Ordering::SeqCst);
                        sessions
                            .cancel_active_turn("session-1")
                            .expect("cancel should signal active turn");
                        std::future::pending::<Result<(String, u64, u64), AppServerError>>().await
                    })
                }
            },
            {
                let shutdown_count = Arc::clone(&shutdown_count);
                move |_runtime| {
                    let shutdown_count = Arc::clone(&shutdown_count);

                    Box::pin(async move {
                        shutdown_count.fetch_add(1, Ordering::SeqCst);
                    })
                }
            },
        )
        .await;

        // Assert
        assert!(matches!(result, Err(AppServerError::InterruptedByUser(_))));
        assert_eq!(run_count.load(Ordering::SeqCst), 1);
        assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
    }

    /// Verifies restored-context retries keep the user prompt while avoiding
    /// transcript replay.
    #[tokio::test]
    async fn run_turn_with_restart_retry_skips_replay_when_runtime_restores_context() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_transcript: None,
            main_checkout_root: None,
            model: "model-a".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(),
            replay_transcript: Some("previous transcript".to_string()),
            provider_conversation_id: Some("thread-123".to_string()),
            persisted_instruction_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
            speed_mode: crate::model::session::SpeedMode::default(),
        };
        let captured_prompt = Arc::new(Mutex::new(String::new()));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            RuntimeInspector {
                matches_request: |runtime: &TestRuntime, request| runtime.model == request.model,
                pid: |_runtime| Some(24),
                provider_conversation_id: |_runtime| Some("thread-123".to_string()),
                retain_runtime_after_turn: true,
                restored_context: |_runtime| true,
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let captured_prompt = Arc::clone(&captured_prompt);
                move |_runtime: &mut TestRuntime, prompt: &TurnPrompt| {
                    let prompt = prompt.to_string();
                    let captured_prompt = Arc::clone(&captured_prompt);

                    Box::pin(async move {
                        if let Ok(mut guard) = captured_prompt.lock() {
                            *guard = prompt;
                        }

                        Ok(("done".to_string(), 1, 1))
                    })
                }
            },
            |_runtime: &mut TestRuntime| Box::pin(async {}),
        )
        .await
        .expect("turn should succeed");

        // Assert
        assert_eq!(response.assistant_message, "done");
        assert!(!response.context_reset);
        assert_eq!(
            response.provider_conversation_id,
            Some("thread-123".to_string())
        );
        assert_eq!(response.pid, Some(24));
        let captured_prompt = captured_prompt
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        assert!(captured_prompt.contains("repository-root-relative POSIX paths"));
        assert!(captured_prompt.ends_with("Do work"));
        assert!(!captured_prompt.contains("previous transcript"));
    }
}
