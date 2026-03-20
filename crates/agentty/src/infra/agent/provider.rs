//! Shared provider registry and transport policy descriptors.

use std::sync::Arc;

use super::backend::{
    AgentBackend, AgentBackendError, AgentPromptTransport, AgentTransport, AppServerThoughtPolicy,
    BuildCommandRequest,
};
use super::protocol;
use super::response_parser::ParsedResponse;
use crate::domain::agent::{AgentKind, AgentModel};
use crate::infra::app_server::AppServerClient;

/// Creates the backend implementation for the selected agent provider.
pub fn create_backend(kind: AgentKind) -> Box<dyn AgentBackend> {
    (provider_descriptor(kind).backend_factory)()
}

/// Returns the app-server client for the selected provider when applicable.
pub(crate) fn create_app_server_client(
    kind: AgentKind,
    default_client: Option<Arc<dyn AppServerClient>>,
) -> Option<Arc<dyn AppServerClient>> {
    (provider_descriptor(kind).app_server_client_factory)(default_client)
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
    provider_descriptor(kind).transport
}

/// Returns whether the provider expects prompts through stdin.
pub(crate) fn prompt_transport(kind: AgentKind) -> AgentPromptTransport {
    provider_descriptor(kind).prompt_transport
}

/// Parses one final assistant payload strictly against the shared protocol and
/// normalizes it for the active request profile.
///
/// # Errors
/// Returns a descriptive error when provider output does not match the
/// required protocol JSON.
pub(crate) fn parse_turn_response(
    _kind: AgentKind,
    response_text: &str,
    protocol_profile: protocol::ProtocolRequestProfile,
) -> Result<protocol::AgentResponse, String> {
    let response = protocol::parse_agent_response_strict(response_text).map_err(|error| {
        format!(
            "Agent output did not match the required JSON schema: \
             {error}\nresponse:\n{response_text}"
        )
    })?;

    Ok(protocol::normalize_turn_response(
        response,
        protocol_profile,
    ))
}

/// Returns whether app-server assistant chunks should be forwarded while the
/// turn is still in progress.
pub(crate) fn should_stream_app_server_assistant_messages(kind: AgentKind) -> bool {
    provider_descriptor(kind).stream_app_server_assistant_messages
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

/// Parses one model string into its owning provider kind.
///
/// # Errors
/// Returns an error when `model` is not a known `AgentModel`.
pub(crate) fn provider_kind_for_model(model: &str) -> Result<AgentKind, String> {
    model
        .parse::<AgentModel>()
        .map(crate::domain::agent::AgentModel::kind)
        .map_err(|error| format!("unknown model `{model}`: {error}"))
}

/// One backend/provider descriptor containing construction and parsing hooks.
struct AgentProviderDescriptor {
    app_server_client_factory:
        fn(Option<Arc<dyn AppServerClient>>) -> Option<Arc<dyn AppServerClient>>,
    backend_factory: fn() -> Box<dyn AgentBackend>,
    parse_response: fn(&str, &str) -> ParsedResponse,
    parse_stream_output_line: fn(&str) -> Option<(String, bool)>,
    app_server_thought_policy: AppServerThoughtPolicy,
    prompt_transport: AgentPromptTransport,
    stream_app_server_assistant_messages: bool,
    transport: AgentTransport,
}

fn provider_descriptor(kind: AgentKind) -> AgentProviderDescriptor {
    match kind {
        AgentKind::Gemini => AgentProviderDescriptor {
            app_server_client_factory: |default_client| {
                Some(default_client.unwrap_or_else(|| {
                    Arc::new(super::app_server::RealGeminiAcpClient::new())
                        as Arc<dyn AppServerClient>
                }))
            },
            backend_factory: || Box::new(super::gemini::GeminiBackend),
            parse_response: super::response_parser::parse_gemini_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_gemini_stream_output_line,
            app_server_thought_policy: AppServerThoughtPolicy::None,
            prompt_transport: AgentPromptTransport::Stdin,
            stream_app_server_assistant_messages: false,
            transport: AgentTransport::AppServer,
        },
        AgentKind::Claude => AgentProviderDescriptor {
            app_server_client_factory: |_default_client| None,
            backend_factory: || Box::new(super::claude::ClaudeBackend),
            parse_response: super::response_parser::parse_claude_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_claude_stream_output_line,
            app_server_thought_policy: AppServerThoughtPolicy::None,
            prompt_transport: AgentPromptTransport::Stdin,
            stream_app_server_assistant_messages: false,
            transport: AgentTransport::Cli,
        },
        AgentKind::Codex => AgentProviderDescriptor {
            app_server_client_factory: |default_client| {
                Some(default_client.unwrap_or_else(|| {
                    Arc::new(super::app_server::RealCodexAppServerClient::new())
                        as Arc<dyn AppServerClient>
                }))
            },
            backend_factory: || Box::new(super::codex::CodexBackend),
            parse_response: super::response_parser::parse_codex_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_codex_stream_output_line,
            app_server_thought_policy: AppServerThoughtPolicy::PhaseLabel,
            prompt_transport: AgentPromptTransport::Argv,
            stream_app_server_assistant_messages: true,
            transport: AgentTransport::AppServer,
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
    /// Ensures providers reject malformed final protocol payloads.
    fn test_parse_turn_response_rejects_invalid_payload() {
        // Arrange
        let raw_response = "plain response";

        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini] {
            // Act
            let result = parse_turn_response(
                kind,
                raw_response,
                protocol::ProtocolRequestProfile::SessionTurn,
            );

            // Assert
            assert!(result.is_err());
        }
    }

    #[test]
    /// Ensures valid session-turn payloads still gain an empty summary when
    /// the response omits it.
    fn test_parse_turn_response_fills_missing_summary_for_session_turn() {
        // Arrange
        let raw_response = r#"{"answer":"done","questions":[],"summary":null}"#;

        // Act
        let result = parse_turn_response(
            AgentKind::Codex,
            raw_response,
            protocol::ProtocolRequestProfile::SessionTurn,
        )
        .expect("valid protocol response should parse");

        // Assert
        assert_eq!(result.answer, "done");
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

    #[test]
    /// Ensures model strings resolve through the shared provider registry.
    fn test_provider_kind_for_model_reports_owner() {
        // Arrange / Act / Assert
        assert_eq!(
            provider_kind_for_model(AgentModel::Gpt53Codex.as_str()).expect("known model"),
            AgentKind::Codex
        );
    }
}
