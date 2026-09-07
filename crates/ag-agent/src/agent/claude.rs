use std::path::Path;
use std::process::{Command, Stdio};

use ag_protocol::{SchemaRequiredPolicy, protocol_output_schema};

use super::backend::{
    AgentBackend, AgentBackendError, BuildCommandRequest, MAX_CONCURRENT_SUBAGENTS,
};
use super::prompt::{CliPromptAccessRootMode, append_cli_prompt_access_directories};

/// Lists the Claude tools Agentty enables for unattended sessions.
///
/// `Bash` remains available for Claude workflows that need shell commands,
/// while `WebSearch` and `WebFetch` let Claude answer current-information
/// prompts without an interactive permission grant.
const CLAUDE_ALLOWED_TOOLS: &str =
    "Bash,Edit,MultiEdit,Write,WebSearch,WebFetch,EnterPlanMode,ExitPlanMode";
/// Lists the non-mutating Claude tools available to research sessions.
const CLAUDE_READ_ONLY_TOOLS: &str = "Read,Glob,Grep,WebSearch,WebFetch";

/// Backend implementation for the Claude CLI.
///
/// Commands are built with `--strict-mcp-config` so provider-level MCP
/// connector defaults (for example Claude.ai account connectors) are ignored
/// unless explicitly configured by Agentty. Claude runs in `stream-json` mode
/// so progress and tool-use events can surface live while the final turn still
/// honors native schema validation.
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
            attachments,
            folder,
            main_checkout_root,
            model,
            permission_mode,
            request_kind,
            prompt: _prompt,
            replay_transcript: _replay_transcript,
            reasoning_level,
            speed_mode,
            ..
        } = request;
        let mut command = Command::new("claude");

        if request_kind.is_resume() {
            command.arg("-c");
        }

        append_cli_prompt_access_directories(
            &mut command,
            folder,
            attachments,
            CliPromptAccessRootMode::AttachmentsOnly,
        );

        command.arg("-p");
        if permission_mode.is_read_only() {
            command
                .arg("--tools")
                .arg(CLAUDE_READ_ONLY_TOOLS)
                .arg("--allowedTools")
                .arg(CLAUDE_READ_ONLY_TOOLS)
                .arg("--permission-mode")
                .arg("plan");
        } else {
            command.arg("--allowedTools").arg(CLAUDE_ALLOWED_TOOLS);
        }
        append_claude_workspace_settings(
            &mut command,
            folder,
            main_checkout_root,
            permission_mode,
            speed_mode,
        );
        command.arg("--input-format").arg("text");
        command.arg("--strict-mcp-config");
        command.arg("--verbose");
        command.arg("--effort").arg(reasoning_level.claude());
        command.arg("--output-format").arg("stream-json");
        command.arg("--json-schema").arg(
            protocol_output_schema(
                request_kind.protocol_profile(),
                SchemaRequiredPolicy::MinimumProtocolKeys,
            )
            .to_string(),
        );
        command
            .env("ANTHROPIC_MODEL", model)
            .env(
                "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS",
                MAX_CONCURRENT_SUBAGENTS.to_string(),
            )
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

/// Appends per-turn Claude Code settings that keep known non-session
/// checkouts read-only.
fn append_claude_workspace_settings(
    command: &mut Command,
    workspace_folder: &Path,
    main_checkout_root: Option<&Path>,
    permission_mode: crate::model::permission::PermissionMode,
    speed_mode: crate::model::session::SpeedMode,
) {
    let mut deny_rules = Vec::new();
    let mut deny_write_paths = Vec::new();
    if let Some(main_checkout_root) = main_checkout_root {
        let main_checkout_rule_path = claude_absolute_permission_path(main_checkout_root);
        deny_rules.push(format!("Edit({main_checkout_rule_path}/**)"));
        deny_write_paths.push(main_checkout_root.to_string_lossy().into_owned());
    }

    let allow_write_paths = if permission_mode.is_read_only() {
        deny_rules.push(format!(
            "Edit({}/**)",
            claude_absolute_permission_path(workspace_folder)
        ));
        deny_write_paths.push(workspace_folder.to_string_lossy().into_owned());
        Vec::new()
    } else {
        vec![workspace_folder.to_string_lossy().into_owned()]
    };
    let mut sandbox = serde_json::json!({
        "enabled": true,
        "filesystem": {
            "allowWrite": allow_write_paths,
            "denyWrite": deny_write_paths,
        }
    });
    if permission_mode.is_read_only() {
        sandbox["network"] = serde_json::json!({
            "allowedDomains": [],
            "allowLocalBinding": false,
            "allowUnixSockets": []
        });
        sandbox["allowUnsandboxedCommands"] = serde_json::json!(false);
    }
    let settings = serde_json::json!({
        "fastMode": speed_mode.claude_fast_mode(),
        "permissions": {
            "deny": deny_rules,
        },
        "sandbox": sandbox
    });

    command.arg("--settings").arg(settings.to_string());
}

/// Returns a Claude Code permission-rule absolute path using the `//`
/// prefix required by `Read` and `Edit` path rules.
fn claude_absolute_permission_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let path_without_root = path.trim_start_matches('/');

    format!("//{path_without_root}")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use ag_protocol::{ProtocolSchemaInstructionMode, TurnPromptAttachment};
    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;
    use crate::agent::prompt as shared_prompt;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::ReasoningLevel;

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    fn utility_request_kind() -> AgentRequestKind {
        AgentRequestKind::UtilityPrompt
    }

    fn settings_argument(command: &Command) -> Value {
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let settings_position = args
            .iter()
            .position(|arg| arg == "--settings")
            .expect("--settings flag should be present");

        serde_json::from_str(&args[settings_position + 1]).expect("settings JSON should parse")
    }

    #[test]
    /// Verifies Claude permission-rule paths use slash separators for glob
    /// matching even when given a Windows-style checkout path.
    fn test_claude_absolute_permission_path_normalizes_windows_separators() {
        // Arrange
        let path = Path::new(r"C:\Users\dev\project");

        // Act
        let rule_path = claude_absolute_permission_path(path);

        // Assert
        assert_eq!(rule_path, "//C:/Users/dev/project");
        assert!(!rule_path.contains('\\'));
    }

    #[test]
    /// Verifies Claude sessions allow Agentty's required edit and web tools.
    fn test_claude_auto_edit_mode_uses_write_capable_allowed_tools() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let main_checkout_root = temp_directory.path().join("main");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: Some(main_checkout_root.as_path()),
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Plan prompt",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
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
        assert!(debug_command.contains("Bash"));
        assert!(debug_command.contains("MultiEdit"));
        assert!(debug_command.contains("Write"));
        assert!(debug_command.contains("WebSearch"));
        assert!(debug_command.contains("WebFetch"));
        assert!(debug_command.contains("--strict-mcp-config"));
        assert!(debug_command.contains("--settings"));
        assert!(debug_command.contains("--effort"));
        assert!(debug_command.contains("--output-format"));
        assert!(debug_command.contains("stream-json"));
        assert!(!debug_command.contains("--permission-mode"));
        assert!(!args.iter().any(String::is_empty));
        assert!(command.get_envs().any(|(key, value)| {
            key == "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS" && value == Some(OsStr::new("2"))
        }));

        let settings = settings_argument(&command);
        assert_eq!(
            settings.get("fastMode").and_then(Value::as_bool),
            Some(false)
        );
        let deny_rules = settings
            .pointer("/permissions/deny")
            .and_then(Value::as_array)
            .expect("deny rules should be present");
        assert!(deny_rules.iter().any(|rule| {
            rule.as_str()
                .is_some_and(|rule| rule.starts_with("Edit(//") && rule.ends_with("/main/**)"))
        }));
        assert_eq!(
            settings
                .pointer("/sandbox/enabled")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(settings.pointer("/sandbox/network").is_none());
    }

    #[test]
    /// Verifies Claude research turns expose only non-mutating tools and a
    /// read-only sandbox.
    fn test_claude_read_only_mode_uses_plan_tools_and_denies_writes() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let main_checkout_root = temp_directory.path().join("main");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: Some(main_checkout_root.as_path()),
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::ReadOnly,
                personality_prompt: None,
                prompt: "Inspect the architecture",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let flag_value = |flag: &str| {
            let position = args
                .iter()
                .position(|argument| argument == flag)
                .expect("flag should be present");

            args[position + 1].as_str()
        };
        let settings = settings_argument(&command);

        // Assert
        assert_eq!(flag_value("--tools"), CLAUDE_READ_ONLY_TOOLS);
        assert_eq!(flag_value("--allowedTools"), CLAUDE_READ_ONLY_TOOLS);
        assert_eq!(flag_value("--permission-mode"), "plan");
        assert!(!CLAUDE_READ_ONLY_TOOLS.contains("Bash"));
        assert!(!CLAUDE_READ_ONLY_TOOLS.contains("Write"));
        assert_eq!(
            settings.pointer("/sandbox/filesystem/allowWrite"),
            Some(&serde_json::json!([]))
        );
        assert_eq!(
            settings
                .pointer("/sandbox/network/allowLocalBinding")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            settings
                .pointer("/sandbox/allowUnsandboxedCommands")
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    /// Verifies Claude fast sessions enable the noninteractive `fastMode`
    /// setting.
    fn test_claude_fast_mode_sets_workspace_setting() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-opus-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Respond quickly",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::Fast,
            },
        )
        .expect("command should build");
        let settings = settings_argument(&command);

        // Assert
        assert_eq!(
            settings.get("fastMode").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    /// Verifies Claude commands pass the selected Opus 5 model through the
    /// Claude Code model environment variable.
    fn test_claude_command_sets_anthropic_model_to_claude_opus_5() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-opus-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Use Opus",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let anthropic_model = command
            .get_envs()
            .find(|(key, _value)| *key == OsStr::new("ANTHROPIC_MODEL"))
            .and_then(|(_key, value)| value)
            .map(|value| value.to_string_lossy().into_owned());

        // Assert
        assert_eq!(anthropic_model, Some("claude-opus-5".to_string()));
    }

    #[test]
    /// Verifies Claude commands pass the selected Opus 4.8 model through the
    /// Claude Code model environment variable.
    fn test_claude_command_sets_anthropic_model_to_claude_opus_48() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-opus-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Use Opus",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let anthropic_model = command
            .get_envs()
            .find(|(key, _value)| *key == OsStr::new("ANTHROPIC_MODEL"))
            .and_then(|(_key, value)| value)
            .map(|value| value.to_string_lossy().into_owned());

        // Assert
        assert_eq!(anthropic_model, Some("claude-opus-5".to_string()));
    }

    #[test]
    /// Verifies the `--effort` flag is passed to Claude with the correct value
    /// for each `ReasoningLevel`.
    fn test_claude_command_passes_effort_flag_for_each_reasoning_level() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;
        let cases = [
            (ReasoningLevel::Low, "low"),
            (ReasoningLevel::Medium, "medium"),
            (ReasoningLevel::High, "high"),
            (ReasoningLevel::XHigh, "max"),
            (ReasoningLevel::Max, "max"),
        ];

        for (reasoning_level, expected_effort) in cases {
            // Act
            let command = AgentBackend::build_command(
                &backend,
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    main_checkout_root: None,
                    replay_transcript: None,
                    model: "claude-sonnet-5",
                    permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                    personality_prompt: None,
                    prompt: "Do work",
                    reasoning_level,
                    request_kind: &session_start_request_kind(),
                    speed_mode: crate::model::session::SpeedMode::default(),
                },
            )
            .expect("command should build");
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();

            // Assert
            let effort_pos = args
                .iter()
                .position(|arg| arg == "--effort")
                .expect("--effort flag should be present");
            assert_eq!(
                args[effort_pos + 1],
                expected_effort,
                "expected effort={expected_effort} for {reasoning_level:?}"
            );
        }
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
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Inspect [Image #1] and [Image #2]",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
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
            shared_prompt::build_prompt_stdin_payload(
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    main_checkout_root: None,
                    replay_transcript: None,
                    model: "claude-sonnet-5",
                    permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                    personality_prompt: None,
                    prompt: "Plan prompt",
                    reasoning_level: ReasoningLevel::default(),
                    request_kind: &session_start_request_kind(),
                    speed_mode: crate::model::session::SpeedMode::default(),
                },
                ProtocolSchemaInstructionMode::TransportSchema,
                "Claude",
            )
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("repository-root-relative POSIX paths"));
        assert!(prompt.contains("`path:line:column`"));
        assert!(!prompt.contains("summary"));
    }

    #[test]
    /// Verifies one-shot Claude prompts keep protocol JSON guidance while
    /// native schema enforcement carries the full response schema.
    fn test_claude_one_shot_command_enforces_json_schema() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let backend = ClaudeBackend;

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Generate title",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &utility_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            shared_prompt::build_prompt_stdin_payload(
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    main_checkout_root: None,
                    replay_transcript: None,
                    model: "claude-sonnet-5",
                    permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                    personality_prompt: None,
                    prompt: "Generate title",
                    reasoning_level: ReasoningLevel::default(),
                    request_kind: &utility_request_kind(),
                    speed_mode: crate::model::session::SpeedMode::default(),
                },
                ProtocolSchemaInstructionMode::TransportSchema,
                "Claude",
            )
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(!prompt.contains("summary"));
        assert!(!prompt.contains("Authoritative JSON Schema:"));
        assert!(debug_command.contains("--output-format"));
        assert!(debug_command.contains("stream-json"));
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
                main_checkout_root: None,
                replay_transcript: None,
                model: "claude-sonnet-5",
                permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                personality_prompt: None,
                prompt: "Return protocol response",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
                speed_mode: crate::model::session::SpeedMode::default(),
            },
        )
        .expect("command should build");
        let debug_command = format!("{command:?}");
        let prompt = String::from_utf8(
            shared_prompt::build_prompt_stdin_payload(
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    main_checkout_root: None,
                    replay_transcript: None,
                    model: "claude-sonnet-5",
                    permission_mode: crate::model::permission::PermissionMode::AutoEdit,
                    personality_prompt: None,
                    prompt: "Return protocol response",
                    reasoning_level: ReasoningLevel::default(),
                    request_kind: &session_start_request_kind(),
                    speed_mode: crate::model::session::SpeedMode::default(),
                },
                ProtocolSchemaInstructionMode::TransportSchema,
                "Claude",
            )
            .expect("prompt payload should build"),
        )
        .expect("prompt payload should be utf-8");

        // Assert
        assert!(debug_command.contains("--json-schema"));
        assert!(debug_command.contains("AgentResponse"));
        assert!(prompt.contains("Structured response protocol:"));
        assert!(!prompt.contains("summary"));
        assert!(!prompt.contains("Authoritative JSON Schema:"));
    }
}
