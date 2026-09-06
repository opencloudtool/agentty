//! Shared app-server runtime client scaffold.

use std::marker::PhantomData;

use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use crate::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse, BorrowedAppServerFuture,
};
use crate::model::agent::ReasoningLevel;
use crate::model::session::SpeedMode;

/// Provider hook surface for the shared app-server client lifecycle.
pub(crate) trait RuntimeClientProvider: Send + Sync + 'static {
    /// Provider-specific runtime state stored per Agentty session.
    type Runtime: RuntimeClientRuntime + 'static;

    /// User-facing provider label used by retry and lock errors.
    fn label() -> &'static str;

    /// Returns whether prompts should include transport-level schema text.
    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode;

    /// Returns whether successful runtimes remain alive between turns.
    fn retain_runtime_after_turn() -> bool;

    /// Starts and bootstraps one provider runtime for a request.
    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>>;

    /// Runs one turn against an already-started provider runtime.
    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        protocol_profile: ProtocolRequestProfile,
        reasoning_level: ReasoningLevel,
        speed_mode: SpeedMode,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>>;
}

/// Runtime query and shutdown hooks shared by provider clients.
pub(crate) trait RuntimeClientRuntime: Send {
    /// Returns whether the runtime can serve one incoming request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool;

    /// Returns the runtime OS process id, when available.
    fn pid(&self) -> Option<u32>;

    /// Returns the active provider-native conversation id, when available.
    fn provider_conversation_id(&self) -> Option<String>;

    /// Returns whether runtime startup restored provider-native context.
    fn restored_context(&self) -> bool;

    /// Terminates the runtime and waits for process exit.
    fn shutdown_runtime(&mut self) -> BorrowedAppServerFuture<'_, ()>;
}

/// Generic app-server client backed by provider-specific runtime hooks.
pub(crate) struct ProviderRuntimeClient<Provider: RuntimeClientProvider> {
    provider: PhantomData<Provider>,
    sessions: AppServerSessionRegistry<Provider::Runtime>,
}

impl<Provider: RuntimeClientProvider> ProviderRuntimeClient<Provider> {
    /// Creates an empty runtime registry for one provider.
    pub(crate) fn new() -> Self {
        Self {
            provider: PhantomData,
            sessions: AppServerSessionRegistry::new(Provider::label()),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<Provider::Runtime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, AppServerError> {
        let stream_tx = stream_tx.clone();
        let shutdown_stream_tx = stream_tx.clone();
        let reasoning_level = request.reasoning_level;
        let protocol_profile = request.request_kind.protocol_profile();
        let speed_mode = request.speed_mode;

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: Self::matches_request,
                pid: Self::pid,
                provider_conversation_id: Self::provider_conversation_id,
                retain_runtime_after_turn: Provider::retain_runtime_after_turn(),
                restored_context: Self::restored_context,
            },
            Provider::schema_instruction_mode(),
            |request| {
                let request = request.clone();

                Provider::start_runtime(request)
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(runtime.pid()));

                Provider::run_turn(
                    runtime,
                    prompt,
                    protocol_profile,
                    reasoning_level,
                    speed_mode,
                    stream_tx,
                )
            },
            move |runtime| Self::shutdown_runtime(runtime, &shutdown_stream_tx),
        )
        .await
    }

    /// Returns whether the runtime can serve one incoming request.
    fn matches_request(runtime: &Provider::Runtime, request: &AppServerTurnRequest) -> bool {
        runtime.matches_request(request)
    }

    /// Returns the runtime OS process id, when available.
    fn pid(runtime: &Provider::Runtime) -> Option<u32> {
        runtime.pid()
    }

    /// Returns the active provider-native conversation id, when available.
    fn provider_conversation_id(runtime: &Provider::Runtime) -> Option<String> {
        runtime.provider_conversation_id()
    }

    /// Returns whether runtime startup restored provider-native context.
    fn restored_context(runtime: &Provider::Runtime) -> bool {
        runtime.restored_context()
    }

    /// Invalidates accounting before releasing a runtime PID, including while
    /// replacement startup or replay is still pending.
    fn shutdown_runtime<'scope>(
        runtime: &'scope mut Provider::Runtime,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, ()> {
        let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(None));

        runtime.shutdown_runtime()
    }
}

impl<Provider: RuntimeClientProvider> Default for ProviderRuntimeClient<Provider> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Provider: RuntimeClientProvider> AppServerClient for ProviderRuntimeClient<Provider> {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let sessions = self.sessions.clone();

        Box::pin(async move { Self::run_turn_internal(&sessions, request, &stream_tx).await })
    }

    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()> {
        let sessions = self.sessions.clone();

        Box::pin(async move {
            let _ = sessions.cancel_active_turn(&session_id);

            let Ok(Some(mut session_runtime)) = sessions.take_session(&session_id) else {
                return;
            };

            session_runtime.shutdown_runtime().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};

    use super::*;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;

    static RUN_COUNT: AtomicUsize = AtomicUsize::new(0);
    static SHUTDOWN_COUNT: AtomicUsize = AtomicUsize::new(0);
    static START_COUNT: AtomicUsize = AtomicUsize::new(0);

    struct TestProvider;

    impl RuntimeClientProvider for TestProvider {
        type Runtime = TestRuntime;

        fn label() -> &'static str {
            "Test"
        }

        fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
            ProtocolSchemaInstructionMode::TransportSchema
        }

        fn retain_runtime_after_turn() -> bool {
            true
        }

        fn start_runtime(
            request: AppServerTurnRequest,
        ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
            Box::pin(async move {
                START_COUNT.fetch_add(1, Ordering::SeqCst);

                Ok(TestRuntime {
                    folder: request.folder,
                    model: request.model,
                    pid: 42,
                    provider_conversation_id: Some("conversation-1".to_string()),
                    restored_context: true,
                    shutdown_count: &SHUTDOWN_COUNT,
                })
            })
        }

        fn run_turn<'scope>(
            _runtime: &'scope mut Self::Runtime,
            _prompt: &'scope TurnPrompt,
            _protocol_profile: ProtocolRequestProfile,
            _reasoning_level: ReasoningLevel,
            _speed_mode: SpeedMode,
            _stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
        ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
            Box::pin(async {
                RUN_COUNT.fetch_add(1, Ordering::SeqCst);

                Ok(("assistant".to_string(), 11, 12))
            })
        }
    }

    struct TestRuntime {
        folder: PathBuf,
        model: String,
        pid: u32,
        provider_conversation_id: Option<String>,
        restored_context: bool,
        shutdown_count: &'static AtomicUsize,
    }

    impl RuntimeClientRuntime for TestRuntime {
        fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
            self.folder == request.folder && self.model == request.model
        }

        fn pid(&self) -> Option<u32> {
            Some(self.pid)
        }

        fn provider_conversation_id(&self) -> Option<String> {
            self.provider_conversation_id.clone()
        }

        fn restored_context(&self) -> bool {
            self.restored_context
        }

        fn shutdown_runtime(&mut self) -> BorrowedAppServerFuture<'_, ()> {
            Box::pin(async move {
                self.shutdown_count.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    fn make_request() -> AppServerTurnRequest {
        AppServerTurnRequest {
            folder: std::env::temp_dir(),
            live_transcript: None,
            main_checkout_root: None,
            model: "test-model".to_string(),
            permission_mode: crate::model::permission::PermissionMode::AutoEdit,
            persisted_instruction_conversation_id: None,
            personality: crate::channel::PersonalityPrompt::default(),
            prompt: TurnPrompt::from_text("Hello".to_string()),
            provider_conversation_id: None,
            reasoning_level: ReasoningLevel::High,
            request_kind: AgentRequestKind::SessionStart,
            replay_transcript: None,
            session_id: "session-1".to_string(),
            speed_mode: SpeedMode::default(),
        }
    }

    #[tokio::test]
    async fn retry_shutdown_clears_pid_before_delayed_replacement_startup() {
        // Arrange
        static RETRY_SHUTDOWN_COUNT: AtomicUsize = AtomicUsize::new(0);
        let sessions = AppServerSessionRegistry::new("Test");
        let request = make_request();
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut release_rx = Some(release_rx);
        let starts = std::sync::Arc::new(AtomicUsize::new(0));
        let start_count = std::sync::Arc::clone(&starts);
        let shutdown_tx = stream_tx.clone();

        // Act
        let turn = tokio::spawn(async move {
            app_server::run_turn_with_restart_retry(
                &sessions,
                request,
                app_server::RuntimeInspector {
                    matches_request: TestRuntime::matches_request,
                    pid: TestRuntime::pid,
                    provider_conversation_id: TestRuntime::provider_conversation_id,
                    retain_runtime_after_turn: true,
                    restored_context: TestRuntime::restored_context,
                },
                ProtocolSchemaInstructionMode::TransportSchema,
                move |request| {
                    let attempt = start_count.fetch_add(1, Ordering::SeqCst);
                    let release = if attempt == 0 {
                        None
                    } else {
                        release_rx.take()
                    };
                    let runtime = TestRuntime {
                        folder: request.folder.clone(),
                        model: request.model.clone(),
                        pid: if attempt == 0 { 42 } else { 84 },
                        provider_conversation_id: None,
                        restored_context: true,
                        shutdown_count: &RETRY_SHUTDOWN_COUNT,
                    };

                    Box::pin(async move {
                        if let Some(release) = release {
                            release.await.expect("release replacement startup");
                        }

                        Ok(runtime)
                    })
                },
                move |runtime, _| {
                    let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(runtime.pid()));
                    let failed = runtime.pid == 42;

                    Box::pin(async move {
                        if failed {
                            return Err(AppServerError::Provider(
                                "first runtime exited".to_string(),
                            ));
                        }

                        Ok(("recovered".to_string(), 1, 1))
                    })
                },
                move |runtime| {
                    ProviderRuntimeClient::<TestProvider>::shutdown_runtime(runtime, &shutdown_tx)
                },
            )
            .await
        });
        let first_pid = stream_rx.recv().await.expect("first runtime published");
        let cleared_pid = stream_rx.recv().await.expect("shutdown clears runtime");

        // Assert
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(first_pid, AppServerStreamEvent::PidUpdate(Some(42)));
        assert_eq!(cleared_pid, AppServerStreamEvent::PidUpdate(None));
        assert!(!turn.is_finished());
        assert!(stream_rx.try_recv().is_err());
        release_tx.send(()).expect("finish replacement startup");
        let response = turn.await.expect("join turn").expect("retry succeeds");
        assert_eq!(
            stream_rx.recv().await,
            Some(AppServerStreamEvent::PidUpdate(Some(84)))
        );
        assert_eq!(response.pid, Some(84));
    }

    #[tokio::test]
    async fn runtime_client_retains_and_replaces_runtime_with_pid_updates() {
        // Arrange
        RUN_COUNT.store(0, Ordering::SeqCst);
        SHUTDOWN_COUNT.store(0, Ordering::SeqCst);
        START_COUNT.store(0, Ordering::SeqCst);

        let client = ProviderRuntimeClient::<TestProvider>::new();
        let request = make_request();
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        // Act
        let response = client
            .run_turn(request, stream_tx)
            .await
            .expect("turn should succeed");
        let mut replacement_request = make_request();
        replacement_request.model = "replacement-model".to_string();
        let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
        client
            .run_turn(replacement_request, replacement_tx)
            .await
            .expect("replacement turn should succeed");
        client.shutdown_session("session-1".to_string()).await;

        // Assert
        assert_eq!(response.assistant_message, "assistant");
        assert!(!response.context_reset);
        assert_eq!(response.input_tokens, 11);
        assert_eq!(response.output_tokens, 12);
        assert_eq!(response.pid, Some(42));
        assert_eq!(
            stream_rx.try_recv().expect("runtime PID before turn"),
            AppServerStreamEvent::PidUpdate(Some(42))
        );
        assert_eq!(
            response.provider_conversation_id,
            Some("conversation-1".to_string())
        );
        assert_eq!(
            replacement_rx.try_recv().expect("retired PID cleared"),
            AppServerStreamEvent::PidUpdate(None)
        );
        assert_eq!(
            replacement_rx
                .try_recv()
                .expect("replacement PID published"),
            AppServerStreamEvent::PidUpdate(Some(42))
        );
        assert_eq!(START_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(RUN_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(SHUTDOWN_COUNT.load(Ordering::SeqCst), 2);
    }
}
