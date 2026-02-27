use std::path::Path;
use std::process::Command;

use askama::Template;

use crate::domain::agent::AgentKind;

/// Askama view model for rendering resume prompts with prior session output.
#[derive(Template)]
#[template(path = "resume_with_session_output_prompt.md", escape = "none")]
struct ResumeWithSessionOutputPromptTemplate<'a> {
    prompt: &'a str,
    session_output: &'a str,
}

/// Builds and configures external agent CLI commands.
#[cfg_attr(test, mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    fn setup(&self, folder: &Path);

    /// Builds a command for an initial task prompt.
    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command;

    /// Builds a command for a resumed task or reply.
    ///
    /// Implementations may intentionally start a fresh conversation when
    /// `session_output` is provided (for example, to replay history after a
    /// model switch).
    ///
    /// # Errors
    /// Returns an error when resume prompt rendering fails.
    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        session_output: Option<String>,
    ) -> Result<Command, String>;
}

/// Creates the backend implementation for the selected agent provider.
pub fn create_backend(kind: AgentKind) -> Box<dyn AgentBackend> {
    match kind {
        AgentKind::Gemini => Box::new(super::gemini::GeminiBackend),
        AgentKind::Claude => Box::new(super::claude::ClaudeBackend),
        AgentKind::Codex => Box::new(super::codex::CodexBackend),
    }
}

/// Builds a resume prompt that optionally prepends previous session output.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn build_resume_prompt(
    prompt: &str,
    session_output: Option<&str>,
) -> Result<String, String> {
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
        format!("Failed to render `resume_with_session_output_prompt.md`: {error}")
    })?;

    Ok(rendered.trim_end().to_string())
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
}
