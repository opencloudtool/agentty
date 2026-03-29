//! Shared channel trait and transport-agnostic turn data types.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::domain::agent::ReasoningLevel;
use crate::infra::agent::AgentResponse;

/// Boxed async result used by [`AgentChannel`] trait methods.
pub type AgentFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One local image attachment referenced from a prompt placeholder.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TurnPromptAttachment {
    /// Inline placeholder token such as `[Image #1]` used in prompt text.
    pub placeholder: String,
    /// Local file path persisted for transport upload.
    pub local_image_path: PathBuf,
}

/// Structured prompt payload for one agent turn.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TurnPrompt {
    /// Ordered local image attachments referenced by `text`.
    pub attachments: Vec<TurnPromptAttachment>,
    /// Prompt text submitted by the user, including inline placeholders.
    pub text: String,
}

/// Ordered content piece produced when serializing one turn prompt.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum TurnPromptContentPart<'prompt> {
    /// One local image attachment referenced from the prompt text.
    Attachment(&'prompt TurnPromptAttachment),
    /// One attachment whose placeholder no longer appears in the prompt text.
    OrphanAttachment(&'prompt TurnPromptAttachment),
    /// One plain-text span from the prompt.
    Text(&'prompt str),
}

impl TurnPrompt {
    /// Creates a text-only prompt payload.
    #[must_use]
    pub fn from_text(text: String) -> Self {
        Self {
            attachments: Vec::new(),
            text,
        }
    }

    /// Returns whether the payload contains no text and no attachments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.attachments.is_empty()
    }

    /// Returns whether the payload contains one or more image attachments.
    #[must_use]
    pub fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    /// Returns the local image paths referenced by this prompt payload.
    pub fn local_image_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.attachments
            .iter()
            .map(|attachment| &attachment.local_image_path)
    }

    /// Returns whether the prompt text contains `needle`.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }

    /// Returns whether the prompt text ends with `suffix`.
    #[must_use]
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.text.ends_with(suffix)
    }

    /// Returns prompt text spans and image attachments in transport order.
    ///
    /// Attachments are ordered by their placeholder position in `text`. Any
    /// attachment whose placeholder no longer exists in the text is appended
    /// after the trailing text so transports can still serialize the image
    /// instead of dropping it silently.
    #[must_use]
    pub(crate) fn content_parts(&self) -> Vec<TurnPromptContentPart<'_>> {
        split_turn_prompt_content(&self.text, &self.attachments)
    }

    /// Returns the prompt text as it should be written into persisted
    /// transcripts.
    ///
    /// Inline `[Image #n]` markers are preserved verbatim. If attachment
    /// metadata somehow survives without its placeholder still present in the
    /// text, the missing placeholders are appended in attachment order so the
    /// transcript does not silently become text-only.
    #[must_use]
    pub fn transcript_text(&self) -> String {
        let mut transcript_text = self.text.clone();
        let missing_placeholders = self
            .attachments
            .iter()
            .filter(|attachment| !self.text.contains(&attachment.placeholder))
            .map(|attachment| attachment.placeholder.as_str())
            .collect::<Vec<_>>();

        if missing_placeholders.is_empty() {
            return transcript_text;
        }

        if transcript_text
            .chars()
            .last()
            .is_some_and(|character| !character.is_whitespace())
        {
            transcript_text.push(' ');
        }

        transcript_text.push_str(&missing_placeholders.join(" "));

        transcript_text
    }
}

/// Splits prompt text into ordered text and attachment parts for transport
/// serialization.
#[must_use]
pub(crate) fn split_turn_prompt_content<'prompt>(
    text: &'prompt str,
    attachments: &'prompt [TurnPromptAttachment],
) -> Vec<TurnPromptContentPart<'prompt>> {
    if attachments.is_empty() {
        return vec![TurnPromptContentPart::Text(text)];
    }

    let mut ordered_attachments = attachments.iter().collect::<Vec<_>>();
    ordered_attachments
        // Attachments without an inline placeholder are appended after the
        // text-bearing attachments so transcript order stays deterministic.
        .sort_by_key(|attachment| text.find(&attachment.placeholder).unwrap_or(usize::MAX));

    let mut content_parts = Vec::new();
    let mut orphan_attachments = Vec::new();
    let mut remaining_text = text;

    for attachment in ordered_attachments {
        if let Some(placeholder_index) = remaining_text.find(&attachment.placeholder) {
            let (before_placeholder, after_placeholder) =
                remaining_text.split_at(placeholder_index);

            if !before_placeholder.is_empty() {
                content_parts.push(TurnPromptContentPart::Text(before_placeholder));
            }

            content_parts.push(TurnPromptContentPart::Attachment(attachment));
            remaining_text = &after_placeholder[attachment.placeholder.len()..];

            continue;
        }

        orphan_attachments.push(attachment);
    }

    if !remaining_text.is_empty() {
        content_parts.push(TurnPromptContentPart::Text(remaining_text));
    }

    content_parts.extend(
        orphan_attachments
            .into_iter()
            .map(TurnPromptContentPart::OrphanAttachment),
    );

    content_parts
}

impl From<String> for TurnPrompt {
    fn from(text: String) -> Self {
        Self::from_text(text)
    }
}

impl From<&str> for TurnPrompt {
    fn from(text: &str) -> Self {
        Self::from_text(text.to_string())
    }
}

impl From<&TurnPrompt> for TurnPrompt {
    fn from(prompt: &TurnPrompt) -> Self {
        prompt.clone()
    }
}

impl fmt::Display for TurnPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl PartialEq<&str> for TurnPrompt {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<TurnPrompt> for &str {
    fn eq(&self, other: &TurnPrompt) -> bool {
        *self == other.text
    }
}

/// Turn initiation mode for [`TurnRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequestKind {
    /// Starts a fresh interactive session turn with no prior context.
    SessionStart,
    /// Resumes an interactive session turn, optionally replaying transcript
    /// output into the next prompt.
    SessionResume {
        /// Prior session output used for history replay when present.
        session_output: Option<String>,
    },
    /// Runs one isolated utility prompt outside the long-lived session flow.
    UtilityPrompt,
}

impl AgentRequestKind {
    /// Returns the protocol request profile derived from this request kind.
    #[must_use]
    pub fn protocol_profile(&self) -> crate::infra::agent::ProtocolRequestProfile {
        match self {
            Self::SessionStart | Self::SessionResume { .. } => {
                crate::infra::agent::ProtocolRequestProfile::SessionTurn
            }
            Self::UtilityPrompt => crate::infra::agent::ProtocolRequestProfile::UtilityPrompt,
        }
    }

    /// Returns whether this request resumes a prior interactive session turn.
    #[must_use]
    pub fn is_resume(&self) -> bool {
        matches!(self, Self::SessionResume { .. })
    }

    /// Returns transcript output used for history replay, when present.
    #[must_use]
    pub fn session_output(&self) -> Option<&str> {
        match self {
            Self::SessionStart | Self::UtilityPrompt => None,
            Self::SessionResume { session_output } => session_output.as_deref(),
        }
    }
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
    /// Canonical request kind that drives transport behavior and protocol
    /// semantics for this turn.
    pub request_kind: AgentRequestKind,
    /// Structured user prompt for the turn.
    pub prompt: TurnPrompt,
    /// Provider-native conversation identifier loaded from persistence.
    ///
    /// When present, app-server channels forward this to the provider runtime
    /// so it can attempt native context resume. CLI channels ignore this field.
    pub provider_conversation_id: Option<String>,
    /// Reasoning effort preference for the turn.
    ///
    /// Ignored by providers/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
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
    /// Sent by CLI channels immediately after spawning the child process
    /// (`Some(pid)`) and again after the child exits (`None`). Consumers
    /// update the shared PID slot used by cancellation signals.
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
    AppServer(#[from] crate::infra::app_server::AppServerError),

    /// A CLI backend command or process execution failure.
    #[error("{0}")]
    Backend(String),

    /// A subprocess IO error such as a spawn failure or unavailable pipe.
    #[error("{0}")]
    Io(String),
}

/// Provider-agnostic session channel for executing agent turns.
///
/// Implementations bridge a specific transport - CLI subprocess or app-server
/// RPC - to the unified [`TurnEvent`] stream consumed by session workers. The
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
    /// Ensures session request kinds derive the session-turn protocol
    /// profile.
    fn test_agent_request_kind_session_variants_use_session_protocol_profile() {
        // Arrange
        let start = AgentRequestKind::SessionStart;
        let resume = AgentRequestKind::SessionResume {
            session_output: Some("prior output".to_string()),
        };

        // Act
        let start_profile = start.protocol_profile();
        let resume_profile = resume.protocol_profile();

        // Assert
        assert_eq!(
            start_profile,
            crate::infra::agent::ProtocolRequestProfile::SessionTurn
        );
        assert_eq!(
            resume_profile,
            crate::infra::agent::ProtocolRequestProfile::SessionTurn
        );
    }

    #[test]
    /// Ensures utility prompts derive the utility protocol profile and never
    /// expose replay output.
    fn test_agent_request_kind_utility_prompt_uses_utility_protocol_profile() {
        // Arrange
        let request_kind = AgentRequestKind::UtilityPrompt;

        // Act
        let protocol_profile = request_kind.protocol_profile();
        let session_output = request_kind.session_output();

        // Assert
        assert_eq!(
            protocol_profile,
            crate::infra::agent::ProtocolRequestProfile::UtilityPrompt
        );
        assert_eq!(session_output, None);
    }

    #[test]
    /// Ensures transcript text keeps inline image placeholders unchanged.
    fn test_turn_prompt_transcript_text_keeps_inline_placeholders() {
        // Arrange
        let prompt = TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            }],
            text: "Review [Image #1] carefully".to_string(),
        };

        // Act
        let transcript_text = prompt.transcript_text();

        // Assert
        assert_eq!(transcript_text, "Review [Image #1] carefully");
    }

    #[test]
    /// Ensures transcript text appends any attachment markers missing from the
    /// text payload.
    fn test_turn_prompt_transcript_text_appends_missing_placeholders() {
        // Arrange
        let prompt = TurnPrompt {
            attachments: vec![
                TurnPromptAttachment {
                    placeholder: "[Image #1]".to_string(),
                    local_image_path: PathBuf::from("/tmp/image-1.png"),
                },
                TurnPromptAttachment {
                    placeholder: "[Image #2]".to_string(),
                    local_image_path: PathBuf::from("/tmp/image-2.png"),
                },
            ],
            text: "Review".to_string(),
        };

        // Act
        let transcript_text = prompt.transcript_text();

        // Assert
        assert_eq!(transcript_text, "Review [Image #1] [Image #2]");
    }

    #[test]
    /// Ensures prompt content parts follow placeholder order and keep orphaned
    /// attachments at the end.
    fn test_split_turn_prompt_content_orders_placeholders_and_appends_orphans() {
        // Arrange
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-1.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-2.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #3]".to_string(),
                local_image_path: PathBuf::from("/tmp/image-3.png"),
            },
        ];

        // Act
        let content_parts =
            split_turn_prompt_content("Compare [Image #2] with [Image #1] now", &attachments);

        // Assert
        assert_eq!(
            content_parts,
            vec![
                TurnPromptContentPart::Text("Compare "),
                TurnPromptContentPart::Attachment(&attachments[1]),
                TurnPromptContentPart::Text(" with "),
                TurnPromptContentPart::Attachment(&attachments[0]),
                TurnPromptContentPart::Text(" now"),
                TurnPromptContentPart::OrphanAttachment(&attachments[2]),
            ]
        );
    }
}
