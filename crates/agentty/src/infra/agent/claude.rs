use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};
use crate::domain::permission::PermissionMode;

/// Backend implementation for the Claude CLI.
pub(super) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) {
        // Claude Code needs no config files
    }

    fn build_start_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
    ) -> Command {
        let prompt = permission_mode.apply_to_prompt(prompt, is_initial_plan_prompt);
        let mut command = Command::new("claude");
        command.arg("-p").arg(prompt.as_ref());
        Self::apply_permission_args(&mut command, permission_mode);
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
        permission_mode: PermissionMode,
        is_initial_plan_prompt: bool,
        session_output: Option<String>,
    ) -> Command {
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let prompt = permission_mode.apply_to_prompt(&prompt, is_initial_plan_prompt);
        let mut command = Command::new("claude");
        command.arg("-c").arg("-p").arg(prompt.as_ref());
        Self::apply_permission_args(&mut command, permission_mode);
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
}

impl ClaudeBackend {
    fn apply_permission_args(command: &mut Command, permission_mode: PermissionMode) {
        match permission_mode {
            PermissionMode::AutoEdit | PermissionMode::Plan => {
                command.arg("--allowedTools").arg("Edit");
            }
            PermissionMode::Autonomous => {
                command.arg("--dangerously-skip-permissions");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_claude_plan_mode_uses_allowed_tools_edit() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_start_command(
            &backend,
            temp_directory.path(),
            "Plan prompt",
            "claude-sonnet-4-6",
            PermissionMode::Plan,
            true,
        );
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("--allowedTools"));
        assert!(debug_command.contains("Edit"));
        assert!(!debug_command.contains("--permission-mode"));
    }
}
