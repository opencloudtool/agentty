use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};
use crate::domain::permission::PermissionMode;

/// Backend implementation for the Gemini CLI.
pub(super) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) {
        // Gemini CLI needs no config files
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
        let approval_mode = match permission_mode {
            PermissionMode::AutoEdit | PermissionMode::Plan => "auto_edit",
            PermissionMode::Autonomous => "yolo",
        };
        let mut command = Command::new("gemini");
        command
            .arg("--prompt")
            .arg(prompt.as_ref())
            .arg("--model")
            .arg(model)
            .arg("--approval-mode")
            .arg(approval_mode)
            .arg("--output-format")
            .arg("stream-json")
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
        let has_history_replay = session_output
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let mut command = self.build_start_command(
            folder,
            &prompt,
            model,
            permission_mode,
            is_initial_plan_prompt,
        );

        if !has_history_replay {
            command.arg("--resume").arg("latest");
        }

        command
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_gemini_setup_creates_no_files() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        AgentBackend::setup(&backend, temp_directory.path());

        // Assert
        assert_eq!(
            std::fs::read_dir(temp_directory.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }
}
