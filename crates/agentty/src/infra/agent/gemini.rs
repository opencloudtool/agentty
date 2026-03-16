use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use super::backend::{AgentBackend, AgentBackendError, AgentTransport, BuildCommandRequest};
use super::prompt::{PromptPreparationRequest, prepare_prompt_text};
use crate::infra::app_server::AppServerClient;
use crate::infra::gemini_acp::RealGeminiAcpClient;

/// Backend implementation for the Gemini CLI.
pub(super) struct GeminiBackend;

impl AgentBackend for GeminiBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Gemini CLI needs no config files
        Ok(())
    }

    fn transport(&self) -> AgentTransport {
        AgentTransport::AppServer
    }

    fn app_server_client(
        &self,
        default_client: Option<Arc<dyn AppServerClient>>,
    ) -> Option<Arc<dyn AppServerClient>> {
        Some(
            default_client.unwrap_or_else(|| {
                Arc::new(RealGeminiAcpClient::new()) as Arc<dyn AppServerClient>
            }),
        )
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            attachments: _attachments,
            folder,
            prompt: _prompt,
            request_kind,
            model,
            reasoning_level: _reasoning_level,
        } = request;
        let has_history_replay = request_kind
            .session_output()
            .is_some_and(|session_output| !session_output.trim().is_empty());
        let mut command = Command::new("gemini");
        command
            .arg("--model")
            .arg(model)
            .arg("--approval-mode")
            .arg("auto_edit")
            .arg("--output-format")
            .arg("stream-json")
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if request_kind.is_resume() && !has_history_replay {
            command.arg("--resume").arg("latest");
        }

        Ok(command)
    }
}

/// Renders the full Gemini prompt text that Agentty streams through stdin.
///
/// # Errors
/// Returns an error when resume or protocol prompt rendering fails.
pub(super) fn build_prompt_stdin_payload(
    request: BuildCommandRequest<'_>,
) -> Result<Vec<u8>, AgentBackendError> {
    let prompt = prepare_prompt_text(PromptPreparationRequest {
        prompt: request.prompt,
        protocol_profile: request.request_kind.protocol_profile(),
        replay_session_output: request.request_kind.session_output(),
        should_replay_session_output: request.request_kind.is_resume(),
    })?;

    Ok(prompt.into_bytes())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::infra::channel::AgentRequestKind;

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn utility_request_kind() -> AgentRequestKind {
        AgentRequestKind::UtilityPrompt
    }

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

    #[test]
    /// Verifies Gemini prompts include repo-root-relative path guidance.
    fn test_gemini_prompt_stdin_payload_includes_repo_root_path_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");

        // Act
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Plan prompt",
                request_kind: &session_start_request_kind(),
                model: "gemini-3-flash-preview",
                reasoning_level: ReasoningLevel::default(),
            })
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("repository-root-relative POSIX paths"));
        assert!(prompt.contains("Paths must be relative to the repository root."));
        assert!(prompt.contains("summary"));
    }

    #[test]
    /// Verifies one-shot Gemini prompts keep the shared schema-only protocol
    /// wrapper.
    fn test_gemini_one_shot_command_uses_schema_only_protocol_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = GeminiBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Generate title",
                request_kind: &utility_request_kind(),
                model: "gemini-3-flash-preview",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Generate title",
                request_kind: &utility_request_kind(),
                model: "gemini-3-flash-preview",
                reasoning_level: ReasoningLevel::default(),
            })
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(prompt.contains("summary"));
        assert!(debug_command.contains("--output-format"));
        assert!(!args.iter().any(String::is_empty));
    }
}
