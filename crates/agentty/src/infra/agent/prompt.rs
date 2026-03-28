use askama::Template;

use super::backend::AgentBackendError;
use super::protocol::{self, ProtocolRequestProfile};

/// Marker used to detect whether protocol instructions are already included
/// in a prompt.
const PROTOCOL_INSTRUCTIONS_MARKER: &str = "Structured response protocol:";

/// Askama view model for rendering resume prompts with prior session output.
#[derive(Template)]
#[template(path = "resume_with_session_output_prompt.md", escape = "none")]
struct ResumeWithSessionOutputPromptTemplate<'a> {
    /// New prompt content appended after the replayed transcript.
    prompt: &'a str,
    /// Prior session output replayed into the follow-up prompt.
    session_output: &'a str,
}

/// Askama view model for rendering structured response protocol
/// instructions.
///
/// The template embeds one shared self-descriptive JSON schema so every
/// provider sees the same prompt-side protocol contract.
#[derive(Template)]
#[template(path = "protocol_instruction_prompt.md", escape = "none")]
struct ProtocolInstructionPromptTemplate<'a> {
    /// Request-family-specific instructions that reinforce the expected
    /// response shape for the active prompt type.
    protocol_usage_instructions: &'a str,
    /// User prompt appended after protocol instructions.
    prompt: &'a str,
    /// Pretty-printed self-descriptive JSON schema contract injected into the
    /// prompt template.
    response_json_schema: &'a str,
}

/// Shared prompt preparation input for one transport turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptPreparationRequest<'a> {
    /// Base user prompt before replay wrapping and protocol instructions.
    pub prompt: &'a str,
    /// Protocol family that determines the rendered instruction envelope.
    pub protocol_profile: ProtocolRequestProfile,
    /// Prior session output available for transcript replay.
    pub replay_session_output: Option<&'a str>,
    /// Whether this turn should wrap the prompt with replay context.
    pub should_replay_session_output: bool,
}

/// Applies transcript replay and protocol instructions to one prompt.
///
/// # Errors
/// Returns an error when replay or instruction templates fail to render.
pub(crate) fn prepare_prompt_text(
    request: PromptPreparationRequest<'_>,
) -> Result<String, AgentBackendError> {
    let prompt = if request.should_replay_session_output {
        build_resume_prompt(request.prompt, request.replay_session_output)?
    } else {
        request.prompt.to_string()
    };

    prepend_protocol_instructions(&prompt, request.protocol_profile)
}

/// Builds a resume prompt that optionally prepends previous session output.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn build_resume_prompt(
    prompt: &str,
    session_output: Option<&str>,
) -> Result<String, AgentBackendError> {
    let Some(session_output) = session_output
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(prompt.to_string());
    };

    let template = ResumeWithSessionOutputPromptTemplate {
        prompt,
        session_output,
    };
    let rendered = template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!(
            "Failed to render `resume_with_session_output_prompt.md`: {error}"
        ))
    })?;

    Ok(rendered.trim_end().to_string())
}

/// Prepends structured response protocol instructions to a prompt.
///
/// Tells agents to emit one top-level JSON object that matches the shared
/// schema so response parsing can deserialize directly into the internal
/// protocol structs, and requires repository-root-relative POSIX file paths in
/// rendered answers. If the prompt already contains the protocol marker, this
/// function returns the prompt unchanged to avoid duplicated guidance.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
// Future: expand `profile`-driven guidance to inject per-request-family
// protocol rules once task-prompt formatting rules are consolidated.
pub(crate) fn prepend_protocol_instructions(
    prompt: &str,
    profile: ProtocolRequestProfile,
) -> Result<String, AgentBackendError> {
    if prompt.contains(PROTOCOL_INSTRUCTIONS_MARKER) {
        return Ok(prompt.to_string());
    }

    let response_json_schema = protocol::agent_response_json_schema_json();
    let template = ProtocolInstructionPromptTemplate {
        protocol_usage_instructions: protocol_usage_instructions(profile),
        prompt,
        response_json_schema: &response_json_schema,
    };
    let rendered = template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!(
            "Failed to render `protocol_instruction_prompt.md`: {error}"
        ))
    })?;

    Ok(rendered.trim_end().to_string())
}

/// Returns request-family-specific protocol guidance for the shared prompt
/// preamble.
fn protocol_usage_instructions(profile: ProtocolRequestProfile) -> &'static str {
    match profile {
        ProtocolRequestProfile::SessionTurn => {
            "For this session turn, keep user-facing content in `answer`, emit clarification \
             prompts through `questions`, and populate `summary` when reporting delivered work."
        }
        ProtocolRequestProfile::UtilityPrompt => {
            "For this one-shot utility prompt, return the entire response as a JSON object like \
             {\"answer\":\"...\",\"questions\":[],\"summary\":null}. Put all useful plain-text \
             output in `answer`."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures resume prompt rendering includes trimmed session output and
    /// the new user prompt.
    fn test_build_resume_prompt_includes_session_output_and_prompt() {
        // Arrange
        let prompt = "Continue and update tests";
        let session_output = Some("  previous output line  \n");

        // Act
        let resume_prompt =
            build_resume_prompt(prompt, session_output).expect("resume prompt should render");

        // Assert
        assert!(resume_prompt.contains("previous output line"));
        assert!(resume_prompt.contains("Continue and update tests"));
    }

    #[test]
    /// Ensures whitespace-only session output does not trigger transcript
    /// wrapping and returns the original prompt.
    fn test_build_resume_prompt_returns_original_prompt_when_output_is_blank() {
        // Arrange
        let prompt = "Follow-up request";
        let session_output = Some("   ");

        // Act
        let resume_prompt =
            build_resume_prompt(prompt, session_output).expect("resume prompt should render");

        // Assert
        assert_eq!(resume_prompt, prompt);
    }

    #[test]
    /// Ensures absent session output keeps resume prompt formatting unchanged.
    fn test_build_resume_prompt_returns_original_prompt_without_output() {
        // Arrange
        let prompt = "Retry merge";

        // Act
        let resume_prompt = build_resume_prompt(prompt, None).expect("resume prompt should render");

        // Assert
        assert_eq!(resume_prompt, prompt);
    }

    #[test]
    /// Ensures session prompts include the critical protocol contract markers.
    fn test_prepend_protocol_instructions_adds_session_protocol_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt =
            prepend_protocol_instructions(prompt, ProtocolRequestProfile::SessionTurn)
                .expect("protocol instruction prompt should render");

        // Assert
        assert!(rendered_prompt.contains("File path output requirements:"));
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(rendered_prompt.contains("Paths must be relative to the repository root."));
        assert!(rendered_prompt.contains("If you run git commands, use read-only commands only"));
        assert!(rendered_prompt.contains("Do not run mutating git commands"));
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("Return a single JSON object"));
        assert!(rendered_prompt.contains("Do not wrap the JSON in markdown code fences."));
        assert!(rendered_prompt.contains("Follow this JSON Schema exactly."));
        assert!(rendered_prompt.contains("Treat the JSON Schema titles and descriptions"));
        assert!(rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.contains("---"));
        assert!(rendered_prompt.contains("For this session turn"));
        assert!(rendered_prompt.contains("summary"));
        assert!(rendered_prompt.contains("turn"));
        assert!(rendered_prompt.contains("session"));
        assert!(rendered_prompt.contains("\"answer\""));
        assert!(rendered_prompt.contains("\"questions\""));
        assert!(rendered_prompt.contains("\"title\""));
        assert!(rendered_prompt.contains("\"description\""));
        assert!(rendered_prompt.contains("summary"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures protocol instructions are not duplicated when already present.
    fn test_prepend_protocol_instructions_is_idempotent() {
        // Arrange
        let prompt =
            prepend_protocol_instructions("Implement feature", ProtocolRequestProfile::SessionTurn)
                .expect("protocol instruction prompt should render");

        // Act
        let rendered_prompt =
            prepend_protocol_instructions(&prompt, ProtocolRequestProfile::UtilityPrompt)
                .expect("protocol instruction prompt should render");

        // Assert
        assert_eq!(rendered_prompt, prompt);
    }

    #[test]
    /// Ensures one-shot prompts reuse the shared schema-only protocol
    /// instructions.
    fn test_prepend_protocol_instructions_reuses_same_contract_for_one_shot() {
        // Arrange
        let prompt = "Generate title";

        // Act
        let rendered_prompt =
            prepend_protocol_instructions(prompt, ProtocolRequestProfile::UtilityPrompt)
                .expect("protocol instruction prompt should render");

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("---"));
        assert!(rendered_prompt.contains("For this one-shot utility prompt"));
        assert!(rendered_prompt.contains(r#"{"answer":"...","questions":[],"summary":null}"#));
        assert!(rendered_prompt.contains("\"summary\""));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures shared prompt preparation applies replay wrapping before
    /// protocol instructions.
    fn test_prepare_prompt_text_applies_replay_and_protocol_instructions() {
        // Arrange
        let request = PromptPreparationRequest {
            prompt: "Continue edits",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_session_output: Some("previous output"),
            should_replay_session_output: true,
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

        // Assert
        assert!(prepared_prompt.contains("Structured response protocol:"));
        assert!(prepared_prompt.contains("previous output"));
        assert!(prepared_prompt.ends_with("Continue edits"));
    }
}
