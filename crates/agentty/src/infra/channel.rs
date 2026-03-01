//! Provider-agnostic agent channel abstraction for session turn execution.
//!
//! Defines the [`AgentChannel`] trait and all associated request, event, and
//! result types used to drive a single agent session turn without coupling
//! callers to a specific transport (CLI subprocess or app-server RPC).

pub mod app_server;
pub mod cli;

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::domain::agent::AgentKind;
use crate::infra::app_server::AppServerClient;
use crate::infra::channel::app_server::AppServerAgentChannel;
use crate::infra::channel::cli::CliAgentChannel;

/// Boxed async result used by [`AgentChannel`] trait methods.
pub type AgentFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Turn initiation mode for [`TurnRequest`].
#[derive(Debug, Clone)]
pub enum TurnMode {
    /// Starts a fresh agent turn with no prior context.
    Start,
    /// Resumes a prior turn, optionally replaying transcript output.
    Resume {
        /// Prior session output used for history replay when present.
        session_output: Option<String>,
    },
}

/// Input payload for one provider-agnostic agent turn.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// Session worktree folder where the agent runs.
    pub folder: PathBuf,
    /// Live session output buffer for app-server context reconstruction.
    ///
    /// App-server clients may read this buffer during a turn to access content
    /// that was streamed before a prior crash, providing a more complete
    /// transcript than the snapshot captured at enqueue time. CLI channels
    /// ignore this field.
    pub live_session_output: Option<Arc<Mutex<String>>>,
    /// Provider-specific model identifier.
    pub model: String,
    /// Turn initiation mode (start or resume).
    pub mode: TurnMode,
    /// User prompt to send.
    pub prompt: String,
}

/// Incremental event emitted during one agent turn.
///
/// Events are sent through an [`mpsc::UnboundedSender`] as the turn
/// progresses, enabling real-time streaming of agent output and progress
/// updates to the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnEvent {
    /// A fragment of the assistant's text response.
    AssistantDelta(String),
    /// The turn completed successfully with final token counts.
    Completed {
        /// Whether the provider reset its context for this turn.
        context_reset: bool,
        /// Input token count for the turn.
        input_tokens: u64,
        /// Output token count for the turn.
        output_tokens: u64,
    },
    /// The turn failed with an error description.
    Failed(String),
    /// A child process PID update.
    ///
    /// Sent by CLI channels immediately after spawning the child process
    /// (`Some(pid)`) and again after the child exits (`None`). Consumers
    /// update the shared PID slot used by cancellation signals.
    PidUpdate(Option<u32>),
    /// A progress description label (tool use, thinking, etc.).
    Progress(String),
}

/// Normalized result returned when one agent turn completes successfully.
#[derive(Debug)]
pub struct TurnResult {
    /// Full assistant message text for the turn.
    pub assistant_message: String,
    /// Whether the provider reset its context to complete this turn.
    pub context_reset: bool,
    /// Input token count for the turn.
    pub input_tokens: u64,
    /// Output token count for the turn.
    pub output_tokens: u64,
}

/// Opaque reference to an active agent session.
pub struct SessionRef {
    /// Stable session identifier.
    pub session_id: String,
}

/// Input payload for initiating a new agent session.
pub struct StartSessionRequest {
    /// Session worktree folder.
    pub folder: PathBuf,
    /// Stable session identifier.
    pub session_id: String,
}

/// Opaque error type for [`AgentChannel`] operations.
#[derive(Debug)]
pub struct AgentError(pub String);

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Provider-agnostic session channel for executing agent turns.
///
/// Implementations bridge a specific transport — CLI subprocess or app-server
/// RPC — to the unified [`TurnEvent`] stream consumed by session workers. The
/// trait is object-safe so it can be held as `Arc<dyn AgentChannel>`.
#[cfg_attr(test, mockall::automock)]
pub trait AgentChannel: Send + Sync {
    /// Initialises a provider session for the given session identifier.
    ///
    /// Implementations that do not maintain persistent sessions return
    /// immediately with a [`SessionRef`] wrapping the supplied identifier.
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>>;

    /// Executes one prompt turn and streams incremental events to `events`.
    ///
    /// Implementations emit [`TurnEvent::AssistantDelta`] and
    /// [`TurnEvent::Progress`] as output arrives. When the turn finishes,
    /// [`TurnResult`] is returned.
    ///
    /// # Errors
    /// Returns [`AgentError`] when the turn cannot be executed (spawn failure,
    /// transport error) or is interrupted by a signal.
    fn run_turn(
        &self,
        session_id: String,
        req: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>>;

    /// Tears down the provider session associated with `session_id`.
    ///
    /// Implementations that do not maintain persistent sessions treat this as
    /// a no-op and always return `Ok(())`.
    fn shutdown_session(&self, session_id: String) -> AgentFuture<Result<(), AgentError>>;
}

/// Creates the provider-specific channel for the given agent kind.
///
/// CLI providers (Claude) use [`CliAgentChannel`]; app-server providers
/// (Gemini, Codex) use [`AppServerAgentChannel`].
pub fn create_agent_channel(
    kind: AgentKind,
    app_server_client: Arc<dyn AppServerClient>,
) -> Arc<dyn AgentChannel> {
    if crate::infra::agent::transport_mode(kind).uses_app_server() {
        Arc::new(AppServerAgentChannel::new(app_server_client))
    } else {
        Arc::new(CliAgentChannel::new(kind))
    }
}
