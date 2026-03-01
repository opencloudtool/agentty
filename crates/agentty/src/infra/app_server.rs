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
    /// Assistant text received while a turn is running.
    AssistantMessage {
        /// Text payload emitted by the provider.
        message: String,
        /// Whether `message` is a partial delta chunk that should be appended
        /// inline without paragraph spacing.
        is_delta: bool,
    },
    /// An `item/started` event produced a progress description.
    ProgressUpdate(String),
}

/// Input payload for one app-server turn execution.
#[derive(Clone)]
pub struct AppServerTurnRequest {
    /// Live in-memory session output buffer updated by the streaming consumer.
    ///
    /// When set, restart-and-retry reads the latest accumulated output from
    /// this buffer instead of the stale `session_output` snapshot, ensuring
    /// content streamed before the crash is included in the replay prompt.
    pub live_session_output: Option<Arc<Mutex<String>>>,
    pub folder: PathBuf,
    pub model: String,
    pub prompt: String,
    pub session_id: String,
    pub session_output: Option<String>,
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

    /// Stores or replaces the runtime for `session_id`, returning ownership
    /// back to the caller when lock acquisition fails.
    ///
    /// This allows callers to shut down process-backed runtimes before
    /// returning an error, preventing orphaned child processes on early exits.
    ///
    /// # Errors
    /// Returns `(error, session)` when the session map lock is poisoned.
    pub fn store_session_or_recover(
        &self,
        session_id: String,
        session: Runtime,
    ) -> Result<(), (String, Runtime)> {
        let lock_error = format!(
            "Failed to lock {} app-server session map",
            self.provider_name
        );
        let Ok(mut sessions) = self.sessions.lock() else {
            return Err((lock_error, session));
        };
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
    let first_attempt_session_output = read_latest_session_output(&request);
    let first_attempt_prompt = match turn_prompt_for_runtime(
        request.prompt.as_str(),
        first_attempt_session_output.as_deref(),
        context_reset,
    ) {
        Ok(first_attempt_prompt) => first_attempt_prompt,
        Err(error) => {
            shutdown_runtime(&mut session_runtime).await;

            return Err(error);
        }
    };
    let first_attempt = run_turn_with_runtime(&mut session_runtime, &first_attempt_prompt).await;
    if let Ok((assistant_message, input_tokens, output_tokens)) = first_attempt {
        let pid = runtime_pid(&session_runtime);
        if let Err((error, mut leaked_runtime)) =
            sessions.store_session_or_recover(session_id, session_runtime)
        {
            shutdown_runtime(&mut leaked_runtime).await;

            return Err(error);
        }

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
    let retry_session_output = read_latest_session_output(&request);
    let retry_attempt_prompt = match turn_prompt_for_runtime(
        request.prompt.as_str(),
        retry_session_output.as_deref(),
        true,
    ) {
        Ok(retry_attempt_prompt) => retry_attempt_prompt,
        Err(error) => {
            shutdown_runtime(&mut restarted_runtime).await;

            return Err(error);
        }
    };
    let retry_attempt = run_turn_with_runtime(&mut restarted_runtime, &retry_attempt_prompt).await;
    match retry_attempt {
        Ok((assistant_message, input_tokens, output_tokens)) => {
            let pid = runtime_pid(&restarted_runtime);
            if let Err((error, mut leaked_runtime)) =
                sessions.store_session_or_recover(session_id, restarted_runtime)
            {
                shutdown_runtime(&mut leaked_runtime).await;

                return Err(error);
            }

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

/// Reads the latest session output, preferring the live buffer over the
/// stale snapshot.
///
/// The live buffer (`live_session_output`) accumulates all streamed content
/// in real time, including output from a turn that failed mid-stream. When
/// available, it provides a more complete transcript than the snapshot
/// captured at turn-enqueue time.
fn read_latest_session_output(request: &AppServerTurnRequest) -> Option<String> {
    if let Some(live_output) = &request.live_session_output
        && let Ok(guard) = live_output.lock()
    {
        let output = guard.clone();
        if !output.trim().is_empty() {
            return Some(output);
        }
    }

    request.session_output.clone()
}

/// Returns the turn prompt, replaying session output after context reset.
///
/// The returned prompt always includes repo-root-relative file path guidance so
/// assistant responses use consistent path references across providers.
///
/// # Errors
/// Returns an error when Askama prompt rendering fails after a context reset.
pub fn turn_prompt_for_runtime(
    prompt: &str,
    session_output: Option<&str>,
    context_reset: bool,
) -> Result<String, String> {
    let turn_prompt = if context_reset {
        crate::infra::agent::build_resume_prompt(prompt, session_output)
            .map_err(|error| error.to_string())?
    } else {
        prompt.to_string()
    };

    crate::infra::agent::prepend_repo_root_path_instructions(&turn_prompt)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
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
    fn turn_prompt_for_runtime_adds_repo_root_path_instructions_without_context_reset() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(prompt, Some("prior context"), false)
            .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.ends_with(prompt));
    }

    #[test]
    fn turn_prompt_for_runtime_replays_session_output_after_context_reset_with_path_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let turn_prompt = turn_prompt_for_runtime(prompt, Some("assistant: proposed plan"), true)
            .expect("turn prompt should render");

        // Assert
        assert!(turn_prompt.contains("repository-root-relative POSIX paths"));
        assert!(turn_prompt.contains("Continue this session using the full transcript below."));
        assert!(turn_prompt.contains("assistant: proposed plan"));
        assert!(turn_prompt.contains(prompt));
    }

    #[test]
    fn read_latest_session_output_prefers_live_buffer_over_snapshot() {
        // Arrange
        let live_output = Arc::new(Mutex::new("live content from stream".to_string()));
        let request = AppServerTurnRequest {
            live_session_output: Some(live_output),
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_id: "session-1".to_string(),
            session_output: Some("stale snapshot".to_string()),
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
            live_session_output: Some(live_output),
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_id: "session-1".to_string(),
            session_output: Some("stale snapshot".to_string()),
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
            live_session_output: None,
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_id: "session-1".to_string(),
            session_output: Some("stale snapshot".to_string()),
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
            live_session_output: None,
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_id: "session-1".to_string(),
            session_output: None,
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
            live_session_output: Some(live_output),
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_id: "session-1".to_string(),
            session_output: Some("stale snapshot".to_string()),
        };
        let captured_retry_prompt = Arc::new(Mutex::new(String::new()));

        // Act
        let response = run_turn_with_restart_retry(
            &sessions,
            request,
            |runtime: &TestRuntime, request: &AppServerTurnRequest| runtime.model == request.model,
            |_runtime| Some(42),
            |request: &AppServerTurnRequest| {
                let model = request.model.clone();

                Box::pin(async move { Ok(TestRuntime { model }) })
            },
            {
                let run_count = Arc::new(AtomicUsize::new(0));
                let captured_retry_prompt = Arc::clone(&captured_retry_prompt);
                move |_runtime: &mut TestRuntime, prompt: &str| {
                    let attempt = run_count.fetch_add(1, Ordering::SeqCst);
                    let prompt = prompt.to_string();
                    let captured_retry_prompt = Arc::clone(&captured_retry_prompt);

                    Box::pin(async move {
                        if attempt == 0 {
                            return Err("first failure".to_string());
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
            live_session_output: None,
            folder: PathBuf::from("/tmp"),
            model: "model-a".to_string(),
            prompt: "Do work".to_string(),
            session_id: "session-1".to_string(),
            session_output: Some("previous output".to_string()),
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
