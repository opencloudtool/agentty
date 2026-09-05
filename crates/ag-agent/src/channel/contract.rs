//! Shared channel trait and provider turn request/result contracts.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use ag_protocol::{AgentResponse, ProtocolRequestProfile, TurnPrompt};
use tokio::sync::mpsc;

use crate::model::agent::ReasoningLevel;
use crate::model::permission::PermissionMode;
use crate::model::session::{ResponseStyle, SpeedMode};

/// Boxed async result used by [`AgentChannel`] trait methods.
pub type AgentFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Live transcript projection used when a provider runtime needs replay text.
pub trait LiveTranscript: fmt::Debug + Send + Sync {
    /// Returns the latest replayable transcript text, when any content exists.
    fn replay_text(&self) -> Option<String>;
}

/// Turn initiation mode for [`TurnRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequestKind {
    /// Runs one focused code review with a direct structured review response.
    FocusedReview,
    /// Starts a fresh interactive session turn with no prior context.
    SessionStart,
    /// Resumes an interactive session turn.
    SessionResume,
    /// Runs one utility prompt with utility protocol requirements.
    ///
    /// Callers may route this through an isolated one-shot channel or through
    /// an existing session channel when the utility work needs provider
    /// conversation continuity without normal post-turn auto-commit handling.
    UtilityPrompt,
    /// Reads provider account metadata without creating an agent turn.
    AccountRead,
}

impl AgentRequestKind {
    /// Returns the protocol request profile derived from this request kind.
    #[must_use]
    pub fn protocol_profile(&self) -> ProtocolRequestProfile {
        match self {
            Self::SessionStart | Self::SessionResume => ProtocolRequestProfile::SessionTurn,
            Self::FocusedReview => ProtocolRequestProfile::FocusedReview,
            Self::UtilityPrompt | Self::AccountRead => ProtocolRequestProfile::UtilityPrompt,
        }
    }

    /// Returns whether this request resumes a prior interactive session turn.
    #[must_use]
    pub fn is_resume(&self) -> bool {
        matches!(self, Self::SessionResume)
    }
}

/// Continuation state for one provider-agnostic agent turn.
///
/// The concrete representation keeps provider-runtime recovery details out of
/// [`TurnRequest`] while allowing CLI channels to consume only replay text.
#[derive(Clone, Debug)]
pub struct TurnContinuation {
    kind: TurnContinuationKind,
}

/// Personality prompt state prepared for one provider turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersonalityPrompt {
    current: Option<String>,
    update: PersonalityPromptUpdate,
}

impl PersonalityPrompt {
    /// Creates one active personality, marking whether delta-mode providers
    /// must receive its body as an update.
    #[must_use]
    pub fn active(prompt: String, changed: bool) -> Self {
        let update = if changed {
            PersonalityPromptUpdate::Set(prompt.clone())
        } else {
            PersonalityPromptUpdate::Unchanged
        };

        Self {
            current: Some(prompt),
            update,
        }
    }

    /// Creates a cleared personality state.
    ///
    /// `changed` is true when a delta-mode provider previously held active
    /// personality instructions and must be told to discard them.
    #[must_use]
    pub fn cleared(changed: bool) -> Self {
        Self {
            current: None,
            update: if changed {
                PersonalityPromptUpdate::Clear
            } else {
                PersonalityPromptUpdate::Unchanged
            },
        }
    }

    /// Returns the current personality body for a full bootstrap.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    pub(crate) fn update(&self) -> &PersonalityPromptUpdate {
        &self.update
    }
}

/// Delta-mode personality change for a provider-managed conversation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum PersonalityPromptUpdate {
    /// Clear personality behavior that was active on the previous turn.
    Clear,
    /// Apply a new or edited personality body.
    Set(String),
    /// Reuse the personality behavior already present in provider context.
    #[default]
    Unchanged,
}

impl TurnContinuation {
    /// Creates continuation state for a fresh turn with no prior context.
    #[must_use]
    pub fn fresh() -> Self {
        Self {
            kind: TurnContinuationKind::Fresh,
        }
    }

    /// Creates continuation state for a stateless turn that replays prior text.
    #[must_use]
    pub fn replaying(replay_transcript: String) -> Self {
        Self {
            kind: TurnContinuationKind::Replay { replay_transcript },
        }
    }

    /// Creates continuation state for a provider runtime that may resume a
    /// native conversation and reconstruct context from a live transcript.
    #[must_use]
    pub fn provider(
        live_transcript: Option<Arc<dyn LiveTranscript>>,
        persisted_instruction_conversation_id: Option<String>,
        provider_conversation_id: Option<String>,
        replay_transcript: Option<String>,
    ) -> Self {
        Self {
            kind: TurnContinuationKind::Provider {
                live_transcript,
                persisted_instruction_conversation_id,
                provider_conversation_id,
                replay_transcript,
            },
        }
    }

    /// Returns replayable transcript text when this turn carries it.
    #[must_use]
    pub fn replay_transcript(&self) -> Option<&str> {
        match &self.kind {
            TurnContinuationKind::Fresh => None,
            TurnContinuationKind::Provider {
                replay_transcript, ..
            } => replay_transcript.as_deref(),
            TurnContinuationKind::Replay { replay_transcript } => Some(replay_transcript.as_str()),
        }
    }

    /// Returns the provider-native conversation identifier when available.
    #[must_use]
    pub fn provider_conversation_id(&self) -> Option<&str> {
        match &self.kind {
            TurnContinuationKind::Provider {
                provider_conversation_id,
                ..
            } => provider_conversation_id.as_deref(),
            TurnContinuationKind::Fresh | TurnContinuationKind::Replay { .. } => None,
        }
    }

    /// Returns the conversation identifier that received the instruction
    /// bootstrap when available.
    #[must_use]
    pub fn persisted_instruction_conversation_id(&self) -> Option<&str> {
        match &self.kind {
            TurnContinuationKind::Provider {
                persisted_instruction_conversation_id,
                ..
            } => persisted_instruction_conversation_id.as_deref(),
            TurnContinuationKind::Fresh | TurnContinuationKind::Replay { .. } => None,
        }
    }

    pub(crate) fn into_parts(self) -> TurnContinuationParts {
        match self.kind {
            TurnContinuationKind::Fresh => TurnContinuationParts::default(),
            TurnContinuationKind::Replay { replay_transcript } => TurnContinuationParts {
                replay_transcript: Some(replay_transcript),
                ..TurnContinuationParts::default()
            },
            TurnContinuationKind::Provider {
                live_transcript,
                persisted_instruction_conversation_id,
                provider_conversation_id,
                replay_transcript,
            } => TurnContinuationParts {
                live_transcript,
                persisted_instruction_conversation_id,
                provider_conversation_id,
                replay_transcript,
            },
        }
    }
}

#[derive(Clone, Debug)]
enum TurnContinuationKind {
    Fresh,
    Provider {
        live_transcript: Option<Arc<dyn LiveTranscript>>,
        persisted_instruction_conversation_id: Option<String>,
        provider_conversation_id: Option<String>,
        replay_transcript: Option<String>,
    },
    Replay {
        replay_transcript: String,
    },
}

#[derive(Default)]
pub(crate) struct TurnContinuationParts {
    pub(crate) live_transcript: Option<Arc<dyn LiveTranscript>>,
    pub(crate) persisted_instruction_conversation_id: Option<String>,
    pub(crate) provider_conversation_id: Option<String>,
    pub(crate) replay_transcript: Option<String>,
}

/// Input payload for one provider-agnostic agent turn.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// Prior context needed to continue this turn.
    pub continuation: TurnContinuation,
    /// Session worktree folder where the agent runs.
    pub folder: PathBuf,
    /// Main repository checkout that must remain read-only during the turn,
    /// when Agentty can resolve it.
    pub main_checkout_root: Option<PathBuf>,
    /// Provider-specific model identifier.
    pub model: String,
    /// Filesystem and command permission policy for this turn.
    pub permission_mode: PermissionMode,
    /// Personality prompt state resolved from the session worktree.
    pub personality: PersonalityPrompt,
    /// Structured prompt payload for the turn.
    pub prompt: TurnPrompt,
    /// Reasoning effort preference for the turn.
    ///
    /// Ignored by providers/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
    /// Canonical request kind that drives transport behavior and protocol
    /// semantics for this turn.
    pub request_kind: AgentRequestKind,
    /// Preferred amount of detail in user-facing session responses.
    pub response_style: ResponseStyle,
    /// Response-speed preference for the turn.
    pub speed_mode: SpeedMode,
}

/// Incremental event emitted during one agent turn.
///
/// Events are sent through an [`mpsc::UnboundedSender`] as the turn
/// progresses, enabling transient loader updates without appending partial turn
/// output into the persisted transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnEvent {
    /// A streamed thinking/planning or tool-status fragment shown in the
    /// transient loader.
    ThoughtDelta(String),
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
    /// Announces a running or retained runtime (`Some(pid)`) or clears it
    /// when the runtime shuts down (`None`). Consumers update resource
    /// accounting. Only CLI transports may use this
    /// PID for cancellation signals; app-server cancellation belongs to the
    /// runtime owner and must not signal an accounting PID.
    PidUpdate(Option<u32>),
}

/// Normalized result returned when one agent turn completes successfully.
#[derive(Debug)]
pub struct TurnResult {
    /// Parsed agent response containing structured protocol messages.
    pub assistant_message: AgentResponse,
    /// Whether the provider reset its context to complete this turn.
    pub context_reset: bool,
    /// Input token count for the turn.
    pub input_tokens: u64,
    /// Output token count for the turn.
    pub output_tokens: u64,
    /// Provider-native conversation identifier observed after the turn.
    ///
    /// App-server providers return this so the worker can persist it for
    /// future runtime restarts. CLI channels always return `None`.
    pub provider_conversation_id: Option<String>,
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

/// Typed error returned by [`AgentChannel`] operations.
///
/// Discriminates failure causes so the app layer can route errors without
/// parsing formatted messages.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// An app-server infrastructure failure propagated from a persistent
    /// provider runtime.
    #[error(transparent)]
    AppServer(#[from] crate::app_server::AppServerError),

    /// A CLI backend command or process execution failure.
    #[error("{0}")]
    Backend(String),

    /// The user explicitly interrupted the active turn.
    #[error("{0}")]
    InterruptedByUser(String),

    /// A subprocess IO error such as a spawn failure or unavailable pipe.
    #[error("{0}")]
    Io(String),
}

/// Provider-agnostic session channel for executing agent turns.
///
/// Implementations bridge a specific transport - CLI subprocess or app-server
/// RPC - to the unified [`TurnEvent`] stream consumed by session workers. The
/// trait is object-safe so it can be held as `Arc<dyn AgentChannel>`.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
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
    /// Implementations may emit [`TurnEvent::ThoughtDelta`] values for
    /// transient loader updates. Final transcript output is derived from the
    /// returned [`TurnResult`] after the turn finishes.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_personality_prompt_tracks_active_change_and_clear_state() {
        // Arrange / Act
        let changed = PersonalityPrompt::active("Review carefully.".to_string(), true);
        let unchanged = PersonalityPrompt::active("Review carefully.".to_string(), false);
        let cleared = PersonalityPrompt::cleared(true);
        let empty = PersonalityPrompt::cleared(false);

        // Assert
        assert_eq!(changed.current(), Some("Review carefully."));
        assert_eq!(
            changed.update(),
            &PersonalityPromptUpdate::Set("Review carefully.".to_string())
        );
        assert_eq!(unchanged.update(), &PersonalityPromptUpdate::Unchanged);
        assert_eq!(cleared.current(), None);
        assert_eq!(cleared.update(), &PersonalityPromptUpdate::Clear);
        assert_eq!(empty.update(), &PersonalityPromptUpdate::Unchanged);
    }

    #[test]
    fn test_turn_continuation_fresh_has_no_context() {
        // Arrange / Act
        let continuation = TurnContinuation::fresh();

        // Assert
        assert_eq!(continuation.replay_transcript(), None);
        assert_eq!(continuation.provider_conversation_id(), None);
        assert_eq!(continuation.persisted_instruction_conversation_id(), None);
    }

    #[test]
    fn test_turn_continuation_replaying_exposes_transcript_only() {
        // Arrange
        let continuation = TurnContinuation::replaying("prior turn".to_string());

        // Act
        let parts = continuation.clone().into_parts();

        // Assert
        assert_eq!(continuation.replay_transcript(), Some("prior turn"));
        assert_eq!(continuation.provider_conversation_id(), None);
        assert!(parts.live_transcript.is_none());
        assert_eq!(parts.persisted_instruction_conversation_id, None);
        assert_eq!(parts.provider_conversation_id, None);
        assert_eq!(parts.replay_transcript.as_deref(), Some("prior turn"));
    }

    #[test]
    fn test_turn_continuation_provider_exposes_persisted_context() {
        // Arrange / Act
        let continuation = TurnContinuation::provider(
            None,
            Some("instruction-1".to_string()),
            Some("thread-1".to_string()),
            Some("prior turn".to_string()),
        );

        // Assert
        assert_eq!(continuation.replay_transcript(), Some("prior turn"));
        assert_eq!(continuation.provider_conversation_id(), Some("thread-1"));
        assert_eq!(
            continuation.persisted_instruction_conversation_id(),
            Some("instruction-1")
        );
    }

    #[test]
    /// Ensures session request kinds derive the session-turn protocol
    /// profile.
    fn test_agent_request_kind_session_variants_use_session_protocol_profile() {
        // Arrange
        let start = AgentRequestKind::SessionStart;
        let resume = AgentRequestKind::SessionResume;

        // Act
        let start_profile = start.protocol_profile();
        let resume_profile = resume.protocol_profile();

        // Assert
        assert_eq!(start_profile, ProtocolRequestProfile::SessionTurn);
        assert_eq!(resume_profile, ProtocolRequestProfile::SessionTurn);
    }

    #[test]
    /// Ensures utility prompts derive the utility protocol profile.
    fn test_agent_request_kind_utility_prompt_uses_utility_protocol_profile() {
        // Arrange
        let request_kind = AgentRequestKind::UtilityPrompt;

        // Act
        let protocol_profile = request_kind.protocol_profile();

        // Assert
        assert_eq!(protocol_profile, ProtocolRequestProfile::UtilityPrompt);
    }

    #[test]
    fn focused_review_request_uses_focused_review_protocol_profile() {
        // Arrange
        let request_kind = AgentRequestKind::FocusedReview;

        // Act
        let protocol_profile = request_kind.protocol_profile();

        // Assert
        assert_eq!(protocol_profile, ProtocolRequestProfile::FocusedReview);
    }

    #[test]
    /// Ensures account-read requests are non-session utility requests.
    fn test_agent_request_kind_account_read_uses_utility_protocol_profile() {
        // Arrange
        let request_kind = AgentRequestKind::AccountRead;

        // Act
        let protocol_profile = request_kind.protocol_profile();

        // Assert
        assert_eq!(protocol_profile, ProtocolRequestProfile::UtilityPrompt);
    }
}
