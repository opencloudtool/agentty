use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use super::backend::{AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest};
use crate::infra::app_server::AppServerClient;
use crate::infra::codex_app_server::RealCodexAppServerClient;

/// Keeps Codex setup wired through [`AgentBackend`] while forbidding direct
/// CLI execution.
///
/// Codex session turns and one-shot utility prompts must always run through
/// `codex app-server`, so `build_command()` fails closed if a caller tries to
/// construct a direct subprocess invocation.
pub(super) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Codex CLI needs no config files
        Ok(())
    }

    fn transport(&self) -> AgentTransport {
        AgentTransport::AppServer
    }

    fn app_server_client(
        &self,
        default_client: Option<Arc<dyn AppServerClient>>,
    ) -> Option<Arc<dyn AppServerClient>> {
        Some(default_client.unwrap_or_else(|| {
            Arc::new(RealCodexAppServerClient::new()) as Arc<dyn AppServerClient>
        }))
    }

    fn build_command<'request>(
        &'request self,
        _request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        Err(AgentBackendError::CommandBuild(
            "Codex direct CLI execution is disabled; use the Codex app-server transport"
                .to_string(),
        ))
    }
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

    /// Verifies direct Codex command construction is rejected because Codex
    /// must run through app-server transport.
    #[test]
    fn build_command_returns_error_for_start_requests() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Run checks",
                request_kind: &session_start_request_kind(),
                model: "gpt-5.3-codex",
                reasoning_level: crate::domain::agent::ReasoningLevel::High,
            },
        )
        .expect_err("command build should fail");

        // Assert
        assert!(error.to_string().contains("app-server transport"));
    }

    /// Verifies direct Codex command construction is rejected for resume
    /// requests that include transcript replay.
    #[test]
    fn build_command_returns_error_for_resume_requests_with_session_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Continue edits",
                request_kind: &session_resume_request_kind(Some("previous assistant output")),
                model: "gpt-5.3-codex",
                reasoning_level: crate::domain::agent::ReasoningLevel::High,
            },
        )
        .expect_err("resume command build should fail");

        // Assert
        assert!(error.to_string().contains("app-server transport"));
    }

    /// Verifies direct Codex command construction is rejected for resume
    /// requests without stored transcript replay.
    #[test]
    fn build_command_returns_error_for_resume_requests_without_session_output() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Continue edits",
                request_kind: &session_resume_request_kind(None),
                model: "gpt-5.3-codex",
                reasoning_level: crate::domain::agent::ReasoningLevel::High,
            },
        )
        .expect_err("resume command build should fail");

        // Assert
        assert!(error.to_string().contains("app-server transport"));
    }

    #[test]
    /// Verifies direct Codex command construction is rejected for plain start
    /// requests regardless of reasoning level.
    fn build_command_returns_error_with_low_reasoning_level() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Run checks",
                request_kind: &session_start_request_kind(),
                model: "gpt-5.3-codex",
                reasoning_level: crate::domain::agent::ReasoningLevel::Low,
            },
        )
        .expect_err("command build should fail");

        // Assert
        assert!(error.to_string().contains("app-server transport"));
    }

    #[test]
    /// Verifies direct Codex command construction is rejected for one-shot
    /// utility prompts.
    fn build_command_returns_error_for_utility_prompts() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = CodexBackend;

        // Act
        let error = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Generate title",
                request_kind: &AgentRequestKind::UtilityPrompt,
                model: "gpt-5.3-codex",
                reasoning_level: crate::domain::agent::ReasoningLevel::Low,
            },
        )
        .expect_err("command build should fail");

        // Assert
        assert!(error.to_string().contains("app-server transport"));
    }
}
