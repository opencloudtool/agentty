use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};

/// Backend implementation for the Claude CLI.
pub(super) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) {
        // Claude Code needs no config files
    }

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut command = Command::new("claude");
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

        command
    }

    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        session_output: Option<String>,
    ) -> Result<Command, String> {
        let prompt = build_resume_prompt(prompt, session_output.as_deref())?;
        let mut command = Command::new("claude");
        command.arg("-c").arg("-p").arg(prompt);
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
        let command = AgentBackend::build_start_command(
            &backend,
            temp_directory.path(),
            "Plan prompt",
            "claude-sonnet-4-6",
        );
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("--allowedTools"));
        assert!(debug_command.contains("Edit,Bash"));
        assert!(!debug_command.contains("--permission-mode"));
    }
}
