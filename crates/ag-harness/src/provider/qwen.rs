use crate::model::ReasoningEffort;
use crate::{chat_completion, telemetry};

pub(crate) const DASHSCOPE_API_KEY_ENV: &str = "DASHSCOPE_API_KEY";
pub(crate) const DASHSCOPE_BASE_URL_ENV: &str = "DASHSCOPE_BASE_URL";

/// Qwen Plus model identifier.
pub const QWEN_PLUS: &str = "qwen-plus";

pub(crate) fn policy(model: &str) -> chat_completion::ChatCompletionProviderPolicy {
    let preserves_reasoning = model.starts_with("qwen3.8-");

    chat_completion::ChatCompletionProviderPolicy {
        display_name: "Qwen",
        reasoning_format: if preserves_reasoning {
            chat_completion::ReasoningFormat::Effort(reasoning_effort_name)
        } else if model == QWEN_PLUS {
            chat_completion::ReasoningFormat::EnableThinking
        } else {
            chat_completion::ReasoningFormat::None
        },
        response_format_with_tools: false,
        structured_output: chat_completion::StructuredOutputMode::JsonObject {
            assistant_reasoning_content: preserves_reasoning,
            tool_result_name: false,
        },
        telemetry_name: telemetry::PROVIDER_ALIBABA_CLOUD,
        unsupported_schema_reason: "Qwen JSON Object mode requires an explicit object root schema",
    }
}

fn reasoning_effort_name(reasoning_effort: ReasoningEffort) -> &'static str {
    match reasoning_effort {
        ReasoningEffort::Low | ReasoningEffort::Medium => reasoning_effort.as_str(),
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => {
            ReasoningEffort::XHigh.as_str()
        }
    }
}

/// Configuration for a Qwen model served through Alibaba Cloud Model Studio's
/// OpenAI-compatible API.
pub struct QwenConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Qwen model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use wiremock::matchers::{bearer_token, body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::chat_completion::{
        ChatCompletion, ChatCompletionBackend, ChatCompletionClient, ChatCompletionError,
        ChatCompletionRequest, ERROR_BODY_LIMIT_BYTES, RESPONSE_ENVELOPE_LIMIT_BYTES,
        STRUCTURED_OUTPUT_INSTRUCTION, SUCCESS_BODY_LIMIT_BYTES,
    };
    use crate::{model, schema_contract, tool};

    struct StubClient;

    #[async_trait]
    impl ChatCompletionClient for StubClient {
        async fn complete(
            &self,
            request: ChatCompletionRequest<'_>,
        ) -> Result<Option<ChatCompletion>, ChatCompletionError> {
            assert_eq!(request.api_key(), "stub-key");
            assert_eq!(
                request.endpoint(),
                "https://stub.example/v1/chat/completions"
            );
            assert_eq!(request.payload()["model"], "qwen-stub");
            assert_eq!(request.payload()["response_format"]["type"], "json_object");

            Ok(Some(ChatCompletion::new(
                "stop".to_string(),
                Some(r#"{"name":"Ada"}"#.to_string()),
            )))
        }
    }

    fn person_schema_value() -> serde_json::Value {
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

    fn read_tool_wire() -> serde_json::Value {
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

    fn qwen(server: &MockServer) -> model::ModelClient {
        qwen_model(server, QWEN_PLUS)
    }

    fn qwen_model(server: &MockServer, model: &str) -> model::ModelClient {
        model::ModelClient::qwen(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: format!("{}/", server.uri()),
            model: model.to_string(),
        })
        .expect("fixture configuration should be valid")
    }

    #[test]
    fn rejects_empty_model_during_construction() {
        // Arrange
        let config = QwenConfig {
            api_key: "test-key".to_string(),
            base_url: "https://example.com".to_string(),
            model: "  ".to_string(),
        };

        // Act
        let error = model::ModelClient::qwen(config)
            .err()
            .expect("empty model configuration should be rejected");

        // Assert
        assert_eq!(error, model::ModelMetadataError::EmptyModel);
    }

    async fn mount_structured_response(
        server: &MockServer,
        prompt: &str,
        schema: &serde_json::Value,
        content: &str,
    ) {
        let schema_instruction = format!("{STRUCTURED_OUTPUT_INSTRUCTION}{schema}");
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .and(body_json(json!({
                "messages": [
                    {"content": schema_instruction, "role": "system"},
                    {"content": prompt, "role": "user"}
                ],
                "model": "qwen-plus",
                "response_format": {"type": "json_object"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": content, "tool_calls": null}
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_tool_response(server: &MockServer, prompt: &str, message: serde_json::Value) {
        mount_read_response(server, prompt, "tool_calls", message).await;
    }

    async fn mount_read_response(
        server: &MockServer,
        prompt: &str,
        finish_reason: &str,
        message: serde_json::Value,
    ) {
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
                "model": "qwen-plus",
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": finish_reason,
                    "message": message
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_qwen_3_8_tool_response(
        server: &MockServer,
        prompt: &str,
        reasoning_content: &str,
    ) {
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
                "model": "qwen3.8-max",
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": null,
                        "reasoning_content": reasoning_content,
                        "tool_calls": [{
                            "id": "call_qwen_read",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                            }
                        }]
                    }
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_qwen_3_8_continuation(
        server: &MockServer,
        prompt: &str,
        result: &str,
        reasoning_content: &str,
    ) {
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
                            "id": "call_qwen_read",
                            "type": "function"
                        }]
                    },
                    {
                        "content": result,
                        "role": "tool",
                        "tool_call_id": "call_qwen_read"
                    }
                ],
                "model": "qwen3.8-max",
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Cargo"}"#}
                }]
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn completes_terminal_response_with_null_tool_calls() {
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
        let model: Box<dyn model::Model> = Box::new(qwen(&server));

        // Act
        let response = model
            .complete(request("extract the name"))
            .await
            .expect("Qwen request should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Ada" })));
    }

    #[tokio::test]
    async fn completes_with_normalized_provider_metadata() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }],
                "id": "response-1",
                "model": "qwen-plus-2026-08-16",
                "system_fingerprint": "fingerprint-1",
                "usage": {
                    "completion_tokens": 9,
                    "completion_tokens_details": {"reasoning_tokens": 2},
                    "prompt_tokens": 12,
                    "prompt_tokens_details": {"cached_tokens": 4},
                    "total_tokens": 21
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model: Box<dyn model::Model> = Box::new(qwen(&server));

        // Act
        let completion = model
            .complete(request("extract the name"))
            .await
            .expect("Qwen completion metadata should decode");
        let metadata = completion
            .metadata()
            .expect("Qwen completion should include metadata");
        let usage = metadata.usage().expect("Qwen usage should be retained");

        // Assert
        assert_eq!(
            completion.response().output(),
            Some(&json!({ "name": "Ada" }))
        );
        assert_eq!(metadata.finish_reason(), "stop");
        assert_eq!(metadata.response_id(), Some("response-1"));
        assert_eq!(metadata.response_model(), Some("qwen-plus-2026-08-16"));
        assert_eq!(metadata.system_fingerprint(), Some("fingerprint-1"));
        assert_eq!(usage.input_tokens(), Some(12));
        assert_eq!(usage.output_tokens(), Some(9));
        assert_eq!(usage.total_tokens(), Some(21));
        assert_eq!(usage.cache_hit_tokens(), Some(4));
        assert_eq!(usage.cache_miss_tokens(), None);
        assert_eq!(usage.reasoning_tokens(), Some(2));
    }

    #[tokio::test]
    async fn advertises_and_decodes_read_tool_call() {
        // Arrange
        let server = MockServer::start().await;
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": "",
                "tool_calls": [{
                    "id": "call_qwen_read",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"Cargo.toml","offset":1,"limit":12}"#
                    }
                }]
            }),
        )
        .await;
        let model = qwen(&server);

        // Act
        let response = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect("Qwen read request should decode");

        // Assert
        assert!(response.output().is_none());
        let call = response
            .call()
            .expect("response should contain a tool call");
        assert_eq!(call.id(), "call_qwen_read");
        assert_eq!(call.name(), "read");
        let arguments = call
            .read_arguments()
            .expect("provider should decode read arguments");
        assert_eq!(arguments.path(), "Cargo.toml");
        assert_eq!(arguments.offset(), Some(1));
        assert_eq!(arguments.limit(), Some(12));
    }

    #[tokio::test]
    async fn sends_tool_result_history_for_continuation() {
        // Arrange
        let server = MockServer::start().await;
        let prompt = "inspect the manifest";
        let result = r#"{"content":"[workspace]","end_line":1,"next_offset":null,"path":"Cargo.toml","start_line":1,"truncated":false}"#;
        let arguments = serde_json::from_value(json!({
            "path": "Cargo.toml",
            "offset": 1,
            "limit": 12
        }))
        .expect("read arguments should be valid");
        let call = tool::ToolCall::read("call_qwen_read".to_string(), arguments, None);
        let mut model_request = read_request(prompt);
        model_request.record_tool_result(call, result.to_string());
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
                        "role": "assistant",
                        "tool_calls": [{
                            "function": {
                                "arguments": r#"{"limit":12,"offset":1,"path":"Cargo.toml"}"#,
                                "name": "read"
                            },
                            "id": "call_qwen_read",
                            "type": "function"
                        }]
                    },
                    {
                        "content": result,
                        "role": "tool",
                        "tool_call_id": "call_qwen_read"
                    }
                ],
                "model": "qwen-plus",
                "tools": [read_tool_wire()]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "content": r#"{"name":"Cargo"}"#,
                        "tool_calls": null
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let model = qwen(&server);

        // Act
        let response = model
            .complete(model_request)
            .await
            .expect("continued Qwen request should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Cargo" })));
    }

    #[tokio::test]
    async fn preserves_qwen_3_8_reasoning_for_tool_continuation() {
        // Arrange
        let server = MockServer::start().await;
        let prompt = "inspect the manifest";
        let result = r#"{"content":"[workspace]","end_line":1,"next_offset":null,"path":"Cargo.toml","start_line":1,"truncated":false}"#;
        let reasoning_content = "I should inspect the manifest before answering.";
        mount_qwen_3_8_tool_response(&server, prompt, reasoning_content).await;
        mount_qwen_3_8_continuation(&server, prompt, result, reasoning_content).await;
        let model = qwen_model(&server, "qwen3.8-max");

        // Act
        let tool_response = model
            .complete(read_request(prompt))
            .await
            .expect("initial Qwen3.8 tool request should succeed");
        let call = tool_response
            .call()
            .expect("initial response should contain a tool call")
            .clone();
        let mut model_request = read_request(prompt);
        model_request.record_tool_result(call, result.to_string());
        let response = model
            .complete(model_request)
            .await
            .expect("continued Qwen3.8 request should succeed");

        // Assert
        assert_eq!(response.output(), Some(&json!({ "name": "Cargo" })));
    }

    #[tokio::test]
    async fn rejects_terminal_response_with_tool_calls() {
        // Arrange
        let server = MockServer::start().await;
        mount_read_response(
            &server,
            "inspect the manifest",
            "stop",
            json!({
                "content": r#"{"name":"Cargo"}"#,
                "tool_calls": [{
                    "id": "call_late",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"Cargo.toml"}"#
                    }
                }]
            }),
        )
        .await;
        let model = qwen(&server);

        // Act
        let error = model
            .complete(read_request("inspect the manifest"))
            .await
            .expect_err("terminal response with tool calls should fail");

        // Assert
        assert!(matches!(
            error,
            model::ModelError::TerminalResponseWithToolCalls
        ));
    }

    #[tokio::test]
    async fn rejects_malformed_and_invalid_read_arguments() {
        // Arrange
        let cases = [
            ("{", "model returned invalid tool arguments:"),
            (
                r#"{"path":"Cargo.toml","offset":0}"#,
                "model returned invalid tool arguments:",
            ),
            (
                r#"{"path":"Cargo.toml","extra":true}"#,
                "model returned invalid tool arguments:",
            ),
        ];

        // Act
        let mut errors = Vec::new();
        for (index, (arguments, expected)) in cases.into_iter().enumerate() {
            let server = MockServer::start().await;
            mount_tool_response(
                &server,
                "inspect the manifest",
                json!({
                    "content": null,
                    "tool_calls": [{
                        "id": format!("call_{index}"),
                        "type": "function",
                        "function": {"name": "read", "arguments": arguments}
                    }]
                }),
            )
            .await;
            let error = qwen(&server)
                .complete(read_request("inspect the manifest"))
                .await
                .expect_err("invalid read arguments should fail");
            errors.push((error.to_string(), expected));
        }

        // Assert
        assert!(
            errors
                .iter()
                .all(|(error, expected)| error.starts_with(expected))
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_tool_type_and_name() {
        // Arrange
        let messages = [
            (
                json!({
                    "content": null,
                    "tool_calls": [{
                        "id": "call_type",
                        "type": "custom"
                    }]
                }),
                "model requested unsupported tool type: custom",
            ),
            (
                json!({
                    "content": null,
                    "tool_calls": [{
                        "id": "call_name",
                        "type": "function",
                        "function": {"name": "write", "arguments": r#"{"path":"Cargo.toml"}"#}
                    }]
                }),
                "model requested unsupported tool: write",
            ),
            (
                json!({
                    "content": null,
                    "tool_calls": [{
                        "id": "call_function_payload",
                        "type": "function"
                    }]
                }),
                "model returned invalid tool arguments:",
            ),
        ];

        // Act
        let mut errors = Vec::new();
        for (message, expected) in messages {
            let server = MockServer::start().await;
            mount_tool_response(&server, "inspect the manifest", message).await;
            let error = qwen(&server)
                .complete(read_request("inspect the manifest"))
                .await
                .expect_err("unsupported tool response should fail");
            errors.push((error.to_string(), expected));
        }

        // Assert
        assert!(errors.iter().all(|(error, expected)| {
            error == expected || (expected.ends_with(':') && error.starts_with(expected))
        }));
    }

    #[tokio::test]
    async fn accepts_bounded_content_with_tool_calls() {
        // Arrange
        let server = MockServer::start().await;
        mount_tool_response(
            &server,
            "inspect the manifest",
            json!({
                "content": "I will inspect the manifest.",
                "tool_calls": [{
                    "id": "call_content",
                    "type": "function",
                    "function": {"name": "read", "arguments": r#"{"path":"Cargo.toml"}"#}
                }]
            }),
        )
        .await;

        // Act
        let response = qwen(&server)
            .complete(read_request("inspect the manifest"))
            .await
            .expect("incidental tool-call content should be ignored");

        // Assert
        assert_eq!(
            response
                .call()
                .expect("response should contain a tool call")
                .id(),
            "call_content"
        );
    }

    #[tokio::test]
    async fn completes_through_injected_client() {
        // Arrange
        let model = ChatCompletionBackend::with_client(
            "stub-key".to_string(),
            "https://stub.example/v1/".to_string(),
            "qwen-stub".to_string(),
            policy("qwen-stub"),
            Arc::new(StubClient),
        );

        // Act
        let output = model
            .generate(&request("extract the name"))
            .await
            .expect("stubbed Qwen request should succeed");

        // Assert
        assert!(matches!(
            output,
            crate::chat_completion::GeneratedResponse::Output { output, .. }
                if output == r#"{"name":"Ada"}"#
        ));
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
        let model = qwen(&server);

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
        let model = qwen(&server);

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
        let model = qwen(&server);

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
                .and_then(serde_json::Value::as_str)
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
        let model = qwen(&server);

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
        let model = qwen(&server);

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
        let model = qwen(&server);
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
        assert!(errors.into_iter().all(|error| matches!(
            error,
            model::ModelError::UnsupportedOutputSchema { reason }
                if reason == "Qwen JSON Object mode requires an explicit object root schema"
        )));
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
        let model = qwen(&server);

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
        let model = qwen(&server);

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
    async fn rejects_successful_response_without_content() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": []
            })))
            .mount(&server)
            .await;
        let model = qwen(&server);

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
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("HTTP failure should fail");

        // Assert
        assert_eq!(
            error.to_string(),
            "model request failed: Qwen returned HTTP 401 Unauthorized: \
             {\"error\":{\"message\":\"invalid API key\"}}"
        );
        let provider_error = std::error::Error::source(&error)
            .expect("HTTP failure should retain its provider error");
        let source = provider_error
            .source()
            .and_then(|source| source.downcast_ref::<reqwest::Error>())
            .expect("HTTP failure should retain its reqwest source");
        assert_eq!(source.status(), Some(reqwest::StatusCode::UNAUTHORIZED));
        assert_eq!(error.error_type(), model::ModelErrorType::Provider);
        assert_eq!(error.http_status(), Some(401));
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
        let model = qwen(&server);

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
                "model request failed: Qwen returned HTTP 500 Internal Server Error: {} ...",
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
        let model = qwen(&server);

        // Act
        let error = model
            .complete(request("hello"))
            .await
            .expect_err("malformed response should fail");

        // Assert
        assert!(matches!(error, model::ModelError::Request(_)));
        assert_eq!(
            error.error_type(),
            model::ModelErrorType::InvalidProviderResponse
        );
        assert_eq!(error.http_status(), None);
        assert!(
            error.to_string().starts_with(
                "model request failed: Chat Completions returned an invalid response:"
            )
        );
    }
}
