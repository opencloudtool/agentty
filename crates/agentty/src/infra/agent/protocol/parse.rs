//! Structured response parsing and streaming normalization helpers.

use super::model::{
    AgentResponse, AgentResponseParseError, AgentResponseSummary, ProtocolRequestProfile,
};

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

    if let Ok(response) = parse_structured_json_response(trimmed) {
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

    if let Ok(response) = parse_structured_json_response(raw) {
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

/// Attempts to parse one schema-driven structured JSON response.
///
/// This relies on the protocol wire type to define the accepted response
/// shape.
fn parse_structured_json_response(raw: &str) -> Result<AgentResponse, serde_json::Error> {
    serde_json::from_str(raw.trim())
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

/// Parses the first valid protocol payload embedded in free-form text.
///
/// This skips non-protocol or invalid JSON objects that may appear before the
/// actual protocol payload.
fn parse_first_valid_embedded_json_response(raw: &str) -> Option<AgentResponse> {
    let mut search_from = 0;

    while let Some((start_index, end_index)) = extract_next_json_object_range(raw, search_from) {
        let json_candidate = raw.get(start_index..end_index)?;
        if let Ok(parsed_response) = parse_structured_json_response(json_candidate) {
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
        assert_eq!(response.answer, "Here is my analysis.");
        assert!(response.questions.is_empty());
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
    /// Strict parsing extracts a valid embedded payload from wrapped text.
    fn test_parse_agent_response_strict_extracts_json_object_from_wrapped_text() {
        // Arrange
        let raw = concat!(
            "Some wrapper text\n",
            r#"{"answer":"Recovered payload","questions":[],"summary":null}"#
        );

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse").answer,
            "Recovered payload"
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
        assert_eq!(response, Err(AgentResponseParseError::InvalidFormat));
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
    /// Falls back to plain text when the payload is not schema-compliant.
    fn test_parse_agent_response_non_schema_payload_falls_back_to_plain_text() {
        // Arrange
        let raw = r#"{"message":"not the expected shape"}"#;

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.to_display_text(), raw);
    }

    #[test]
    /// Strict parsing accepts an empty JSON object because the protocol wire
    /// type supplies defaults for omitted fields.
    fn test_parse_agent_response_strict_accepts_empty_json_object() {
        // Arrange
        let raw = "{}";

        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(
            response.expect("response should parse"),
            AgentResponse::plain("")
        );
    }

    #[test]
    /// Preserves wrapped code-fence payloads by extracting the JSON object.
    fn test_parse_agent_response_structured_json_in_code_fence_extracts_payload() {
        // Arrange
        let raw = concat!(
            "```json\n",
            r#"{"answer":"Need details.","questions":[],"summary":null}"#,
            "\n```"
        );

        // Act
        let response = parse_agent_response(raw);

        // Assert
        assert_eq!(response.to_display_text(), "Need details.");
    }
}
