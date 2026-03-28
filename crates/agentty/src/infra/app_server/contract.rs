//! Shared app-server contracts and request/response types.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::domain::agent::ReasoningLevel;
use crate::infra::app_server::AppServerError;
use crate::infra::channel::{AgentRequestKind, TurnPrompt};

/// Boxed async result used by [`AppServerClient`] trait methods.
pub type AppServerFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Boxed async result that can borrow values from the current call frame.
pub(crate) type BorrowedAppServerFuture<'scope, T> =
    Pin<Box<dyn Future<Output = T> + Send + 'scope>>;

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
        /// Optional provider phase label for this assistant item.
        ///
        /// Codex `item/completed` agent messages may include `phase` values
        /// (for example, from multi-phase prompting flows). Providers that do
        /// not expose phases set this to `None`.
        phase: Option<String>,
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
    /// Session worktree folder where the provider runtime executes.
    pub folder: PathBuf,
    /// Live in-memory session output buffer updated by the streaming consumer.
    ///
    /// When set, restart-and-retry reads the latest accumulated output from
    /// this buffer instead of the stale `session_output` snapshot, ensuring
    /// content streamed before the crash is included in the replay prompt.
    pub live_session_output: Option<Arc<Mutex<String>>>,
    /// Provider-specific model identifier.
    pub model: String,
    /// Structured user prompt for this turn.
    pub prompt: TurnPrompt,
    /// Canonical request kind that drives transport behavior and protocol
    /// semantics for this turn.
    pub request_kind: AgentRequestKind,
    /// Provider-native thread/session id used to resume context in a newly
    /// started runtime.
    pub provider_conversation_id: Option<String>,
    /// Reasoning effort preference for this turn.
    ///
    /// Ignored by providers/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
    /// Stable agentty session id.
    pub session_id: String,
}

/// Normalized result for one app-server turn.
#[derive(Debug)]
pub struct AppServerTurnResponse {
    pub assistant_message: String,
    pub context_reset: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub pid: Option<u32>,
    /// Provider-native thread/session id observed after the turn.
    pub provider_conversation_id: Option<String>,
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
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>>;

    /// Stops and forgets a session runtime, if one exists.
    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()>;
}
