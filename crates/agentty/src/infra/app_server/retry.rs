//! Shared app-server restart and retry orchestration.

use super::contract::{
    AppServerFuture, AppServerTurnRequest, AppServerTurnResponse, BorrowedAppServerFuture,
};
use super::error::AppServerError;
use super::prompt::{read_latest_session_output, turn_prompt_for_runtime};
use super::registry::AppServerSessionRegistry;
use crate::infra::channel::TurnPrompt;

/// Callbacks for inspecting runtime state during turn execution.
///
/// Bundles the query functions that [`run_turn_with_restart_retry`] uses to
/// check whether the runtime matches the current request, whether it restored
/// provider-native context, and to extract identifiers.
pub struct RuntimeInspector<Runtime> {
    /// Returns `true` when the existing runtime is compatible with the request.
    pub matches_request: fn(&Runtime, &AppServerTurnRequest) -> bool,
    /// Returns the OS process id of the runtime, when available.
    pub pid: fn(&Runtime) -> Option<u32>,
    /// Returns the provider-native conversation id, when available.
    pub provider_conversation_id: fn(&Runtime) -> Option<String>,
    /// Returns `true` when the runtime bootstrapped by restoring prior context.
    pub restored_context: fn(&Runtime) -> bool,
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
/// # Errors
/// Returns an error when runtime startup/execution fails, retry fails, or the
/// session registry lock is unavailable.
pub async fn run_turn_with_restart_retry<Runtime, StartRuntime, RunTurn, ShutdownRuntime>(
    sessions: &AppServerSessionRegistry<Runtime>,
    request: AppServerTurnRequest,
    inspector: RuntimeInspector<Runtime>,
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
    let mut session_runtime = sessions.take_session(&session_id)?;

    if session_runtime
        .as_ref()
        .is_some_and(|runtime| !(inspector.matches_request)(runtime, &request))
    {
        if let Some(runtime) = session_runtime.as_mut() {
            shutdown_runtime(runtime).await;
        }

        session_runtime = None;
    }

    let had_existing_runtime = session_runtime.is_some();
    let mut session_runtime = match session_runtime {
        Some(existing_runtime) => existing_runtime,
        None => start_runtime(&request).await?,
    };
    let first_replays = needs_replay(had_existing_runtime, &request, &inspector, &session_runtime);
    let first_prompt = match build_attempt_prompt(
        &request,
        first_replays,
        &mut shutdown_runtime,
        &mut session_runtime,
    )
    .await
    {
        Ok(prompt) => prompt,
        Err(error) => return Err(error),
    };
    let first_attempt = run_turn_with_runtime(&mut session_runtime, &first_prompt).await;
    if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
        let pid = (inspector.pid)(&session_runtime);
        let provider_conversation_id = (inspector.provider_conversation_id)(&session_runtime);
        if let Err((error, mut leaked)) =
            sessions.store_session_or_recover(session_id, session_runtime)
        {
            shutdown_runtime(&mut leaked).await;

            return Err(error);
        }

        return Ok(AppServerTurnResponse {
            assistant_message,
            context_reset: first_replays,
            input_tokens,
            output_tokens,
            pid,
            provider_conversation_id,
        });
    }

    let first_error = first_attempt
        .err()
        .unwrap_or_else(|| AppServerError::Provider("App-server turn failed".to_string()));
    shutdown_runtime(&mut session_runtime).await;
    let mut restarted = start_runtime(&request).await?;
    let retry_replays = needs_replay(false, &request, &inspector, &restarted);
    let retry_prompt = match build_attempt_prompt(
        &request,
        retry_replays,
        &mut shutdown_runtime,
        &mut restarted,
    )
    .await
    {
        Ok(prompt) => prompt,
        Err(error) => return Err(error),
    };
    match run_turn_with_runtime(&mut restarted, &retry_prompt).await {
        Ok((assistant_message, input_tokens, output_tokens)) => {
            let pid = (inspector.pid)(&restarted);
            let provider_conversation_id = (inspector.provider_conversation_id)(&restarted);
            if let Err((error, mut leaked)) =
                sessions.store_session_or_recover(session_id, restarted)
            {
                shutdown_runtime(&mut leaked).await;

                return Err(error);
            }

            Ok(AppServerTurnResponse {
                assistant_message,
                context_reset: retry_replays,
                input_tokens,
                output_tokens,
                pid,
                provider_conversation_id,
            })
        }
        Err(retry_error) => {
            shutdown_runtime(&mut restarted).await;

            Err(AppServerError::RetryExhausted {
                provider: sessions.provider_name(),
                first_error: first_error.to_string(),
                retry_error: retry_error.to_string(),
            })
        }
    }
}

/// Returns `true` when the attempt should replay prior session output as
/// context for the runtime.
fn needs_replay<Runtime>(
    had_existing_runtime: bool,
    request: &AppServerTurnRequest,
    inspector: &RuntimeInspector<Runtime>,
    runtime: &Runtime,
) -> bool {
    !had_existing_runtime
        && read_latest_session_output(request)
            .as_deref()
            .is_some_and(|session_output| !session_output.trim().is_empty())
        && !(inspector.restored_context)(runtime)
}

/// Prepares the prompt for one turn attempt, shutting down the runtime on
/// failure.
async fn build_attempt_prompt<Runtime, ShutdownRuntime>(
    request: &AppServerTurnRequest,
    replays_context: bool,
    shutdown_runtime: &mut ShutdownRuntime,
    runtime: &mut Runtime,
) -> Result<TurnPrompt, AppServerError>
where
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let session_output = read_latest_session_output(request);

    match turn_prompt_for_runtime(
        &request.prompt,
        &request.request_kind,
        session_output.as_deref(),
        replays_context,
    ) {
        Ok(prompt) => Ok(prompt),
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

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::infra::channel::{AgentRequestKind, TurnPrompt};

    struct TestRuntime {
        model: String,
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn session_resume_request_kind(session_output: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume {
            session_output: session_output.map(ToString::to_string),
        }
    }

    #[test]
    fn take_session_returns_stored_runtime() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        sessions
            .store_session(
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
            false,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.contains("summary"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(
            prompt,
            &session_resume_request_kind(Some("assistant: proposed plan")),
            Some("assistant: proposed plan"),
            true,
        )
        .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.contains("Continue this session using the full transcript below."));
        assert!(turn_prompt.contains("assistant: proposed plan"));
        assert!(turn_prompt.contains(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_uses_shared_protocol_wrapper_for_utility_prompts() {
        // Arrange
        let prompt = "Generate title";

        // Act
        let turn_prompt =
            turn_prompt_for_runtime(prompt, &AgentRequestKind::UtilityPrompt, None, false)
                .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("summary"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn read_latest_session_output_prefers_live_buffer_over_snapshot() {
        // Arrange
        let live_output = Arc::new(Mutex::new("live content from stream".to_string()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: Some(live_output),
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("live content from stream".to_string()));
    }

    #[test]
    fn read_latest_session_output_falls_back_to_snapshot_when_live_buffer_is_empty() {
        // Arrange
        let live_output = Arc::new(Mutex::new(String::new()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: Some(live_output),
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("stale snapshot".to_string()));
    }

    #[test]
    fn read_latest_session_output_falls_back_to_snapshot_when_no_live_buffer() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, Some("stale snapshot".to_string()));
    }

    #[test]
    fn read_latest_session_output_returns_none_when_both_are_absent() {
        // Arrange
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_start_request_kind(),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
        };

        // Act
        let output = read_latest_session_output(&request);

        // Assert
        assert_eq!(output, None);
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_uses_live_output_on_retry() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let live_output = Arc::new(Mutex::new("streamed before crash".to_string()));
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: Some(live_output),
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("stale snapshot")),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
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
                restored_context: |_runtime| false,
            },
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
            "retry prompt should contain live output, not stale snapshot"
        );
        assert!(
            !retry_prompt.contains("stale snapshot"),
            "retry prompt should use live output instead of stale snapshot"
        );
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_restarts_once_after_first_failure() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("previous output")),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
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
                restored_context: |_runtime| false,
            },
            {
                let start_count = Arc::clone(&start_count);
                move |request: &AppServerTurnRequest| {
                    let start_count = Arc::clone(&start_count);
                    let model = request.model.clone();

                    Box::pin(async move {
                        start_count.fetch_add(1, Ordering::SeqCst);

                        Ok(TestRuntime { model })
                    })
                }
            },
            {
                let run_count = Arc::clone(&run_count);
                move |_runtime, _prompt| {
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
    }

    /// Verifies restored-context retries keep the user prompt while avoiding
    /// transcript replay.
    #[tokio::test]
    async fn run_turn_with_restart_retry_skips_replay_when_runtime_restores_context() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            live_session_output: None,
            model: "model-a".to_string(),
            prompt: "Do work".into(),
            request_kind: session_resume_request_kind(Some("previous output")),
            provider_conversation_id: Some("thread-123".to_string()),
            reasoning_level: ReasoningLevel::default(),
            session_id: "session-1".to_string(),
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
                restored_context: |_runtime| true,
            },
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
        assert!(!captured_prompt.contains("previous output"));
    }
}
