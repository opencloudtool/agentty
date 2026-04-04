//! Gemini ACP prompt-completion parsing helpers.

use serde_json::Value;

use crate::infra::app_server::AppServerError;

/// Normalized data extracted from one ACP `session/prompt` completion
/// response.
#[derive(Debug)]
pub(super) struct PromptCompletion {
    /// Final assistant message when the completion returned one.
    pub(super) assistant_message: Option<String>,
    /// Reported prompt input token count.
    pub(super) input_tokens: u64,
    /// Reported prompt output token count.
    pub(super) output_tokens: u64,
}

/// Parses one completed `session/prompt` response into normalized turn fields.
pub(super) fn parse_prompt_completion_response(
    response_value: &Value,
) -> Result<PromptCompletion, AppServerError> {
    let result = response_value.get("result").ok_or_else(|| {
        AppServerError::Provider(
            "Gemini ACP `session/prompt` response missing `result`".to_string(),
        )
    })?;
    let (input_tokens, output_tokens) = extract_prompt_usage_tokens(result);
    let assistant_message = extract_prompt_result_text(result);

    Ok(PromptCompletion {
        assistant_message,
        input_tokens,
        output_tokens,
    })
}

/// Extracts prompt completion usage values from ACP result payloads.
pub(super) fn extract_prompt_usage_tokens(result: &Value) -> (u64, u64) {
    extract_token_count_object(result.get("usage"))
        .or_else(|| extract_meta_quota_token_count(result))
        .or_else(|| extract_meta_model_usage_totals(result))
        .unwrap_or((0, 0))
}

/// Extracts prompt usage totals from the current Gemini ACP `_meta.quota`
/// result payload shape.
fn extract_meta_quota_token_count(result: &Value) -> Option<(u64, u64)> {
    let quota = result.get("_meta")?.get("quota")?;
    extract_token_count_object(quota.get("token_count").or_else(|| quota.get("tokenCount")))
}

/// Extracts prompt usage totals by summing `_meta.quota.model_usage`
/// entries when the aggregate token count is absent.
fn extract_meta_model_usage_totals(result: &Value) -> Option<(u64, u64)> {
    let quota = result.get("_meta")?.get("quota")?;
    let model_usage = quota
        .get("model_usage")
        .or_else(|| quota.get("modelUsage"))?
        .as_array()?;
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut found_usage = false;

    for model_usage_entry in model_usage {
        if let Some((model_input_tokens, model_output_tokens)) = extract_token_count_object(
            model_usage_entry
                .get("token_count")
                .or_else(|| model_usage_entry.get("tokenCount")),
        ) {
            input_tokens += model_input_tokens;
            output_tokens += model_output_tokens;
            found_usage = true;
        }
    }

    if !found_usage {
        return None;
    }

    Some((input_tokens, output_tokens))
}

/// Extracts normalized prompt token counts from one usage/token-count object.
fn extract_token_count_object(value: Option<&Value>) -> Option<(u64, u64)> {
    let usage = value?;
    let input_tokens = usage
        .get("inputTokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

/// Extracts assistant text from known ACP prompt completion result shapes.
pub(super) fn extract_prompt_result_text(result: &Value) -> Option<String> {
    if let Some(response_text) = result.get("response").and_then(Value::as_str) {
        return Some(response_text.to_string());
    }

    if let Some(message_text) = result.get("text").and_then(Value::as_str) {
        return Some(message_text.to_string());
    }

    if let Some(content) = result.get("content")
        && let Some(content_text) = extract_text_from_content_value(content)
        && !content_text.is_empty()
    {
        return Some(content_text);
    }

    if let Some(message) = result.get("message") {
        if let Some(message_text) = message.get("text").and_then(Value::as_str) {
            return Some(message_text.to_string());
        }

        if let Some(content) = message.get("content")
            && let Some(content_text) = extract_text_from_content_value(content)
            && !content_text.is_empty()
        {
            return Some(content_text);
        }
    }

    let output_items = result.get("output").and_then(Value::as_array)?;
    let mut output_text = String::new();
    for output_item in output_items {
        if let Some(item_text) = output_item.get("text").and_then(Value::as_str) {
            output_text.push_str(item_text);

            continue;
        }

        if let Some(content) = output_item.get("content")
            && let Some(content_text) = extract_text_from_content_value(content)
        {
            output_text.push_str(&content_text);
        }
    }
    if output_text.is_empty() {
        return None;
    }

    Some(output_text)
}

/// Extracts text from ACP content values represented as strings, arrays, or
/// nested objects.
pub(super) fn extract_text_from_content_value(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut combined_text = String::new();
            for part in parts {
                if let Some(part_text) = extract_text_from_content_value(part) {
                    combined_text.push_str(&part_text);
                }
            }
            if combined_text.is_empty() {
                return None;
            }

            Some(combined_text)
        }
        Value::Object(_) => {
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }

            if let Some(parts_text) = content
                .get("parts")
                .and_then(extract_text_from_content_value)
                && !parts_text.is_empty()
            {
                return Some(parts_text);
            }

            if let Some(nested_content_text) = content
                .get("content")
                .and_then(extract_text_from_content_value)
                && !nested_content_text.is_empty()
            {
                return Some(nested_content_text);
            }

            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_prompt_usage_tokens_reads_snake_case_fields() {
        // Arrange
        let result = serde_json::json!({
            "usage": {
                "input_tokens": 14,
                "output_tokens": 6
            }
        });

        // Act
        let usage = extract_prompt_usage_tokens(&result);

        // Assert
        assert_eq!(usage, (14, 6));
    }

    #[test]
    fn extract_prompt_usage_tokens_reads_meta_quota_token_count_fields() {
        // Arrange
        let result = serde_json::json!({
            "_meta": {
                "quota": {
                    "token_count": {
                        "input_tokens": 21,
                        "output_tokens": 8
                    }
                }
            }
        });

        // Act
        let usage = extract_prompt_usage_tokens(&result);

        // Assert
        assert_eq!(usage, (21, 8));
    }

    #[test]
    fn extract_prompt_usage_tokens_sums_meta_model_usage_when_total_is_missing() {
        // Arrange
        let result = serde_json::json!({
            "_meta": {
                "quota": {
                    "model_usage": [{
                        "token_count": {
                            "input_tokens": 13,
                            "output_tokens": 5
                        }
                    }, {
                        "token_count": {
                            "input_tokens": 8,
                            "output_tokens": 3
                        }
                    }]
                }
            }
        });

        // Act
        let usage = extract_prompt_usage_tokens(&result);

        // Assert
        assert_eq!(usage, (21, 8));
    }

    #[test]
    fn extract_prompt_result_text_reads_nested_message_content_parts() {
        // Arrange
        let result = serde_json::json!({
            "message": {
                "content": {
                    "parts": [
                        {"text": "Part one"},
                        {"content": [{"text": " and part two"}]}
                    ]
                }
            }
        });

        // Act
        let message = extract_prompt_result_text(&result);

        // Assert
        assert_eq!(message, Some("Part one and part two".to_string()));
    }

    #[test]
    fn extract_text_from_content_value_returns_none_for_empty_nested_content() {
        // Arrange
        let content = serde_json::json!({
            "content": []
        });

        // Act
        let text = extract_text_from_content_value(&content);

        // Assert
        assert_eq!(text, None);
    }
}
