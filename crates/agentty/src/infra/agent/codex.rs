use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, BuildCommandRequest, build_resume_prompt,
    prepend_protocol_instructions, prepend_repo_root_path_instructions,
};
use crate::domain::agent::ReasoningLevel;

/// Uses non-interactive Codex commands so Agentty can capture piped output.
///
/// Interactive `codex` requires a TTY and fails in this app with
/// `Error: stdout is not a terminal`, so this backend runs
/// `codex exec --full-auto`. Resume uses `codex exec resume --last --full-auto`
/// only when replay history is not injected into the prompt. Project
/// instruction discovery is left to Codex's native `AGENTS.md` loading in the
/// current worktree.
pub(super) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Codex CLI needs no config files
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            attachments: _attachments,
            folder,
            mode,
            model,
            reasoning_level,
        } = request;
        let has_history_replay = mode
            .session_output()
            .is_some_and(|session_output| !session_output.trim().is_empty());
        let prompt = match mode {
            AgentCommandMode::Start { prompt } | AgentCommandMode::OneShot { prompt } => {
                prompt.to_string()
            }
            AgentCommandMode::Resume {
                prompt,
                session_output,
            } => build_resume_prompt(prompt, session_output)?,
        };
        let prompt = prepend_repo_root_path_instructions(&prompt)?;
        let prompt = prepend_protocol_instructions(&prompt)?;

        let mut command = Command::new("codex");
        command.arg("exec");

        if matches!(mode, AgentCommandMode::Resume { .. }) {
            command.arg("resume");
        }

        if matches!(mode, AgentCommandMode::Resume { .. }) && !has_history_replay {
            command.arg("--last");
        }

        command
            .arg("-c")
            .arg(model_reasoning_effort_config(reasoning_level))
            .arg("--model")
            .arg(model)
            .arg("--full-auto")
            .arg("--json")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

/// Renders the Codex CLI `model_reasoning_effort` override value.
fn model_reasoning_effort_config(reasoning_level: ReasoningLevel) -> String {
    format!(r#"model_reasoning_effort="{}""#, reasoning_level.codex())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    /// Verifies start commands include the shared protocol envelope and
    /// reasoning configuration passed to Codex.
    #[test]
    fn build_start_command_includes_protocol_and_reasoning_settings() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Run checks",
                },
                model: "gpt-5.3-codex",
                reasoning_level: ReasoningLevel::High,
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("-c"));
        assert!(debug_command.contains("model_reasoning_effort"));
        assert!(debug_command.contains("high"));
        assert!(debug_command.contains("Run checks"));
        assert!(debug_command.contains("Structured response protocol:"));
        assert!(debug_command.contains("Follow this JSON Schema exactly."));
        assert!(debug_command.contains("Authoritative JSON Schema:"));
    }

    /// Verifies resume command composes replay-based prompt content when
    /// session output is available.
    #[test]
    fn build_resume_command_includes_session_output_replay() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Resume {
                    prompt: "Continue edits",
                    session_output: Some("previous assistant output"),
                },
                model: "gpt-5.3-codex",
                reasoning_level: ReasoningLevel::High,
            },
        )
        .expect("resume command should build");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("-c"));
        assert!(debug_command.contains("model_reasoning_effort"));
        assert!(debug_command.contains("high"));
        assert!(debug_command.contains("Continue this session using the full transcript below."));
        assert!(debug_command.contains("previous assistant output"));
    }

    /// Verifies resume command keeps a plain user prompt when no session output
    /// is available for replay.
    #[test]
    fn build_resume_command_uses_plain_prompt_without_session_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Resume {
                    prompt: "Continue edits",
                    session_output: None,
                },
                model: "gpt-5.3-codex",
                reasoning_level: ReasoningLevel::High,
            },
        )
        .expect("resume command should build");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("exec"));
        assert!(debug_command.contains("resume"));
        assert!(debug_command.contains("--last"));
        assert!(debug_command.contains("-c"));
        assert!(debug_command.contains("model_reasoning_effort"));
        assert!(debug_command.contains("high"));
        assert!(debug_command.contains("Continue edits"));
        assert!(!debug_command.contains("Continue this session using the full transcript below."));
    }

    #[test]
    /// Verifies Codex prompts include repo-root-relative path guidance.
    fn build_command_includes_repo_root_path_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Run checks",
                },
                model: "gpt-5.3-codex",
                reasoning_level: ReasoningLevel::Low,
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("repository-root-relative POSIX paths"));
        assert!(debug_command.contains("Paths must be relative to the repository root."));
        assert!(debug_command.contains(r#"model_reasoning_effort=\"low\""#));
    }

    #[test]
    /// Verifies one-shot Codex prompts keep the shared schema-only protocol
    /// wrapper.
    fn build_one_shot_command_uses_schema_only_protocol_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::OneShot {
                    prompt: "Generate title",
                },
                model: "gpt-5.3-codex",
                reasoning_level: ReasoningLevel::Low,
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("Structured response protocol:"));
        assert!(
            !debug_command
                .contains("Emit the top-level `summary` field required by the JSON Schema.")
        );
    }
}
