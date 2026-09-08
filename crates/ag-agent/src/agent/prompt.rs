//! Shared prompt-shaping helpers for agent-facing markdown prompts.

use std::path::{Path, PathBuf};
use std::process::Command;

use ag_protocol::{
    ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt, TurnPromptAttachment,
    TurnPromptContentPart, prepend_protocol_instructions as protocol_prepend_instructions,
    prepend_protocol_refresh_reminder as protocol_prepend_refresh_reminder,
    split_turn_prompt_content,
};
use askama::Template;

use super::backend::{AgentBackendError, BuildCommandRequest};
use super::instruction::InstructionDeliveryMode;
use crate::channel::PersonalityPromptUpdate;
use crate::model::session::ResponseStyle;

/// Askama view model for rendering resume prompts with prior transcript text.
#[derive(Template)]
#[template(path = "resume_with_transcript_prompt.md", escape = "none")]
struct ResumeWithTranscriptPromptTemplate<'a> {
    /// New prompt content appended after the replayed transcript.
    prompt: &'a str,
    /// Prior transcript text replayed into the follow-up prompt.
    transcript: &'a str,
}

/// Askama view model for placing personality instructions before a turn.
#[derive(Template)]
#[template(path = "personality_prompt.md", escape = "none")]
struct PersonalityPromptTemplate<'a> {
    /// Markdown heading describing a bootstrap or delta update.
    heading: &'a str,
    /// Personality instructions or clearing guidance.
    personality: &'a str,
    /// Remaining turn prompt content.
    prompt: &'a str,
}

/// Askama view model for placing response-style guidance before a turn.
#[derive(Template)]
#[template(path = "response_style_prompt.md", escape = "none")]
struct ResponseStylePromptTemplate<'a> {
    /// Guidance corresponding to the selected response style.
    instruction: &'a str,
    /// Remaining turn prompt content.
    prompt: &'a str,
}

/// Shared prompt preparation input for one transport turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptPreparationRequest<'a> {
    /// Delivery mode selected for the current provider attempt.
    pub instruction_delivery_mode: InstructionDeliveryMode,
    /// Current personality body used for full instruction bootstraps.
    pub personality_prompt: Option<&'a str>,
    /// Personality change used only for delta delivery.
    pub personality_update: &'a PersonalityPromptUpdate,
    /// Base user prompt before replay wrapping and protocol instructions.
    pub prompt: &'a str,
    /// Protocol family that determines the rendered instruction envelope.
    pub protocol_profile: ProtocolRequestProfile,
    /// Prior transcript text available for replay.
    pub replay_transcript: Option<&'a str>,
    /// Schema guidance mode selected from the provider's structured-output
    /// capability.
    pub schema_instruction_mode: ProtocolSchemaInstructionMode,
    /// Workspace folder rendered into the isolation contract as the only
    /// writable root for the turn.
    pub workspace_root: &'a Path,
}

/// Controls which directories CLI prompt transports expose as filesystem access
/// roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliPromptAccessRootMode {
    /// Expose only attachment parent directories.
    AttachmentsOnly,
    /// Expose the workspace folder first, then attachment parent directories.
    WorkspaceThenAttachments,
}

/// Applies transcript replay and protocol instructions to one prompt.
///
/// # Errors
/// Returns an error when replay or instruction templates fail to render.
pub(crate) fn prepare_prompt_text(
    request: PromptPreparationRequest<'_>,
) -> Result<String, AgentBackendError> {
    match request.instruction_delivery_mode {
        InstructionDeliveryMode::BootstrapFull => {
            let prompt = prepend_personality_prompt(request.prompt, request.personality_prompt)?;

            Ok(protocol_prepend_instructions(
                &prompt,
                request.protocol_profile,
                request.schema_instruction_mode,
                request.workspace_root,
            ))
        }
        InstructionDeliveryMode::DeltaOnly => {
            let prompt = prepend_personality_update(request.prompt, request.personality_update)?;

            Ok(protocol_prepend_refresh_reminder(
                &prompt,
                request.protocol_profile,
                request.workspace_root,
            ))
        }
        InstructionDeliveryMode::BootstrapWithReplay => {
            let prompt = build_resume_prompt(request.prompt, request.replay_transcript)?;
            let prompt = prepend_personality_prompt(&prompt, request.personality_prompt)?;

            Ok(protocol_prepend_instructions(
                &prompt,
                request.protocol_profile,
                request.schema_instruction_mode,
                request.workspace_root,
            ))
        }
    }
}

/// Builds a resume prompt that optionally prepends previous transcript text.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn build_resume_prompt(
    prompt: &str,
    replay_transcript: Option<&str>,
) -> Result<String, AgentBackendError> {
    let Some(transcript) = replay_transcript
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(prompt.to_string());
    };

    let template = ResumeWithTranscriptPromptTemplate { prompt, transcript };

    render_template("resume_with_transcript_prompt.md", &template)
}

/// Builds the full prompt text for a CLI provider.
///
/// This shared helper keeps attachment placeholder rendering and provider
/// protocol preparation in one place for both argv and stdin transports while
/// preserving backend-specific error labels.
///
/// # Errors
/// Returns an error when attachment path rendering, resume wrapping, or
/// protocol prompt rendering fails.
pub(crate) fn build_cli_prompt_text(
    request: BuildCommandRequest<'_>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    backend_display_name: &str,
) -> Result<String, AgentBackendError> {
    let prompt =
        render_prompt_with_local_images(request.prompt, request.attachments, backend_display_name)?;

    prepare_prompt_text(PromptPreparationRequest {
        instruction_delivery_mode: if request.request_kind.is_resume() {
            InstructionDeliveryMode::BootstrapWithReplay
        } else {
            InstructionDeliveryMode::BootstrapFull
        },
        personality_prompt: request.personality_prompt,
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: &prompt,
        protocol_profile: request.request_kind.protocol_profile(),
        replay_transcript: request.replay_transcript,
        schema_instruction_mode,
        workspace_root: request.folder,
    })
}

/// Prepends response-style guidance to interactive session turns.
pub(crate) fn apply_response_style_prompt(
    mut prompt: TurnPrompt,
    protocol_profile: ProtocolRequestProfile,
    response_style: ResponseStyle,
) -> Result<TurnPrompt, AgentBackendError> {
    if protocol_profile == ProtocolRequestProfile::SessionTurn {
        let prompt_text = prompt.agent_text();
        let template = ResponseStylePromptTemplate {
            instruction: response_style.prompt_instruction(),
            prompt: &prompt_text,
        };
        prompt.text = render_template("response_style_prompt.md", &template)?;
    }

    Ok(prompt)
}

/// Builds a full prompt payload to stream over stdin for CLI providers.
///
/// # Errors
/// Returns an error when the shared CLI prompt text cannot be rendered.
pub(crate) fn build_prompt_stdin_payload(
    request: BuildCommandRequest<'_>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    backend_display_name: &str,
) -> Result<Vec<u8>, AgentBackendError> {
    build_cli_prompt_text(request, schema_instruction_mode, backend_display_name)
        .map(String::into_bytes)
}

/// Prepends current personality instructions to one full bootstrap prompt.
fn prepend_personality_prompt(
    prompt: &str,
    personality_prompt: Option<&str>,
) -> Result<String, AgentBackendError> {
    let Some(personality) = personality_prompt
        .map(str::trim)
        .filter(|personality| !personality.is_empty())
    else {
        return Ok(prompt.to_string());
    };
    let template = PersonalityPromptTemplate {
        heading: "# Personality",
        personality,
        prompt,
    };

    render_template("personality_prompt.md", &template)
}

/// Prepends one changed or cleared personality instruction for delta mode.
fn prepend_personality_update(
    prompt: &str,
    personality_update: &PersonalityPromptUpdate,
) -> Result<String, AgentBackendError> {
    let personality = match personality_update {
        PersonalityPromptUpdate::Clear => {
            "The session personality has been cleared. Continue without the previous personality \
             instructions."
        }
        PersonalityPromptUpdate::Set(personality) => personality.trim(),
        PersonalityPromptUpdate::Unchanged => return Ok(prompt.to_string()),
    };
    let template = PersonalityPromptTemplate {
        heading: "# Personality Update",
        personality,
        prompt,
    };

    render_template("personality_prompt.md", &template)
}

/// Appends CLI prompt filesystem access roots as `--add-dir` arguments.
///
/// Claude only needs pasted-image parent directories because its process
/// working directory is already the session workspace. Antigravity derives its
/// editable workspace from ordered `--add-dir` roots, so it uses
/// [`CliPromptAccessRootMode::WorkspaceThenAttachments`] to keep the workspace
/// root first.
pub(crate) fn append_cli_prompt_access_directories(
    command: &mut Command,
    workspace_folder: &Path,
    attachments: &[TurnPromptAttachment],
    root_mode: CliPromptAccessRootMode,
) {
    for directory in cli_prompt_access_directories(workspace_folder, attachments, root_mode) {
        command.arg("--add-dir").arg(directory);
    }
}

/// Replaces inline image placeholders with provider-usable local image paths.
///
/// The function preserves attachment ordering through prompt content parsing
/// and appends any orphaned attachments that no longer have a placeholder in
/// the prompt text.
///
/// # Errors
/// Returns an error when any local image path is not valid UTF-8.
pub(crate) fn render_prompt_with_local_images(
    prompt: &str,
    attachments: &[TurnPromptAttachment],
    backend_display_name: &str,
) -> Result<String, AgentBackendError> {
    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let mut rendered_prompt = String::new();

    for content_part in split_turn_prompt_content(prompt, attachments) {
        match content_part {
            TurnPromptContentPart::Text(text) => rendered_prompt.push_str(text),
            TurnPromptContentPart::Attachment(attachment) => {
                let attachment_path = attachment_path_for_prompt(backend_display_name, attachment)?;
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

                rendered_prompt.push_str(&attachment_path_for_prompt(
                    backend_display_name,
                    attachment,
                )?);
                rendered_prompt.push('\n');
            }
        }
    }

    Ok(rendered_prompt)
}

/// Returns ordered filesystem access roots for CLI prompt image access.
///
/// Directory paths are deduplicated and sorted for deterministic subprocess
/// argument ordering. When `root_mode` requests the workspace, the session
/// folder appears before attachment directories and is never duplicated.
pub(crate) fn cli_prompt_access_directories(
    workspace_folder: &Path,
    attachments: &[TurnPromptAttachment],
    root_mode: CliPromptAccessRootMode,
) -> Vec<PathBuf> {
    let mut attachment_directories = attachments
        .iter()
        .filter_map(|attachment| attachment.local_image_path.parent())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    attachment_directories.sort();
    attachment_directories.dedup();

    if matches!(root_mode, CliPromptAccessRootMode::AttachmentsOnly) {
        return attachment_directories;
    }

    attachment_directories
        .retain(|attachment_directory| attachment_directory.as_path() != workspace_folder);

    let mut workspace_directories = Vec::with_capacity(attachment_directories.len() + 1);
    workspace_directories.push(workspace_folder.to_path_buf());
    workspace_directories.extend(attachment_directories);

    workspace_directories
}

/// Returns one attachment path for prompt injection as strict UTF-8 text.
///
/// # Errors
/// Returns an error when the attachment path cannot be represented as UTF-8.
fn attachment_path_for_prompt(
    backend_display_name: &str,
    attachment: &TurnPromptAttachment,
) -> Result<String, AgentBackendError> {
    attachment
        .local_image_path
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AgentBackendError::CommandBuild(format!(
                "{backend_display_name} prompt image path is not valid UTF-8"
            ))
        })
}

/// Builds a Markdown code-fence delimiter long enough to safely wrap an
/// arbitrary prompt payload.
///
/// Returns a string of backticks whose length exceeds the longest run of
/// consecutive backticks found anywhere in `content`, with a minimum length
/// of three. This prevents a triple-backtick fence from being terminated
/// prematurely when the payload itself contains Markdown fences (for example,
/// when reviewing changes to Markdown or prompt-template files).
pub fn diff_fence(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current_run = 0usize;
    for character in content.chars() {
        if character == '`' {
            current_run += 1;
            if current_run > max_run {
                max_run = current_run;
            }
        } else {
            current_run = 0;
        }
    }

    let fence_length = std::cmp::max(3, max_run + 1);

    "`".repeat(fence_length)
}

/// Renders one Askama markdown template and trims the trailing newline added
/// by file-based templates.
fn render_template(
    template_name: &str,
    template: &impl Template,
) -> Result<String, AgentBackendError> {
    let rendered = template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!("Failed to render `{template_name}`: {error}"))
    })?;

    Ok(rendered.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn repair_bootstrap_applies_schema_once_for_each_provider_and_profile() {
        // Arrange
        let repair = ag_protocol::build_protocol_repair_prompt("bad JSON", "original response")
            .expect("repair body");
        for kind in [
            crate::model::agent::AgentKind::Gemini,
            crate::model::agent::AgentKind::Codex,
            crate::model::agent::AgentKind::Claude,
            crate::model::agent::AgentKind::Antigravity,
        ] {
            for profile in [
                ProtocolRequestProfile::SessionTurn,
                ProtocolRequestProfile::UtilityPrompt,
                ProtocolRequestProfile::FocusedReview,
            ] {
                let schema_mode = super::super::protocol_schema_instruction_mode(kind);

                // Act
                let prompt = prepare_prompt_text(PromptPreparationRequest {
                    instruction_delivery_mode: InstructionDeliveryMode::BootstrapFull,
                    personality_prompt: None,
                    personality_update: &PersonalityPromptUpdate::Unchanged,
                    prompt: &repair,
                    protocol_profile: profile,
                    replay_transcript: None,
                    schema_instruction_mode: schema_mode,
                    workspace_root: test_workspace_root(),
                })
                .expect("prepared repair");

                // Assert
                assert_eq!(
                    prompt.matches("Authoritative JSON Schema:").count(),
                    usize::from(schema_mode == ProtocolSchemaInstructionMode::PromptSchema)
                );
                assert!(prompt.starts_with("File path output requirements:"));
                assert!(prompt.ends_with(&repair));
            }
        }
    }

    /// Returns the workspace root used by prompt preparation tests.
    fn test_workspace_root() -> &'static Path {
        Path::new("/tmp/agentty-wt/session-1")
    }

    /// Collapses rendered prompt whitespace for semantic assertions.
    fn normalize_prompt(prompt: &str) -> String {
        prompt.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn response_style_prompt_wraps_session_turns_and_preserves_attachments() {
        // Arrange
        let attachment = TurnPromptAttachment {
            local_image_path: PathBuf::from("/tmp/example.png"),
            placeholder: "[Image #1]".to_string(),
        };

        // Act
        let prompts = ResponseStyle::ALL.map(|response_style| {
            apply_response_style_prompt(
                TurnPrompt {
                    attachments: vec![attachment.clone()],
                    text: "Explain [Image #1]".to_string(),
                    text_source: ag_protocol::TurnPromptTextSource::UserPrompt,
                },
                ProtocolRequestProfile::SessionTurn,
                response_style,
            )
            .expect("response style prompt should render")
        });

        // Assert
        for (prompt, response_style) in prompts.iter().zip(ResponseStyle::ALL) {
            assert!(prompt.text.starts_with("# Response Style\n\n"));
            assert!(prompt.text.contains(response_style.prompt_instruction()));
            assert!(prompt.text.ends_with("Explain [Image #1]"));
            assert_eq!(prompt.attachments, vec![attachment.clone()]);
            assert_eq!(
                prompt.text_source,
                ag_protocol::TurnPromptTextSource::UserPrompt
            );
        }
    }

    #[test]
    fn response_style_prompt_leaves_utility_prompts_unchanged() {
        // Arrange
        let prompt = TurnPrompt::from_agent_data("Generate a title".to_string());

        // Act
        let styled_prompt = apply_response_style_prompt(
            prompt.clone(),
            ProtocolRequestProfile::UtilityPrompt,
            ResponseStyle::Detailed,
        )
        .expect("utility prompt should remain valid");

        // Assert
        assert_eq!(styled_prompt, prompt);
    }

    #[test]
    /// Ensures the diff fence falls back to three backticks when the content
    /// contains no backtick runs.
    fn test_diff_fence_returns_minimum_three_backticks_for_plain_diff() {
        // Arrange
        let diff = "diff --git a/a.rs b/a.rs\n+fn main() {}\n";

        // Act
        let fence = diff_fence(diff);

        // Assert
        assert_eq!(fence, "```");
    }

    #[test]
    /// Ensures the diff fence grows to exceed the longest backtick run in the
    /// diff so a Markdown triple-backtick fence inside the diff cannot
    /// terminate the outer wrapper fence.
    fn test_diff_fence_exceeds_longest_backtick_run_in_diff() {
        // Arrange
        let diff = "+```\nsample\n+```\n";

        // Act
        let fence = diff_fence(diff);

        // Assert
        assert_eq!(fence, "````");
    }

    #[test]
    /// Ensures longer backtick runs keep producing a strictly longer fence so
    /// nested or unusually long code fences in the diff stay contained.
    fn test_diff_fence_handles_long_backtick_runs() {
        // Arrange
        let diff = "prefix `````diff\ncontent\n`````\n";

        // Act
        let fence = diff_fence(diff);

        // Assert
        assert_eq!(fence, "``````");
    }

    #[test]
    /// Ensures resume prompt rendering includes trimmed transcript text and
    /// the new user prompt.
    fn test_build_resume_prompt_includes_replay_transcript_and_prompt() {
        // Arrange
        let prompt = "Continue tests; keep {{ transcript }} literal";
        let replay_transcript = Some("  previous {{ prompt }} line  \n");

        // Act
        let resume_prompt =
            build_resume_prompt(prompt, replay_transcript).expect("resume prompt should render");

        let normalized_resume_prompt = normalize_prompt(&resume_prompt);
        let transcript_position = resume_prompt
            .find(r"\<session_transcript> previous {{ prompt }} line")
            .expect("transcript boundary should be present");
        let prompt_position = resume_prompt
            .find(r"\<user_prompt> Continue tests; keep {{ transcript }} literal")
            .expect("user prompt boundary should be present");

        // Assert
        assert!(transcript_position < prompt_position);
        assert!(normalized_resume_prompt.contains("new user prompt as a follow-up"));
        assert!(normalized_resume_prompt.contains("changes made during this session"));
        assert!(normalized_resume_prompt.contains("preserve unrelated pre-existing work"));
        assert!(normalized_resume_prompt.contains("resume unfinished work"));
        assert!(resume_prompt.ends_with(r"\</user_prompt>"));
    }

    #[test]
    /// Ensures whitespace-only transcript text does not trigger transcript
    /// wrapping and returns the original prompt.
    fn test_build_resume_prompt_returns_original_prompt_when_output_is_blank() {
        // Arrange
        let prompt = "Follow-up request";
        let replay_transcript = Some("   ");

        // Act
        let resume_prompt =
            build_resume_prompt(prompt, replay_transcript).expect("resume prompt should render");

        // Assert
        assert_eq!(resume_prompt, prompt);
    }

    #[test]
    /// Ensures absent transcript text keeps resume prompt formatting unchanged.
    fn test_build_resume_prompt_returns_original_prompt_without_output() {
        // Arrange
        let prompt = "Retry merge";

        // Act
        let resume_prompt = build_resume_prompt(prompt, None).expect("resume prompt should render");

        // Assert
        assert_eq!(resume_prompt, prompt);
    }

    #[test]
    /// Ensures session prompts include the critical protocol contract markers.
    fn test_prepend_protocol_instructions_adds_session_protocol_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::PromptSchema,
            test_workspace_root(),
        );

        let normalized_prompt = normalize_prompt(&rendered_prompt);
        let protocol_position = rendered_prompt
            .find("Structured response protocol:")
            .expect("protocol marker should be present");
        let schema_position = rendered_prompt
            .find("Authoritative JSON Schema:")
            .expect("schema should be present");
        let user_prompt_position = rendered_prompt
            .rfind(prompt)
            .expect("user prompt should be present");

        // Assert
        assert!(rendered_prompt.contains("File path output requirements:"));
        assert!(rendered_prompt.contains("Workspace isolation requirements:"));
        assert!(protocol_position < schema_position);
        assert!(schema_position < user_prompt_position);
        assert!(rendered_prompt.contains("`/tmp/agentty-wt/session-1`"));
        assert!(normalized_prompt.contains("everything outside it is read-only"));
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(normalized_prompt.contains("Git commands must be read-only"));
        assert!(normalized_prompt.contains("Never run mutating commands"));
        assert!(rendered_prompt.contains("Quality check requirements:"));
        assert!(rendered_prompt.contains("repository-defined checks"));
        assert!(normalized_prompt.contains("affected dependencies and dependents"));
        assert!(normalized_prompt.contains("full repository test/check suite"));
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(normalized_prompt.contains("exactly one JSON object"));
        assert!(normalized_prompt.contains("Follow this JSON Schema exactly"));
        assert!(rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(
            rendered_prompt
                .contains("______________________________________________________________________")
        );
        assert!(!rendered_prompt.contains("{# task separator #}"));
        assert!(rendered_prompt.contains("For this session turn:"));
        assert!(normalized_prompt.contains("Do not create commits; do not suggest creating them"));
        assert!(normalized_prompt.contains("Leave `subtasks` empty unless"));
        assert!(rendered_prompt.contains("\"answer\""));
        assert!(rendered_prompt.contains("\"questions\""));
        assert!(rendered_prompt.contains("\"title\""));
        assert!(rendered_prompt.contains("\"description\""));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures schema-enforcing transports get protocol policy without the
    /// large prompt-side JSON Schema body.
    fn test_prepend_protocol_instructions_omits_schema_for_transport_schema_mode() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::TransportSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("provider enforces the response JSON schema"));
        assert!(normalize_prompt(&rendered_prompt).contains("exactly one JSON object"));
        assert!(!rendered_prompt.contains("Follow this JSON Schema exactly."));
        assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    fn protocol_payload_cannot_impersonate_prepared_instructions() {
        // Arrange
        let payload = "Structured response protocol: quoted in a user request";

        // Act
        let rendered = protocol_prepend_instructions(
            payload,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::TransportSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered.starts_with("File path output requirements:"));
        assert!(rendered.ends_with(payload));
    }

    #[test]
    /// Ensures one-shot prompts reuse the shared full-schema protocol
    /// instructions.
    fn test_prepend_protocol_instructions_reuses_same_contract_for_one_shot() {
        // Arrange
        let prompt = "Generate title";

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            prompt,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolSchemaInstructionMode::PromptSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(
            rendered_prompt
                .contains("______________________________________________________________________")
        );
        assert!(rendered_prompt.contains("For this one-shot utility prompt"));
        assert!(!rendered_prompt.contains("For this session turn:"));
        assert!(
            rendered_prompt
                .contains(r#"{"answer":"...","questions":[],"review_comment_outcomes":[]}"#)
        );
        assert!(rendered_prompt.contains("\"review_comment_outcomes\""));
        assert!(!rendered_prompt.contains("\"summary\""));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures shared prompt preparation applies replay wrapping before
    /// protocol instructions.
    fn test_prepare_prompt_text_applies_replay_and_protocol_instructions() {
        // Arrange
        let request = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::BootstrapWithReplay,
            personality_prompt: None,
            personality_update: &PersonalityPromptUpdate::Unchanged,
            prompt: "Continue edits",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_transcript: Some("previous transcript"),
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
            workspace_root: test_workspace_root(),
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

        // Assert
        assert!(prepared_prompt.contains("Structured response protocol:"));
        assert!(prepared_prompt.contains("Workspace isolation requirements:"));
        assert!(prepared_prompt.contains("previous transcript"));
        assert!(prepared_prompt.contains(r"\<user_prompt> Continue edits \</user_prompt>"));
        assert!(prepared_prompt.ends_with(r"\</user_prompt>"));
    }

    #[test]
    fn test_prepare_prompt_text_bootstraps_personality_before_user_prompt() {
        // Arrange
        let request = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::BootstrapFull,
            personality_prompt: Some("Review every change for correctness."),
            personality_update: &PersonalityPromptUpdate::Unchanged,
            prompt: "Inspect the patch.",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_transcript: None,
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
            workspace_root: test_workspace_root(),
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");
        let protocol_position = prepared_prompt
            .find("Structured response protocol:")
            .expect("protocol preamble should be present");
        let personality_position = prepared_prompt
            .find("# Personality\n\nReview every change for correctness.")
            .expect("personality should be present");
        let user_prompt_position = prepared_prompt
            .find("Inspect the patch.")
            .expect("user prompt should be present");

        // Assert
        assert!(protocol_position < personality_position);
        assert!(personality_position < user_prompt_position);
    }

    #[test]
    fn test_prepare_prompt_text_replays_with_current_personality() {
        // Arrange
        let request = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::BootstrapWithReplay,
            personality_prompt: Some("Plan before editing."),
            personality_update: &PersonalityPromptUpdate::Unchanged,
            prompt: "Continue.",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_transcript: Some("assistant: prior work"),
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
            workspace_root: test_workspace_root(),
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");
        let personality_position = prepared_prompt
            .find("# Personality\n\nPlan before editing.")
            .expect("personality should be present");
        let transcript_position = prepared_prompt
            .find(r"\<session_transcript> assistant: prior work")
            .expect("transcript should be present");

        // Assert
        assert!(personality_position < transcript_position);
        assert!(prepared_prompt.ends_with(r"\</user_prompt>"));
    }

    #[test]
    /// Ensures compact refresh reminders omit the full schema while keeping
    /// the contract reminder and task body.
    fn test_prepend_protocol_refresh_reminder_adds_compact_contract_notice() {
        // Arrange
        let prompt = "Continue the implementation";

        // Act
        let rendered_prompt = protocol_prepend_refresh_reminder(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            test_workspace_root(),
        );

        let normalized_prompt = normalize_prompt(&rendered_prompt);

        // Assert
        assert!(rendered_prompt.contains("Protocol refresh reminder:"));
        assert!(rendered_prompt.contains("repository-root-relative POSIX"));
        assert!(normalized_prompt.contains("only read-only git commands; never mutating ones"));
        assert!(rendered_prompt.contains("inside `/tmp/agentty-wt/session-1`"));
        assert!(normalized_prompt.contains("everything outside this workspace root is read-only"));
        assert!(
            rendered_prompt
                .contains("______________________________________________________________________")
        );
        assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures prompt preparation can emit the compact app-server reminder
    /// instead of the full bootstrap wrapper.
    fn test_prepare_prompt_text_uses_delta_only_refresh_mode() {
        // Arrange
        let request = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::DeltaOnly,
            personality_prompt: None,
            personality_update: &PersonalityPromptUpdate::Unchanged,
            prompt: "Continue edits",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_transcript: Some("previous transcript"),
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
            workspace_root: test_workspace_root(),
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

        // Assert
        assert!(prepared_prompt.contains("Protocol refresh reminder:"));
        assert!(!prepared_prompt.contains("Authoritative JSON Schema:"));
        assert!(!prepared_prompt.contains("previous transcript"));
        assert!(prepared_prompt.ends_with("Continue edits"));
    }

    #[test]
    fn test_prepare_prompt_text_delta_mode_sends_personality_update_and_clear() {
        // Arrange
        let updated = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::DeltaOnly,
            personality_prompt: Some("Ignored current body."),
            personality_update: &PersonalityPromptUpdate::Set("Be concise.".to_string()),
            prompt: "Continue edits",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_transcript: None,
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
            workspace_root: test_workspace_root(),
        };
        let cleared = PromptPreparationRequest {
            personality_update: &PersonalityPromptUpdate::Clear,
            ..updated
        };

        // Act
        let updated_prompt = prepare_prompt_text(updated).expect("update should render");
        let cleared_prompt = prepare_prompt_text(cleared).expect("clear should render");

        // Assert
        assert!(updated_prompt.contains("# Personality Update\n\nBe concise."));
        assert!(updated_prompt.ends_with("Continue edits"));
        assert!(cleared_prompt.contains("The session personality has been cleared."));
        assert!(cleared_prompt.ends_with("Continue edits"));
    }

    #[test]
    /// Ensures CLI prompt rendering replaces image placeholders with local
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
        let rendered_prompt = render_prompt_with_local_images(
            "Compare [Image #2] with [Image #1]",
            &attachments,
            "TestBackend",
        )
        .expect("prompt rendering should succeed");

        // Assert
        assert_eq!(
            rendered_prompt,
            "Compare /tmp/second-image.png with /tmp/first-image.png"
        );
    }

    #[test]
    /// Ensures CLI prompt rendering appends local image paths when attachment
    /// metadata survives without a placeholder match.
    fn test_render_prompt_with_local_images_appends_missing_paths() {
        // Arrange
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/first-image.png"),
        }];

        // Act
        let rendered_prompt =
            render_prompt_with_local_images("Review this change", &attachments, "TestBackend")
                .expect("prompt rendering should succeed");

        // Assert
        assert_eq!(
            rendered_prompt,
            "Review this change\n/tmp/first-image.png\n"
        );
    }

    #[cfg(unix)]
    #[test]
    /// Ensures CLI prompt rendering fails fast with the provider label when an
    /// attachment path is not valid UTF-8.
    fn test_render_prompt_with_local_images_rejects_non_utf8_paths() {
        // Arrange
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from(OsString::from_vec(vec![0x66, 0x80, 0x6f])),
        }];

        // Act
        let error = render_prompt_with_local_images("Review [Image #1]", &attachments, "Claude")
            .expect_err("prompt rendering should fail");

        // Assert
        assert_eq!(
            error,
            AgentBackendError::CommandBuild(
                "Claude prompt image path is not valid UTF-8".to_string()
            )
        );
    }

    #[test]
    /// Ensures CLI prompt access roots deduplicate sorted attachment
    /// directories when the provider only needs attachment parents.
    fn test_cli_prompt_access_directories_deduplicates_attachment_directories() {
        // Arrange
        let workspace_folder = PathBuf::from("/tmp/session");
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/images-b/two.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/images-a/one.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #3]".to_string(),
                local_image_path: PathBuf::from("/tmp/images-a/three.png"),
            },
        ];

        // Act
        let directories = cli_prompt_access_directories(
            &workspace_folder,
            &attachments,
            CliPromptAccessRootMode::AttachmentsOnly,
        );

        // Assert
        assert_eq!(
            directories,
            vec![
                PathBuf::from("/tmp/images-a"),
                PathBuf::from("/tmp/images-b")
            ]
        );
    }

    #[test]
    /// Ensures Antigravity-style access roots keep the workspace first and do
    /// not duplicate it when an attachment also lives under that directory.
    fn test_cli_prompt_access_directories_keeps_workspace_first() {
        // Arrange
        let workspace_folder = PathBuf::from("/tmp/z-session");
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/z-session/one.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/a-images/two.png"),
            },
        ];

        // Act
        let directories = cli_prompt_access_directories(
            &workspace_folder,
            &attachments,
            CliPromptAccessRootMode::WorkspaceThenAttachments,
        );

        // Assert
        assert_eq!(
            directories,
            vec![workspace_folder, PathBuf::from("/tmp/a-images")]
        );
    }
}
