//! Protocol-owned prompt envelopes for agent-facing instruction text.

use std::path::Path;

use askama::Template;

use super::model::ProtocolRequestProfile;
use super::schema::protocol_json_schema_json;

/// Combined budget for the JSON-encoded response and diagnostic.
const REPAIR_PAYLOAD_MAX_BYTES: usize = 128 * 1024;
/// Maximum encoded diagnostic size, including any truncation notice.
const REPAIR_PARSE_ERROR_MAX_BYTES: usize = 4 * 1024;

/// Controls whether bootstrap prompt instructions include the full protocol
/// JSON Schema text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolSchemaInstructionMode {
    /// Include the full self-descriptive JSON Schema in the prompt because
    /// the provider does not enforce Agentty's response schema natively.
    PromptSchema,
    /// Omit the full schema text because the provider enforces the same
    /// response schema through its transport-level structured output API.
    TransportSchema,
}

impl ProtocolSchemaInstructionMode {
    /// Returns whether bootstrap instructions should embed the full JSON
    /// Schema text in the prompt body.
    fn includes_response_json_schema(self) -> bool {
        matches!(self, Self::PromptSchema)
    }
}

/// Prepends structured response protocol instructions to a prompt.
///
/// Tells agents to emit one top-level JSON object that matches Agentty's
/// structured protocol while selecting the cheapest safe schema guidance for
/// the current provider. Providers without native structured output receive
/// the full JSON Schema in the prompt; providers with native enforcement get
/// policy and field-routing instructions only. `workspace_root` names the
/// only writable directory for the turn. The transport selects bootstrap or
/// refresh delivery explicitly and calls this once for an unwrapped payload;
/// payload text never determines whether policy is applied.
#[must_use]
pub fn prepend_protocol_instructions(
    prompt: &str,
    profile: ProtocolRequestProfile,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    workspace_root: &Path,
) -> String {
    let protocol_usage_instructions = render_protocol_usage_instructions(profile);
    let workspace_root = workspace_root.display().to_string();
    if !schema_instruction_mode.includes_response_json_schema() {
        let template = ProtocolInstructionPolicyPromptTemplate {
            prompt,
            protocol_usage_instructions: &protocol_usage_instructions,
            workspace_root: &workspace_root,
        };

        return render_template("protocol_instruction_policy_prompt.md", &template);
    }

    let response_json_schema = protocol_json_schema_json(profile);
    let template = ProtocolInstructionPromptTemplate {
        prompt,
        protocol_usage_instructions: &protocol_usage_instructions,
        response_json_schema: &response_json_schema,
        workspace_root: &workspace_root,
    };

    render_template("protocol_instruction_prompt.md", &template)
}

/// Prepends a compact refresh reminder for providers that already received
/// the full instruction contract in the active context.
///
/// The reminder repeats the workspace-isolation boundary for
/// `workspace_root` so long-lived provider contexts keep the rule even after
/// provider-side context compaction.
#[must_use]
pub fn prepend_protocol_refresh_reminder(
    prompt: &str,
    profile: ProtocolRequestProfile,
    workspace_root: &Path,
) -> String {
    let protocol_refresh_instructions = render_protocol_refresh_instructions(profile);
    let workspace_root = workspace_root.display().to_string();
    let template = ProtocolRefreshPromptTemplate {
        prompt,
        protocol_refresh_instructions: &protocol_refresh_instructions,
        workspace_root: &workspace_root,
    };

    render_template("protocol_refresh_prompt.md", &template)
}

/// Builds the protocol repair prompt text for one failed parse attempt.
///
/// Returns a schema-free body with the lossless response and a bounded
/// diagnostic. Callers must apply the shared protocol envelope using the
/// request's profile and provider schema mode before submission.
///
/// # Errors
/// Returns an error instead of truncating a response when the combined
/// JSON-encoded response and diagnostic exceed 128 KiB, including string
/// delimiters and escapes. Static envelope instructions are outside this
/// budget.
pub fn build_protocol_repair_prompt(
    parse_error: &str,
    malformed_response: &str,
) -> Result<String, String> {
    if malformed_response.len() > REPAIR_PAYLOAD_MAX_BYTES {
        return Err(format!(
            "Protocol repair skipped: response exceeds the {REPAIR_PAYLOAD_MAX_BYTES}-byte \
             lossless repair limit ({} bytes)",
            malformed_response.len()
        ));
    }

    let malformed_response = serde_json::json!(malformed_response).to_string();
    if malformed_response.len() > REPAIR_PAYLOAD_MAX_BYTES {
        return Err(format!(
            "Protocol repair skipped: JSON-encoded response exceeds the \
             {REPAIR_PAYLOAD_MAX_BYTES}-byte lossless repair limit ({} bytes)",
            malformed_response.len()
        ));
    }

    let parse_error = encode_repair_parse_error(parse_error);
    let payload_bytes = malformed_response.len() + parse_error.len();
    if payload_bytes > REPAIR_PAYLOAD_MAX_BYTES {
        return Err(format!(
            "Protocol repair skipped: combined JSON-encoded response and diagnostic exceed the \
             {REPAIR_PAYLOAD_MAX_BYTES}-byte lossless repair limit ({payload_bytes} bytes)"
        ));
    }

    let template = ProtocolRepairPromptTemplate {
        malformed_response: &malformed_response,
        parse_error: &parse_error,
    };

    Ok(render_template("protocol_repair_prompt.md", &template))
}

/// Encodes a diagnostic without allocating in proportion to untrusted input.
fn encode_repair_parse_error(parse_error: &str) -> String {
    const NOTICE: &str = " [diagnostic truncated]";

    let end = parse_error.floor_char_boundary(parse_error.len().min(REPAIR_PARSE_ERROR_MAX_BYTES));
    let encoded = serde_json::json!(&parse_error[..end]).to_string();
    if end == parse_error.len() && encoded.len() <= REPAIR_PARSE_ERROR_MAX_BYTES {
        return encoded;
    }

    // JSON escapes consume at most six bytes per input byte. Reserve room
    // for delimiters and the notice before selecting a UTF-8-safe prefix.
    let prefix_bytes = (REPAIR_PARSE_ERROR_MAX_BYTES - NOTICE.len() - 2) / 6;
    let end = parse_error.floor_char_boundary(parse_error.len().min(prefix_bytes));

    serde_json::json!(format!("{}{NOTICE}", &parse_error[..end])).to_string()
}

/// Askama view model for protocol instructions when the transport enforces
/// the response schema.
#[derive(Template)]
#[template(path = "protocol_instruction_policy_prompt.md", escape = "none")]
struct ProtocolInstructionPolicyPromptTemplate<'a> {
    prompt: &'a str,
    protocol_usage_instructions: &'a str,
    workspace_root: &'a str,
}

/// Askama view model for full protocol instructions with prompt-side schema.
#[derive(Template)]
#[template(path = "protocol_instruction_prompt.md", escape = "none")]
struct ProtocolInstructionPromptTemplate<'a> {
    prompt: &'a str,
    protocol_usage_instructions: &'a str,
    response_json_schema: &'a str,
    workspace_root: &'a str,
}

/// Askama view model for compact refresh reminders.
#[derive(Template)]
#[template(path = "protocol_refresh_prompt.md", escape = "none")]
struct ProtocolRefreshPromptTemplate<'a> {
    prompt: &'a str,
    protocol_refresh_instructions: &'a str,
    workspace_root: &'a str,
}

/// Askama view model for repair prompts after protocol parse failures.
#[derive(Template)]
#[template(path = "protocol_repair_prompt.md", escape = "none")]
struct ProtocolRepairPromptTemplate<'a> {
    malformed_response: &'a str,
    parse_error: &'a str,
}

/// Askama view model for session-turn protocol usage instructions.
#[derive(Template)]
#[template(path = "protocol_instruction_session_turn_usage.md", escape = "none")]
struct ProtocolInstructionSessionTurnUsageTemplate;

/// Askama view model for one-shot protocol usage instructions.
#[derive(Template)]
#[template(path = "protocol_instruction_utility_prompt_usage.md", escape = "none")]
struct ProtocolInstructionUtilityPromptUsageTemplate;

/// Askama view model for direct focused-review protocol usage instructions.
#[derive(Template)]
#[template(path = "protocol_instruction_focused_review_usage.md", escape = "none")]
struct ProtocolInstructionFocusedReviewUsageTemplate;

/// Askama view model for session-turn refresh instructions.
#[derive(Template)]
#[template(path = "protocol_refresh_session_turn_instruction.md", escape = "none")]
struct ProtocolRefreshSessionTurnInstructionTemplate;

/// Askama view model for one-shot refresh instructions.
#[derive(Template)]
#[template(
    path = "protocol_refresh_utility_prompt_instruction.md",
    escape = "none"
)]
struct ProtocolRefreshUtilityPromptInstructionTemplate;

/// Renders the protocol usage instructions for one request profile.
fn render_protocol_usage_instructions(profile: ProtocolRequestProfile) -> String {
    if matches!(profile, ProtocolRequestProfile::FocusedReview) {
        return render_template(
            "protocol_instruction_focused_review_usage.md",
            &ProtocolInstructionFocusedReviewUsageTemplate,
        );
    }

    if matches!(profile, ProtocolRequestProfile::SessionTurn) {
        return render_template(
            "protocol_instruction_session_turn_usage.md",
            &ProtocolInstructionSessionTurnUsageTemplate,
        );
    }

    render_template(
        "protocol_instruction_utility_prompt_usage.md",
        &ProtocolInstructionUtilityPromptUsageTemplate,
    )
}

/// Renders the compact protocol refresh instructions for one request profile.
fn render_protocol_refresh_instructions(profile: ProtocolRequestProfile) -> String {
    if matches!(profile, ProtocolRequestProfile::FocusedReview) {
        return render_template(
            "protocol_instruction_focused_review_usage.md",
            &ProtocolInstructionFocusedReviewUsageTemplate,
        );
    }

    if matches!(profile, ProtocolRequestProfile::SessionTurn) {
        return render_template(
            "protocol_refresh_session_turn_instruction.md",
            &ProtocolRefreshSessionTurnInstructionTemplate,
        );
    }

    render_template(
        "protocol_refresh_utility_prompt_instruction.md",
        &ProtocolRefreshUtilityPromptInstructionTemplate,
    )
}

/// Renders one Askama template and removes trailing whitespace.
fn render_template(template_name: &str, template: &impl Template) -> String {
    let rendered = match template.render() {
        Ok(rendered) => rendered,
        Err(error) => format!("Failed to render `{template_name}`: {error}"),
    };

    rendered.trim_end().to_string()
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
