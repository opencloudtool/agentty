use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;

use askama::Template;

use crate::domain::agent::AgentKind;
use crate::infra::agent::response_parser::ParsedResponse;

/// Marker used to detect whether question instructions are already included
/// in a prompt.
const QUESTION_INSTRUCTIONS_MARKER: &str = "Clarification questions:";

/// Marker used to detect whether repo-root path instructions are already
/// included in a prompt.
const REPO_ROOT_PATH_INSTRUCTIONS_MARKER: &str = "repository-root-relative POSIX paths";

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

/// Request payload used to build provider commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCommandRequest<'a> {
    /// Working directory where the command will run.
    pub folder: &'a Path,
    /// Prompt mode and optional replay payload.
    pub mode: AgentCommandMode<'a>,
    /// Provider-specific model identifier.
    pub model: &'a str,
}

/// Prompt mode for command construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCommandMode<'a> {
    /// Starts a fresh turn.
    Start {
        /// User prompt to send.
        prompt: &'a str,
    },
    /// Resumes a prior turn, optionally replaying transcript output.
    Resume {
        /// User prompt to send.
        prompt: &'a str,
        /// Prior session output used for history replay when present.
        session_output: Option<&'a str>,
    },
}

impl<'a> AgentCommandMode<'a> {
    /// Returns the user prompt for this command mode.
    pub fn prompt(self) -> &'a str {
        match self {
            Self::Start { prompt } | Self::Resume { prompt, .. } => prompt,
        }
    }

    /// Returns transcript output used for resume replay, when present.
    pub fn session_output(self) -> Option<&'a str> {
        match self {
            Self::Start { .. } => None,
            Self::Resume { session_output, .. } => session_output,
        }
    }
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

/// Askama view model for rendering resume prompts with prior session output.
#[derive(Template)]
#[template(path = "resume_with_session_output_prompt.md", escape = "none")]
struct ResumeWithSessionOutputPromptTemplate<'a> {
    prompt: &'a str,
    session_output: &'a str,
}

/// Askama view model for rendering clarification-question format instructions.
#[derive(Template)]
#[template(path = "question_instruction_prompt.md", escape = "none")]
struct QuestionInstructionPromptTemplate<'a> {
    prompt: &'a str,
}

/// Askama view model for rendering repo-root file path contract instructions.
#[derive(Template)]
#[template(path = "repo_root_path_prompt.md", escape = "none")]
struct RepoRootPathPromptTemplate<'a> {
    prompt: &'a str,
}

/// Builds and configures external agent CLI commands.
#[cfg_attr(test, mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    ///
    /// # Errors
    /// Returns an error when one-time backend setup cannot be completed.
    fn setup(&self, folder: &Path) -> Result<(), AgentBackendError>;

    /// Builds one command for a start or resume interaction.
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
    provider_descriptor(kind).transport
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

/// Prepends mandatory repo-root-relative file path instructions to a prompt.
///
/// If the prompt already contains the path-instruction marker, this function
/// returns the prompt unchanged to avoid duplicated guidance.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn prepend_repo_root_path_instructions(
    prompt: &str,
) -> Result<String, AgentBackendError> {
    if prompt.contains(REPO_ROOT_PATH_INSTRUCTIONS_MARKER) {
        return Ok(prompt.to_string());
    }

    let template = RepoRootPathPromptTemplate { prompt };
    let rendered = template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!(
            "Failed to render `repo_root_path_prompt.md`: {error}"
        ))
    })?;

    Ok(rendered.trim_end().to_string())
}

/// Prepends clarification-question format instructions to a prompt.
///
/// Tells agents to include a `**Questions**` section with numbered items when
/// they need user clarification. If the prompt already contains the
/// question-instruction marker, this function returns the prompt unchanged to
/// avoid duplicated guidance.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn prepend_question_instructions(prompt: &str) -> Result<String, AgentBackendError> {
    if prompt.contains(QUESTION_INSTRUCTIONS_MARKER) {
        return Ok(prompt.to_string());
    }

    let template = QuestionInstructionPromptTemplate { prompt };
    let rendered = template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!(
            "Failed to render `question_instruction_prompt.md`: {error}"
        ))
    })?;

    Ok(rendered.trim_end().to_string())
}

/// One backend/provider descriptor containing construction and parsing hooks.
struct AgentProviderDescriptor {
    backend_factory: fn() -> Box<dyn AgentBackend>,
    parse_response: fn(&str, &str) -> ParsedResponse,
    parse_stream_output_line: fn(&str) -> Option<(String, bool)>,
    transport: AgentTransport,
}

fn provider_descriptor(kind: AgentKind) -> AgentProviderDescriptor {
    match kind {
        AgentKind::Gemini => AgentProviderDescriptor {
            backend_factory: || Box::new(super::gemini::GeminiBackend),
            parse_response: super::response_parser::parse_gemini_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_gemini_stream_output_line,
            transport: AgentTransport::AppServer,
        },
        AgentKind::Claude => AgentProviderDescriptor {
            backend_factory: || Box::new(super::claude::ClaudeBackend),
            parse_response: super::response_parser::parse_claude_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_claude_stream_output_line,
            transport: AgentTransport::Cli,
        },
        AgentKind::Codex => AgentProviderDescriptor {
            backend_factory: || Box::new(super::codex::CodexBackend),
            parse_response: super::response_parser::parse_codex_response_with_fallback,
            parse_stream_output_line: super::response_parser::parse_codex_stream_output_line,
            transport: AgentTransport::AppServer,
        },
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
    /// Ensures repo-root path instructions are prepended to plain prompts.
    fn test_prepend_repo_root_path_instructions_adds_contract() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = prepend_repo_root_path_instructions(prompt)
            .expect("path instruction prompt should render");

        // Assert
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(rendered_prompt.contains("Paths must be relative to the repository root."));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures path instructions are not duplicated when already present.
    fn test_prepend_repo_root_path_instructions_is_idempotent() {
        // Arrange
        let prompt = prepend_repo_root_path_instructions("Implement feature")
            .expect("path instruction prompt should render");

        // Act
        let rendered_prompt = prepend_repo_root_path_instructions(&prompt)
            .expect("path instruction prompt should render");

        // Assert
        assert_eq!(rendered_prompt, prompt);
    }

    #[test]
    /// Ensures question instructions are prepended to plain prompts.
    fn test_prepend_question_instructions_adds_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = prepend_question_instructions(prompt)
            .expect("question instruction prompt should render");

        // Assert
        assert!(rendered_prompt.contains("Clarification questions:"));
        assert!(rendered_prompt.contains("**Questions**"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures question instructions are not duplicated when already present.
    fn test_prepend_question_instructions_is_idempotent() {
        // Arrange
        let prompt = prepend_question_instructions("Implement feature")
            .expect("question instruction prompt should render");

        // Act
        let rendered_prompt = prepend_question_instructions(&prompt)
            .expect("question instruction prompt should render");

        // Assert
        assert_eq!(rendered_prompt, prompt);
    }

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
}
