use std::path::Path;
use std::process::Command;

use crate::domain::agent::AgentKind;
use crate::domain::permission::PermissionMode;

const RESUME_WITH_SESSION_OUTPUT_PROMPT_TEMPLATE: &str =
    include_str!("../../../resources/resume_with_session_output_prompt.md");

/// Builds and configures external agent CLI commands.
#[cfg_attr(test, mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    fn setup(&self, folder: &Path);

    /// Builds a command for an initial task prompt.
    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command;

    /// Builds a command for a resumed task or reply.
    ///
    /// Implementations may intentionally start a fresh conversation when
    /// `session_output` is provided (for example, to replay history after a
    /// model switch).
    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command;
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
pub(crate) fn build_resume_prompt(prompt: &str, session_output: Option<&str>) -> String {
    let Some(session_output) = session_output
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return prompt.to_string();
    };

    RESUME_WITH_SESSION_OUTPUT_PROMPT_TEMPLATE
        .trim_end()
        .replace("{session_output}", session_output)
        .replace("{prompt}", prompt)
}
