use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, BuildCommandRequest, build_resume_prompt,
};

/// Backend implementation for the Claude CLI.
pub(super) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Claude Code needs no config files
        Ok(())
    }

    fn build_command(
        &self,
        request: BuildCommandRequest<'_>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            folder,
            mode,
            model,
        } = request;
        let prompt = match mode {
            AgentCommandMode::Start { prompt } => prompt.to_string(),
            AgentCommandMode::Resume {
                prompt,
                session_output,
            } => build_resume_prompt(prompt, session_output)?,
        };
        let mut command = Command::new("claude");

        if matches!(mode, AgentCommandMode::Resume { .. }) {
            command.arg("-c");
        }

        command.arg("-p").arg(prompt);
        command.arg("--allowedTools").arg("Edit,Bash");
        command
            .arg("--verbose")
            .arg("--output-format")
            .arg("stream-json")
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_claude_auto_edit_mode_uses_allowed_tools_edit() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Plan prompt",
                },
                model: "claude-sonnet-4-6",
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("--allowedTools"));
        assert!(debug_command.contains("Edit,Bash"));
        assert!(!debug_command.contains("--permission-mode"));
    }
}
