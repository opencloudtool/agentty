use std::path::Path;
use std::process::Command;

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};

/// Keeps Codex setup wired through [`AgentBackend`] while always routing turns
/// through the Codex app-server runtime.
///
/// Codex session turns and one-shot utility prompts run on top of
/// `codex app-server`, so `build_command()` constructs the long-lived runtime
/// process command instead of a one-shot CLI prompt invocation.
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
        Ok(build_app_server_command(request))
    }
}

/// Builds the persistent `codex app-server` runtime command for one session.
///
/// The prompt payload is sent later over JSON-RPC, so prompt text, request
/// kind, attachments, and reasoning level do not change the spawned process.
fn build_app_server_command(request: BuildCommandRequest<'_>) -> Command {
    let BuildCommandRequest {
        attachments: _attachments,
        folder,
        prompt: _prompt,
        request_kind: _request_kind,
        model,
        reasoning_level: _reasoning_level,
    } = request;
    let mut command = Command::new("codex");
    command
        .arg("--model")
        .arg(model)
        .arg("app-server")
        .arg("--listen")
        .arg("stdio://")
        .current_dir(folder);

    command
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::infra::channel::AgentRequestKind;

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn session_resume_request_kind(session_output: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume {
            session_output: session_output.map(ToString::to_string),
        }
    }

    /// Verifies Codex start requests build the persistent app-server command.
    #[test]
    fn build_command_builds_app_server_runtime_for_start_requests() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Run checks",
                request_kind: &session_start_request_kind(),
                model: "gpt-5.4",
                reasoning_level: crate::domain::agent::ReasoningLevel::High,
            },
        )
        .expect("command build should succeed");
        let debug_command = format!("{command:?}");

        // Assert
        assert!(debug_command.contains("codex"));
        assert!(debug_command.contains("app-server"));
        assert!(debug_command.contains("stdio://"));
    }

    /// Verifies resume requests reuse the same Codex runtime launch command.
    #[test]
    fn build_command_builds_app_server_runtime_for_resume_requests() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Continue edits",
                request_kind: &session_resume_request_kind(Some("previous assistant output")),
                model: "gpt-5.4",
                reasoning_level: crate::domain::agent::ReasoningLevel::High,
            },
        )
        .expect("resume command build should succeed");
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            arguments,
            vec!["--model", "gpt-5.4", "app-server", "--listen", "stdio://"]
        );
    }

    /// Verifies utility prompts use the same app-server runtime launch path.
    #[test]
    fn build_command_builds_app_server_runtime_for_utility_prompts() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Generate title",
                request_kind: &AgentRequestKind::UtilityPrompt,
                model: "gpt-5.4",
                reasoning_level: crate::domain::agent::ReasoningLevel::Low,
            },
        )
        .expect("utility command build should succeed");

        // Assert
        assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
    }
}
