use std::error::Error;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{model, schema_contract, tool};

pub(crate) const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
const JSON_STRING_MAX_EXPANSION: usize = 6;
const MAX_RATE_LIMIT_RETRIES: usize = 5;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_TRANSPORT_RETRIES: usize = 1;
pub(crate) const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);
pub(crate) const SUCCESS_BODY_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_ENVELOPE_LIMIT_BYTES;
pub(crate) const STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Return only one JSON object. The object must validate against this JSON Schema. ",
    "Do not include Markdown fences or any other text.\n\nJSON Schema:\n",
);

/// Structured-output representation selected by one Chat Completions provider.
#[derive(Clone, Copy)]
pub(crate) enum StructuredOutputMode {
    JsonObject {
        assistant_reasoning_content: bool,
        tool_result_name: bool,
    },
    JsonSchema,
}

impl StructuredOutputMode {
    fn assistant_reasoning_content(self) -> bool {
        matches!(
            self,
            Self::JsonObject {
                assistant_reasoning_content: true,
                ..
            }
        )
    }

    fn tool_result_name(self) -> bool {
        matches!(
            self,
            Self::JsonObject {
                tool_result_name: true,
                ..
            }
        )
    }
}

/// Provider policy applied by the shared Chat Completions backend.
#[derive(Clone, Copy)]
pub(crate) struct ChatCompletionProviderPolicy {
    pub(crate) display_name: &'static str,
    pub(crate) response_format_with_tools: bool,
    pub(crate) structured_output: StructuredOutputMode,
    pub(crate) telemetry_name: &'static str,
    pub(crate) unsupported_schema_reason: &'static str,
}

/// Provider-neutral result of decoding one Chat Completions choice.
pub(crate) enum GeneratedResponse {
    Failed {
        error: model::ModelError,
        metadata: model::CompletionMetadata,
    },
    Output {
        metadata: model::CompletionMetadata,
        output: String,
    },
    ToolCall {
        call: tool::ToolCall,
        metadata: model::CompletionMetadata,
    },
    ToolCalls {
        calls: Vec<tool::ToolCall>,
        metadata: model::CompletionMetadata,
    },
}

impl GeneratedResponse {
    fn failed(error: model::ModelError, metadata: model::CompletionMetadata) -> Self {
        Self::Failed { error, metadata }
    }
}

/// Shared structured-output backend for OpenAI-compatible Chat Completions
/// APIs.
pub(crate) struct ChatCompletionBackend {
    api_key: String,
    base_url: String,
    client: Arc<dyn ChatCompletionClient>,
    model: String,
    policy: ChatCompletionProviderPolicy,
}

impl ChatCompletionBackend {
    /// Creates a structured-output backend with the production HTTP client.
    pub(crate) fn new(
        api_key: String,
        base_url: String,
        model: String,
        policy: ChatCompletionProviderPolicy,
    ) -> Self {
        Self::with_client(api_key, base_url, model, policy, default_client())
    }

    /// Returns the backend's telemetry identity.
    pub(crate) fn identity(&self) -> (&'static str, &str) {
        (self.policy.telemetry_name, &self.model)
    }

    /// Generates raw structured output through the shared wire lifecycle.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        if !request.schema().has_object_root() {
            return Err(model::ModelError::UnsupportedOutputSchema {
                reason: self.policy.unsupported_schema_reason.to_string(),
            });
        }
        let tools: Vec<_> = request
            .tools()
            .iter()
            .map(ChatCompletionTool::from)
            .collect();
        let response_format = (tools.is_empty() || self.policy.response_format_with_tools)
            .then(|| self.response_format(request.schema()));
        let payload = ChatCompletionPayload {
            messages: self.messages(request)?,
            model: &self.model,
            response_format,
            tools,
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let completion = self
            .client
            .complete(ChatCompletionRequest::new(
                &self.api_key,
                endpoint(&self.base_url),
                payload,
            ))
            .await
            .map_err(|error| self.map_completion_error(error))?
            .ok_or(model::ModelError::InvalidResponse)?;
        let (metadata, content, reasoning_content, tool_calls) = completion.into_parts();
        match metadata.finish_reason() {
            "stop" if !tool_calls.is_empty() => Ok(GeneratedResponse::failed(
                model::ModelError::TerminalResponseWithToolCalls,
                metadata,
            )),
            "stop" => Ok(match content {
                Some(output) => GeneratedResponse::Output { metadata, output },
                None => GeneratedResponse::failed(model::ModelError::InvalidResponse, metadata),
            }),
            "tool_calls" => Ok(Self::decode_tool_call(
                request,
                content.as_deref(),
                self.policy
                    .structured_output
                    .assistant_reasoning_content()
                    .then_some(reasoning_content.as_deref())
                    .flatten(),
                tool_calls,
                metadata,
            )),
            _ => {
                let reason = schema_contract::bounded_diagnostic(metadata.finish_reason());

                Ok(GeneratedResponse::failed(
                    model::ModelError::IncompleteResponse { reason },
                    metadata,
                ))
            }
        }
    }

    /// Creates a structured-output backend with an injected transport client.
    pub(crate) fn with_client(
        api_key: String,
        base_url: String,
        model: String,
        policy: ChatCompletionProviderPolicy,
        client: Arc<dyn ChatCompletionClient>,
    ) -> Self {
        Self {
            api_key,
            base_url,
            client,
            model,
            policy,
        }
    }

    fn messages(
        &self,
        request: &model::ModelRequest,
    ) -> Result<Vec<ChatCompletionMessagePayload>, model::ModelError> {
        let mut messages = Vec::with_capacity(request.messages().len() + 1);
        if matches!(
            self.policy.structured_output,
            StructuredOutputMode::JsonObject { .. }
        ) {
            messages.push(ChatCompletionMessagePayload::Text {
                content: format!(
                    "{STRUCTURED_OUTPUT_INSTRUCTION}{}",
                    request.schema().value()
                ),
                role: "system",
            });
        }
        for message in request.messages() {
            match message {
                model::ModelMessage::Assistant(content) => {
                    messages.push(ChatCompletionMessagePayload::Text {
                        content: content.clone(),
                        role: "assistant",
                    });
                }
                model::ModelMessage::System(content) => {
                    messages.push(ChatCompletionMessagePayload::Text {
                        content: content.clone(),
                        role: "system",
                    });
                }
                model::ModelMessage::User(content) => {
                    messages.push(ChatCompletionMessagePayload::Text {
                        content: content.clone(),
                        role: "user",
                    });
                }
                model::ModelMessage::AssistantToolCall(call) => {
                    messages.push(self.assistant_tool_call_message(std::slice::from_ref(call))?);
                }
                model::ModelMessage::AssistantToolCalls(calls) => {
                    messages.push(self.assistant_tool_call_message(calls)?);
                }
                model::ModelMessage::ToolResult {
                    call_id,
                    content,
                    name,
                } => {
                    messages.push(ChatCompletionMessagePayload::ToolResult {
                        content: content.clone(),
                        name: self
                            .policy
                            .structured_output
                            .tool_result_name()
                            .then(|| name.clone()),
                        role: "tool",
                        tool_call_id: call_id.clone(),
                    });
                }
            }
        }

        Ok(messages)
    }

    fn assistant_tool_call_message(
        &self,
        calls: &[tool::ToolCall],
    ) -> Result<ChatCompletionMessagePayload, model::ModelError> {
        let reasoning_content = self
            .policy
            .structured_output
            .assistant_reasoning_content()
            .then(|| {
                calls
                    .first()
                    .and_then(tool::ToolCall::reasoning_content)
                    .map(str::to_string)
            })
            .flatten();
        let tool_calls = calls
            .iter()
            .map(|call| {
                Ok(ChatCompletionOutgoingToolCall {
                    function: ChatCompletionOutgoingFunctionCall {
                        arguments: call.arguments_json().map_err(model::ModelError::request)?,
                        name: call.name().to_string(),
                    },
                    id: call.id().to_string(),
                    kind: "function",
                })
            })
            .collect::<Result<_, model::ModelError>>()?;

        Ok(ChatCompletionMessagePayload::AssistantToolCall {
            content: None,
            reasoning_content,
            role: "assistant",
            tool_calls,
        })
    }

    fn response_format<'a>(&self, schema: &'a schema_contract::OutputSchema) -> ResponseFormat<'a> {
        match self.policy.structured_output {
            StructuredOutputMode::JsonObject { .. } => ResponseFormat {
                json_schema: None,
                kind: "json_object",
            },
            StructuredOutputMode::JsonSchema => ResponseFormat {
                json_schema: Some(JsonSchemaResponseFormat {
                    name: "ag_harness_output",
                    schema: schema.value(),
                }),
                kind: "json_schema",
            },
        }
    }

    fn map_completion_error(&self, error: ChatCompletionError) -> model::ModelError {
        match error {
            ChatCompletionError::Http {
                body,
                source,
                status,
            } => {
                model::ModelError::provider_request(self.policy.display_name, body, source, status)
            }
            error @ ChatCompletionError::InvalidResponse(_) => {
                model::ModelError::classified_request(
                    model::ModelErrorType::InvalidProviderResponse,
                    None,
                    error.into(),
                )
            }
            ChatCompletionError::ResponseBodyTooLarge => model::ModelError::ResponseBodyTooLarge,
            error @ ChatCompletionError::Transport(_) => model::ModelError::classified_request(
                model::ModelErrorType::Transport,
                None,
                error.into(),
            ),
        }
    }

    fn decode_tool_call(
        request: &model::ModelRequest,
        content: Option<&str>,
        reasoning_content: Option<&str>,
        calls: Vec<ChatCompletionToolCall>,
        metadata: model::CompletionMetadata,
    ) -> GeneratedResponse {
        match Self::decode_tool_call_parts(request, content, reasoning_content, calls) {
            Ok(mut calls) if calls.len() == 1 => GeneratedResponse::ToolCall {
                call: calls.remove(0),
                metadata,
            },
            Ok(calls) => GeneratedResponse::ToolCalls { calls, metadata },
            Err(error) => GeneratedResponse::failed(error, metadata),
        }
    }

    fn decode_tool_call_parts(
        request: &model::ModelRequest,
        content: Option<&str>,
        reasoning_content: Option<&str>,
        calls: Vec<ChatCompletionToolCall>,
    ) -> Result<Vec<tool::ToolCall>, model::ModelError> {
        if let Some(content) = content {
            schema_contract::ensure_content_size(content).map_err(model::ModelError::from)?;
        }
        if calls.is_empty() {
            return Err(model::ModelError::MissingToolCall);
        }
        let mut decoded = Vec::with_capacity(calls.len());
        for (index, call) in calls.into_iter().enumerate() {
            decoded.push(Self::decode_one_tool_call(
                request,
                call,
                (index == 0).then_some(reasoning_content).flatten(),
            )?);
        }
        model::ensure_unique_tool_call_ids(&decoded)?;

        Ok(decoded)
    }

    fn decode_one_tool_call(
        request: &model::ModelRequest,
        call: ChatCompletionToolCall,
        reasoning_content: Option<&str>,
    ) -> Result<tool::ToolCall, model::ModelError> {
        if call.kind != "function" {
            return Err(model::ModelError::UnsupportedToolType {
                kind: schema_contract::bounded_diagnostic(call.kind),
            });
        }
        let function = serde_json::from_value::<ChatCompletionFunctionCall>(call.function)
            .map_err(|error| model::ModelError::InvalidToolArguments {
                reason: schema_contract::bounded_diagnostic(error),
            })?;
        if !request.advertises_tool(&function.name) {
            return Err(model::ModelError::UnsupportedToolName {
                name: schema_contract::bounded_diagnostic(function.name),
            });
        }
        tool::ToolCall::from_json(
            call.id,
            &function.name,
            &function.arguments,
            reasoning_content.map(str::to_string),
        )
    }
}

/// One provider-authenticated request using the Chat Completions wire API.
pub(crate) struct ChatCompletionRequest<'a> {
    api_key: &'a str,
    endpoint: String,
    payload: Value,
}

impl<'a> ChatCompletionRequest<'a> {
    /// Creates a request from provider-owned authentication and payload data.
    pub(crate) fn new(api_key: &'a str, endpoint: String, payload: Value) -> Self {
        Self {
            api_key,
            endpoint,
            payload,
        }
    }

    /// Returns the provider authentication token.
    pub(crate) fn api_key(&self) -> &str {
        self.api_key
    }

    /// Returns the provider request endpoint.
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the serialized provider request body.
    pub(crate) fn payload(&self) -> &Value {
        &self.payload
    }
}

/// Provider-independent fields extracted from the first completion choice.
pub(crate) struct ChatCompletion {
    content: Option<String>,
    metadata: model::CompletionMetadata,
    reasoning_content: Option<String>,
    tool_calls: Vec<ChatCompletionToolCall>,
}

impl ChatCompletion {
    /// Creates one normalized completion choice.
    pub(crate) fn new(finish_reason: String, content: Option<String>) -> Self {
        Self {
            content,
            metadata: model::CompletionMetadata::new(finish_reason, None, None, None, None),
            reasoning_content: None,
            tool_calls: Vec::new(),
        }
    }

    /// Consumes the choice into provider-interpreted completion fields.
    fn into_parts(
        self,
    ) -> (
        model::CompletionMetadata,
        Option<String>,
        Option<String>,
        Vec<ChatCompletionToolCall>,
    ) {
        (
            self.metadata,
            self.content,
            self.reasoning_content,
            self.tool_calls,
        )
    }
}

/// Decoded client boundary between provider adapters and the Chat Completions
/// API.
#[async_trait]
pub(crate) trait ChatCompletionClient: Send + Sync {
    async fn complete(
        &self,
        request: ChatCompletionRequest<'_>,
    ) -> Result<Option<ChatCompletion>, ChatCompletionError>;
}

/// Builds the Chat Completions endpoint for a provider base URL.
pub(crate) fn endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// Creates the production Chat Completions client implementation.
pub(crate) fn default_client() -> Arc<dyn ChatCompletionClient> {
    Arc::new(ReqwestChatCompletionClient {
        client: reqwest::Client::new(),
    })
}

struct ReqwestChatCompletionClient {
    client: reqwest::Client,
}

#[async_trait]
impl ChatCompletionClient for ReqwestChatCompletionClient {
    async fn complete(
        &self,
        request: ChatCompletionRequest<'_>,
    ) -> Result<Option<ChatCompletion>, ChatCompletionError> {
        let mut rate_limit_retries = 0_usize;
        let mut transport_retries = 0_usize;
        let mut response = loop {
            let response = self
                .client
                .post(request.endpoint())
                .bearer_auth(request.api_key())
                .timeout(REQUEST_TIMEOUT)
                .json(request.payload())
                .send()
                .await;
            let mut response = match response {
                Ok(response) => response,
                Err(_) if transport_retries < MAX_TRANSPORT_RETRIES => {
                    transport_retries += 1;
                    tokio::time::sleep(RETRY_DELAY).await;

                    continue;
                }
                Err(error) => return Err(ChatCompletionError::transport(error)),
            };

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                && rate_limit_retries < MAX_RATE_LIMIT_RETRIES
            {
                let delay = rate_limit_retry_delay(response.headers(), rate_limit_retries);
                rate_limit_retries += 1;
                drop(response);
                tokio::time::sleep(delay).await;

                continue;
            }
            if let Err(source) = response.error_for_status_ref() {
                let status = response.status();
                let body = error_body_summary(&mut response).await;

                return Err(ChatCompletionError::Http {
                    body,
                    source,
                    status,
                });
            }

            break response;
        };

        let body = success_body(&mut response).await?;
        let response = serde_json::from_slice::<ChatCompletionResponse>(&body)
            .map_err(ChatCompletionError::InvalidResponse)?;

        Ok(response.into_completion().map(|(choice, metadata)| {
            let mut completion = ChatCompletion::new(choice.finish_reason, choice.message.content);
            completion.metadata = metadata;
            completion.reasoning_content = choice.message.reasoning_content;
            completion.tool_calls = choice.message.tool_calls.unwrap_or_default();

            completion
        }))
    }
}

fn rate_limit_retry_delay(headers: &reqwest::header::HeaderMap, retry: usize) -> Duration {
    let provider_delay = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_default();
    let backoff = RETRY_DELAY.saturating_mul(1_u32 << retry.min(31));

    provider_delay.max(backoff).min(MAX_RETRY_DELAY)
}

async fn error_body_summary(response: &mut reqwest::Response) -> String {
    let mut body = Vec::new();
    let mut is_truncated = false;
    let read_error = loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break None,
            Err(error) => break Some(error),
        };

        let remaining = ERROR_BODY_LIMIT_BYTES.saturating_sub(body.len());

        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            is_truncated = true;

            break None;
        }

        body.extend_from_slice(&chunk);
    };

    let mut summary = String::from_utf8_lossy(&body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if is_truncated {
        summary.push_str(" ...");
    }
    if let Some(error) = read_error {
        if !summary.is_empty() {
            summary.push(' ');
        }
        let _ = write!(&mut summary, "[error body read failed: {error}]");
    }

    summary
}

async fn success_body(response: &mut reqwest::Response) -> Result<Vec<u8>, ChatCompletionError> {
    let limit = u64::try_from(SUCCESS_BODY_LIMIT_BYTES).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|content_length| content_length > limit)
    {
        return Err(ChatCompletionError::ResponseBodyTooLarge);
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(ChatCompletionError::transport)?
    {
        append_success_chunk(&mut body, &chunk)?;
    }

    Ok(body)
}

fn append_success_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), ChatCompletionError> {
    let remaining = SUCCESS_BODY_LIMIT_BYTES.saturating_sub(body.len());
    if chunk.len() > remaining {
        return Err(ChatCompletionError::ResponseBodyTooLarge);
    }

    body.extend_from_slice(chunk);

    Ok(())
}

/// Failure produced by a Chat Completions client implementation.
#[derive(Debug, Error)]
pub(crate) enum ChatCompletionError {
    #[error("Chat Completions returned HTTP {status}: {body}")]
    Http {
        body: String,
        #[source]
        source: reqwest::Error,
        status: reqwest::StatusCode,
    },
    #[error("Chat Completions returned an invalid response: {0}")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("Chat Completions response body exceeds the size limit")]
    ResponseBodyTooLarge,
    #[error("Chat Completions transport failed: {0}")]
    Transport(#[source] Box<dyn Error + Send + Sync>),
}

impl ChatCompletionError {
    fn transport(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Transport(Box::new(error))
    }
}

#[derive(Serialize)]
struct ChatCompletionPayload<'a> {
    messages: Vec<ChatCompletionMessagePayload>,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatCompletionTool<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatCompletionMessagePayload {
    Text {
        content: String,
        role: &'static str,
    },
    AssistantToolCall {
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        role: &'static str,
        tool_calls: Vec<ChatCompletionOutgoingToolCall>,
    },
    ToolResult {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        role: &'static str,
        tool_call_id: String,
    },
}

#[derive(Serialize)]
struct ChatCompletionOutgoingToolCall {
    function: ChatCompletionOutgoingFunctionCall,
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct ChatCompletionOutgoingFunctionCall {
    arguments: String,
    name: String,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<JsonSchemaResponseFormat<'a>>,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonSchemaResponseFormat<'a> {
    name: &'static str,
    schema: &'a Value,
}

#[derive(Serialize)]
struct ChatCompletionTool<'a> {
    function: ChatCompletionFunction<'a>,
    #[serde(rename = "type")]
    kind: &'static str,
}

impl<'a> From<&'a tool::ToolDefinition> for ChatCompletionTool<'a> {
    fn from(definition: &'a tool::ToolDefinition) -> Self {
        Self {
            function: ChatCompletionFunction {
                description: definition.description(),
                name: definition.name(),
                parameters: definition.parameters(),
            },
            kind: "function",
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionFunction<'a> {
    description: &'static str,
    name: &'static str,
    parameters: &'a Value,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    system_fingerprint: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<ChatCompletionUsage>,
}

impl ChatCompletionResponse {
    fn into_completion(self) -> Option<(ChatCompletionChoice, model::CompletionMetadata)> {
        let Self {
            choices,
            id,
            model: response_model,
            system_fingerprint,
            usage,
        } = self;
        let choice = choices.into_iter().next()?;
        let metadata = model::CompletionMetadata::new(
            choice.finish_reason.clone(),
            id,
            response_model,
            system_fingerprint,
            usage.map(model::CompletionUsage::from),
        );

        Some((choice, metadata))
    }
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    finish_reason: String,
    message: ChatCompletionMessage,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatCompletionToolCall>>,
}

#[derive(Deserialize)]
struct ChatCompletionUsage {
    #[serde(default, deserialize_with = "deserialize_optional")]
    completion_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    completion_tokens_details: Option<CompletionTokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_cache_hit_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_cache_miss_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_tokens_details: Option<PromptTokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    total_tokens: Option<u64>,
}

impl From<ChatCompletionUsage> for model::CompletionUsage {
    fn from(usage: ChatCompletionUsage) -> Self {
        Self::new(
            usage.prompt_cache_hit_tokens.or_else(|| {
                usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens)
            }),
            usage.prompt_cache_miss_tokens,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens),
            usage.total_tokens,
        )
    }
}

#[derive(Deserialize)]
struct CompletionTokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct PromptTokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    cached_tokens: Option<u64>,
}

fn deserialize_optional<'de, DeserializerType, ValueType>(
    deserializer: DeserializerType,
) -> Result<Option<ValueType>, DeserializerType::Error>
where
    DeserializerType: Deserializer<'de>,
    ValueType: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;

    Ok(serde_json::from_value(value).ok())
}

#[derive(Deserialize)]
struct ChatCompletionToolCall {
    #[serde(default)]
    function: Value,
    id: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatCompletionFunctionCall {
    arguments: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::net::TcpListener;

    use super::*;

    fn native_schema_backend() -> ChatCompletionBackend {
        ChatCompletionBackend::with_client(
            "test-key".to_string(),
            "https://example.com/v1".to_string(),
            "native-schema-model".to_string(),
            ChatCompletionProviderPolicy {
                display_name: "Native schema provider",
                response_format_with_tools: true,
                structured_output: StructuredOutputMode::JsonSchema,
                telemetry_name: "native_schema",
                unsupported_schema_reason: "object schema required",
            },
            default_client(),
        )
    }

    #[test]
    fn builds_endpoint_from_base_url() {
        // Arrange and Act
        let endpoint = endpoint("https://example.com/v1///");

        // Assert
        assert_eq!(endpoint, "https://example.com/v1/chat/completions");
    }

    #[test]
    fn serializes_tool_history_for_native_json_schema_provider() {
        // Arrange
        let backend = native_schema_backend();
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let arguments = serde_json::from_value(serde_json::json!({
            "path": "Cargo.toml"
        }))
        .expect("read arguments should be valid");
        let mut request = model::ModelRequest::new("inspect the manifest", schema);
        request.record_tool_result(
            tool::ToolCall::read("call_read".to_string(), arguments, None),
            "result".to_string(),
        );

        // Act
        let messages = serde_json::to_value(
            backend
                .messages(&request)
                .expect("tool history should serialize"),
        )
        .expect("messages should encode as JSON");

        // Assert
        assert_eq!(
            messages,
            serde_json::json!([
                {"content": "inspect the manifest", "role": "user"},
                {
                    "content": null,
                    "role": "assistant",
                    "tool_calls": [{
                        "function": {
                            "arguments": r#"{"path":"Cargo.toml"}"#,
                            "name": "read"
                        },
                        "id": "call_read",
                        "type": "function"
                    }]
                },
                {
                    "content": "result",
                    "role": "tool",
                    "tool_call_id": "call_read"
                }
            ])
        );
    }

    #[test]
    fn serializes_batched_tool_history_for_native_json_schema_provider() {
        // Arrange
        let backend = native_schema_backend();
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let calls = [
            ("call_manifest", "Cargo.toml"),
            ("call_readme", "README.md"),
        ]
        .into_iter()
        .map(|(id, path)| {
            let arguments = serde_json::from_value(serde_json::json!({ "path": path }))
                .expect("read arguments should be valid");

            tool::ToolCall::read(id.to_string(), arguments, None)
        })
        .collect();
        let mut request = model::ModelRequest::new("inspect both files", schema);
        request.record_tool_results(
            calls,
            vec!["manifest result".to_string(), "readme result".to_string()],
        );

        // Act
        let messages = serde_json::to_value(
            backend
                .messages(&request)
                .expect("batched tool history should serialize"),
        )
        .expect("messages should encode as JSON");

        // Assert
        assert_eq!(
            messages,
            serde_json::json!([
                {"content": "inspect both files", "role": "user"},
                {
                    "content": null,
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "function": {
                                "arguments": r#"{"path":"Cargo.toml"}"#,
                                "name": "read"
                            },
                            "id": "call_manifest",
                            "type": "function"
                        },
                        {
                            "function": {
                                "arguments": r#"{"path":"README.md"}"#,
                                "name": "read"
                            },
                            "id": "call_readme",
                            "type": "function"
                        }
                    ]
                },
                {
                    "content": "manifest result",
                    "role": "tool",
                    "tool_call_id": "call_manifest"
                },
                {
                    "content": "readme result",
                    "role": "tool",
                    "tool_call_id": "call_readme"
                }
            ])
        );
    }

    #[test]
    fn serializes_conversation_history() {
        // Arrange
        let backend = native_schema_backend();
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let mut request = model::ModelRequest::new("first question", schema.clone());
        request.record_output(&serde_json::json!({"message": "first answer"}));
        let mut messages = request.into_messages();
        messages.insert(
            0,
            model::ModelMessage::System("read-only instructions".to_string()),
        );
        let request = model::ModelRequest::with_history(messages, "second question", schema);

        // Act
        let messages = serde_json::to_value(
            backend
                .messages(&request)
                .expect("conversation history should serialize"),
        )
        .expect("messages should encode as JSON");

        // Assert
        assert_eq!(
            messages,
            serde_json::json!([
                {"content": "read-only instructions", "role": "system"},
                {"content": "first question", "role": "user"},
                {"content": r#"{"message":"first answer"}"#, "role": "assistant"},
                {"content": "second question", "role": "user"}
            ])
        );
    }

    #[test]
    fn decodes_complete_metadata_from_first_choice() {
        // Arrange
        let response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
            "choices": [
                {
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                },
                {
                    "finish_reason": "length",
                    "message": {"content": "ignored"}
                }
            ],
            "id": "response-1",
            "model": "provider-model",
            "system_fingerprint": "fingerprint-1",
            "usage": {
                "completion_tokens": 21,
                "completion_tokens_details": {"reasoning_tokens": 3},
                "prompt_cache_hit_tokens": 5,
                "prompt_cache_miss_tokens": 8,
                "prompt_tokens": 13,
                "prompt_tokens_details": {"cached_tokens": 99},
                "total_tokens": 34,
                "unknown_usage_field": 55
            },
            "unknown_response_field": true
        }))
        .expect("complete response metadata should decode");

        // Act
        let (choice, metadata) = response
            .into_completion()
            .expect("first completion choice should exist");
        let usage = metadata.usage().expect("usage should be retained");

        // Assert
        assert_eq!(choice.message.content.as_deref(), Some(r#"{"name":"Ada"}"#));
        assert_eq!(metadata.finish_reason(), "stop");
        assert_eq!(metadata.response_id(), Some("response-1"));
        assert_eq!(metadata.response_model(), Some("provider-model"));
        assert_eq!(metadata.system_fingerprint(), Some("fingerprint-1"));
        assert_eq!(usage.cache_hit_tokens(), Some(5));
        assert_eq!(usage.cache_miss_tokens(), Some(8));
        assert_eq!(usage.input_tokens(), Some(13));
        assert_eq!(usage.output_tokens(), Some(21));
        assert_eq!(usage.reasoning_tokens(), Some(3));
        assert_eq!(usage.total_tokens(), Some(34));
    }

    #[test]
    fn decodes_partial_usage_without_estimating_missing_counts() {
        // Arrange
        let response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {"content": null}
            }],
            "id": 42,
            "model": ["unexpected"],
            "system_fingerprint": {"unexpected": true},
            "usage": {
                "completion_tokens": "unknown",
                "completion_tokens_details": {"reasoning_tokens": -1},
                "prompt_tokens_details": {"cached_tokens": 7}
            }
        }))
        .expect("partial usage should decode");

        // Act
        let (_, metadata) = response
            .into_completion()
            .expect("completion choice should exist");
        let usage = metadata.usage().expect("partial usage should be retained");

        // Assert
        assert_eq!(usage.cache_hit_tokens(), Some(7));
        assert_eq!(usage.cache_miss_tokens(), None);
        assert_eq!(usage.input_tokens(), None);
        assert_eq!(usage.output_tokens(), None);
        assert_eq!(usage.reasoning_tokens(), None);
        assert_eq!(usage.total_tokens(), None);
        assert_eq!(metadata.response_id(), None);
        assert_eq!(metadata.response_model(), None);
        assert_eq!(metadata.system_fingerprint(), None);
    }

    #[test]
    fn preserves_missing_metadata_and_empty_choices() {
        // Arrange
        let response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "{}"}
            }]
        }))
        .expect("minimal response should decode");
        let empty_response = serde_json::from_value::<ChatCompletionResponse>(serde_json::json!({
            "choices": []
        }))
        .expect("empty choices should decode");

        // Act
        let (_, metadata) = response
            .into_completion()
            .expect("minimal response should retain its choice");

        // Assert
        assert_eq!(metadata.response_id(), None);
        assert_eq!(metadata.response_model(), None);
        assert_eq!(metadata.system_fingerprint(), None);
        assert_eq!(metadata.usage(), None);
        assert!(empty_response.into_completion().is_none());
    }

    #[test]
    fn decodes_advertised_write_tool_call() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("update", schema).with_tool(tool::ToolDefinition::write());
        let calls = vec![ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "write",
                "arguments": r#"{"path":"src/lib.rs","patch":"--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"}"#
            }),
            id: "call_write".to_string(),
            kind: "function".to_string(),
        }];

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            Some("I will update the requested file."),
            None,
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::ToolCall {
                ref call,
                ref metadata,
            } if metadata.finish_reason() == "tool_calls"
                && call.name() == "write"
                    && call.write_arguments().is_some_and(|arguments| {
                        arguments.path() == "src/lib.rs"
                            && arguments.patch().starts_with("--- a/src/lib.rs")
                    })
        ));
    }

    #[test]
    fn rejects_oversized_content_with_tool_calls() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
        let content = "x".repeat(schema_contract::RESPONSE_CONTENT_LIMIT_BYTES + 1);
        let calls = vec![ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "read",
                "arguments": r#"{"path":"Cargo.toml"}"#
            }),
            id: "call_read".to_string(),
            kind: "function".to_string(),
        }];

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            Some(&content),
            None,
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::Failed {
                error: model::ModelError::ResponseContentTooLarge,
                ..
            }
        ));
    }

    #[test]
    fn retains_metadata_for_invalid_advertised_write_arguments() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("update", schema).with_tool(tool::ToolDefinition::write());
        let calls = vec![ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "write",
                "arguments": r#"{"path":"src/lib.rs","patch":""}"#
            }),
            id: "call_write".to_string(),
            kind: "function".to_string(),
        }];

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            None,
            None,
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::Failed {
                error: model::ModelError::InvalidToolArguments { .. },
                metadata,
            } if metadata.finish_reason() == "tool_calls"
        ));
    }

    #[test]
    fn decodes_schema_valid_read_rejection_for_tool_feedback() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
        let calls = vec![ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "read",
                "arguments": r#"{"action":"search"}"#
            }),
            id: "call_search".to_string(),
            kind: "function".to_string(),
        }];

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            None,
            None,
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::ToolCall { ref call, .. }
                if call.read_arguments().is_some_and(|arguments| {
                    arguments.validation_error()
                        == Some("search requires a query and accepts only an optional path and limit")
                })
        ));
    }

    #[test]
    fn rejects_nul_search_query_as_invalid_tool_arguments() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
        let calls = vec![ChatCompletionToolCall {
            function: serde_json::json!({
                "name": "read",
                "arguments": r#"{"action":"search","query":"needle\u0000suffix"}"#
            }),
            id: "call_search".to_string(),
            kind: "function".to_string(),
        }];

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            None,
            None,
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::Failed {
                error: model::ModelError::InvalidToolArguments { .. },
                ..
            }
        ));
    }

    #[test]
    fn decodes_multiple_advertised_tool_calls() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
        let calls = ["Cargo.toml", "README.md"]
            .into_iter()
            .enumerate()
            .map(|(index, path)| ChatCompletionToolCall {
                function: serde_json::json!({
                    "name": "read",
                    "arguments": serde_json::json!({"path": path}).to_string()
                }),
                id: format!("call_{index}"),
                kind: "function".to_string(),
            })
            .collect();

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            None,
            Some("reasoning"),
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::ToolCalls { calls, metadata }
                if metadata.finish_reason() == "tool_calls"
                    && calls.len() == 2
                    && calls[0].read_arguments().is_some_and(|arguments| {
                        arguments.path() == "Cargo.toml"
                    })
                    && calls[1].read_arguments().is_some_and(|arguments| {
                        arguments.path() == "README.md"
                    })
        ));
    }

    #[test]
    fn rejects_duplicate_tool_call_ids() {
        // Arrange
        let schema = schema_contract::OutputSchema::new(serde_json::json!({
            "type": "object"
        }))
        .expect("schema should be valid");
        let request =
            model::ModelRequest::new("inspect", schema).with_tool(tool::ToolDefinition::read());
        let calls = ["Cargo.toml", "README.md"]
            .into_iter()
            .map(|path| ChatCompletionToolCall {
                function: serde_json::json!({
                    "name": "read",
                    "arguments": serde_json::json!({"path": path}).to_string()
                }),
                id: "duplicate_call".to_string(),
                kind: "function".to_string(),
            })
            .collect();

        // Act
        let response = ChatCompletionBackend::decode_tool_call(
            &request,
            None,
            None,
            calls,
            model::CompletionMetadata::new("tool_calls".to_string(), None, None, None, None),
        );

        // Assert
        assert!(matches!(
            response,
            GeneratedResponse::Failed {
                error: model::ModelError::DuplicateToolCallId { id },
                metadata,
            } if id == "duplicate_call" && metadata.finish_reason() == "tool_calls"
        ));
    }

    #[test]
    fn rejects_success_chunk_that_exceeds_remaining_capacity() {
        // Arrange
        let mut body = vec![0; SUCCESS_BODY_LIMIT_BYTES - 1];

        // Act
        let error = append_success_chunk(&mut body, &[0, 1])
            .expect_err("chunk exceeding the limit should fail");

        // Assert
        assert!(matches!(error, ChatCompletionError::ResponseBodyTooLarge));
    }

    #[test]
    fn wraps_transport_error_with_its_source() {
        // Arrange
        let source = io::Error::other("connection reset");

        // Act
        let error = ChatCompletionError::transport(source);

        // Assert
        assert_eq!(
            error.to_string(),
            "Chat Completions transport failed: connection reset"
        );
        assert_eq!(
            std::error::Error::source(&error)
                .expect("transport failure should retain its source")
                .to_string(),
            "connection reset"
        );
    }

    #[test]
    fn normalizes_transport_error_classification() {
        // Arrange
        let backend = ChatCompletionBackend::with_client(
            "test-key".to_string(),
            "https://example.com/v1".to_string(),
            "model".to_string(),
            ChatCompletionProviderPolicy {
                display_name: "Provider",
                response_format_with_tools: true,
                structured_output: StructuredOutputMode::JsonSchema,
                telemetry_name: "provider",
                unsupported_schema_reason: "object schema required",
            },
            default_client(),
        );
        let transport_error = ChatCompletionError::transport(io::Error::other("offline"));

        // Act
        let error = backend.map_completion_error(transport_error);

        // Assert
        assert_eq!(error.error_type(), model::ModelErrorType::Transport);
        assert_eq!(error.http_status(), None);
        assert_eq!(
            error.to_string(),
            "model request failed: Chat Completions transport failed: offline"
        );
    }

    #[test]
    fn rate_limit_retry_delay_uses_bounded_headers_and_backoff() {
        // Arrange
        let mut headers = reqwest::header::HeaderMap::new();

        // Act and Assert
        assert_eq!(rate_limit_retry_delay(&headers, 0), Duration::from_secs(1));
        assert_eq!(rate_limit_retry_delay(&headers, 1), Duration::from_secs(2));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "1".parse().expect("valid header"),
        );
        assert_eq!(rate_limit_retry_delay(&headers, 2), Duration::from_secs(4));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "99".parse().expect("valid header"),
        );
        assert_eq!(rate_limit_retry_delay(&headers, 0), MAX_RETRY_DELAY);
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_bytes(b"invalid").expect("valid header bytes"),
        );
        assert_eq!(rate_limit_retry_delay(&headers, 0), RETRY_DELAY);
    }

    #[tokio::test]
    async fn retries_rate_limit_response_before_decoding_success() {
        // Arrange
        let listener = TcpListener::bind("127.0.0.1:0").expect("retry listener should bind");
        let address = listener
            .local_addr()
            .expect("retry listener should have an address");
        let success_body = r#"{"choices":[{"finish_reason":"stop","message":{"content":"{\"message\":\"ok\"}"}}]}"#;
        let server = tokio::task::spawn_blocking(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener
                    .accept()
                    .expect("retry listener should accept a request");
                let mut request = [0; 2_048];
                assert!(
                    stream.read(&mut request).expect("request should be read") > 0,
                    "retry request should not be empty"
                );
                if attempt == 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 429 Too Many Requests\r\n\
                              Content-Length: 0\r\n\
                              Retry-After: 0\r\n\
                              Connection: close\r\n\r\n",
                        )
                        .expect("rate-limit response should be written");
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        success_body.len(),
                        success_body
                    )
                    .expect("success response should be written");
                }
            }
        });
        let client = ReqwestChatCompletionClient {
            client: reqwest::Client::new(),
        };
        let request = ChatCompletionRequest::new(
            "test-key",
            format!("http://{address}"),
            serde_json::json!({}),
        );

        // Act
        let completion = client
            .complete(request)
            .await
            .expect("retry should recover")
            .expect("response should contain one choice");
        server.await.expect("retry server should finish");

        // Assert
        assert_eq!(completion.content.as_deref(), Some(r#"{"message":"ok"}"#));
    }

    #[tokio::test]
    async fn retries_transport_failure_before_decoding_success() {
        // Arrange
        let listener = TcpListener::bind("127.0.0.1:0").expect("retry listener should bind");
        let address = listener
            .local_addr()
            .expect("retry listener should have an address");
        let success_body = r#"{"choices":[{"finish_reason":"stop","message":{"content":"{\"message\":\"ok\"}"}}]}"#;
        let server = tokio::task::spawn_blocking(move || {
            let (mut failed_stream, _) = listener
                .accept()
                .expect("retry listener should accept the failed request");
            let mut request = [0; 2_048];
            assert!(
                failed_stream
                    .read(&mut request)
                    .expect("failed request should be read")
                    > 0,
                "failed request should not be empty"
            );
            drop(failed_stream);

            let (mut stream, _) = listener
                .accept()
                .expect("retry listener should accept the successful request");
            assert!(
                stream
                    .read(&mut request)
                    .expect("successful request should be read")
                    > 0,
                "successful request should not be empty"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                success_body.len(),
                success_body
            )
            .expect("success response should be written");
        });
        let client = ReqwestChatCompletionClient {
            client: reqwest::Client::new(),
        };
        let request = ChatCompletionRequest::new(
            "test-key",
            format!("http://{address}"),
            serde_json::json!({}),
        );

        // Act
        let completion = client
            .complete(request)
            .await
            .expect("transport retry should recover")
            .expect("response should contain one choice");
        server.await.expect("retry server should finish");

        // Assert
        assert_eq!(completion.content.as_deref(), Some(r#"{"message":"ok"}"#));
    }

    #[tokio::test]
    async fn retains_http_status_when_error_body_read_fails() {
        // Arrange
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("truncated-response listener should bind");
        let address = listener
            .local_addr()
            .expect("truncated-response listener should have an address");
        let server = tokio::task::spawn_blocking(move || {
            for _ in 0..=MAX_RATE_LIMIT_RETRIES {
                let (mut stream, _) = listener
                    .accept()
                    .expect("truncated-response listener should accept a request");
                let mut request = [0; 2_048];
                let bytes_read = stream
                    .read(&mut request)
                    .expect("truncated-response server should read the request");
                assert!(
                    bytes_read > 0,
                    "truncated-response request should not be empty"
                );
                stream
                    .write_all(
                        b"HTTP/1.1 429 Too Many Requests\r\n\
                          Content-Length: 64\r\n\
                          Retry-After: 0\r\n\
                          Connection: close\r\n\r\n\
                          partial error body",
                    )
                    .expect("truncated-response server should write the response");
            }
        });
        let client = ReqwestChatCompletionClient {
            client: reqwest::Client::new(),
        };
        let request = ChatCompletionRequest::new(
            "test-key",
            format!("http://{address}"),
            serde_json::json!({}),
        );

        // Act
        let result = client.complete(request).await;
        server
            .await
            .expect("truncated-response server should finish");

        // Assert
        assert!(result.is_err(), "truncated HTTP error response should fail");
        let error = result
            .err()
            .expect("truncated HTTP error response should contain an error");
        assert!(matches!(
            &error,
            ChatCompletionError::Http { status, .. }
                if *status == reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(error.to_string().contains("error body read failed"));
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<reqwest::Error>())
            .expect("HTTP failure should retain its status-bearing source");
        assert_eq!(
            source.status(),
            Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
    }
}
