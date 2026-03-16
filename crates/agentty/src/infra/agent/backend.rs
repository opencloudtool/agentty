use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use super::protocol;
use super::response_parser::ParsedResponse;
use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::infra::app_server::AppServerClient;
use crate::infra::channel::{AgentRequestKind, TurnPromptAttachment};

/// Transport runtime used to execute turns for one backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTransport {
    /// Provider runs through persistent app-server sessions.
    AppServer,
    /// Provider runs as direct CLI subprocess commands.
    Cli,
}

impl AgentTransport {
    /// Returns whether this transport uses app-server sessions.
    pub fn uses_app_server(self) -> bool {
        matches!(self, Self::AppServer)
    }
}

/// Prompt delivery mode used by one provider backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentPromptTransport {
    /// Prompt is passed inline through argv.
    Argv,
    /// Prompt is streamed through stdin.
    Stdin,
}

/// Final protocol validation mode used after transport output is collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentResponsePolicy {
    /// Invalid protocol payloads fall back to plain text.
    BestEffort,
    /// Invalid protocol payloads fail the turn immediately.
    Strict,
}

/// App-server thought-stream classification policy for one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppServerThoughtPolicy {
    /// Provider does not expose dedicated thought phases.
    None,
    /// Provider uses phase labels to distinguish thought chunks.
    PhaseLabel,
}

/// Request payload used to build provider commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCommandRequest<'a> {
    /// Ordered local image attachments referenced from the prompt body.
    pub attachments: &'a [TurnPromptAttachment],
    /// Working directory where the command will run.
    pub folder: &'a Path,
    /// User prompt to send.
    pub prompt: &'a str,
    /// Canonical request kind that drives execution and protocol semantics.
    pub request_kind: &'a AgentRequestKind,
    /// Provider-specific model identifier.
    pub model: &'a str,
    /// Reasoning effort preference for this turn.
    ///
    /// Ignored by backends/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
}

/// Error type for backend setup and command construction failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentBackendError {
    /// One-time backend setup failure.
    Setup(String),
    /// Per-command build failure.
    CommandBuild(String),
}

impl fmt::Display for AgentBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(message) | Self::CommandBuild(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl Error for AgentBackendError {}

/// Builds and configures external agent CLI commands.
#[cfg_attr(test, mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    ///
    /// # Errors
    /// Returns an error when one-time backend setup cannot be completed.
    fn setup(&self, folder: &Path) -> Result<(), AgentBackendError>;

    /// Returns the transport mode owned by this backend implementation.
    fn transport(&self) -> AgentTransport;

    /// Returns the app-server client used by this backend when it executes
    /// turns through a persistent runtime.
    ///
    /// `default_client` acts as an optional override for tests or injected
    /// environments. CLI-only backends return `None`.
    fn app_server_client(
        &self,
        default_client: Option<Arc<dyn AppServerClient>>,
    ) -> Option<Arc<dyn AppServerClient>>;

    /// Builds one command for a start, resume, or one-shot interaction.
    ///
    /// # Errors
    /// Returns an error when prompt rendering or provider argument
    /// construction fails.
    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError>;
}

/// Creates the backend implementation for the selected agent provider.
pub fn create_backend(kind: AgentKind) -> Box<dyn AgentBackend> {
    (provider_descriptor(kind).backend_factory)()
}

/// Parses provider output and returns final response content and usage stats.
pub fn parse_response(kind: AgentKind, stdout: &str, stderr: &str) -> ParsedResponse {
    (provider_descriptor(kind).parse_response)(stdout, stderr)
}

/// Parses one stream line into incremental text and content classification.
///
/// Returns `(text, is_response_content)` where `is_response_content` is `true`
/// for model-authored content and `false` for progress updates.
pub(crate) fn parse_stream_output_line(
    kind: AgentKind,
    stdout_line: &str,
) -> Option<(String, bool)> {
    (provider_descriptor(kind).parse_stream_output_line)(stdout_line)
}

/// Returns transport mode for the selected provider.
pub fn transport_mode(kind: AgentKind) -> AgentTransport {
    create_backend(kind).transport()
}

/// Returns whether the provider expects prompts through stdin.
pub(crate) fn prompt_transport(kind: AgentKind) -> AgentPromptTransport {
    provider_descriptor(kind).prompt_transport
}

/// Parses one final assistant payload according to the provider response
/// policy and normalizes it for the active protocol profile.
///
/// # Errors
/// Returns a descriptive error when a strict provider emits invalid protocol
/// JSON.
pub(crate) fn parse_turn_response(
    kind: AgentKind,
    response_text: &str,
    protocol_profile: protocol::ProtocolRequestProfile,
) -> Result<protocol::AgentResponse, String> {
    let response = match provider_descriptor(kind).response_policy {
        AgentResponsePolicy::BestEffort => protocol::parse_agent_response(response_text),
        AgentResponsePolicy::Strict => protocol::parse_agent_response_strict(response_text)
            .map_err(|error| {
                format!(
                    "Agent output did not match the required JSON schema: \
                     {error}\nresponse:\n{response_text}"
                )
            })?,
    };

    Ok(protocol::normalize_turn_response(
        response,
        protocol_profile,
    ))
}

/// Returns whether app-server assistant chunks should be forwarded while the
/// turn is still in progress.
pub(crate) fn should_stream_app_server_assistant_messages(kind: AgentKind) -> bool {
    !matches!(
        provider_descriptor(kind).response_policy,
        AgentResponsePolicy::Strict
    )
}

/// Returns whether one app-server assistant chunk should be treated as
/// thought text instead of transcript output.
pub(crate) fn is_app_server_thought_chunk(
    kind: AgentKind,
    is_delta: bool,
    phase: Option<&str>,
) -> bool {
    if !is_delta {
        return false;
    }

    match provider_descriptor(kind).app_server_thought_policy {
        AppServerThoughtPolicy::None => false,
        AppServerThoughtPolicy::PhaseLabel => phase.is_some_and(is_codex_thought_phase_label),
    }
}

/// Builds one optional stdin payload for providers that stream prompts instead
/// of sending them through argv.
///
/// # Errors
/// Returns an error when provider-specific prompt rendering fails.
pub(crate) fn build_command_stdin_payload(
    kind: AgentKind,
    request: BuildCommandRequest<'_>,
) -> Result<Option<Vec<u8>>, AgentBackendError> {
    match prompt_transport(kind) {
        AgentPromptTransport::Argv => Ok(None),
        AgentPromptTransport::Stdin => match kind {
            AgentKind::Gemini => super::gemini::build_prompt_stdin_payload(request).map(Some),
            AgentKind::Claude => super::claude::build_prompt_stdin_payload(request).map(Some),
            AgentKind::Codex => Ok(None),
        },
    }
}

/// One backend/provider descriptor containing construction and parsing hooks.
struct AgentProviderDescriptor {
    backend_factory: fn() -> Box<dyn AgentBackend>,
    prompt_transport: AgentPromptTransport,
    parse_response: fn(&str, &str) -> ParsedResponse,
    parse_stream_output_line: fn(&str) -> Option<(String, bool)>,
    response_policy: AgentResponsePolicy,
    app_server_thought_policy: AppServerThoughtPolicy,
}

fn provider_descriptor(kind: AgentKind) -> AgentProviderDescriptor {
    match kind {
        AgentKind::Gemini => AgentProviderDescriptor {
            backend_factory: || Box::new(super::gemini::GeminiBackend),
            prompt_transport: AgentPromptTransport::Stdin,
            parse_response: super::response_parser::parse_gemini_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_gemini_stream_output_line,
            response_policy: AgentResponsePolicy::Strict,
            app_server_thought_policy: AppServerThoughtPolicy::None,
        },
        AgentKind::Claude => AgentProviderDescriptor {
            backend_factory: || Box::new(super::claude::ClaudeBackend),
            prompt_transport: AgentPromptTransport::Stdin,
            parse_response: super::response_parser::parse_claude_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_claude_stream_output_line,
            response_policy: AgentResponsePolicy::Strict,
            app_server_thought_policy: AppServerThoughtPolicy::None,
        },
        AgentKind::Codex => AgentProviderDescriptor {
            backend_factory: || Box::new(super::codex::CodexBackend),
            prompt_transport: AgentPromptTransport::Argv,
            parse_response: super::response_parser::parse_codex_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_codex_stream_output_line,
            response_policy: AgentResponsePolicy::BestEffort,
            app_server_thought_policy: AppServerThoughtPolicy::PhaseLabel,
        },
    }
}

/// Returns whether one Codex phase label denotes thought/planning text.
///
/// Phase matching is case-insensitive so provider variants such as `Thinking`
/// and `PLAN` continue to route to thought deltas.
fn is_codex_thought_phase_label(phase: &str) -> bool {
    let normalized_phase = phase.trim();

    normalized_phase.eq_ignore_ascii_case("thinking")
        || normalized_phase.eq_ignore_ascii_case("plan")
        || normalized_phase.eq_ignore_ascii_case("reasoning")
        || normalized_phase.eq_ignore_ascii_case("thought")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures transport capability is provided by infra backend descriptors,
    /// not domain enums.
    fn test_transport_mode_reports_expected_transport_by_provider() {
        // Arrange
        let claude_kind = AgentKind::Claude;
        let codex_kind = AgentKind::Codex;
        let gemini_kind = AgentKind::Gemini;

        // Act
        let claude_transport = transport_mode(claude_kind);
        let codex_transport = transport_mode(codex_kind);
        let gemini_transport = transport_mode(gemini_kind);

        // Assert
        assert_eq!(claude_transport, AgentTransport::Cli);
        assert_eq!(codex_transport, AgentTransport::AppServer);
        assert_eq!(gemini_transport, AgentTransport::AppServer);
    }

    #[test]
    /// Ensures prompt delivery is also derived from the shared provider
    /// descriptor.
    fn test_prompt_transport_reports_expected_mode_by_provider() {
        // Arrange
        let claude_kind = AgentKind::Claude;
        let codex_kind = AgentKind::Codex;
        let gemini_kind = AgentKind::Gemini;

        // Act
        let claude_transport = prompt_transport(claude_kind);
        let codex_transport = prompt_transport(codex_kind);
        let gemini_transport = prompt_transport(gemini_kind);

        // Assert
        assert_eq!(claude_transport, AgentPromptTransport::Stdin);
        assert_eq!(codex_transport, AgentPromptTransport::Argv);
        assert_eq!(gemini_transport, AgentPromptTransport::Stdin);
    }

    #[test]
    /// Ensures strict providers reject malformed final protocol payloads.
    fn test_parse_turn_response_rejects_invalid_payload_for_strict_provider() {
        // Arrange
        let raw_response = "plain response";

        // Act
        let result = parse_turn_response(
            AgentKind::Claude,
            raw_response,
            protocol::ProtocolRequestProfile::SessionTurn,
        );

        // Assert
        assert!(result.is_err());
    }

    #[test]
    /// Ensures best-effort providers preserve plain-text payloads while still
    /// normalizing the protocol profile.
    fn test_parse_turn_response_keeps_plain_text_for_best_effort_provider() {
        // Arrange
        let raw_response = "plain response";

        // Act
        let result = parse_turn_response(
            AgentKind::Codex,
            raw_response,
            protocol::ProtocolRequestProfile::SessionTurn,
        )
        .expect("best-effort provider should accept plain text");

        // Assert
        assert_eq!(result.answer, "plain response");
        assert_eq!(
            result.summary,
            Some(protocol::AgentResponseSummary {
                session: String::new(),
                turn: String::new(),
            })
        );
    }

    #[test]
    /// Ensures Codex app-server phase labels map to thought deltas through the
    /// shared provider descriptor.
    fn test_is_app_server_thought_chunk_reports_codex_phase_labels() {
        // Arrange / Act / Assert
        assert!(is_app_server_thought_chunk(
            AgentKind::Codex,
            true,
            Some("thinking"),
        ));
        assert!(!is_app_server_thought_chunk(
            AgentKind::Gemini,
            true,
            Some("thinking"),
        ));
    }
}
