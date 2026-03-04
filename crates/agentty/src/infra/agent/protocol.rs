//! Structured agent communication protocol types, schema generation, and
//! response parsing.
//!
//! Defines the [`AgentResponse`] payload returned by agent turns, the
//! [`agent_response_output_schema`] JSON Schema used by schema-capable
//! providers, and [`parse_agent_response`] which deserializes raw provider
//! output into structured messages.
//!
//! Parsing first attempts strict whole-response JSON decoding that matches the
//! schema. When parsing fails, the raw payload is preserved as a single
//! `answer` message for display continuity.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Message kind tag used by one [`AgentResponseMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentResponseMessageKind {
    /// Standard answer text.
    Answer,
    /// Plan text awaiting approval.
    Plan,
    /// Clarification request text.
    ///
    /// Question-specific handling is intentionally not wired yet; this variant
    /// is preserved for future agentty-level flows.
    Question,
}

/// One structured message emitted by the assistant protocol payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentResponseMessage {
    /// Message kind selector.
    #[serde(rename = "type")]
    pub kind: AgentResponseMessageKind,
    /// Human-readable markdown text for this message.
    pub text: String,
}

impl AgentResponseMessage {
    /// Constructs one `answer` protocol message.
    pub fn answer(text: impl Into<String>) -> Self {
        Self {
            kind: AgentResponseMessageKind::Answer,
            text: text.into(),
        }
    }

    /// Constructs one `plan` protocol message.
    pub fn plan(text: impl Into<String>) -> Self {
        Self {
            kind: AgentResponseMessageKind::Plan,
            text: text.into(),
        }
    }

    /// Constructs one `question` protocol message.
    pub fn question(text: impl Into<String>) -> Self {
        Self {
            kind: AgentResponseMessageKind::Question,
            text: text.into(),
        }
    }
}

/// Wire-format protocol payload used for schema-driven provider output.
///
/// Providers that support output schemas (for example, Codex app-server) are
/// asked to emit this object as the entire assistant response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentResponse {
    /// Ordered response messages emitted for this turn.
    pub messages: Vec<AgentResponseMessage>,
}

/// Structured response parsing failure details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentResponseParseError {
    /// Response was empty or whitespace-only.
    Empty,
    /// Response did not contain a valid protocol payload.
    InvalidFormat,
}

impl fmt::Display for AgentResponseParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "response is empty"),
            Self::InvalidFormat => write!(formatter, "response is not valid protocol JSON"),
        }
    }
}

impl AgentResponse {
    /// Creates a plain response from raw text as one `answer` message.
    ///
    /// Used as a safe fallback when provider output is not schema-compliant.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            messages: vec![AgentResponseMessage::answer(text)],
        }
    }

    /// Returns display text by joining all non-empty messages with blank lines.
    pub fn to_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        for message in &self.messages {
            push_display_message(&mut display_messages, &message.text);
        }

        display_messages.join("\n\n")
    }

    /// Returns transcript text for session output by joining non-empty
    /// `answer` messages with blank lines.
    ///
    /// `plan` and `question` messages remain available in `messages` for
    /// dedicated UX flows and are intentionally excluded from regular session
    /// transcript output.
    pub fn to_answer_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        for message in &self.messages {
            if message.kind != AgentResponseMessageKind::Answer {
                continue;
            }

            push_display_message(&mut display_messages, &message.text);
        }

        display_messages.join("\n\n")
    }
}

/// Appends one non-empty display message.
fn push_display_message(display_messages: &mut Vec<String>, text: &str) {
    if text.trim().is_empty() {
        return;
    }

    display_messages.push(text.to_string());
}

/// Returns the JSON Schema used for structured assistant output.
///
/// The returned value is passed directly to providers that support enforced
/// output schemas. Its shape mirrors [`AgentResponse`].
pub(crate) fn agent_response_output_schema() -> Value {
    let schema = schemars::schema_for!(AgentResponse);

    // Serialization should always succeed for schema documents. This fallback
    // keeps transport setup non-panicking under strict lint settings.
    match serde_json::to_value(schema) {
        Ok(mut value) => {
            normalize_schema_for_codex(&mut value);

            value
        }
        Err(_) => Value::Null,
    }
}

/// Returns a pretty-printed JSON Schema string for prompt instruction
/// templating.
///
/// This is used by prompt builders for providers that cannot enforce
/// `outputSchema` at transport level and must be guided by in-prompt schema
/// text instead.
pub(crate) fn agent_response_output_schema_json() -> String {
    let schema = agent_response_output_schema();

    match serde_json::to_string_pretty(&schema) {
        Ok(schema_json) => schema_json,
        Err(_) => "null".to_string(),
    }
}

/// Parses a raw assistant message into an [`AgentResponse`].
///
/// Parsing order:
/// 1. Whole-response JSON that matches [`AgentResponse`].
/// 2. Plain-text fallback (`answer` message preserving the original payload).
pub(crate) fn parse_agent_response(raw: &str) -> AgentResponse {
    parse_agent_response_strict(raw).unwrap_or_else(|_| AgentResponse::plain(raw))
}

/// Parses one raw assistant message strictly as protocol payload.
///
/// Parsing order:
/// 1. Whole-response JSON that matches [`AgentResponse`].
/// 2. First extractable top-level JSON object inside `raw`.
///
/// # Errors
/// Returns [`AgentResponseParseError`] when no valid protocol payload is found.
pub(crate) fn parse_agent_response_strict(
    raw: &str,
) -> Result<AgentResponse, AgentResponseParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AgentResponseParseError::Empty);
    }

    if let Some(response) = parse_structured_json_response(trimmed) {
        return Ok(response);
    }

    let Some(json_candidate) = extract_first_json_object(trimmed) else {
        return Err(AgentResponseParseError::InvalidFormat);
    };

    parse_structured_json_response(json_candidate).ok_or(AgentResponseParseError::InvalidFormat)
}

/// Normalizes one streamed assistant chunk for transcript display.
///
/// Returns:
/// - `Some(display_text)` for plain text chunks or complete structured JSON
///   payloads containing at least one non-empty `answer` message.
/// - `None` for protocol JSON fragments that should be suppressed until the
///   final assembled response arrives.
pub(crate) fn normalize_stream_assistant_chunk(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }

    if let Some(response) = parse_structured_json_response(raw) {
        let display_text = response.to_answer_display_text();
        if display_text.trim().is_empty() {
            return None;
        }

        return Some(display_text);
    }

    if is_likely_protocol_json_fragment(raw) {
        return None;
    }

    Some(raw.to_string())
}

/// Builds one follow-up repair prompt that asks the model to emit only a valid
/// protocol JSON object.
pub(crate) fn build_protocol_repair_prompt(invalid_response: &str) -> String {
    let schema_json = agent_response_output_schema_json();

    format!(
        "Your previous response did not match the required JSON schema.\nReturn only one valid \
         JSON object that strictly follows this schema.\nDo not include markdown fences or any \
         extra text.\n\nSchema:\n{schema_json}\n\nPrevious response:\n{invalid_response}"
    )
}

/// Attempts to parse one schema-driven structured JSON response.
fn parse_structured_json_response(raw: &str) -> Option<AgentResponse> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let payload = serde_json::from_str::<AgentResponse>(trimmed).ok()?;
    if payload.messages.is_empty() {
        return None;
    }

    Some(payload)
}

/// Normalizes one schema tree for Codex `response_format` compatibility.
///
/// Codex rejects schemas that use `oneOf` for enum-like constants. Schemars
/// can emit this shape for simple Rust enums, so this normalizer rewrites
/// those fragments to string `enum` definitions.
fn normalize_schema_for_codex(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for nested_value in object.values_mut() {
                normalize_schema_for_codex(nested_value);
            }

            normalize_ref_object_for_codex(object);

            let one_of_values = object
                .get("oneOf")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_object)
                        .map(|item| item.get("const").and_then(Value::as_str))
                        .collect::<Option<Vec<_>>>()
                })
                .map(|option| {
                    option.map(|values| {
                        values
                            .into_iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                });

            if let Some(Some(enum_variants)) = one_of_values {
                object.remove("oneOf");
                object.insert("type".to_string(), Value::String("string".to_string()));
                object.insert(
                    "enum".to_string(),
                    Value::Array(enum_variants.into_iter().map(Value::String).collect()),
                );
            }
        }
        Value::Array(array) => {
            for nested_value in array {
                normalize_schema_for_codex(nested_value);
            }
        }
        _ => {}
    }
}

/// Returns whether one stream chunk looks like partial protocol JSON payload.
fn is_likely_protocol_json_fragment(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }

    if is_json_punctuation_only(trimmed) {
        return true;
    }

    let has_protocol_key = trimmed.contains("\"messages\"")
        || trimmed.contains("\"type\"")
        || trimmed.contains("\"text\"");
    if !has_protocol_key {
        return false;
    }

    trimmed.contains('{')
        || trimmed.contains('}')
        || trimmed.contains('[')
        || trimmed.contains(']')
        || trimmed.contains(':')
        || trimmed.contains(',')
}

/// Returns whether a chunk contains only JSON punctuation/signature symbols.
fn is_json_punctuation_only(value: &str) -> bool {
    value
        .chars()
        .all(|character| matches!(character, '{' | '}' | '[' | ']' | ':' | ',' | '"'))
}

/// Extracts the first complete top-level JSON object from free-form text.
fn extract_first_json_object(raw: &str) -> Option<&str> {
    let mut object_start: Option<usize> = None;
    let mut brace_depth: usize = 0;
    let mut in_string = false;
    let mut is_escaped = false;

    for (index, character) in raw.char_indices() {
        if object_start.is_none() {
            if character == '{' {
                object_start = Some(index);
                brace_depth = 1;
            }

            continue;
        }

        if is_escaped {
            is_escaped = false;
            continue;
        }

        if in_string {
            match character {
                '\\' => is_escaped = true,
                '"' => in_string = false,
                _ => {}
            }

            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => brace_depth += 1,
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if brace_depth == 0
                    && let Some(start_index) = object_start
                {
                    let end_index = index + character.len_utf8();
                    return raw.get(start_index..end_index);
                }
            }
            _ => {}
        }
    }

    None
}

/// Rewrites one `$ref` schema object to Codex-compatible form.
///
/// Codex rejects sibling keywords alongside `$ref` (for example
/// `{ "$ref": "...", "description": "..." }`), so this keeps only the
/// reference key when present.
fn normalize_ref_object_for_codex(object: &mut serde_json::Map<String, Value>) {
    let Some(reference) = object.get("$ref").cloned() else {
        return;
    };

    object.clear();
    object.insert("$ref".to_string(), reference);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Parses a full JSON response object into structured messages.
    fn test_parse_agent_response_structured_json_payload() {
        // Arrange
        let raw = r#"{"messages":[{"type":"answer","text":"Here is my analysis."}]}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(
            response,
            AgentResponse {
                messages: vec![AgentResponseMessage::answer("Here is my analysis.")],
            }
        );
        assert_eq!(response.to_display_text(), "Here is my analysis.");
    }

    #[test]
    /// Strict parsing accepts a complete schema payload.
    fn test_parse_agent_response_strict_structured_json_payload() {
        // Arrange
        let raw = r#"{"messages":[{"type":"answer","text":"Here is my analysis."}]}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response,
            Ok(AgentResponse {
                messages: vec![AgentResponseMessage::answer("Here is my analysis.")],
            })
        );
    }

    #[test]
    /// Strict parsing extracts and parses the first JSON object in mixed text.
    fn test_parse_agent_response_strict_extracts_json_object_from_wrapped_text() {
        // Arrange
        let raw =
            "Header text\n{\"messages\":[{\"type\":\"answer\",\"text\":\"Done.\"}]}\nFooter text";

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response,
            Ok(AgentResponse {
                messages: vec![AgentResponseMessage::answer("Done.")],
            })
        );
    }

    #[test]
    /// Strict parsing rejects non-protocol plain text.
    fn test_parse_agent_response_strict_rejects_plain_text() {
        // Arrange
        let raw = "Just plain text";

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(response, Err(AgentResponseParseError::InvalidFormat));
    }

    #[test]
    /// Converts complete structured stream payloads into display text.
    fn test_normalize_stream_assistant_chunk_structured_payload() {
        // Arrange
        let raw = r#"{"messages":[{"type":"answer","text":"Done."}]}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some("Done.".to_string()));
    }

    #[test]
    /// Suppresses complete structured payloads that contain only questions.
    fn test_normalize_stream_assistant_chunk_question_only_payload() {
        // Arrange
        let raw = r#"{"messages":[{"type":"question","text":"Need details?"}]}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, None);
    }

    #[test]
    /// Suppresses partial protocol JSON fragments from streamed output.
    fn test_normalize_stream_assistant_chunk_protocol_fragment() {
        // Arrange
        let raw = r#"{"messages":[{"type":"answer","#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, None);
    }

    #[test]
    /// Preserves non-protocol plain text stream chunks.
    fn test_normalize_stream_assistant_chunk_plain_text() {
        // Arrange
        let raw = "Plain assistant text";

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some(raw.to_string()));
    }

    #[test]
    /// Parses mixed message arrays and preserves all text in display order.
    fn test_parse_agent_response_structured_json_with_mixed_messages() {
        // Arrange
        let raw = r#"{"messages":[{"type":"answer","text":"Completed implementation."},{"type":"question","text":"Need one decision."}]}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.messages.len(), 2);
        assert_eq!(
            response.to_display_text(),
            "Completed implementation.\n\nNeed one decision."
        );
    }

    #[test]
    /// Builds transcript text from only `answer` messages.
    fn test_to_answer_display_text_uses_only_answer_messages() {
        // Arrange
        let response = AgentResponse {
            messages: vec![
                AgentResponseMessage::answer("Completed implementation."),
                AgentResponseMessage::question("Need one decision."),
                AgentResponseMessage::plan("Run checks."),
                AgentResponseMessage::answer("Applied updates."),
            ],
        };

        // Act
        let display_text = response.to_answer_display_text();

        // Assert
        assert_eq!(
            display_text,
            "Completed implementation.\n\nApplied updates."
        );
    }

    #[test]
    /// Falls back to plain text for payloads with an empty `messages` array.
    fn test_parse_agent_response_empty_messages_falls_back_to_plain_text() {
        // Arrange
        let raw = r#"{"messages":[]}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Extracts and parses structured JSON wrapped in markdown code fences.
    fn test_parse_agent_response_structured_json_in_code_fence_extracts_payload() {
        // Arrange
        let raw =
            "```json\n{\"messages\":[{\"type\":\"question\",\"text\":\"Need details.\"}]}\n```";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(
            response,
            AgentResponse {
                messages: vec![AgentResponseMessage::question("Need details.")],
            }
        );
        assert_eq!(response.to_display_text(), "Need details.");
    }

    #[test]
    /// Falls back to plain text for non-schema payloads.
    fn test_parse_agent_response_non_schema_payload_falls_back_to_plain_text() {
        // Arrange
        let raw = "Here is my analysis.\n\n---agentty-meta---\n{\"type\": \"answer\"}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Falls back to plain text when no structured protocol is present.
    fn test_parse_agent_response_plain_text_fallback() {
        // Arrange
        let raw = "Just a normal response with no metadata.";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Falls back to plain text when JSON parsing fails.
    fn test_parse_agent_response_malformed_json_fallback() {
        // Arrange
        let raw = "{invalid json}";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Falls back to plain text when structured payload has unknown fields.
    fn test_parse_agent_response_unknown_top_level_fields_fallback() {
        // Arrange
        let raw = r#"{"messages":[{"type":"answer","text":"Response text."}],"future_field":true,"extra":42}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Falls back to plain text when message entries include unknown fields.
    fn test_parse_agent_response_unknown_message_fields_fallback() {
        // Arrange
        let raw =
            r#"{"messages":[{"type":"question","text":"Need details","variants":["A","B"]}]}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Keeps plain payload whitespace in fallback mode.
    fn test_parse_agent_response_preserves_fallback_whitespace() {
        // Arrange
        let raw = "Response text.  \n\n\n";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Round-trips structured payloads through serialization and
    /// deserialization.
    fn test_agent_response_serde_round_trip() {
        // Arrange
        let response = AgentResponse {
            messages: vec![
                AgentResponseMessage::plan("Plan step 1"),
                AgentResponseMessage::question("Need one decision"),
            ],
        };

        // Act
        let json = serde_json::to_string(&response).expect("serialization should succeed");
        let deserialized: AgentResponse =
            serde_json::from_str(&json).expect("deserialization should succeed");

        // Assert
        assert_eq!(deserialized, response);
    }

    #[test]
    /// Creates a plain response via the convenience constructor.
    fn test_agent_response_plain_constructor() {
        // Arrange / Act
        let response = AgentResponse::plain("Hello");

        // Assert
        assert_eq!(response.to_display_text(), "Hello");
        assert_eq!(
            response,
            AgentResponse {
                messages: vec![AgentResponseMessage::answer("Hello")],
            }
        );
    }

    #[test]
    /// Builds a schema object with required `messages` field.
    fn test_agent_response_output_schema_contains_required_fields() {
        // Arrange / Act
        let schema = agent_response_output_schema();
        let required_fields = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("schema required fields should exist");
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("schema properties should exist");

        // Assert
        assert!(
            required_fields
                .iter()
                .any(|field| field.as_str() == Some("messages"))
        );
        assert!(properties.contains_key("messages"));
    }

    #[test]
    /// Ensures generated schema avoids `oneOf` so Codex `outputSchema`
    /// accepts the payload contract.
    fn test_agent_response_output_schema_does_not_contain_one_of() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(!contains_schema_key(&schema, "oneOf"));
    }

    #[test]
    /// Preserves message kind enum values after `oneOf` normalization.
    fn test_agent_response_output_schema_contains_message_type_enum_values() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(
            contains_schema_enum_values(&schema, &["answer", "plan", "question"]),
            "message type enum values should exist in schema"
        );
    }

    #[test]
    /// Ensures no schema object uses `$ref` with sibling keys.
    fn test_agent_response_output_schema_ref_objects_have_no_sibling_keywords() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(!contains_ref_with_sibling_keywords(&schema));
    }

    #[test]
    /// Exposes a parseable pretty JSON schema string for prompt templating.
    fn test_agent_response_output_schema_json_is_parseable_value() {
        // Arrange / Act
        let schema_json = agent_response_output_schema_json();
        let parsed_schema: Value =
            serde_json::from_str(&schema_json).expect("schema string should parse as JSON");
        let schema_value = agent_response_output_schema();

        // Assert
        assert_eq!(parsed_schema, schema_value);
    }

    /// Recursively checks whether one JSON value tree contains a schema key.
    fn contains_schema_key(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(object) => {
                if object.contains_key(key) {
                    return true;
                }

                object
                    .values()
                    .any(|nested_value| contains_schema_key(nested_value, key))
            }
            Value::Array(array) => array
                .iter()
                .any(|nested_value| contains_schema_key(nested_value, key)),
            _ => false,
        }
    }

    /// Recursively checks whether one schema tree contains an `enum` array
    /// with the exact ordered values.
    fn contains_schema_enum_values(value: &Value, expected_values: &[&str]) -> bool {
        match value {
            Value::Object(object) => {
                let has_expected_enum =
                    object
                        .get("enum")
                        .and_then(Value::as_array)
                        .is_some_and(|enum_values| {
                            enum_values
                                .iter()
                                .map(Value::as_str)
                                .collect::<Option<Vec<_>>>()
                                .is_some_and(|values| values == expected_values)
                        });
                if has_expected_enum {
                    return true;
                }

                object
                    .values()
                    .any(|nested_value| contains_schema_enum_values(nested_value, expected_values))
            }
            Value::Array(array) => array
                .iter()
                .any(|nested_value| contains_schema_enum_values(nested_value, expected_values)),
            _ => false,
        }
    }

    /// Recursively checks whether any `$ref` object has extra sibling keys.
    fn contains_ref_with_sibling_keywords(value: &Value) -> bool {
        match value {
            Value::Object(object) => {
                if object.contains_key("$ref") && object.len() > 1 {
                    return true;
                }

                object.values().any(contains_ref_with_sibling_keywords)
            }
            Value::Array(array) => array.iter().any(contains_ref_with_sibling_keywords),
            _ => false,
        }
    }
}
