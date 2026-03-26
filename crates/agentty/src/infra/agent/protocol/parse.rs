//! Structured response parsing and streaming normalization helpers.

use serde_json::Value;

use super::model::{
    AgentResponse, AgentResponseParseError, AgentResponseSummary, ProtocolRequestProfile,
};

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
/// The final assistant payload must match [`AgentResponse`] and contain at
/// least one recognized protocol key (`answer`, `questions`, or `summary`).
///
/// When a provider prepends stray prose before the final schema object, this
/// still recovers the trailing protocol payload as long as nothing except
/// whitespace follows the JSON object. As a further resilience fallback,
/// markdown code fences wrapping the JSON object are stripped before parsing
/// when neither direct parsing nor trailing-object recovery succeeds.
/// Top-level fields may rely on the wire type's defaults.
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

    let direct_parse = parse_structured_json_response_with_reason(trimmed);
    if let Ok(response) = direct_parse {
        return Ok(response);
    }

    let direct_parse_error = match direct_parse {
        Err(error) => error.to_string(),
        Ok(_) => unreachable!("direct parse branch already returned successful parse"),
    };

    if let Some(inner) = strip_markdown_code_fence(trimmed) {
        if let Some(response) = parse_structured_json_response_with_recovery(inner) {
            return Ok(response);
        }

        let fence_parse_error = parse_structured_json_response_with_reason(inner)
            .err()
            .map_or_else(
                || "no protocol payload found in markdown code fence".to_string(),
                |error| error.to_string(),
            );

        return Err(AgentResponseParseError::InvalidFormat {
            reason: format!("markdown code fence extraction failed ({fence_parse_error})"),
        });
    }

    if let Some(response) = recover_embedded_structured_json_response(trimmed) {
        return Ok(response);
    }

    Err(AgentResponseParseError::InvalidFormat {
        reason: format!(
            "direct parse failed ({direct_parse_error}); no markdown wrapper/embedded protocol \
             object found"
        ),
    })
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

    if let Some(response) = parse_structured_json_response_with_recovery(raw) {
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

/// Builds one multi-line debug report for a protocol parsing failure.
///
/// The report summarizes response sizing, markdown wrapping, JSON parse
/// diagnostics, and any visible top-level keys so schema mismatch errors
/// include enough context to diagnose malformed provider output quickly.
pub(crate) fn format_protocol_parse_debug_details(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut detail_lines = vec![
        format!("response_len: {} chars", raw.chars().count()),
        format!("response_lines: {}", raw.lines().count()),
        format!("trimmed_len: {} chars", trimmed.chars().count()),
        format!(
            "wrapped_in_markdown_fence: {}",
            strip_markdown_code_fence(trimmed).is_some()
        ),
    ];

    if trimmed.is_empty() {
        return detail_lines.join("\n");
    }

    push_character_boundary_debug_lines(&mut detail_lines, trimmed);
    push_json_debug_lines(&mut detail_lines, "direct_json", trimmed);

    if let Some(inner) = strip_markdown_code_fence(trimmed) {
        detail_lines.push(format!(
            "code_fence_inner_len: {} chars",
            inner.chars().count()
        ));
        push_json_debug_lines(&mut detail_lines, "code_fence_json", inner);
    }

    if let Some(embedded_value) = find_last_embedded_json_value(trimmed) {
        detail_lines.push("embedded_json_candidate: found".to_string());
        push_json_value_debug_lines(&mut detail_lines, "embedded_json", &embedded_value);
    } else {
        detail_lines.push("embedded_json_candidate: none".to_string());
    }

    detail_lines.push(format!(
        "response_preview:\n{}",
        format_debug_preview(trimmed, 240)
    ));

    detail_lines.join("\n")
}

/// Attempts to parse one schema-driven structured JSON response.
///
/// The raw text must parse as a JSON object containing at least one
/// recognized protocol key (`answer`, `questions`, or `summary`). Returns
/// `None` when parsing fails or no recognized keys are present.
fn parse_structured_json_response(raw: &str) -> Option<AgentResponse> {
    parse_structured_json_response_with_reason(raw).ok()
}

/// Parses one schema-driven JSON response and returns the structured error
/// detail when the payload cannot be parsed or validated.
fn parse_structured_json_response_with_reason(
    raw: &str,
) -> Result<AgentResponse, AgentResponseParseError> {
    let value: Value = serde_json::from_str(raw.trim()).map_err(|error| {
        AgentResponseParseError::InvalidFormat {
            reason: format!("invalid JSON ({error})"),
        }
    })?;

    if !value_has_recognized_protocol_key(&value) {
        return Err(AgentResponseParseError::InvalidFormat {
            reason: format!(
                "json object is missing all protocol keys ({})",
                PROTOCOL_KEYS.join(", ")
            ),
        });
    }

    serde_json::from_value(value).map_err(|error| AgentResponseParseError::InvalidFormat {
        reason: format!("schema validation failed ({error})"),
    })
}

/// Top-level keys the protocol recognizes in a structured response payload.
const PROTOCOL_KEYS: &[&str] = &["answer", "questions", "summary"];

/// Returns whether a parsed JSON value is an object containing at least one
/// recognized protocol key.
fn value_has_recognized_protocol_key(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| PROTOCOL_KEYS.iter().any(|key| object.contains_key(*key)))
}

/// Parses one full protocol payload and then falls back to recovering a
/// trailing schema object from wrapped provider output.
fn parse_structured_json_response_with_recovery(raw: &str) -> Option<AgentResponse> {
    parse_structured_json_response(raw).or_else(|| recover_embedded_structured_json_response(raw))
}

/// Recovers one trailing protocol payload from provider output that starts
/// with extra prose before the final JSON object.
///
/// This intentionally keeps trailing text strict: once a candidate JSON object
/// parses successfully, only whitespace may remain after it. The candidate
/// must also contain at least one recognized protocol key.
fn recover_embedded_structured_json_response(raw: &str) -> Option<AgentResponse> {
    let value = find_last_embedded_json_value(raw)?;
    if !value_has_recognized_protocol_key(&value) {
        return None;
    }

    serde_json::from_value(value).ok()
}

/// Strips a leading markdown code fence and trailing closing fence from a
/// trimmed response payload, returning the inner content if the pattern
/// matches.
fn strip_markdown_code_fence(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("```")?;
    let body_start = rest.find('\n').map(|index| index + 1)?;
    let body = &rest[body_start..];
    let inner = body.strip_suffix("```")?.trim();

    if inner.is_empty() {
        return None;
    }

    Some(inner)
}

/// Finds the last JSON object embedded in a response when it consumes the full
/// trailing suffix except for whitespace.
fn find_last_embedded_json_value(raw: &str) -> Option<Value> {
    for (start_index, _) in raw.match_indices('{').rev() {
        let candidate = &raw[start_index..];
        let mut deserializer = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
        let Some(Ok(value)) = deserializer.next() else {
            continue;
        };
        let trailing_text = &candidate[deserializer.byte_offset()..];
        if !trailing_text.trim().is_empty() {
            continue;
        }

        return Some(value);
    }

    None
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

/// Appends stable character-boundary diagnostics for one trimmed response.
fn push_character_boundary_debug_lines(detail_lines: &mut Vec<String>, trimmed: &str) {
    if let Some(first_character) = trimmed.chars().next() {
        detail_lines.push(format!("first_non_whitespace_char: {first_character:?}"));
    }

    if let Some(last_character) = trimmed.chars().last() {
        detail_lines.push(format!("last_non_whitespace_char: {last_character:?}"));
    }
}

/// Appends either JSON parse failure details or top-level JSON shape details.
fn push_json_debug_lines(detail_lines: &mut Vec<String>, label: &str, raw: &str) {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => push_json_value_debug_lines(detail_lines, label, &value),
        Err(error) => {
            detail_lines.push(format!("{label}_error: {error}"));
            detail_lines.push(format!(
                "{label}_error_category: {}",
                describe_json_error_category(&error)
            ));
            detail_lines.push(format!(
                "{label}_error_location: line {}, column {}",
                error.line(),
                error.column()
            ));
        }
    }
}

/// Appends the top-level JSON type and protocol-key visibility for one value.
fn push_json_value_debug_lines(detail_lines: &mut Vec<String>, label: &str, value: &Value) {
    detail_lines.push(format!("{label}_type: {}", describe_json_type(value)));

    if let Some(object) = value.as_object() {
        let mut keys = object.keys().cloned().collect::<Vec<_>>();
        keys.sort_unstable();

        let recognized_keys = PROTOCOL_KEYS
            .iter()
            .filter(|key| object.contains_key(**key))
            .map(|key| (*key).to_string())
            .collect::<Vec<_>>();
        let missing_keys = PROTOCOL_KEYS
            .iter()
            .filter(|key| !object.contains_key(**key))
            .map(|key| (*key).to_string())
            .collect::<Vec<_>>();

        detail_lines.push(format!("{label}_keys: {}", format_debug_list(&keys)));
        detail_lines.push(format!(
            "{label}_recognized_protocol_keys: {}",
            format_debug_list(&recognized_keys)
        ));
        detail_lines.push(format!(
            "{label}_missing_protocol_keys: {}",
            format_debug_list(&missing_keys)
        ));
    }
}

/// Returns one stable label for a top-level JSON value type.
fn describe_json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Returns one stable label for the serde JSON error category.
fn describe_json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

/// Formats one debug list as a comma-separated string or `(none)`.
fn format_debug_list(items: &[String]) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }

    items.join(", ")
}

/// Truncates a debug preview while preserving the original leading content.
fn format_debug_preview(raw: &str, max_chars: usize) -> String {
    let preview = raw.chars().take(max_chars).collect::<String>();
    let total_chars = raw.chars().count();
    if total_chars <= max_chars {
        return preview;
    }

    format!(
        "{preview}\n... [truncated {} chars]",
        total_chars - max_chars
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Strict parsing accepts a complete schema payload.
    fn test_parse_agent_response_strict_structured_json_payload() {
        // Arrange
        let raw = r#"{"answer":"Here is my analysis.","questions":[],"summary":null}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse").answer,
            "Here is my analysis."
        );
    }

    #[test]
    /// Strict parsing accepts summary-only payloads that still match the
    /// protocol shape.
    fn test_parse_agent_response_strict_summary_only_payload() {
        // Arrange
        let raw = r#"{"summary":{"session":"Current diff summary","turn":"Turn summary"}}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse").summary,
            Some(AgentResponseSummary {
                session: "Current diff summary".to_string(),
                turn: "Turn summary".to_string(),
            })
        );
    }

    #[test]
    /// Strict parsing recovers a trailing protocol payload when a provider
    /// prepends extra prose before the final JSON object.
    fn test_parse_agent_response_strict_recovers_wrapped_text() {
        // Arrange
        let raw = concat!(
            "Some wrapper text\n",
            r#"{"answer":"Recovered payload","questions":[],"summary":null}"#
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse"),
            AgentResponse::plain("Recovered payload")
        );
    }

    #[test]
    /// Strict parsing rejects plain text that contains no protocol payload.
    fn test_parse_agent_response_strict_rejects_plain_text() {
        // Arrange
        let raw = "plain text";

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert!(response.is_err());
    }

    #[test]
    /// Debug formatting reports JSON parser location details for plain-text
    /// responses that never produced protocol JSON.
    fn test_format_protocol_parse_debug_details_reports_plain_text_json_error() {
        // Arrange
        let raw = "plain text";

        // Act
        let details = format_protocol_parse_debug_details(raw);

        // Assert
        assert!(details.contains("response_len: 10 chars"));
        assert!(details.contains("first_non_whitespace_char: 'p'"));
        assert!(details.contains("direct_json_error_category: syntax"));
        assert!(details.contains("direct_json_error_location: line 1, column 1"));
        assert!(details.contains("embedded_json_candidate: none"));
    }

    #[test]
    /// Debug formatting reports visible top-level keys when the response is
    /// valid JSON but does not include any protocol fields.
    fn test_format_protocol_parse_debug_details_reports_unrecognized_json_keys() {
        // Arrange
        let raw = r#"{"message":"not the expected shape"}"#;

        // Act
        let details = format_protocol_parse_debug_details(raw);

        // Assert
        assert!(details.contains("direct_json_type: object"));
        assert!(details.contains("direct_json_keys: message"));
        assert!(details.contains("direct_json_recognized_protocol_keys: (none)"));
        assert!(details.contains("direct_json_missing_protocol_keys: answer, questions, summary"));
    }

    #[test]
    /// Keeps only the answer field when a full structured payload arrives in a
    /// stream chunk.
    fn test_normalize_stream_assistant_chunk_structured_payload() {
        // Arrange
        let raw = r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}],"summary":null}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some("Done.".to_string()));
    }

    #[test]
    /// Keeps only the answer field when wrapped prose precedes a full
    /// structured payload in one stream chunk.
    fn test_normalize_stream_assistant_chunk_recovers_wrapped_structured_payload() {
        // Arrange
        let raw = concat!(
            "Let me format that cleanly.\n",
            r#"{"answer":"Done.","questions":[],"summary":null}"#
        );

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some("Done.".to_string()));
    }

    #[test]
    /// Suppresses question-only payloads while streaming.
    fn test_normalize_stream_assistant_chunk_question_only_payload() {
        // Arrange
        let raw = r#"{"answer":"","questions":[{"text":"Need clarification.","options":[]}],"summary":null}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, None);
    }

    #[test]
    /// Suppresses summary-only payloads while streaming because they do not
    /// add transcript text.
    fn test_normalize_stream_assistant_chunk_summary_only_payload() {
        // Arrange
        let raw = r#"{"summary":{"session":"Current diff summary","turn":"Turn summary"}}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, None);
    }

    #[test]
    /// Suppresses partial protocol fragments while streaming.
    fn test_normalize_stream_assistant_chunk_protocol_fragment() {
        // Arrange
        let raw = r#"{"answer":"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, None);
    }

    #[test]
    /// Preserves plain text stream chunks unchanged.
    fn test_normalize_stream_assistant_chunk_plain_text() {
        // Arrange
        let raw = "Plain response line";

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some("Plain response line".to_string()));
    }

    #[test]
    /// Fills in empty summaries for session turns.
    fn test_normalize_turn_response_fills_missing_summary_for_session_turn() {
        // Arrange
        let response = AgentResponse::plain("done");

        // Act
        let normalized = normalize_turn_response(response, ProtocolRequestProfile::SessionTurn);

        // Assert
        assert_eq!(
            normalized.summary,
            Some(AgentResponseSummary {
                session: String::new(),
                turn: String::new(),
            })
        );
    }

    #[test]
    /// Leaves one-shot prompt summaries unset.
    fn test_normalize_turn_response_keeps_missing_summary_for_utility_prompt() {
        // Arrange
        let response = AgentResponse::plain("done");

        // Act
        let normalized = normalize_turn_response(response, ProtocolRequestProfile::UtilityPrompt);

        // Assert
        assert_eq!(normalized.summary, None);
    }

    #[test]
    /// Strict parsing rejects JSON objects with only unrecognized fields
    /// because at least one protocol key must be present.
    fn test_parse_agent_response_strict_rejects_unrecognized_only_fields() {
        // Arrange
        let raw = r#"{"message":"not the expected shape"}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert!(response.is_err());
    }

    #[test]
    /// Strict parsing rejects an empty JSON object because no recognized
    /// protocol key is present.
    fn test_parse_agent_response_strict_rejects_empty_json_object() {
        // Arrange
        let raw = "{}";

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert!(response.is_err());
    }

    #[test]
    /// Strict parsing strips code fences and recovers the inner JSON payload.
    fn test_parse_agent_response_strict_strips_code_fenced_payload() {
        // Arrange
        let raw = concat!(
            "```json\n",
            r#"{"answer":"Need details.","questions":[],"summary":null}"#,
            "\n```"
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse").answer,
            "Need details."
        );
    }

    #[test]
    /// Strict parsing strips code fences even when leading/trailing whitespace
    /// surrounds the fenced block.
    fn test_parse_agent_response_strict_strips_code_fenced_payload_with_whitespace() {
        // Arrange
        let raw = concat!(
            "\n\n```json\n",
            r#"{"answer":"Recovered.","questions":[],"summary":null}"#,
            "\n```\n"
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse").answer,
            "Recovered."
        );
    }

    #[test]
    /// Strict parsing strips plain code fences without a language tag.
    fn test_parse_agent_response_strict_strips_plain_code_fenced_payload() {
        // Arrange
        let raw = concat!(
            "```\n",
            r#"{"answer":"Plain fence.","questions":[],"summary":null}"#,
            "\n```"
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse").answer,
            "Plain fence."
        );
    }

    #[test]
    /// Strict parsing tolerates extra top-level fields that providers may add
    /// beyond the protocol schema.
    fn test_parse_agent_response_strict_tolerates_extra_top_level_fields() {
        // Arrange
        let raw =
            r#"{"answer":"Hello.","questions":[],"summary":null,"reasoning":"internal thought"}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(response.expect("response should parse").answer, "Hello.");
    }

    #[test]
    /// Strict parsing tolerates extra fields inside nested summary objects.
    fn test_parse_agent_response_strict_tolerates_extra_summary_fields() {
        // Arrange
        let raw = r#"{"answer":"Done.","questions":[],"summary":{"turn":"Fixed bug","session":"Bug fix session","confidence":"high"}}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        let response = response.expect("response should parse");
        assert_eq!(response.answer, "Done.");
        assert_eq!(
            response.summary,
            Some(AgentResponseSummary {
                session: "Bug fix session".to_string(),
                turn: "Fixed bug".to_string(),
            })
        );
    }

    #[test]
    /// Strict parsing tolerates extra fields inside nested question objects.
    fn test_parse_agent_response_strict_tolerates_extra_question_fields() {
        // Arrange
        let raw = r#"{"answer":"","questions":[{"text":"Which approach?","options":["A","B"],"priority":"high"}],"summary":null}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        let questions = response.expect("response should parse").question_items();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].text, "Which approach?");
    }

    #[test]
    /// Stream normalization tolerates extra fields in a complete structured
    /// payload.
    fn test_normalize_stream_assistant_chunk_tolerates_extra_fields() {
        // Arrange
        let raw = r#"{"answer":"Streaming done.","questions":[],"summary":null,"metadata":{}}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some("Streaming done.".to_string()));
    }

    #[test]
    /// Stream normalization passes unrecognized-only JSON through as plain
    /// text instead of silently suppressing it as an empty protocol response.
    fn test_normalize_stream_assistant_chunk_passes_unrecognized_json_as_plain_text() {
        // Arrange
        let raw = r#"{"message":"not the expected shape"}"#;

        // Act
        let normalized = normalize_stream_assistant_chunk(raw);

        // Assert
        assert_eq!(normalized, Some(raw.to_string()));
    }

    #[test]
    /// Parser accepts a payload with `questions` but no `answer` key,
    /// exercising the documented asymmetry where the parser is lenient
    /// (any recognized key suffices) while the prompt schema requires
    /// `answer`.
    fn test_parse_agent_response_strict_accepts_questions_without_answer() {
        // Arrange
        let raw = r#"{"questions":[{"text":"Which approach?"}]}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        let response = response.expect("parser should accept questions-only payload");
        assert_eq!(response.answer, "");
        assert_eq!(response.question_items().len(), 1);
    }

    #[test]
    /// Parser accepts a payload with `summary` but no `answer` key,
    /// exercising the documented asymmetry where the parser is lenient
    /// (any recognized key suffices) while the prompt schema requires
    /// `answer`.
    fn test_parse_agent_response_strict_accepts_summary_without_answer() {
        // Arrange
        let raw = r#"{"summary":{"turn":"Fixed bug","session":"Bug fix session"}}"#;

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        let response = response.expect("parser should accept summary-only payload");
        assert_eq!(response.answer, "");
        assert_eq!(
            response.summary,
            Some(AgentResponseSummary {
                session: "Bug fix session".to_string(),
                turn: "Fixed bug".to_string(),
            })
        );
    }

    #[test]
    /// Recovery path skips non-protocol JSON objects embedded in prose when
    /// they contain no recognized protocol keys.
    fn test_parse_agent_response_strict_rejects_wrapped_non_protocol_json() {
        // Arrange
        let raw = concat!(
            "Some wrapper text\n",
            r#"{"reasoning":"internal thought","confidence":0.9}"#
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert!(response.is_err());
    }

    #[test]
    /// Strict parsing still rejects trailing wrapper text after a recovered
    /// schema object.
    fn test_parse_agent_response_strict_rejects_trailing_wrapper_after_payload() {
        // Arrange
        let raw = concat!(
            "Some wrapper text\n",
            r#"{"answer":"Recovered payload","questions":[],"summary":null}"#,
            "\ntrailing wrapper text"
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert!(response.is_err());
    }
}
