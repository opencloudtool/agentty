use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};

/// Backend implementation for the Gemini CLI.
pub(super) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) {
        // Gemini CLI needs no config files
    }

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut command = Command::new("gemini");
        command
            .arg("--prompt")
            .arg(prompt)
            .arg("--model")
            .arg(model)
            .arg("--approval-mode")
            .arg("auto_edit")
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
        session_output: Option<String>,
    ) -> Result<Command, String> {
        let has_history_replay = session_output
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let prompt = build_resume_prompt(prompt, session_output.as_deref())?;
        let mut command = self.build_start_command(folder, &prompt, model);

        if !has_history_replay {
            command.arg("--resume").arg("latest");
        }

        Ok(command)
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
