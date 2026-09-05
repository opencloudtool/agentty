//! Shared app-server contracts and request/response types.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use ag_protocol::TurnPrompt;
use tokio::sync::mpsc;

use crate::app_server::AppServerError;
use crate::channel::{AgentRequestKind, LiveTranscript, PersonalityPrompt};
use crate::model::agent::ReasoningLevel;
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;

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
    /// Runtime process available before executing a turn or retry.
    PidUpdate(Option<u32>),
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
    /// Live in-memory transcript source updated by the streaming consumer.
    ///
    /// When set, restart-and-retry reads the latest accumulated transcript
    /// from this source instead of the queued snapshot, ensuring content
    /// streamed before the crash is included in the replay prompt.
    pub live_transcript: Option<Arc<dyn LiveTranscript>>,
    /// Main repository checkout that must remain read-only during the turn,
    /// when Agentty can resolve it.
    pub main_checkout_root: Option<PathBuf>,
    /// Provider-specific model identifier.
    pub model: String,
    /// Filesystem and command permission policy for this turn.
    pub permission_mode: PermissionMode,
    /// Personality prompt state resolved for this turn.
    pub personality: PersonalityPrompt,
    /// Structured prompt payload for this turn.
    pub prompt: TurnPrompt,
    /// Canonical request kind that drives transport behavior and protocol
    /// semantics for this turn.
    pub request_kind: AgentRequestKind,
    /// Replayable transcript text captured when the turn was queued.
    pub replay_transcript: Option<String>,
    /// Provider-native thread/session id used to resume context in a newly
    /// started runtime.
    pub provider_conversation_id: Option<String>,
    /// Persisted provider-native conversation id that already received the
    /// full instruction bootstrap, when available.
    pub persisted_instruction_conversation_id: Option<String>,
    /// Reasoning effort preference for this turn.
    ///
    /// Ignored by providers/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
    /// Stable agentty session id.
    pub session_id: String,
    /// Response-speed preference for this turn.
    pub speed_mode: SpeedMode,
}

/// Normalized result for one app-server turn.
#[derive(Debug)]
pub struct AppServerTurnResponse {
    /// Final assistant payload returned by the provider runtime.
    pub assistant_message: String,
    /// Whether the provider reset its native context during the turn.
    pub context_reset: bool,
    /// Input token count reported for the completed turn.
    pub input_tokens: u64,
    /// Output token count reported for the completed turn.
    pub output_tokens: u64,
    /// Provider runtime process identifier when retained after the turn.
    /// Cleared once a non-retained runtime has shut down.
    pub pid: Option<u32>,
    /// Provider-native thread/session id observed after the turn.
    pub provider_conversation_id: Option<String>,
}

/// Persistent app-server session boundary used by session workers.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
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
