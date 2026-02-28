use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, BuildCommandRequest, build_resume_prompt,
};

/// Backend implementation for the Gemini CLI.
pub(super) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Gemini CLI needs no config files
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
        let has_history_replay = mode
            .session_output()
            .is_some_and(|session_output| !session_output.trim().is_empty());
        let prompt = match mode {
            AgentCommandMode::Start { prompt } => prompt.to_string(),
            AgentCommandMode::Resume {
                prompt,
                session_output,
            } => build_resume_prompt(prompt, session_output)?,
        };
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

        if matches!(mode, AgentCommandMode::Resume { .. }) && !has_history_replay {
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
        AgentBackend::setup(&backend, temp_directory.path()).expect("setup should succeed");

        // Assert
        assert_eq!(
            std::fs::read_dir(temp_directory.path())
                .expect("failed to read dir")
                .count(),
            0
        );
    }
}
