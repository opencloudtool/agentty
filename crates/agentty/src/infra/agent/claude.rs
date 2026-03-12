use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, BuildCommandRequest,
    ProtocolInstructionMode, build_resume_prompt, prepend_protocol_instructions,
    prepend_repo_root_path_instructions,
};
use crate::infra::agent::protocol::agent_response_output_schema_json;

/// Lists the Claude tools Agentty enables for unattended sessions, including
/// file editing, multi-edit, and write operations.
const CLAUDE_ALLOWED_TOOLS: &str = "Edit,MultiEdit,Write,Bash,EnterPlanMode,ExitPlanMode";

/// Backend implementation for the Claude CLI.
///
/// Commands are built with `--strict-mcp-config` so provider-level MCP
/// connector defaults (for example Claude.ai account connectors) are ignored
/// unless explicitly configured by Agentty.
pub(super) struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn setup(&self, _folder: &Path) -> Result<(), AgentBackendError> {
        // Claude Code needs no config files
        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            reasoning_level: _reasoning_level,
            folder,
            mode,
            model,
        } = request;
        let mut command = Command::new("claude");

        if matches!(mode, AgentCommandMode::Resume { .. }) {
            command.arg("-c");
        }

        command.arg("-p");
        command.arg("--allowedTools").arg(CLAUDE_ALLOWED_TOOLS);
        command.arg("--input-format").arg("text");
        command.arg("--strict-mcp-config");
        command.arg("--verbose");
        command.arg("--output-format").arg("json");
        command
            .arg("--json-schema")
            .arg(agent_response_output_schema_json());
        command
            .env("ANTHROPIC_MODEL", model)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

/// Renders the full Claude prompt text that Agentty streams through stdin.
///
/// # Errors
/// Returns an error when resume or protocol prompt rendering fails.
pub(super) fn build_prompt_stdin_payload(
    request: BuildCommandRequest<'_>,
) -> Result<Vec<u8>, AgentBackendError> {
    let prompt = match request.mode {
        AgentCommandMode::Start { prompt } | AgentCommandMode::OneShot { prompt } => {
            prompt.to_string()
        }
        AgentCommandMode::Resume {
            prompt,
            session_output,
        } => build_resume_prompt(prompt, session_output)?,
    };
    let prompt = prepend_repo_root_path_instructions(&prompt)?;
    let prompt = prepend_protocol_instructions(
        &prompt,
        ProtocolInstructionMode::WithoutSchema,
        request.mode.protocol_prompt_kind(),
    )?;

    Ok(prompt.into_bytes())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::ReasoningLevel;

    #[test]
    /// Verifies Claude sessions allow Agentty's required write-capable tools.
    fn test_claude_auto_edit_mode_uses_write_capable_allowed_tools() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                reasoning_level: ReasoningLevel::default(),
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Plan prompt",
                },
                model: "claude-sonnet-4-6",
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert!(debug_command.contains("--allowedTools"));
        assert!(debug_command.contains(CLAUDE_ALLOWED_TOOLS));
        assert!(debug_command.contains("MultiEdit"));
        assert!(debug_command.contains("Write"));
        assert!(debug_command.contains("--strict-mcp-config"));
        assert!(debug_command.contains("--output-format"));
        assert!(debug_command.contains("json"));
        assert!(!debug_command.contains("--permission-mode"));
        assert!(!args.iter().any(String::is_empty));
    }

    #[test]
    /// Verifies Claude prompts include repo-root-relative path guidance.
    fn test_claude_prompt_stdin_payload_includes_repo_root_path_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");

        // Act
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                reasoning_level: ReasoningLevel::default(),
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Plan prompt",
                },
                model: "claude-sonnet-4-6",
            })
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("repository-root-relative POSIX paths"));
        assert!(prompt.contains("Paths must be relative to the repository root."));
    }

    #[test]
    /// Verifies one-shot Claude prompts keep protocol JSON and skip session
    /// change-summary requirements.
    fn test_claude_one_shot_command_enforces_json_schema_without_change_summary() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                reasoning_level: ReasoningLevel::default(),
                folder: temp_directory.path(),
                mode: AgentCommandMode::OneShot {
                    prompt: "Generate title",
                },
                model: "claude-sonnet-4-6",
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                reasoning_level: ReasoningLevel::default(),
                folder: temp_directory.path(),
                mode: AgentCommandMode::OneShot {
                    prompt: "Generate title",
                },
                model: "claude-sonnet-4-6",
            })
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(!prompt.contains("## Change Summary"));
        assert!(debug_command.contains("--output-format"));
        assert!(debug_command.contains("json"));
        assert!(debug_command.contains("--json-schema"));
        assert!(debug_command.contains("--input-format"));
    }

    #[test]
    /// Verifies structured Claude commands include native JSON schema
    /// validation.
    fn test_claude_start_command_includes_json_schema() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                reasoning_level: ReasoningLevel::default(),
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Return protocol response",
                },
                model: "claude-sonnet-4-6",
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                reasoning_level: ReasoningLevel::default(),
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Return protocol response",
                },
                model: "claude-sonnet-4-6",
            })
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(debug_command.contains("--json-schema"));
        assert!(debug_command.contains("AgentResponse"));
        assert!(prompt.contains("Structured response protocol:"));
        assert!(prompt.contains("## Change Summary"));
    }
}
