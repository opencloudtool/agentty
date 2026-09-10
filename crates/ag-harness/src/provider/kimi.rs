use crate::model::ReasoningEffort;
use crate::{chat_completion, telemetry};

pub(crate) const KIMI_API_KEY_ENV: &str = "KIMI_API_KEY";
pub(crate) const KIMI_BASE_URL_ENV: &str = "KIMI_BASE_URL";

/// Kimi K2.6 model identifier.
pub const KIMI_K2_6: &str = "kimi-k2.6";

pub(crate) fn policy(model: &str) -> chat_completion::ChatCompletionProviderPolicy {
    chat_completion::ChatCompletionProviderPolicy {
        display_name: "Kimi",
        reasoning_format: match model {
            "kimi-k2.6" => chat_completion::ReasoningFormat::Thinking {
                disable_supported: true,
                preserve_reasoning: true,
            },
            "kimi-k2.7-code" => chat_completion::ReasoningFormat::Thinking {
                disable_supported: false,
                preserve_reasoning: false,
            },
            "kimi-k3" => chat_completion::ReasoningFormat::Effort(reasoning_effort_name),
            _ => chat_completion::ReasoningFormat::None,
        },
        response_format_with_tools: false,
        structured_output: chat_completion::StructuredOutputMode::JsonObject {
            assistant_reasoning_content: matches!(
                model,
                "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k3"
            ),
            tool_result_name: true,
        },
        telemetry_name: telemetry::PROVIDER_MOONSHOT_AI,
        unsupported_schema_reason: "Kimi JSON Object mode requires an explicit object root schema",
    }
}

fn reasoning_effort_name(reasoning_effort: ReasoningEffort) -> &'static str {
    match reasoning_effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium | ReasoningEffort::High => "high",
        ReasoningEffort::XHigh | ReasoningEffort::Max => "max",
    }
}

/// Configuration for a Kimi model served through Moonshot AI's
/// OpenAI-compatible API.
pub struct KimiConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Kimi model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::matchers::{bearer_token, body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::chat_completion::{
        ERROR_BODY_LIMIT_BYTES, RESPONSE_ENVELOPE_LIMIT_BYTES, STRUCTURED_OUTPUT_INSTRUCTION,
        SUCCESS_BODY_LIMIT_BYTES,
    };
    use crate::{model, schema_contract, tool};

    fn person_schema_value() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn person_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(person_schema_value()).expect("schema should be valid")
    }

    fn request(prompt: &str) -> model::ModelRequest {
        model::ModelRequest::new(prompt, person_schema())
    }

    fn read_request(prompt: &str) -> model::ModelRequest {
        request(prompt).with_tool(tool::ToolDefinition::read())
    }

    fn read_tool_wire() -> Value {
        let definition = tool::ToolDefinition::read();

        json!({
            "type": "function",
            "function": {
                "description": definition.description(),
                "name": definition.name(),
                "parameters": definition.parameters()
            }
        })
    }

    fn escaped_value_schema() -> crate::OutputSchema {
        crate::OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            },
            "required": ["value"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn kimi(server: &MockServer) -> model::ModelClient {
        kimi_model(server, "kimi-k2.6")
    }

    fn kimi_model(server: &MockServer, model: &str) -> model::ModelClient {
        model::ModelClient::kimi(KimiConfig {
            api_key: "test-key".to_string(),
            base_url: format!("{}/", server.uri()),
            model: model.to_string(),
        })
        .expect("fixture configuration should be valid")
    }

    #[test]
    fn metadata_exposes_provider_and_model() {
        // Arrange
        let model = model::ModelClient::kimi(KimiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.moonshot.example/v1".to_string(),
            model: "kimi-k2.6".to_string(),
        })
        .expect("fixture configuration should be valid");

        // Act
        let metadata = model.metadata();

        // Assert
        assert_eq!(metadata.provider(), "moonshot_ai");
        assert_eq!(metadata.model(), "kimi-k2.6");
    }

    #[test]
    fn rejects_empty_model_during_construction() {
        // Arrange
        let config = KimiConfig {
            api_key: "test-key".to_string(),
            base_url: "https://api.moonshot.example/v1".to_string(),
            model: "  ".to_string(),
        };

        // Act
        let error = model::ModelClient::kimi(config)
            .err()
            .expect("empty model configuration should be rejected");

        // Assert
        assert_eq!(error, model::ModelMetadataError::EmptyModel);
    }

    async fn mount_structured_response(
        server: &MockServer,
        prompt: &str,
        schema: &Value,
        content: &str,
    ) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!("{STRUCTURED_OUTPUT_INSTRUCTION}{schema}"),
                        "role": "system"
                    },
                    {"content": prompt, "role": "user"}
                ],
                "model": "kimi-k2.6",
                "response_format": {"type": "json_object"},
                "thinking": {"keep": "all", "type": "enabled"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content}
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_tool_response(server: &MockServer, prompt: &str, message: Value) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!(
                            "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                            person_schema_value()
                        ),
                        "role": "system"
                    },
                    {"content": prompt, "role": "user"}
                ],
                "model": "kimi-k2.6",
                "thinking": {"keep": "all", "type": "enabled"},
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": message
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn accepts_object_root_in_type_array() {
        // Arrange
        let server = MockServer::start().await;
        let schema_value = json!({ "type": ["object"] });
        mount_structured_response(&server, "return an object", &schema_value, "{}").await;
        let model = kimi(&server);
        let schema = crate::OutputSchema::new(schema_value).expect("schema should be valid");

        // Act
        let response = model
            .complete(model::ModelRequest::new("return an object", schema))
            .await
            .expect("Kimi request should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({})));
    }

    #[tokio::test]
    async fn completes_structured_request() {
        // Arrange
        let server = MockServer::start().await;
        let schema_value = person_schema_value();
        mount_structured_response(
            &server,
            "extract the name",
            &schema_value,
            r#"{"name":"Ada"}"#,
        )
        .await;
        let model = kimi(&server);

        // Act
        let response = model
            .complete(request("extract the name"))
            .await
            .expect("Kimi request should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
    }

    #[tokio::test]
    async fn advertises_and_decodes_read_tool_call() {
        // Arrange
        let server = MockServer::start().await;
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": null,
                "tool_calls": [{
                    "id": "call_kimi_read",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                    }
                }]
            }),
        )
        .await;
        let model = kimi(&server);

        // Act
        let response = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect("Kimi read request should decode");

        // Assert
        assert!(response.output().is_none());
        let call = response
            .call()
            .expect("response should contain a tool call");
        assert_eq!(call.id(), "call_kimi_read");
        assert_eq!(call.name(), "read");
        let arguments = call
            .read_arguments()
            .expect("provider should decode read arguments");
        assert_eq!(arguments.path(), "Cargo.toml");
        assert_eq!(arguments.offset(), Some(1));
        assert_eq!(arguments.limit(), Some(12));
    }

    #[tokio::test]
    async fn preserves_reasoning_and_named_tool_result_for_continuation() {
        // Arrange
        let server = MockServer::start().await;
        let prompt = "inspect the manifest";
        let result = r#"{"content":"[workspace]","end_line":1,"next_offset":null,"path":"Cargo.toml","start_line":1,"truncated":false}"#;
        let reasoning_content = "I should inspect the manifest before answering.";
        mount_tool_response(
            &server,
            prompt,
            json!({
                "content": null,
                "reasoning_content": reasoning_content,
                "tool_calls": [{
                    "id": "call_kimi_read",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                    }
                }]
            }),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!(
                            "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                            person_schema_value()
                        ),
                        "role": "system"
                    },
                    {"content": prompt, "role": "user"},
                    {
                        "content": null,
                        "reasoning_content": reasoning_content,
                        "role": "assistant",
                        "tool_calls": [{
                            "function": {
                                "arguments": r#"{"limit":12,"offset":1,"path":"Cargo.toml"}"#,
                                "name": "read"
                            },
                            "id": "call_kimi_read",
                            "type": "function"
                        }]
                    },
                    {
                        "content": result,
                        "name": "read",
                        "role": "tool",
                        "tool_call_id": "call_kimi_read"
                    }
                ],
                "model": "kimi-k2.6",
                "thinking": {"keep": "all", "type": "enabled"},
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Cargo"}"#}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let tool_response = model
            .complete(read_request(prompt))
            .await
            .expect("initial Kimi tool request should succeed");
        let call = tool_response
            .call()
            .expect("initial response should contain a tool call")
            .clone();
        let mut model_request = read_request(prompt);
        model_request.record_tool_result(call, result.to_string());
        let response = model
            .complete(model_request)
            .await
            .expect("continued Kimi request should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Cargo" })));
    }

    #[tokio::test]
    async fn preserves_terminal_k3_reasoning_for_next_session_turn() {
        // Arrange
        let server = MockServer::start().await;
        let first_prompt = "identify the person";
        let second_prompt = "repeat the person";
        let reasoning_content = "The requested name is Ada.";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!(
                            "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                            person_schema_value()
                        ),
                        "role": "system"
                    },
                    {"content": first_prompt, "role": "user"}
                ],
                "model": "kimi-k3",
                "response_format": {"type": "json_object"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "content": r#"{"name":"Ada"}"#,
                        "reasoning_content": reasoning_content
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!(
                            "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                            person_schema_value()
                        ),
                        "role": "system"
                    },
                    {"content": first_prompt, "role": "user"},
                    {
                        "content": r#"{"name":"Ada"}"#,
                        "reasoning_content": reasoning_content,
                        "role": "assistant"
                    },
                    {"content": second_prompt, "role": "user"}
                ],
                "model": "kimi-k3",
                "response_format": {"type": "json_object"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let harness = crate::Harness::new(kimi_model(&server, "kimi-k3"))
            .database(directory.path().join("harness.db"));
        let mut session = harness
            .session("kimi-k3-reasoning", person_schema())
            .create()
            .await
            .expect("session should be created");

        // Act
        let first = session
            .send(first_prompt)
            .await
            .expect("first K3 turn should succeed");
        let second = session
            .send(second_prompt)
            .await
            .expect("continued K3 turn should succeed");

        // Assert
        assert_eq!(first.output(), &json!({ "name": "Ada" }));
        assert_eq!(second.output(), first.output());
    }

    #[tokio::test]
    async fn preserves_terminal_k2_6_reasoning_for_next_session_turn() {
        // Arrange
        let server = MockServer::start().await;
        let first_prompt = "identify the person";
        let second_prompt = "repeat the person";
        let reasoning_content = "The requested name is Ada.";
        let thinking = json!({"keep": "all", "type": "enabled"});
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!(
                            "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                            person_schema_value()
                        ),
                        "role": "system"
                    },
                    {"content": first_prompt, "role": "user"}
                ],
                "model": "kimi-k2.6",
                "response_format": {"type": "json_object"},
                "thinking": thinking
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "content": r#"{"name":"Ada"}"#,
                        "reasoning_content": reasoning_content
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {
                        "content": format!(
                            "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                            person_schema_value()
                        ),
                        "role": "system"
                    },
                    {"content": first_prompt, "role": "user"},
                    {
                        "content": r#"{"name":"Ada"}"#,
                        "reasoning_content": reasoning_content,
                        "role": "assistant"
                    },
                    {"content": second_prompt, "role": "user"}
                ],
                "model": "kimi-k2.6",
                "response_format": {"type": "json_object"},
                "thinking": thinking
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let harness =
            crate::Harness::new(kimi(&server)).database(directory.path().join("harness.db"));
        let mut session = harness
            .session("kimi-k2.6-reasoning", person_schema())
            .create()
            .await
            .expect("session should be created");

        // Act
        let first = session
            .send(first_prompt)
            .await
            .expect("first K2.6 turn should succeed");
        let second = session
            .send(second_prompt)
            .await
            .expect("continued K2.6 turn should succeed");

        // Assert
        assert_eq!(first.output(), &json!({ "name": "Ada" }));
        assert_eq!(second.output(), first.output());
    }

    #[tokio::test]
    async fn rejects_missing_tool_call() {
        // Arrange
        let server = MockServer::start().await;
        mount_tool_response(&server, "inspect the manifest", json!({"content": null})).await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("missing tool call should fail");

        // Assert
        assert_eq!(error.to_string(), "model returned no tool call");
    }

    #[tokio::test]
    async fn rejects_invalid_read_range() {
        // Arrange
        let server = MockServer::start().await;
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": null,
                "tool_calls": [{
                    "id": "call_invalid_limit",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"Cargo.toml","limit":0}"#
                    }
                }]
            }),
        )
        .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("zero read limit should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::InvalidToolArguments { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_read_arguments() {
        // Arrange
        let server = MockServer::start().await;
        let arguments = format!(
            r#"{{"path":"{}"}}"#,
            "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES)
        );
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": null,
                "tool_calls": [{
                    "id": "call_oversized",
                    "type": "function",
                    "function": {"name": "read", "arguments": arguments}
                }]
            }),
        )
        .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("oversized read arguments should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
    }

    #[tokio::test]
    async fn rejects_oversized_reasoning_content() {
        // Arrange
        let server = MockServer::start().await;
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": null,
                "reasoning_content":
                    "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1),
                "tool_calls": [{
                    "id": "call_oversized_reasoning",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"Cargo.toml"}"#
                    }
                }]
            }),
        )
        .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("oversized reasoning content should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
    }

    #[tokio::test]
    async fn rejects_structured_response_stopped_for_length() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "length",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("truncated response should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::IncompleteResponse { reason } if reason == "length"
        ));
    }

    #[tokio::test]
    async fn bounds_incomplete_response_reason() {
        // Arrange
        let server = MockServer::start().await;
        let finish_reason = "x".repeat(1024);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": finish_reason.clone(),
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("incomplete response should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::IncompleteResponse { reason }
                if reason == schema_contract::bounded_diagnostic(finish_reason)
        ));
    }

    #[tokio::test]
    async fn accepts_near_limit_escaped_structured_output() {
        // Arrange
        let server = MockServer::start().await;
        let empty_content =
            serde_json::to_string(&json!({ "value": "" })).expect("content should serialize");
        let value =
            "\\".repeat((schema_contract::RESPONSE_CONTENT_LIMIT_BYTES - empty_content.len()) / 2);
        let content =
            serde_json::to_string(&json!({ "value": value })).expect("content should serialize");
        let body = serde_json::to_vec(&json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": content}
            }]
        }))
        .expect("response should serialize");
        assert!(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES - content.len() <= 1);
        assert!(
            body.len()
                > schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + RESPONSE_ENVELOPE_LIMIT_BYTES
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let response = model
            .complete(model::ModelRequest::new(
                "return escaped content",
                escaped_value_schema(),
            ))
            .await
            .expect("near-limit escaped output should succeed");

        // Assert
        assert_eq!(
            response
                .output()
                .expect("response should contain terminal output")
                .get("value")
                .and_then(Value::as_str)
                .map(str::len),
            Some(value.len())
        );
    }

    #[tokio::test]
    async fn rejects_oversized_success_body_before_decoding() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'x';
                SUCCESS_BODY_LIMIT_BYTES
                    + 1
            ]))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("oversized successful response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseBodyTooLarge));
    }

    #[tokio::test]
    async fn rejects_oversized_response_content() {
        // Arrange
        let server = MockServer::start().await;
        let content = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content}
                }]
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("oversized response content should fail");

        // Assert
        assert!(matches!(error, model::ModelError::ResponseContentTooLarge));
    }

    #[tokio::test]
    async fn rejects_schemas_without_explicit_object_root() {
        // Arrange
        let server = MockServer::start().await;
        let model = kimi(&server);
        let schema_values = [
            json!({ "type": "array" }),
            json!({ "not": { "type": "object" } }),
            json!({
                "$defs": {
                    "result": { "type": "object" }
                },
                "$ref": "#/$defs/result"
            }),
        ];

        // Act
        let mut errors = Vec::new();
        for schema_value in schema_values {
            let schema = crate::OutputSchema::new(schema_value).expect("schema should be valid");
            errors.push(
                model
                    .complete(model::ModelRequest::new("list names", schema))
                    .await
                    .expect_err("schema without an explicit object root should fail"),
            );
        }

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| matches!(error, model::ModelError::UnsupportedOutputSchema { .. }))
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("request recording should be enabled")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_malformed_structured_output() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": "not JSON"}
                }]
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("malformed JSON should fail");

        // Assert
        assert!(matches!(error, model::ModelError::InvalidJson { .. }));
    }

    #[tokio::test]
    async fn rejects_structured_output_schema_violation() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":42}"#}
                }]
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("extract the name"))
            .await
            .expect_err("schema violation should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::SchemaViolation { path, reason }
                if path == "/name" && reason.contains("string")
        ));
    }

    #[tokio::test]
    async fn rejects_successful_response_without_choices() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": []
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("missing response choice should fail");

        // Assert
        assert!(matches!(error, model::ModelError::InvalidResponse));
    }

    #[tokio::test]
    async fn rejects_successful_response_without_content() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": null}
                }]
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("missing response content should fail");

        // Assert
        assert!(matches!(error, model::ModelError::InvalidResponse));
    }

    #[tokio::test]
    async fn returns_request_error_for_http_failure() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid API key"}
            })))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");

        // Assert
        assert_eq!(
            error.to_string(),
            "model request failed: Kimi returned HTTP 401 Unauthorized: \
             {\"error\":{\"message\":\"invalid API key\"}}"
        );
        let provider_error = std::error::Error::source(&error)
            .expect("HTTP failure should retain its provider error");
        let source = provider_error
            .source()
            .and_then(|source| source.downcast_ref::<reqwest::Error>())
            .expect("HTTP failure should retain its reqwest source");
        assert_eq!(source.status(), Some(reqwest::StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn bounds_http_error_body() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(500).set_body_string("x".repeat(ERROR_BODY_LIMIT_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");
        let message = error.to_string();

        // Assert
        assert_eq!(
            message,
            format!(
                "model request failed: Kimi returned HTTP 500 Internal Server Error: {} ...",
                "x".repeat(ERROR_BODY_LIMIT_BYTES)
            )
        );
    }

    #[tokio::test]
    async fn returns_request_error_for_malformed_response() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
            .mount(&server)
            .await;
        let model = kimi(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("malformed response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::Request(_)));
    }
}
