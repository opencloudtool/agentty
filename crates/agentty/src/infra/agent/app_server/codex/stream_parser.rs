//! Codex app-server stream parsing helpers.

use serde_json::Value;

use crate::infra::agent;
use crate::infra::agent::protocol::parse_agent_response_strict;

/// Extracted assistant message payload from one Codex stream line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExtractedAgentMessage {
    /// Message text extracted from Codex content payloads.
    pub(super) message: String,
    /// Optional phase label emitted by Codex for the assistant item.
    pub(super) phase: Option<String>,
}

/// Returns the completed assistant message that should back the final turn
/// result.
///
/// Preference order is:
/// 1. Latest valid protocol payload that includes `summary`
/// 2. Latest valid protocol payload without `summary`
/// 3. Latest non-empty plain-text assistant message
pub(super) fn preferred_completed_assistant_message(assistant_messages: &[String]) -> String {
    if let Some(protocol_with_summary) = assistant_messages.iter().rev().find_map(|message| {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return None;
        }

        let response = parse_agent_response_strict(trimmed_message).ok()?;
        response.summary.as_ref()?;

        Some(trimmed_message.to_string())
    }) {
        return protocol_with_summary;
    }

    if let Some(protocol_payload) = assistant_messages.iter().rev().find_map(|message| {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return None;
        }

        let response = parse_agent_response_strict(trimmed_message).ok()?;
        if response.answer.trim().is_empty()
            && response.questions.is_empty()
            && response.summary.is_none()
        {
            return None;
        }

        Some(trimmed_message.to_string())
    }) {
        return protocol_payload;
    }

    assistant_messages
        .iter()
        .rev()
        .find_map(|message| {
            let trimmed_message = message.trim();
            if trimmed_message.is_empty() {
                return None;
            }

            Some(trimmed_message.to_string())
        })
        .unwrap_or_default()
}

/// Extracts the turn id from a successful `turn/start` response payload.
pub(super) fn extract_turn_id_from_turn_start_response(response_value: &Value) -> Option<String> {
    let result = response_value.get("result")?;

    result
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            result
                .get("turn")
                .and_then(|turn| turn.get("turnId"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            result
                .get("turn")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            result
                .get("turnId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            result
                .get("turn_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

/// Extracts the active turn id from one `turn/started` notification payload.
///
/// Supports nested `params.turn.id` and flat `params.turnId` / `params.turn_id`
/// shapes.
pub(super) fn extract_turn_id_from_turn_started_notification(
    response_value: &Value,
) -> Option<String> {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/started") {
        return None;
    }

    let params = response_value.get("params")?;

    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turnId"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            params
                .get("turnId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            params
                .get("turn_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

/// Extracts incremental assistant message text from delta notifications.
///
/// Supports both legacy `item/updated` payloads and current v2 thought-delta
/// notifications.
pub(super) fn extract_agent_message_delta(response_value: &Value) -> Option<ExtractedAgentMessage> {
    let method = response_value.get("method").and_then(Value::as_str)?;
    if matches!(
        method,
        "item/plan/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/text_delta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/summary_text_delta"
    ) {
        let delta = response_value
            .get("params")?
            .get("delta")
            .and_then(Value::as_str)?;
        if delta.trim().is_empty() {
            return None;
        }
        let phase = if method == "item/plan/delta" {
            Some("plan".to_string())
        } else {
            Some("thinking".to_string())
        };

        return Some(ExtractedAgentMessage {
            message: delta.to_string(),
            phase,
        });
    }

    if method != "item/updated" {
        return None;
    }

    let item = response_value.get("params")?.get("item")?;
    let phase = item
        .get("phase")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let item_type = item.get("type")?.as_str()?.to_ascii_lowercase();
    if item_type != "reasoning" && item_type != "thought" {
        return None;
    }

    let delta = item.get("delta").and_then(Value::as_str)?;
    if delta.trim().is_empty() {
        return None;
    }

    Some(ExtractedAgentMessage {
        message: delta.to_string(),
        phase: phase.or(Some("thinking".to_string())),
    })
}

/// Extracts completed assistant message text from an `item/completed` line.
///
/// Only completed assistant-message item types are treated as final assistant
/// output. Internal planning/reasoning items are intentionally ignored so
/// thought text does not leak into persisted turn responses.
pub(super) fn extract_agent_message(response_value: &Value) -> Option<ExtractedAgentMessage> {
    if response_value.get("method").and_then(Value::as_str) != Some("item/completed") {
        return None;
    }

    let item = response_value.get("params")?.get("item")?;
    let phase = item
        .get("phase")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let item_type = item.get("type")?.as_str()?.to_ascii_lowercase();
    if !is_completed_assistant_message_item_type(&item_type) {
        return None;
    }
    if is_codex_thought_phase(phase.as_deref()) {
        return None;
    }

    if let Some(item_text) = item.get("text").and_then(Value::as_str) {
        if agent::is_codex_completion_status_message(item_text) {
            return None;
        }

        return Some(ExtractedAgentMessage {
            message: item_text.to_string(),
            phase,
        });
    }

    let content = item.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for content_item in content {
        if let Some(text) = content_item.get("text").and_then(Value::as_str) {
            parts.push(text.to_string());
        }
    }

    if parts.is_empty() {
        return None;
    }

    let message = parts.join("\n\n");
    if agent::is_codex_completion_status_message(&message) {
        return None;
    }

    Some(ExtractedAgentMessage { message, phase })
}

/// Returns whether one `item/completed` type should be finalized as assistant
/// output.
pub(super) fn is_completed_assistant_message_item_type(item_type: &str) -> bool {
    matches!(
        item_type,
        "agentmessage" | "agent_message" | "assistantmessage" | "assistant_message"
    )
}

/// Returns whether one Codex assistant item `phase` denotes thought/planning
/// text instead of final assistant output.
pub(super) fn is_codex_thought_phase(phase: Option<&str>) -> bool {
    let Some(phase_value) = phase else {
        return false;
    };

    let normalized_phase = phase_value.trim();

    normalized_phase.eq_ignore_ascii_case("thinking")
        || normalized_phase.eq_ignore_ascii_case("plan")
        || normalized_phase.eq_ignore_ascii_case("reasoning")
        || normalized_phase.eq_ignore_ascii_case("thought")
}

/// Parses `turn/completed` notifications and maps failures to user errors.
pub(super) fn parse_turn_completed(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<Result<(), String>> {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }
    let expected_turn_id = expected_turn_id?;

    let turn_id = extract_turn_id_from_turn_completed_notification(response_value)?;
    if turn_id != expected_turn_id {
        return None;
    }

    let status = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    match status {
        "completed" => Some(Ok(())),
        "failed" => Some(Err(extract_turn_completed_error_message(response_value)
            .unwrap_or_else(|| "Codex app-server turn failed".to_string()))),
        "" => Some(Err("Codex app-server `turn/completed` response is \
                        missing `turn.status`"
            .to_string())),
        other => Some(Err(extract_turn_completed_error_message(response_value)
            .unwrap_or_else(|| {
                format!("Codex app-server turn ended with non-completed status `{other}`")
            }))),
    }
}

/// Returns `true` when `turn/completed` indicates a delegated-turn handoff.
pub(super) fn is_interrupted_turn_completion_without_error(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> bool {
    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return false;
    }

    let Some(expected_turn_id) = expected_turn_id else {
        return false;
    };

    let Some(turn_id) = extract_turn_id_from_turn_completed_notification(response_value) else {
        return false;
    };
    if turn_id != expected_turn_id {
        return false;
    }

    let status = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "interrupted" {
        return false;
    }

    extract_turn_completed_error_message(response_value).is_none()
}

/// Extracts a delegated turn id from `turn/completed` during handoff waits.
pub(super) fn extract_handoff_turn_id_from_completion(
    response_value: &Value,
    expected_turn_id: Option<&str>,
    waiting_for_handoff_turn_completion: bool,
) -> Option<String> {
    if expected_turn_id.is_some() || !waiting_for_handoff_turn_completion {
        return None;
    }

    if response_value.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return None;
    }

    extract_turn_id_from_turn_completed_notification(response_value).map(ToString::to_string)
}

/// Extracts an optional turn-level error message from `turn/completed`.
pub(super) fn extract_turn_completed_error_message(response_value: &Value) -> Option<String> {
    let error = response_value
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("error"))?;
    let message = error.get("message").and_then(Value::as_str)?;
    let error_info = error
        .get("codexErrorInfo")
        .and_then(Value::as_str)
        .unwrap_or("");

    if error_info.is_empty() {
        Some(message.to_string())
    } else {
        Some(format!("[{error_info}] {message}"))
    }
}

/// Returns whether a turn error message indicates context window overflow.
pub(super) fn is_context_window_exceeded_error(error_message: &str) -> bool {
    error_message.contains("ContextWindowExceeded")
        || error_message.contains("context_window_exceeded")
}

/// Extracts one turn id from a `turn/completed` notification payload.
pub(super) fn extract_turn_id_from_turn_completed_notification(
    response_value: &Value,
) -> Option<&str> {
    let params = response_value.get("params")?;

    params
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turnId"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| params.get("turnId").and_then(Value::as_str))
        .or_else(|| params.get("turn_id").and_then(Value::as_str))
}

/// Extracts a progress description from an `item/started` notification.
pub(super) fn extract_item_started_progress(response_value: &Value) -> Option<String> {
    if response_value.get("method").and_then(Value::as_str) != Some("item/started") {
        return None;
    }

    let raw_item_type = response_value
        .get("params")?
        .get("item")?
        .get("type")?
        .as_str()?;

    let normalized_item_type = camel_to_snake(raw_item_type);

    agent::compact_codex_progress_message(&normalized_item_type)
}

/// Converts a camelCase string to `snake_case`.
pub(super) fn camel_to_snake(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 4);

    for (index, character) in input.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                result.push('_');
            }
            result.push(character.to_ascii_lowercase());
        } else {
            result.push(character);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_item_started_progress_normalizes_camel_case_item_type() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/started",
            "params": {
                "item": {
                    "type": "commandExecution"
                }
            }
        });

        // Act
        let progress = extract_item_started_progress(&response_value);

        // Assert
        assert_eq!(progress, Some("Running a command".to_string()));
    }

    #[test]
    fn extract_turn_id_from_turn_started_notification_supports_nested_flat_turn_fields() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/started",
            "params": {
                "turn": {
                    "turn_id": "turn-nested"
                }
            }
        });

        // Act
        let turn_id = extract_turn_id_from_turn_started_notification(&response_value);

        // Assert
        assert_eq!(turn_id, Some("turn-nested".to_string()));
    }

    #[test]
    fn parse_turn_completed_ignores_other_turn_ids() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "delegated-turn",
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = parse_turn_completed(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(turn_result, None);
    }

    #[test]
    fn extract_agent_message_delta_preserves_legacy_phase_labels() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "item/updated",
            "params": {
                "item": {
                    "type": "reasoning",
                    "phase": "plan",
                    "delta": "outline"
                }
            }
        });

        // Act
        let delta = extract_agent_message_delta(&response_value);

        // Assert
        assert_eq!(
            delta,
            Some(ExtractedAgentMessage {
                message: "outline".to_string(),
                phase: Some("plan".to_string()),
            })
        );
    }
}
