//! Structured agent communication protocol types, schema generation, and
//! response parsing.
//!
//! Defines the [`AgentResponse`] payload returned by agent turns, the
//! prompt-facing and transport-facing JSON Schema renderings derived from that
//! model, and [`parse_agent_response`] which deserializes raw provider output
//! into one markdown answer, a bounded list of clarification questions, and
//! the optional structured session summary block.
//!
//! Parsing first attempts strict whole-response JSON decoding that matches the
//! schema. When parsing fails, the raw payload is preserved as a single
//! `answer` string for display continuity.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hard cap on the number of clarification questions extracted from one agent
/// response. Prevents runaway output from flooding the question UI even when
/// the agent ignores the prompt-level limit.
///
/// This constant is also injected into the protocol instruction prompt
/// templates so the prompt-level guidance and the server-side cap stay in
/// sync automatically.
pub(crate) const MAX_QUESTIONS: usize = 5;

/// Protocol-owned request family preserved across prompt submission and repair
/// retries.
///
/// Session discussion turns and isolated utility prompts share the same
/// top-level [`AgentResponse`] schema. Agentty still carries the request
/// family through transport boundaries so call sites can keep one consistent
/// protocol contract even when some callers ignore parts of the response, such
/// as the optional top-level `summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolRequestProfile {
    /// Interactive session turn.
    SessionTurn,
    /// Isolated utility prompt.
    UtilityPrompt,
}

/// One extracted question with predefined answer choices.
///
/// The UI and persistence layers use this as the canonical clarification
/// question representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "QuestionItem",
    description = "One clarification question emitted by the assistant protocol payload. Keep \
                   each item focused to one actionable decision."
)]
pub struct QuestionItem {
    /// Predefined answer choices the user can select from.
    #[serde(default)]
    #[schemars(
        title = "options",
        description = "Predefined answer choices the user can select from. Keep this list focused \
                       to 1-3 likely answers, put the recommended choice first, and omit deferral \
                       or non-answer choices. Defaults to an empty list when omitted."
    )]
    pub options: Vec<String>,
    /// The clarification question text.
    #[schemars(
        title = "text",
        description = "Human-readable markdown text for this question. Ask one specific \
                       actionable question instead of bundling multiple decisions into one item."
    )]
    pub text: String,
}

impl QuestionItem {
    /// Constructs one clarification question without predefined answer
    /// options.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            options: Vec::new(),
            text: text.into(),
        }
    }

    /// Constructs one clarification question with predefined answer options.
    pub fn with_options(text: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            options,
            text: text.into(),
        }
    }
}

/// Structured session summary block emitted alongside protocol messages.
///
/// Session-discussion turns use this object instead of embedding the change
/// summary inside `answer` message text. One-shot prompts set the top-level
/// `summary` field to `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "AgentResponseSummary",
    description = "Structured session summary block emitted alongside protocol messages instead \
                   of embedding the change summary inside `answer` markdown on session-discussion \
                   turns."
)]
pub struct AgentResponseSummary {
    /// Cumulative summary of active changes on the current session branch.
    #[schemars(
        title = "session",
        description = "Cumulative summary of active changes on the current session branch."
    )]
    pub session: String,
    /// Concise summary of only the work completed in the current turn.
    #[schemars(
        title = "turn",
        description = "Concise summary of only the work completed in the current turn."
    )]
    pub turn: String,
}

/// Wire-format protocol payload used for schema-driven provider output.
///
/// Providers that support output schemas (for example, Codex app-server) are
/// asked to emit this object as the entire assistant response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "AgentResponse",
    description = "Wire-format protocol payload used for schema-driven provider output. Return \
                   this object as the entire assistant response payload. Providers that support \
                   output schemas (for example, Codex app-server) are asked to emit this object \
                   directly."
)]
pub struct AgentResponse {
    /// Markdown answer text emitted for this turn.
    #[serde(default)]
    #[schemars(
        title = "answer",
        description = "Markdown answer text for delivered work, status updates, or concise \
                       completion notes. Keep clarification requests out of this field and emit \
                       them through `questions` instead. Defaults to an empty string when omitted."
    )]
    pub answer: String,
    /// Ordered clarification questions emitted for this turn.
    #[serde(default)]
    #[schemars(
        title = "questions",
        description = "Ordered clarification questions emitted for this turn. Emit at most \
                       `MAX_QUESTIONS` items, and use an empty array when no user input is \
                       required. Defaults to an empty array when omitted."
    )]
    pub questions: Vec<QuestionItem>,
    /// Structured summary for session-discussion turns, or `None` for legacy
    /// payloads and one-shot prompts.
    #[serde(default)]
    #[schemars(
        title = "summary",
        description = "Structured summary for session-discussion turns, kept outside `answer` \
                       markdown. Use `null` for one-shot prompts and legacy payloads."
    )]
    pub summary: Option<AgentResponseSummary>,
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
    /// Creates a plain response from raw text as one `answer` string.
    ///
    /// Used as a safe fallback when provider output is not schema-compliant.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            answer: text.into(),
            questions: Vec::new(),
            summary: None,
        }
    }

    /// Returns display text by joining non-empty answer and question text with
    /// blank lines.
    pub fn to_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        push_display_message(&mut display_messages, &self.answer);
        push_question_display_messages(&mut display_messages, &self.questions);

        display_messages.join("\n\n")
    }

    /// Returns transcript text for session output by joining non-empty
    /// `answer` content with blank lines.
    pub fn to_answer_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        push_display_message(&mut display_messages, &self.answer);

        display_messages.join("\n\n")
    }

    /// Returns the answer as one single-item vector when it is non-empty.
    pub fn answers(&self) -> Vec<String> {
        let answer = self.to_answer_display_text();
        if answer.is_empty() {
            return Vec::new();
        }

        vec![answer]
    }

    /// Returns up to [`MAX_QUESTIONS`] clarification questions in response
    /// order.
    pub fn question_items(&self) -> Vec<QuestionItem> {
        self.questions.iter().take(MAX_QUESTIONS).cloned().collect()
    }
}

/// Returns the JSON Schema used for structured assistant output.
///
/// The returned value is passed directly to providers that support enforced
/// output schemas. It starts from the self-descriptive response schema and then
/// applies compatibility normalization required by schema-enforcing agents.
pub(crate) fn agent_response_output_schema() -> Value {
    let mut value = agent_response_json_schema();
    normalize_schema_for_transport(&mut value);

    value
}

/// Returns a pretty-printed JSON Schema string for prompt instruction
/// templating.
///
/// This keeps the raw `schemars` metadata intact so inline prompt guidance can
/// show a fully self-descriptive schema document.
pub(crate) fn agent_response_json_schema_json() -> String {
    let schema = agent_response_json_schema();

    stringify_schema_json(&schema)
}

/// Returns a pretty-printed JSON Schema string for prompt instruction
/// templating.
///
/// This is used by prompt builders for providers that cannot enforce
/// `outputSchema` at transport level and must be guided by in-prompt schema
/// text instead, or by native schema-validation flags that accept a serialized
/// schema document.
pub(crate) fn agent_response_output_schema_json() -> String {
    let schema = agent_response_output_schema();

    stringify_schema_json(&schema)
}

/// Parses a raw assistant message into an [`AgentResponse`].
///
/// Parsing order:
/// 1. Whole-response JSON that matches [`AgentResponse`].
/// 2. Plain-text fallback (`answer` string preserving the original payload).
pub(crate) fn parse_agent_response(raw: &str) -> AgentResponse {
    parse_agent_response_strict(raw).unwrap_or_else(|_| AgentResponse::plain(raw))
}

/// Normalizes one parsed turn response according to the request profile.
///
/// Interactive session turns expect a summary block on every response so the
/// worker can persist and render a `Change Summary` section even when no
/// change text exists. Some providers still emit `summary: null` for compliant
/// session-turn JSON, so this fills in an empty summary object that downstream
/// rendering already maps to `No changes`.
pub(crate) fn normalize_turn_response(
    mut response: AgentResponse,
    protocol_profile: ProtocolRequestProfile,
) -> AgentResponse {
    if matches!(protocol_profile, ProtocolRequestProfile::SessionTurn) && response.summary.is_none()
    {
        response.summary = Some(AgentResponseSummary {
            turn: String::new(),
            session: String::new(),
        });
    }

    response
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

    parse_first_valid_embedded_json_response(trimmed).ok_or(AgentResponseParseError::InvalidFormat)
}

/// Normalizes one streamed assistant chunk for transcript display.
///
/// Returns:
/// - `Some(display_text)` for plain text chunks or complete structured JSON
///   payloads containing non-empty `answer` text.
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

/// Returns the self-descriptive JSON Schema for the response payload.
///
/// This preserves the raw `schemars` output, including metadata such as
/// `title` and `description`, so prompt templates can show models the richest
/// possible schema contract.
fn agent_response_json_schema() -> Value {
    let schema = schemars::schema_for!(AgentResponse);
    let mut schema_value = serde_json::to_value(schema).unwrap_or(Value::Null);

    inject_dynamic_schema_guidance(&mut schema_value);

    schema_value
}

/// Injects dynamic prompt guidance that depends on runtime constants into the
/// schema metadata shown to providers.
fn inject_dynamic_schema_guidance(schema: &mut Value) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(questions_property) = properties
        .get_mut("questions")
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    questions_property.insert(
        "description".to_string(),
        Value::String(format!(
            "Ordered clarification questions emitted for this turn. Emit at most {MAX_QUESTIONS} \
             items, and use an empty array when no user input is required. Defaults to an empty \
             array when omitted."
        )),
    );
}

/// Pretty-prints one schema document for prompt or transport wiring.
fn stringify_schema_json(schema: &Value) -> String {
    match serde_json::to_string_pretty(schema) {
        Ok(schema_json) => schema_json,
        Err(_) => "null".to_string(),
    }
}

/// Attempts to parse one schema-driven structured JSON response.
fn parse_structured_json_response(raw: &str) -> Option<AgentResponse> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let payload = serde_json::from_str::<AgentResponse>(trimmed).ok()?;
    if payload.answer.trim().is_empty() && payload.questions.is_empty() {
        return None;
    }

    Some(payload)
}

/// Normalizes one schema tree for transport-level provider compatibility.
///
/// Codex rejects schemas that use `oneOf` for enum-like constants. Schemars
/// can emit this shape for simple Rust enums, so this normalizer rewrites
/// those fragments to string `enum` definitions.
fn normalize_schema_for_transport(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for nested_value in object.values_mut() {
                normalize_schema_for_transport(nested_value);
            }

            normalize_ref_object_for_codex(object);
            normalize_required_for_codex(object);

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
                normalize_schema_for_transport(nested_value);
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

    let has_protocol_key = trimmed.contains("\"answer\"")
        || trimmed.contains("\"questions\"")
        || trimmed.contains("\"text\"")
        || trimmed.contains("\"options\"")
        || trimmed.contains("\"summary\"");
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

/// Appends non-empty clarification question text in order.
fn push_question_display_messages(display_messages: &mut Vec<String>, questions: &[QuestionItem]) {
    for question in questions {
        push_display_message(display_messages, &question.text);
    }
}

/// Appends one non-empty display message.
fn push_display_message(display_messages: &mut Vec<String>, text: &str) {
    if text.trim().is_empty() {
        return;
    }

    display_messages.push(text.to_string());
}

/// Parses the first valid protocol payload embedded in free-form text.
///
/// This skips non-protocol or invalid JSON objects that may appear before the
/// actual protocol payload.
fn parse_first_valid_embedded_json_response(raw: &str) -> Option<AgentResponse> {
    let mut search_from = 0;

    while let Some((start_index, end_index)) = extract_next_json_object_range(raw, search_from) {
        let json_candidate = raw.get(start_index..end_index)?;
        if let Some(parsed_response) = parse_structured_json_response(json_candidate) {
            return Some(parsed_response);
        }

        search_from = start_index + 1;
    }

    None
}

/// Extracts the next complete top-level JSON object byte range starting at
/// `search_from`.
fn extract_next_json_object_range(raw: &str, search_from: usize) -> Option<(usize, usize)> {
    if search_from >= raw.len() || !raw.is_char_boundary(search_from) {
        return None;
    }

    let mut object_start: Option<usize> = None;
    let mut brace_depth: usize = 0;
    let mut in_string = false;
    let mut is_escaped = false;

    for (relative_index, character) in raw[search_from..].char_indices() {
        let index = search_from + relative_index;
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
                    return Some((start_index, end_index));
                }
            }
            _ => {}
        }
    }

    None
}

/// Ensures all `properties` keys appear in `required` for Codex compatibility.
///
/// Codex rejects schemas where `properties` contains keys not listed in
/// `required`. Schemars omits optional fields from `required`, so this
/// normalizer adds any missing property keys.
fn normalize_required_for_codex(object: &mut serde_json::Map<String, Value>) {
    let Some(properties) = object.get("properties").and_then(Value::as_object) else {
        return;
    };

    let property_keys: Vec<String> = properties.keys().cloned().collect();
    if property_keys.is_empty() {
        return;
    }

    let required = object
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));

    let Some(required_array) = required.as_array_mut() else {
        return;
    };

    for key in &property_keys {
        let already_listed = required_array
            .iter()
            .any(|value| value.as_str() == Some(key));

        if !already_listed {
            required_array.push(Value::String(key.clone()));
        }
    }
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
    /// Parses a full JSON response object into the top-level answer and
    /// question fields.
    fn test_parse_agent_response_structured_json_payload() {
        // Arrange
        let raw = r#"{"answer":"Here is my analysis.","questions":[],"summary":null}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(
            response,
            AgentResponse {
                answer: "Here is my analysis.".to_string(),
                questions: Vec::new(),
                summary: None,
            }
        );
        assert_eq!(response.to_display_text(), "Here is my analysis.");
    }

    #[test]
    /// Strict parsing accepts a complete schema payload.
    fn test_parse_agent_response_strict_structured_json_payload() {
        // Arrange
        let raw = r#"{"answer":"Here is my analysis.","questions":[],"summary":null}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response,
            Ok(AgentResponse {
                answer: "Here is my analysis.".to_string(),
                questions: Vec::new(),
                summary: None,
            })
        );
    }

    #[test]
    /// Strict parsing extracts and parses the first JSON object in mixed text.
    fn test_parse_agent_response_strict_extracts_json_object_from_wrapped_text() {
        // Arrange
        let raw =
            "Header text\n{\"answer\":\"Done.\",\"questions\":[],\"summary\":null}\nFooter text";

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response,
            Ok(AgentResponse {
                answer: "Done.".to_string(),
                questions: Vec::new(),
                summary: None,
            })
        );
    }

    #[test]
    /// Strict parsing skips invalid embedded objects before a valid payload.
    fn test_parse_agent_response_strict_skips_invalid_embedded_json_before_valid_payload() {
        // Arrange
        let raw = concat!(
            "Pseudo schema\n",
            "{\"answer\":42,\"questions\":\"bad\"}\n",
            "Actual payload\n",
            "{\"answer\":\"Recovered.\",\"questions\":[],\"summary\":null}"
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response,
            Ok(AgentResponse {
                answer: "Recovered.".to_string(),
                questions: Vec::new(),
                summary: None,
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
        let raw = r#"{"answer":"Done.","questions":[],"summary":null}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some("Done.".to_string()));
    }

    #[test]
    /// Suppresses complete structured payloads that contain only questions.
    fn test_normalize_stream_assistant_chunk_question_only_payload() {
        // Arrange
        let raw =
            r#"{"answer":"","questions":[{"text":"Need details?","options":[]}],"summary":null}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, None);
    }

    #[test]
    /// Suppresses partial protocol JSON fragments from streamed output.
    fn test_normalize_stream_assistant_chunk_protocol_fragment() {
        // Arrange
        let raw = r#"{"answer":"partial","#;

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
    /// Preserves answer and question text in display order.
    fn test_parse_agent_response_structured_json_with_questions() {
        // Arrange
        let raw = r#"{"answer":"Completed implementation.","questions":[{"text":"Need one decision.","options":[]}],"summary":null}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(
            response.to_display_text(),
            "Completed implementation.\n\nNeed one decision."
        );
    }

    #[test]
    /// Builds transcript text from only the top-level answer field.
    fn test_to_answer_display_text_uses_only_answer_field() {
        // Arrange
        let response = AgentResponse {
            answer: "Completed implementation.".to_string(),
            questions: vec![QuestionItem::new("Need one decision.")],
            summary: None,
        };

        // Act
        let display_text = response.to_answer_display_text();

        // Assert
        assert_eq!(display_text, "Completed implementation.");
    }

    #[test]
    /// Returns the answer as one single ordered transcript item.
    fn test_answers_returns_single_answer_item() {
        // Arrange
        let response = AgentResponse {
            answer: "Completed implementation.".to_string(),
            questions: vec![QuestionItem::new("Need one decision.")],
            summary: None,
        };

        // Act
        let answers = response.answers();

        // Assert
        assert_eq!(answers, vec!["Completed implementation.".to_string()]);
    }

    #[test]
    /// Returns ordered clarification questions for question-mode routing.
    fn test_question_items_returns_only_question_items_in_order() {
        // Arrange
        let response = AgentResponse {
            answer: "Completed implementation.".to_string(),
            questions: vec![
                QuestionItem::new("Need one decision."),
                QuestionItem::new("Need migration details?"),
            ],
            summary: None,
        };

        // Act
        let items = response.question_items();

        // Assert
        assert_eq!(
            items,
            vec![
                QuestionItem {
                    options: Vec::new(),
                    text: "Need one decision.".to_string(),
                },
                QuestionItem {
                    options: Vec::new(),
                    text: "Need migration details?".to_string(),
                },
            ]
        );
    }

    #[test]
    /// Caps extracted question items at [`MAX_QUESTIONS`].
    fn test_question_items_caps_at_max_questions() {
        // Arrange
        let questions: Vec<QuestionItem> = (0..20)
            .map(|index| QuestionItem::new(format!("Question {index}?")))
            .collect();
        let response = AgentResponse {
            answer: String::new(),
            questions,
            summary: None,
        };

        // Act
        let items = response.question_items();

        // Assert
        assert_eq!(items.len(), MAX_QUESTIONS);
        assert_eq!(items[0].text, "Question 0?");
        assert_eq!(
            items[MAX_QUESTIONS - 1].text,
            format!("Question {}?", MAX_QUESTIONS - 1)
        );
    }

    #[test]
    /// Extracts question items with predefined answer options.
    fn test_question_items_preserves_options() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: vec![QuestionItem::with_options(
                "Which approach?",
                vec!["Option A".to_string(), "Option B".to_string()],
            )],
            summary: None,
        };

        // Act
        let items = response.question_items();

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Which approach?");
        assert_eq!(items[0].options, vec!["Option A", "Option B"]);
    }

    #[test]
    /// Parses a structured response summary from JSON.
    fn test_parse_agent_response_preserves_summary_field() {
        // Arrange
        let raw = r#"{"answer":"Done.","questions":[],"summary":{"turn":"- Updated the protocol payload.","session":"- Session branch now uses structured summaries."}}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(
            response.summary,
            Some(AgentResponseSummary {
                session: "- Session branch now uses structured summaries.".to_string(),
                turn: "- Updated the protocol payload.".to_string(),
            })
        );
    }

    #[test]
    /// Verifies session-turn responses synthesize an empty summary when the
    /// provider returns `summary: null`.
    fn test_normalize_turn_response_fills_missing_summary_for_session_turn() {
        // Arrange
        let response = AgentResponse {
            answer: "Done.".to_string(),
            questions: Vec::new(),
            summary: None,
        };

        // Act
        let normalized = normalize_turn_response(response, ProtocolRequestProfile::SessionTurn);

        // Assert
        assert_eq!(
            normalized.summary,
            Some(AgentResponseSummary {
                turn: String::new(),
                session: String::new(),
            })
        );
    }

    #[test]
    /// Verifies utility-prompt responses keep `summary: null`.
    fn test_normalize_turn_response_keeps_missing_summary_for_utility_prompt() {
        // Arrange
        let response = AgentResponse {
            answer: "Done.".to_string(),
            questions: Vec::new(),
            summary: None,
        };

        // Act
        let normalized = normalize_turn_response(response, ProtocolRequestProfile::UtilityPrompt);

        // Assert
        assert_eq!(normalized.summary, None);
    }

    #[test]
    /// Parses question options from JSON.
    fn test_parse_agent_response_question_with_options() {
        // Arrange
        let raw = r#"{"answer":"","questions":[{"text":"Pick one:","options":["A","B","C"]}],"summary":null}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.questions.len(), 1);
        assert_eq!(response.questions[0].options, vec!["A", "B", "C"]);
    }

    #[test]
    /// Parses a response when `questions` is omitted and defaults it to an
    /// empty array.
    fn test_parse_agent_response_defaults_missing_questions_field() {
        // Arrange
        let raw = r#"{"answer":"Done.","summary":null}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.answer, "Done.");
        assert!(response.questions.is_empty());
    }

    #[test]
    /// Parses a response when `answer` is omitted and defaults it to an empty
    /// string.
    fn test_parse_agent_response_defaults_missing_answer_field() {
        // Arrange
        let raw = r#"{"questions":[{"text":"Need details?","options":[]}],"summary":null}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.answer, "");
        assert_eq!(response.questions, vec![QuestionItem::new("Need details?")]);
    }

    #[test]
    /// Parses question items when `options` is omitted and defaults it to an
    /// empty array.
    fn test_parse_agent_response_defaults_missing_question_options_field() {
        // Arrange
        let raw = r#"{"answer":"","questions":[{"text":"Need details?"}],"summary":null}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.questions, vec![QuestionItem::new("Need details?")]);
    }

    #[test]
    /// Falls back to plain text for payloads with no answer and no questions.
    fn test_parse_agent_response_empty_payload_falls_back_to_plain_text() {
        // Arrange
        let raw = r#"{"answer":"","questions":[],"summary":null}"#;

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
        let raw = "```json\n{\"answer\":\"\",\"questions\":[{\"text\":\"Need \
                   details.\",\"options\":[]}],\"summary\":null}\n```";

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(
            response,
            AgentResponse {
                answer: String::new(),
                questions: vec![QuestionItem::new("Need details.")],
                summary: None,
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
        let raw = r#"{"answer":"Response text.","questions":[],"summary":null,"future_field":true,"extra":42}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response, AgentResponse::plain(raw));
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Falls back to plain text when question entries include unknown fields.
    fn test_parse_agent_response_unknown_question_fields_fallback() {
        // Arrange
        let raw = r#"{"answer":"","questions":[{"text":"Need details","options":[],"variants":["A","B"]}],"summary":null}"#;

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
            answer: "Step 1".to_string(),
            questions: vec![QuestionItem::new("Need one decision")],
            summary: Some(AgentResponseSummary {
                session: "- Session changes remain pending.".to_string(),
                turn: "- Added the protocol summary field.".to_string(),
            }),
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
                answer: "Hello".to_string(),
                questions: Vec::new(),
                summary: None,
            }
        );
    }

    #[test]
    /// Builds a schema object with required top-level response fields.
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
                .any(|field| field.as_str() == Some("answer"))
        );
        assert!(properties.contains_key("answer"));
        assert!(properties.contains_key("questions"));
        assert!(properties.contains_key("summary"));
    }

    #[test]
    /// Ensures every `properties` object has all keys listed in `required`.
    fn test_agent_response_output_schema_all_properties_are_required() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(
            all_properties_in_required(&schema),
            "every properties key must appear in required for Codex compatibility"
        );
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
    /// Ensures no schema object uses `$ref` with sibling keys.
    fn test_agent_response_output_schema_ref_objects_have_no_sibling_keywords() {
        // Arrange / Act
        let schema = agent_response_output_schema();

        // Assert
        assert!(!contains_ref_with_sibling_keywords(&schema));
    }

    #[test]
    /// Exposes a parseable pretty JSON schema string for prompt templating.
    fn test_agent_response_json_schema_json_is_parseable_value() {
        // Arrange / Act
        let schema_json = agent_response_json_schema_json();
        let parsed_schema: Value =
            serde_json::from_str(&schema_json).expect("schema string should parse as JSON");
        let schema_value = agent_response_json_schema();

        // Assert
        assert_eq!(parsed_schema, schema_value);
    }

    #[test]
    /// Keeps response schemas self-descriptive so inline prompt docs include
    /// explicit top-level `schemars` metadata.
    fn test_agent_response_json_schema_preserves_explicit_payload_metadata() {
        // Arrange / Act
        let schema = agent_response_json_schema();

        // Assert
        assert_eq!(
            schema.get("title").and_then(Value::as_str),
            Some("AgentResponse")
        );
        assert_eq!(
            schema.get("description").and_then(Value::as_str),
            Some(
                "Wire-format protocol payload used for schema-driven provider output. Return this \
                 object as the entire assistant response payload. Providers that support output \
                 schemas (for example, Codex app-server) are asked to emit this object directly."
            )
        );
    }

    #[test]
    /// Keeps nested response-schema models self-descriptive for inline docs.
    fn test_agent_response_json_schema_preserves_nested_metadata() {
        // Arrange / Act
        let schema = agent_response_json_schema();
        let question_definition = schema
            .get("$defs")
            .and_then(|value| value.get("QuestionItem"))
            .and_then(Value::as_object)
            .expect("question definition should exist");
        let summary_definition = schema
            .get("$defs")
            .and_then(|value| value.get("AgentResponseSummary"))
            .and_then(Value::as_object)
            .expect("summary definition should exist");

        // Assert
        assert_eq!(
            question_definition.get("title").and_then(Value::as_str),
            Some("QuestionItem")
        );
        assert_eq!(
            question_definition
                .get("description")
                .and_then(Value::as_str),
            Some(
                "One clarification question emitted by the assistant protocol payload. Keep each \
                 item focused to one actionable decision."
            )
        );
        assert_eq!(
            summary_definition.get("title").and_then(Value::as_str),
            Some("AgentResponseSummary")
        );
        assert_eq!(
            summary_definition
                .get("description")
                .and_then(Value::as_str),
            Some(
                "Structured session summary block emitted alongside protocol messages instead of \
                 embedding the change summary inside `answer` markdown on session-discussion \
                 turns."
            )
        );
    }

    #[test]
    /// Keeps response-schema fields self-descriptive for inline schema docs.
    fn test_agent_response_json_schema_preserves_field_metadata() {
        // Arrange / Act
        let schema = agent_response_json_schema();
        let response_properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("response properties should exist");
        let question_definition = schema
            .get("$defs")
            .and_then(|value| value.get("QuestionItem"))
            .and_then(Value::as_object)
            .expect("question definition should exist");
        let summary_definition = schema
            .get("$defs")
            .and_then(|value| value.get("AgentResponseSummary"))
            .and_then(Value::as_object)
            .expect("summary definition should exist");
        let question_properties = question_definition
            .get("properties")
            .and_then(Value::as_object)
            .expect("question properties should exist");
        let summary_properties = summary_definition
            .get("properties")
            .and_then(Value::as_object)
            .expect("summary properties should exist");
        let expected_questions_description = format!(
            "Ordered clarification questions emitted for this turn. Emit at most {MAX_QUESTIONS} \
             items, and use an empty array when no user input is required. Defaults to an empty \
             array when omitted."
        );

        // Assert
        assert_schema_property_title_and_description(
            response_properties,
            "answer",
            "answer",
            "Markdown answer text for delivered work, status updates, or concise completion \
             notes. Keep clarification requests out of this field and emit them through \
             `questions` instead. Defaults to an empty string when omitted.",
        );
        assert_eq!(
            response_properties
                .get("questions")
                .and_then(|value| value.get("description"))
                .and_then(Value::as_str),
            Some(expected_questions_description.as_str())
        );
        assert_schema_property_title_and_description(
            response_properties,
            "summary",
            "summary",
            "Structured summary for session-discussion turns, kept outside `answer` markdown. Use \
             `null` for one-shot prompts and legacy payloads.",
        );
        assert_schema_property_title_and_description(
            question_properties,
            "text",
            "text",
            "Human-readable markdown text for this question. Ask one specific actionable question \
             instead of bundling multiple decisions into one item.",
        );
        assert_schema_property_title(question_properties, "options", "options");
        assert_schema_property_title_and_description(
            summary_properties,
            "turn",
            "turn",
            "Concise summary of only the work completed in the current turn.",
        );
        assert_schema_property_title_and_description(
            summary_properties,
            "session",
            "session",
            "Cumulative summary of active changes on the current session branch.",
        );
    }

    #[test]
    /// Preserves optional prompt fields in the raw schema instead of forcing
    /// transport-only requirements into prompt docs.
    fn test_agent_response_json_schema_keeps_optional_summary_field() {
        // Arrange / Act
        let schema = agent_response_json_schema();
        let response_required_fields = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let question_definition = schema
            .get("$defs")
            .and_then(|value| value.get("QuestionItem"))
            .and_then(Value::as_object)
            .expect("question definition should exist");
        let question_required_fields = question_definition
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Assert
        assert!(
            response_required_fields
                .iter()
                .all(|field| field.as_str() != Some("summary")),
            "raw prompt schema should keep optional summary fields optional"
        );
        assert!(
            question_required_fields
                .iter()
                .all(|field| field.as_str() != Some("options")),
            "question schema should keep `options` optional for omitted empty lists"
        );
    }

    #[test]
    /// Exposes a parseable pretty JSON schema string for transport-level
    /// schema enforcement.
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

    /// Recursively checks that every object with `properties` lists all
    /// property keys in `required`.
    fn all_properties_in_required(value: &Value) -> bool {
        match value {
            Value::Object(object) => {
                if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                    let required_keys: Vec<&str> = object
                        .get("required")
                        .and_then(Value::as_array)
                        .map(|array| array.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();

                    for key in properties.keys() {
                        if !required_keys.contains(&key.as_str()) {
                            return false;
                        }
                    }
                }

                object.values().all(all_properties_in_required)
            }
            Value::Array(array) => array.iter().all(all_properties_in_required),
            _ => true,
        }
    }

    /// Asserts one property schema has the expected `title`.
    fn assert_schema_property_title(
        properties: &serde_json::Map<String, Value>,
        property_name: &str,
        expected_title: &str,
    ) {
        assert_eq!(
            properties
                .get(property_name)
                .and_then(|value| value.get("title"))
                .and_then(Value::as_str),
            Some(expected_title)
        );
    }

    /// Asserts one property schema has the expected `title` and
    /// `description`.
    fn assert_schema_property_title_and_description(
        properties: &serde_json::Map<String, Value>,
        property_name: &str,
        expected_title: &str,
        expected_description: &str,
    ) {
        assert_schema_property_title(properties, property_name, expected_title);
        assert_eq!(
            properties
                .get(property_name)
                .and_then(|value| value.get("description"))
                .and_then(Value::as_str),
            Some(expected_description)
        );
    }
}
