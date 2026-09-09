//! Structured response parsing and streaming normalization helpers.

use serde_json::Value;

use super::model::{AgentResponse, AgentResponseParseError, ProtocolRequestProfile};
use super::review::{FocusedReview, FocusedReviewSeverity};

/// Top-level keys the protocol recognizes in a structured response payload.
const PROTOCOL_KEYS: &[&str] = &[
    "answer",
    "questions",
    "review_comment_outcomes",
    "subtasks",
    "verification_verdicts",
];

/// Parses one raw assistant message strictly as protocol payload.
///
/// The final assistant payload must match [`AgentResponse`] and contain at
/// least one recognized protocol key (`answer`, `questions`,
/// `review_comment_outcomes`, `subtasks`, or `verification_verdicts`).
///
/// When a provider prepends stray prose before the final schema object, this
/// still recovers the trailing protocol payload as long as nothing except
/// whitespace follows the JSON object. As a further resilience fallback,
/// markdown code fences wrapping the JSON object are stripped before parsing
/// when neither direct parsing nor trailing-object recovery succeeds. An
/// additional fallback extracts JSON from an embedded code fence preceded by
/// prose text (e.g., commentary followed by a fenced JSON block).
/// Top-level fields may rely on the wire type's defaults.
///
/// # Errors
/// Returns [`AgentResponseParseError`] when no valid protocol payload is found.
pub fn parse_agent_response_strict(raw: &str) -> Result<AgentResponse, AgentResponseParseError> {
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

    if let Some(inner) = find_embedded_code_fence_content(trimmed)
        && let Some(response) = parse_structured_json_response_with_recovery(inner)
    {
        return Ok(response);
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

/// Parses one response against the schema selected for its request profile.
///
/// Focused reviews arrive as direct [`FocusedReview`] objects so native
/// provider schemas can enforce their fields. The validated object is
/// normalized back into `AgentResponse::answer` for existing application
/// consumers.
///
/// # Errors
/// Returns [`AgentResponseParseError`] when the response is empty or does not
/// match the profile's structured response schema.
pub fn parse_protocol_response_strict(
    raw: &str,
    profile: ProtocolRequestProfile,
) -> Result<AgentResponse, AgentResponseParseError> {
    if !matches!(profile, ProtocolRequestProfile::FocusedReview) {
        return parse_agent_response_strict(raw);
    }

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AgentResponseParseError::Empty);
    }

    let review = serde_json::from_str::<FocusedReview>(trimmed).map_err(|error| {
        AgentResponseParseError::InvalidFormat {
            reason: format!("focused review parse failed ({error})"),
        }
    })?;
    let answer = focused_review_answer(review);

    Ok(AgentResponse::plain(answer))
}

/// Serializes a validated focused review through infallible JSON values.
fn focused_review_answer(review: FocusedReview) -> String {
    let project_impact = review
        .project_impact
        .into_iter()
        .map(Value::String)
        .collect();
    let suggestions = review
        .suggestions
        .into_iter()
        .map(|suggestion| {
            let severity = match suggestion.severity {
                FocusedReviewSeverity::High => "high",
                FocusedReviewSeverity::Medium => "medium",
            };

            Value::Object(
                [
                    ("details".to_string(), Value::String(suggestion.details)),
                    ("severity".to_string(), Value::String(severity.to_string())),
                ]
                .into_iter()
                .collect(),
            )
        })
        .collect();

    Value::Object(
        [
            ("project_impact".to_string(), Value::Array(project_impact)),
            ("suggestions".to_string(), Value::Array(suggestions)),
        ]
        .into_iter()
        .collect(),
    )
    .to_string()
}

/// Builds one multi-line debug report for a protocol parsing failure.
///
/// The report summarizes response sizing, markdown wrapping, JSON parse
/// diagnostics, and any visible top-level keys so schema mismatch errors
/// include enough context to diagnose malformed provider output quickly.
///
/// Every line is *derived* metadata: sizes, parser locations, and key names.
/// No provider payload text is reproduced. Turn errors are rendered into the
/// session transcript, so quoting the payload here would print raw provider
/// output into the chat.
pub fn format_protocol_parse_debug_details(raw: &str) -> String {
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

    detail_lines.join("\n")
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

/// Returns whether a parsed JSON value is an object containing at least one
/// recognized protocol key.
fn value_has_recognized_protocol_key(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| PROTOCOL_KEYS.iter().any(|key| object.contains_key(*key)))
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

/// Parses one full protocol payload and then falls back to recovering a
/// trailing schema object from wrapped provider output.
fn parse_structured_json_response_with_recovery(raw: &str) -> Option<AgentResponse> {
    parse_structured_json_response(raw).or_else(|| recover_embedded_structured_json_response(raw))
}

/// Attempts to parse one schema-driven structured JSON response.
///
/// The raw text must parse as a JSON object containing at least one
/// recognized protocol key from [`PROTOCOL_KEYS`]. Returns `None` when parsing
/// fails or no recognized keys are present.
fn parse_structured_json_response(raw: &str) -> Option<AgentResponse> {
    parse_structured_json_response_with_reason(raw).ok()
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

/// Extracts the inner content from the last markdown code fence embedded in a
/// response that also contains surrounding prose text.
///
/// Handles the pattern where a provider prepends commentary before a fenced
/// JSON payload (e.g., `"Some explanation\n` ` ```json\n{...}\n``` ` `"`).
fn find_embedded_code_fence_content(raw: &str) -> Option<&str> {
    let closing_fence_start = raw.rfind("```")?;
    let before_closing = raw[..closing_fence_start].trim_end();

    let opening_fence_start = before_closing.rfind("```")?;
    let after_opening_backticks = &before_closing[opening_fence_start + 3..];

    let body_start = after_opening_backticks.find('\n').map(|index| index + 1)?;
    let inner = &after_opening_backticks[body_start..];
    let trimmed = inner.trim();

    if trimmed.is_empty() {
        return None;
    }

    Some(trimmed)
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

/// Formats one debug list as a comma-separated string or `(none)`.
fn format_debug_list(items: &[String]) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }

    items.join(", ")
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

#[cfg(test)]
#[path = "parse_test.rs"]
mod tests;
