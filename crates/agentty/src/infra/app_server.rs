//! Shared app-server abstractions used by provider-specific implementations.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// Boxed async result used by [`AppServerClient`] trait methods.
pub type AppServerFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Boxed async result that can borrow values from the current call frame.
type BorrowedAppServerFuture<'scope, T> = Pin<Box<dyn Future<Output = T> + Send + 'scope>>;

/// Incremental event emitted during one app-server turn.
///
/// The caller receives these events through an [`mpsc::UnboundedSender`]
/// channel while the turn is in progress, enabling real-time streaming of
/// agent output and progress updates to the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum AppServerStreamEvent {
    /// Incremental assistant message text received while a turn is running.
    AssistantMessage(String),
    /// An `item/started` event produced a progress description.
    ProgressUpdate(String),
}

/// Input payload for one app-server turn execution.
#[derive(Clone)]
pub struct AppServerTurnRequest {
    pub folder: PathBuf,
    pub model: String,
    pub prompt: String,
    pub session_output: Option<String>,
    pub session_id: String,
}

/// Normalized result for one app-server turn.
pub struct AppServerTurnResponse {
    pub assistant_message: String,
    pub context_reset: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub pid: Option<u32>,
}

/// Shared runtime registry used by app-server providers.
///
/// Each session id maps to one runtime process. Workers temporarily remove a
/// runtime while executing a turn and store it back when the turn succeeds.
pub struct AppServerSessionRegistry<Runtime> {
    provider_name: &'static str,
    sessions: Arc<Mutex<HashMap<String, Runtime>>>,
}

impl<Runtime> AppServerSessionRegistry<Runtime> {
    /// Creates an empty session runtime registry for one provider.
    pub fn new(provider_name: &'static str) -> Self {
        Self {
            provider_name,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Removes and returns the runtime stored for `session_id`.
    ///
    /// # Errors
    /// Returns an error when the session map lock is poisoned.
    pub fn take_session(&self, session_id: &str) -> Result<Option<Runtime>, String> {
        let mut sessions = self.sessions.lock().map_err(|_| {
            format!(
                "Failed to lock {} app-server session map",
                self.provider_name
            )
        })?;

        Ok(sessions.remove(session_id))
    }

    /// Stores or replaces the runtime for `session_id`.
    ///
    /// # Errors
    /// Returns an error when the session map lock is poisoned.
    pub fn store_session(&self, session_id: String, session: Runtime) -> Result<(), String> {
        let mut sessions = self.sessions.lock().map_err(|_| {
            format!(
                "Failed to lock {} app-server session map",
                self.provider_name
            )
        })?;
        sessions.insert(session_id, session);

        Ok(())
    }

    /// Returns the provider label used in user-facing retry errors.
    pub fn provider_name(&self) -> &'static str {
        self.provider_name
    }
}

/// Clones the registry handle by sharing the same underlying session map.
impl<Runtime> Clone for AppServerSessionRegistry<Runtime> {
    fn clone(&self) -> Self {
        Self {
            provider_name: self.provider_name,
            sessions: Arc::clone(&self.sessions),
        }
    }
}

/// Persistent app-server session boundary used by session workers.
#[cfg_attr(test, mockall::automock)]
pub trait AppServerClient: Send + Sync {
    /// Executes one prompt turn for a session and returns normalized output.
    ///
    /// Intermediate events (agent messages, progress updates) are sent through
    /// `stream_tx` as they arrive, enabling the caller to display streaming
    /// output before the turn completes.
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, String>>;

    /// Stops and forgets a session runtime, if one exists.
    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()>;
}

/// Runs one app-server turn with restart-and-retry semantics.
///
/// Runtime lifecycle details (`start`, per-turn execution, and shutdown) are
/// injected by the provider. The function keeps a session-scoped runtime in
/// `sessions`, invalidates it when request shape changes, and retries once
/// after restarting the runtime when the first attempt fails.
///
/// # Errors
/// Returns an error when runtime startup/execution fails, retry fails, or the
/// session registry lock is unavailable.
pub async fn run_turn_with_restart_retry<
    Runtime,
    MatchesRequest,
    RuntimePid,
    StartRuntime,
    RunTurn,
    ShutdownRuntime,
>(
    sessions: &AppServerSessionRegistry<Runtime>,
    request: AppServerTurnRequest,
    matches_request: MatchesRequest,
    runtime_pid: RuntimePid,
    mut start_runtime: StartRuntime,
    mut run_turn_with_runtime: RunTurn,
    mut shutdown_runtime: ShutdownRuntime,
) -> Result<AppServerTurnResponse, String>
where
    MatchesRequest: Fn(&Runtime, &AppServerTurnRequest) -> bool,
    RuntimePid: Fn(&Runtime) -> Option<u32>,
    StartRuntime: FnMut(&AppServerTurnRequest) -> AppServerFuture<Result<Runtime, String>>,
    RunTurn:
        for<'scope> FnMut(
            &'scope mut Runtime,
            &'scope str,
        )
            -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), String>>,
    ShutdownRuntime: for<'scope> FnMut(&'scope mut Runtime) -> BorrowedAppServerFuture<'scope, ()>,
{
    let mut context_reset = false;
    let session_id = request.session_id.clone();
    let mut session_runtime = sessions.take_session(&session_id)?;

    if session_runtime
        .as_ref()
        .is_some_and(|runtime| !matches_request(runtime, &request))
    {
        if let Some(runtime) = session_runtime.as_mut() {
            shutdown_runtime(runtime).await;
        }

        session_runtime = None;
        context_reset = true;
    }

    let mut session_runtime = match session_runtime {
        Some(existing_runtime) => existing_runtime,
        None => start_runtime(&request).await?,
    };
    let first_attempt_prompt = turn_prompt_for_runtime(
        request.prompt.as_str(),
        request.session_output.as_deref(),
        context_reset,
    );
    let first_attempt = run_turn_with_runtime(&mut session_runtime, &first_attempt_prompt).await;
    if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
        let pid = runtime_pid(&session_runtime);
        sessions.store_session(session_id, session_runtime)?;

        return Ok(AppServerTurnResponse {
            assistant_message,
            context_reset,
            input_tokens,
            output_tokens,
            pid,
        });
    }

    let first_error = match first_attempt {
        Ok(_) => "App-server turn failed".to_string(),
        Err(error) => error,
    };
    shutdown_runtime(&mut session_runtime).await;

    let mut restarted_runtime = start_runtime(&request).await?;
    let retry_attempt_prompt = turn_prompt_for_runtime(
        request.prompt.as_str(),
        request.session_output.as_deref(),
        true,
    );
    let retry_attempt = run_turn_with_runtime(&mut restarted_runtime, &retry_attempt_prompt).await;
    match retry_attempt {
        Ok((assistant_message, input_tokens, output_tokens)) => {
            let pid = runtime_pid(&restarted_runtime);
            sessions.store_session(session_id, restarted_runtime)?;

            Ok(AppServerTurnResponse {
                assistant_message,
                context_reset: true,
                input_tokens,
                output_tokens,
                pid,
            })
        }
        Err(retry_error) => {
            shutdown_runtime(&mut restarted_runtime).await;

            Err(format!(
                "{} app-server failed, then retry failed after restart: first error: \
                 {first_error}; retry error: {retry_error}",
                sessions.provider_name()
            ))
        }
    }
}

/// Returns the turn prompt, replaying session output after context reset.
pub fn turn_prompt_for_runtime(
    prompt: &str,
    session_output: Option<&str>,
    context_reset: bool,
) -> String {
    if !context_reset {
        return prompt.to_string();
    }

    crate::infra::agent::build_resume_prompt(prompt, session_output)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TestRuntime {
        model: String,
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
    fn turn_prompt_for_runtime_returns_original_prompt_without_context_reset() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(prompt, Some("prior context"), false);

        // Assert
        assert_eq!(turn_prompt, prompt);
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(prompt, Some("assistant: proposed plan"), true);

        // Assert
        assert!(turn_prompt.contains("Continue this session using the full transcript below."));
        assert!(turn_prompt.contains("assistant: proposed plan"));
        assert!(turn_prompt.contains(prompt));
    }

    #[tokio::test]
    async fn run_turn_with_restart_retry_restarts_once_after_first_failure() {
        // Arrange
        let sessions = AppServerSessionRegistry::new("Test");
        let request = AppServerTurnRequest {
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_output: Some("previous output".to_string()),
            session_id: "session-1".to_string(),
        };
        let start_count = Arc::new(AtomicUsize::new(0));
        let run_count = Arc::new(AtomicUsize::new(0));
        let shutdown_count = Arc::new(AtomicUsize::new(0));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            |runtime: &TestRuntime, request: &AppServerTurnRequest| runtime.model == request.model,
            |_runtime| Some(42),
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
                            return Err("first failure".to_string());
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
        assert_eq!(start_count.load(Ordering::SeqCst), 2);
        assert_eq!(run_count.load(Ordering::SeqCst), 2);
        assert_eq!(shutdown_count.load(Ordering::SeqCst), 1);
    }
}
