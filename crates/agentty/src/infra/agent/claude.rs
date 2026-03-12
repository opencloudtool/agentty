use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{
    AgentBackend, AgentBackendError, AgentCommandMode, BuildCommandRequest,
    ProtocolInstructionMode, build_resume_prompt, prepend_protocol_instructions,
    prepend_repo_root_path_instructions,
};
use crate::infra::agent::protocol::agent_response_output_schema_json;
use crate::infra::channel::{
    TurnPromptAttachment, TurnPromptContentPart, split_turn_prompt_content,
};

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
            attachments: _attachments,
            folder,
            mode,
            model,
            reasoning_level: _reasoning_level,
        } = request;
        let mut command = Command::new("claude");

        if matches!(mode, AgentCommandMode::Resume { .. }) {
            command.arg("-c");
        }

        append_attachment_access_directories(&mut command, request.attachments);

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
            render_prompt_with_local_images(prompt, request.attachments)?
        }
        AgentCommandMode::Resume {
            prompt,
            session_output,
        } => build_resume_prompt(
            &render_prompt_with_local_images(prompt, request.attachments)?,
            session_output,
        )?,
    };
    let prompt = prepend_repo_root_path_instructions(&prompt)?;
    let prompt = prepend_protocol_instructions(
        &prompt,
        ProtocolInstructionMode::WithoutSchema,
        request.mode.protocol_prompt_kind(),
    )?;

    Ok(prompt.into_bytes())
}

/// Adds Claude file-access roots for prompt attachments that live outside the
/// current worktree.
///
/// Claude Code restricts filesystem access to the current working directory
/// unless extra roots are granted explicitly. Pasted prompt images are stored
/// under the Agentty temp directory, so their parent directories must be added
/// with `--add-dir` for Claude to inspect them.
fn append_attachment_access_directories(
    command: &mut Command,
    attachments: &[TurnPromptAttachment],
) {
    let mut attachment_directories = attachments
        .iter()
        .filter_map(|attachment| attachment.local_image_path.parent())
        .map(std::path::Path::to_path_buf)
        .collect::<Vec<_>>();

    attachment_directories.sort();
    attachment_directories.dedup();

    for attachment_directory in attachment_directories {
        command.arg("--add-dir").arg(attachment_directory);
    }
}

/// Replaces inline prompt-image placeholders with Claude-readable local image
/// paths while preserving attachment order.
///
/// Claude Code accepts image paths embedded directly in the prompt text, so
/// Agentty rewrites `[Image #n]` placeholders to the persisted local file
/// paths before streaming the prompt over stdin.
///
/// # Errors
/// Returns an error when any attachment path is not valid UTF-8, because the
/// prompt protocol can only carry UTF-8 text and lossy conversion could point
/// Claude at the wrong file.
fn render_prompt_with_local_images(
    prompt: &str,
    attachments: &[TurnPromptAttachment],
) -> Result<String, AgentBackendError> {
    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let mut rendered_prompt = String::new();

    for content_part in split_turn_prompt_content(prompt, attachments) {
        match content_part {
            TurnPromptContentPart::Text(text) => rendered_prompt.push_str(text),
            TurnPromptContentPart::Attachment(attachment) => {
                let attachment_path = attachment_path_for_prompt(attachment)?;
                rendered_prompt.push_str(&attachment_path);
            }
            TurnPromptContentPart::OrphanAttachment(attachment) => {
                if !rendered_prompt.is_empty()
                    && rendered_prompt
                        .chars()
                        .last()
                        .is_some_and(|character| !character.is_whitespace())
                {
                    rendered_prompt.push('\n');
                }

                rendered_prompt.push_str(&attachment_path_for_prompt(attachment)?);
                rendered_prompt.push('\n');
            }
        }
    }

    Ok(rendered_prompt)
}

/// Returns one prompt attachment path as strict UTF-8 for Claude stdin
/// rendering.
///
/// Claude receives attachment paths through the UTF-8 prompt body, so invalid
/// UTF-8 paths must fail fast instead of being silently rewritten with lossy
/// replacement characters.
fn attachment_path_for_prompt(
    attachment: &TurnPromptAttachment,
) -> Result<String, AgentBackendError> {
    attachment
        .local_image_path
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AgentBackendError::CommandBuild(
                "Claude prompt image path is not valid UTF-8".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Plan prompt",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
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
    /// Verifies Claude turns grant filesystem access to pasted-image parent
    /// directories.
    fn test_claude_command_adds_attachment_access_directories() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/agentty/images/one.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/agentty/images/two.png"),
            },
        ];

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &attachments,
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Inspect [Image #1] and [Image #2]",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "--add-dir")
                .count(),
            1
        );
        assert!(args.contains(&"/tmp/agentty/images".to_string()));
    }

    #[test]
    /// Verifies Claude prompts include repo-root-relative path guidance.
    fn test_claude_prompt_stdin_payload_includes_repo_root_path_instructions() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");

        // Act
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Plan prompt",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
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
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::OneShot {
                    prompt: "Generate title",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::OneShot {
                    prompt: "Generate title",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
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
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Return protocol response",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            build_prompt_stdin_payload(BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                mode: AgentCommandMode::Start {
                    prompt: "Return protocol response",
                },
                model: "claude-sonnet-4-6",
                reasoning_level: ReasoningLevel::default(),
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

    #[test]
    /// Verifies Claude prompt rendering replaces image placeholders with local
    /// file paths in placeholder order.
    fn test_render_prompt_with_local_images_replaces_placeholders_in_order() {
        // Arrange
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/first-image.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/second-image.png"),
            },
        ];

        // Act
        let rendered_prompt =
            render_prompt_with_local_images("Compare [Image #2] with [Image #1]", &attachments)
                .expect("prompt rendering should succeed");

        // Assert
        assert_eq!(
            rendered_prompt,
            "Compare /tmp/second-image.png with /tmp/first-image.png"
        );
    }

    #[test]
    /// Verifies Claude prompt rendering appends local image paths when
    /// attachment metadata survives without a placeholder match.
    fn test_render_prompt_with_local_images_appends_missing_paths() {
        // Arrange
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/first-image.png"),
        }];

        // Act
        let rendered_prompt = render_prompt_with_local_images("Review this change", &attachments)
            .expect("prompt rendering should succeed");

        // Assert
        assert_eq!(
            rendered_prompt,
            "Review this change\n/tmp/first-image.png\n"
        );
    }

    #[cfg(unix)]
    #[test]
    /// Verifies Claude prompt rendering fails fast when an attachment path is
    /// not valid UTF-8.
    fn test_render_prompt_with_local_images_rejects_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // Arrange
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from(OsString::from_vec(vec![0x66, 0x80, 0x6f])),
        }];

        // Act
        let error = render_prompt_with_local_images("Review [Image #1]", &attachments)
            .expect_err("prompt rendering should fail");

        // Assert
        assert_eq!(
            error,
            AgentBackendError::CommandBuild(
                "Claude prompt image path is not valid UTF-8".to_string()
            )
        );
    }
}
