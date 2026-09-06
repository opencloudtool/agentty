use std::collections::HashSet;
use std::error::Error;
use std::ops::Deref;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::lifecycle::{LifecycleEmitter, LifecycleObserver, ModelResponseType};
use crate::provider::{self, KimiConfig, MuseConfig, QwenConfig};
use crate::schema_contract::{OutputSchema, OutputValidationError, bounded_diagnostic};
use crate::{chat_completion, telemetry, tool};

/// Object-safe boundary for provider-neutral model requests.
///
/// [`ModelClient`] implements this trait so applications can select supported
/// providers dynamically without exposing provider backends or raw generation.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Model: Send + Sync {
    /// Returns the configured model identity when the implementation exposes
    /// it.
    fn metadata(&self) -> Option<ModelMetadata> {
        None
    }

    /// Completes one model request with optional provider metadata and
    /// continuation state.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError>;
}

/// Application-facing client for provider-neutral model requests.
///
/// Provider request execution remains private so every request passes through
/// [`ModelClient::complete`], which owns telemetry and structured-output
/// validation.
pub struct ModelClient {
    backend: chat_completion::ChatCompletionBackend,
    lifecycle: LifecycleEmitter,
    metadata: ModelMetadata,
}

impl ModelClient {
    /// Creates a client backed by Moonshot AI's Kimi API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn kimi(config: KimiConfig) -> Result<Self, ModelMetadataError> {
        Self::chat_completion(
            config.api_key,
            config.base_url,
            config.model,
            provider::KIMI_POLICY,
        )
    }

    /// Creates a client backed by Meta's Model API for Muse models.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn muse(config: MuseConfig) -> Result<Self, ModelMetadataError> {
        Self::chat_completion(
            config.api_key,
            config.base_url,
            config.model,
            provider::MUSE_POLICY,
        )
    }

    /// Creates a client backed by Alibaba Cloud Model Studio's Qwen API.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when the configured model identifier is
    /// empty or contains only whitespace.
    pub fn qwen(config: QwenConfig) -> Result<Self, ModelMetadataError> {
        Self::chat_completion(
            config.api_key,
            config.base_url,
            config.model,
            provider::QWEN_POLICY,
        )
    }

    /// Returns the validated provider and model identity retained by the
    /// client.
    pub fn metadata(&self) -> &ModelMetadata {
        &self.metadata
    }

    /// Sends metadata-only request lifecycle events to `observer`.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.lifecycle = LifecycleEmitter::new(observer);

        self
    }

    /// Completes one model request through the shared telemetry and
    /// structured-output lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the provider request fails or its response
    /// cannot be converted to the provider-neutral response.
    pub async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let metrics = telemetry::RequestMetrics::start(self.metadata());
        let lifecycle = if request.lifecycle_observed() {
            None
        } else {
            self.lifecycle
                .start_model_request(Some(self.metadata.clone()), 0, None)
        };
        let operation = self.backend.generate(&request);
        let generated = match lifecycle.as_ref() {
            Some(lifecycle) => lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (result, failure_metadata) = match generated {
            Ok(chat_completion::GeneratedResponse::Failed { error, metadata }) => {
                (Err(error), Some(metadata))
            }
            Ok(chat_completion::GeneratedResponse::Output { metadata, output }) => {
                match request.schema().parse_and_validate(&output) {
                    Ok(response) => (
                        Ok(ModelCompletion::new(
                            metadata,
                            ModelResponse::from_output(response),
                        )),
                        None,
                    ),
                    Err(error) => (Err(ModelError::from(error)), Some(metadata)),
                }
            }
            Ok(chat_completion::GeneratedResponse::ToolCall { call, metadata }) => (
                Ok(ModelCompletion::new(
                    metadata,
                    ModelResponse::tool_call(call),
                )),
                None,
            ),
            Ok(chat_completion::GeneratedResponse::ToolCalls { calls, metadata }) => (
                Ok(ModelCompletion::new(
                    metadata,
                    ModelResponse::tool_calls(calls),
                )),
                None,
            ),
            Err(error) => (Err(error), None),
        };

        match &result {
            Ok(completion) => {
                if let Some(metadata) = completion.metadata() {
                    metrics.completed(metadata);
                }
            }
            Err(error) => metrics.failed(error, failure_metadata.as_ref()),
        }

        if let Some(lifecycle) = lifecycle {
            match &result {
                Ok(completion) => lifecycle.completed(
                    completion.metadata.clone(),
                    completion.response.response_type(),
                ),
                Err(error) => lifecycle.failed(error.error_type(), error.http_status()),
            }
        }

        result
    }

    fn chat_completion(
        api_key: String,
        base_url: String,
        model: String,
        policy: chat_completion::ChatCompletionProviderPolicy,
    ) -> Result<Self, ModelMetadataError> {
        let backend = chat_completion::ChatCompletionBackend::new(api_key, base_url, model, policy);
        let (provider, model) = backend.identity();
        let metadata = ModelMetadata::new(provider, model)?;

        Ok(Self {
            backend,
            lifecycle: LifecycleEmitter::default(),
            metadata,
        })
    }
}

#[async_trait]
impl Model for ModelClient {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.metadata.clone())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        ModelClient::complete(self, request).await
    }
}

/// Validated provider and model identity used by the shared client lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelMetadata {
    model: String,
    provider: &'static str,
}

impl ModelMetadata {
    /// Creates metadata for one provider model.
    ///
    /// # Errors
    ///
    /// Returns [`ModelMetadataError`] when `provider` or `model` is empty or
    /// contains only whitespace.
    pub fn new(
        provider: &'static str,
        model: impl Into<String>,
    ) -> Result<Self, ModelMetadataError> {
        if provider.trim().is_empty() {
            return Err(ModelMetadataError::EmptyProvider);
        }
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ModelMetadataError::EmptyModel);
        }

        Ok(Self { model, provider })
    }

    /// Returns the model identifier sent to the provider.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the provider identifier used by telemetry.
    pub fn provider(&self) -> &'static str {
        self.provider
    }
}

/// Invalid identity attributes supplied by a model provider.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ModelMetadataError {
    /// The provider identifier is empty or contains only whitespace.
    #[error("model provider must not be empty")]
    EmptyProvider,
    /// The model identifier is empty or contains only whitespace.
    #[error("model identifier must not be empty")]
    EmptyModel,
}

/// Provider-neutral input for one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    lifecycle_observed: bool,
    messages: Vec<ModelMessage>,
    prompt: String,
    provider_session_id: Option<String>,
    schema: OutputSchema,
    tools: Vec<tool::ToolDefinition>,
}

impl ModelRequest {
    /// Creates a model request whose response must match `schema`.
    pub fn new(prompt: impl Into<String>, schema: OutputSchema) -> Self {
        let prompt = prompt.into();

        Self {
            lifecycle_observed: false,
            messages: vec![ModelMessage::User(prompt.clone())],
            prompt,
            provider_session_id: None,
            schema,
            tools: Vec::new(),
        }
    }

    pub(crate) fn with_history(
        messages: Vec<ModelMessage>,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Self {
        let prompt = prompt.into();
        let mut messages = messages;
        messages.push(ModelMessage::User(prompt.clone()));

        Self {
            lifecycle_observed: false,
            messages,
            prompt,
            provider_session_id: None,
            schema,
            tools: Vec::new(),
        }
    }

    /// Advertises one native function tool for this request.
    #[must_use]
    pub fn with_tool(mut self, tool: tool::ToolDefinition) -> Self {
        if !self.advertises_tool(tool.name()) {
            self.tools.push(tool);
        }

        self
    }

    /// Returns the request prompt.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the opaque provider conversation identifier to resume, when
    /// native continuation is available.
    pub fn provider_session_id(&self) -> Option<&str> {
        self.provider_session_id.as_deref()
    }

    /// Returns the schema that the response must match.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the native function tools explicitly advertised by the caller.
    pub fn tools(&self) -> &[tool::ToolDefinition] {
        &self.tools
    }

    /// Returns the ordered conversation to send to the provider, including
    /// the system prompt, retained turns, current prompt, and tool results.
    ///
    /// Model adapters must use this history rather than only [`Self::prompt`]
    /// to support chat and tool execution. The harness owns its mutation.
    pub fn messages(&self) -> &[ModelMessage] {
        &self.messages
    }

    pub(crate) fn advertises_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name() == name)
    }

    pub(crate) fn lifecycle_observed(&self) -> bool {
        self.lifecycle_observed
    }

    pub(crate) fn mark_lifecycle_observed(&mut self) {
        self.lifecycle_observed = true;
    }

    pub(crate) fn set_provider_session_id(&mut self, provider_session_id: Option<String>) {
        self.provider_session_id = provider_session_id;
    }

    pub(crate) fn record_tool_result(&mut self, call: tool::ToolCall, content: String) {
        let call_id = call.id().to_string();
        let name = call.name().to_string();
        self.messages.push(ModelMessage::AssistantToolCall(call));
        self.messages.push(ModelMessage::ToolResult {
            call_id,
            content,
            name,
        });
    }

    pub(crate) fn record_tool_results(
        &mut self,
        calls: Vec<tool::ToolCall>,
        contents: Vec<String>,
    ) {
        debug_assert_eq!(calls.len(), contents.len());
        let results: Vec<_> = calls
            .iter()
            .zip(contents)
            .map(|(call, content)| (call.id().to_string(), call.name().to_string(), content))
            .collect();
        self.messages.push(ModelMessage::AssistantToolCalls(calls));
        self.messages
            .extend(
                results
                    .into_iter()
                    .map(|(call_id, name, content)| ModelMessage::ToolResult {
                        call_id,
                        content,
                        name,
                    }),
            );
    }

    pub(crate) fn record_output(&mut self, output: &Value) {
        self.messages
            .push(ModelMessage::Assistant(output.to_string()));
    }

    pub(crate) fn into_messages(self) -> Vec<ModelMessage> {
        self.messages
    }
}

/// Provider-neutral conversation entry supplied through
/// [`ModelRequest::messages`].
///
/// Tool results retain their call identifiers so adapters can correlate them
/// with the preceding assistant calls without parsing provider wire formats.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelMessage {
    /// Validated structured assistant output serialized as JSON text.
    Assistant(String),
    /// One assistant tool call, followed by its result.
    AssistantToolCall(tool::ToolCall),
    /// An ordered assistant tool-call batch, followed by its ordered results.
    AssistantToolCalls(Vec<tool::ToolCall>),
    /// Application-provided instructions for the conversation.
    System(String),
    /// Harness-produced feedback for one assistant tool call.
    ToolResult {
        /// Identifier of the corresponding assistant tool call.
        call_id: String,
        /// Serialized tool output or corrective failure feedback.
        content: String,
        /// Built-in tool name.
        name: String,
    },
    /// User prompt for a retained or current turn.
    User(String),
}

impl ModelMessage {
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Assistant(content) | Self::System(content) | Self::User(content) => content.len(),
            Self::AssistantToolCall(call) => {
                let arguments = call
                    .arguments_json()
                    .map_or(usize::MAX, |arguments| arguments.len());

                call.id()
                    .len()
                    .saturating_add(call.name().len())
                    .saturating_add(arguments)
                    .saturating_add(call.reasoning_content().map_or(0, str::len))
            }
            Self::AssistantToolCalls(calls) => calls.iter().fold(0, |bytes, call| {
                let arguments = call
                    .arguments_json()
                    .map_or(usize::MAX, |arguments| arguments.len());

                bytes
                    .saturating_add(call.id().len())
                    .saturating_add(call.name().len())
                    .saturating_add(arguments)
                    .saturating_add(call.reasoning_content().map_or(0, str::len))
            }),
            Self::ToolResult {
                call_id,
                content,
                name,
            } => call_id
                .len()
                .saturating_add(content.len())
                .saturating_add(name.len()),
        }
    }
}

/// One model response paired with normalized provider completion metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCompletion {
    metadata: Option<CompletionMetadata>,
    provider_session_id: Option<String>,
    response: ModelResponse,
}

impl ModelCompletion {
    /// Creates a completion from normalized metadata and a model response.
    pub fn new(metadata: CompletionMetadata, response: ModelResponse) -> Self {
        Self {
            metadata: Some(metadata),
            provider_session_id: None,
            response,
        }
    }

    /// Creates a completion without provider-reported metadata.
    pub fn from_response(response: ModelResponse) -> Self {
        Self {
            metadata: None,
            provider_session_id: None,
            response,
        }
    }

    /// Attaches the opaque provider session identifier returned by this turn.
    #[must_use]
    pub fn with_provider_session_id(mut self, provider_session_id: impl Into<String>) -> Self {
        self.provider_session_id = Some(provider_session_id.into());

        self
    }

    /// Returns the normalized metadata reported by the provider.
    pub fn metadata(&self) -> Option<&CompletionMetadata> {
        self.metadata.as_ref()
    }

    /// Returns the opaque provider session identifier for the next turn.
    pub fn provider_session_id(&self) -> Option<&str> {
        self.provider_session_id.as_deref()
    }

    /// Returns the provider-neutral model response.
    pub fn response(&self) -> &ModelResponse {
        &self.response
    }

    /// Consumes the completion and returns its provider-neutral response.
    pub fn into_response(self) -> ModelResponse {
        self.response
    }

    pub(crate) fn into_parts(self) -> (ModelResponse, Option<CompletionMetadata>, Option<String>) {
        (self.response, self.metadata, self.provider_session_id)
    }
}

impl Deref for ModelCompletion {
    type Target = ModelResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

/// Provider-reported facts about one completed model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionMetadata {
    finish_reason: String,
    response_id: Option<String>,
    response_model: Option<String>,
    system_fingerprint: Option<String>,
    usage: Option<CompletionUsage>,
}

impl CompletionMetadata {
    /// Creates normalized provider completion metadata.
    pub fn new(
        finish_reason: String,
        response_id: Option<String>,
        response_model: Option<String>,
        system_fingerprint: Option<String>,
        usage: Option<CompletionUsage>,
    ) -> Self {
        Self {
            finish_reason,
            response_id,
            response_model,
            system_fingerprint,
            usage,
        }
    }

    /// Returns the provider's reason that generation stopped.
    pub fn finish_reason(&self) -> &str {
        &self.finish_reason
    }

    /// Returns the provider-assigned response identifier, when reported.
    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    /// Returns the model identifier reported in the response, when present.
    pub fn response_model(&self) -> Option<&str> {
        self.response_model.as_deref()
    }

    /// Returns the provider's backend fingerprint, when reported.
    pub fn system_fingerprint(&self) -> Option<&str> {
        self.system_fingerprint.as_deref()
    }

    /// Returns provider-reported token usage, when present.
    pub fn usage(&self) -> Option<&CompletionUsage> {
        self.usage.as_ref()
    }
}

/// Provider-reported token counts for one completed model request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionUsage {
    cache_hit: Option<u64>,
    cache_miss: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    total: Option<u64>,
}

impl CompletionUsage {
    /// Creates normalized provider-reported token usage.
    pub fn new(
        cache_hit_tokens: Option<u64>,
        cache_miss_tokens: Option<u64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        reasoning_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self {
            cache_hit: cache_hit_tokens,
            cache_miss: cache_miss_tokens,
            input: input_tokens,
            output: output_tokens,
            reasoning: reasoning_tokens,
            total: total_tokens,
        }
    }

    /// Returns input tokens served from a provider cache, when reported.
    pub fn cache_hit_tokens(self) -> Option<u64> {
        self.cache_hit
    }

    /// Returns input tokens that missed a provider cache, when reported.
    pub fn cache_miss_tokens(self) -> Option<u64> {
        self.cache_miss
    }

    /// Returns the provider-reported input token count.
    pub fn input_tokens(self) -> Option<u64> {
        self.input
    }

    /// Returns the provider-reported output token count.
    pub fn output_tokens(self) -> Option<u64> {
        self.output
    }

    /// Returns output tokens used for provider-exposed reasoning, when
    /// reported.
    pub fn reasoning_tokens(self) -> Option<u64> {
        self.reasoning
    }

    /// Returns the provider-reported total token count.
    pub fn total_tokens(self) -> Option<u64> {
        self.total
    }
}

/// Provider-neutral output from one model request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelResponse {
    /// Terminal structured model output.
    ///
    /// [`ModelClient`] and [`crate::Harness`] validate this value locally
    /// against the request schema before returning it to applications.
    Output(Value),
    /// One validated native function call requiring application handling.
    ToolCall(tool::ToolCall),
    /// Multiple validated native function calls from one model response.
    ///
    /// The harness rejects an empty batch as a missing tool call.
    ToolCalls(Vec<tool::ToolCall>),
}

impl ModelResponse {
    /// Returns terminal structured output, when present.
    pub fn output(&self) -> Option<&Value> {
        match self {
            Self::Output(output) => Some(output),
            Self::ToolCall(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns the intermediate native function call, when present.
    pub fn call(&self) -> Option<&tool::ToolCall> {
        match self {
            Self::ToolCall(call) => Some(call),
            Self::Output(_) | Self::ToolCalls(_) => None,
        }
    }

    /// Returns every intermediate native function call in this response.
    pub fn calls(&self) -> &[tool::ToolCall] {
        match self {
            Self::Output(_) => &[],
            Self::ToolCall(call) => std::slice::from_ref(call),
            Self::ToolCalls(calls) => calls,
        }
    }

    fn from_output(output: Value) -> Self {
        Self::Output(output)
    }

    fn tool_call(call: tool::ToolCall) -> Self {
        Self::ToolCall(call)
    }

    fn tool_calls(calls: Vec<tool::ToolCall>) -> Self {
        Self::ToolCalls(calls)
    }

    pub(crate) fn response_type(&self) -> ModelResponseType {
        match self {
            Self::Output(_) => ModelResponseType::Output,
            Self::ToolCall(_) | Self::ToolCalls(_) => ModelResponseType::ToolCall,
        }
    }
}

/// Failure returned while completing a model request.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The provider request or response decoding failed.
    #[error("model request failed: {0}")]
    Request(#[source] Box<dyn Error + Send + Sync>),
    /// The provider returned a successful response without assistant content.
    #[error("model returned no response content")]
    InvalidResponse,
    /// The provider could not restore the requested native session.
    #[error("provider session is unavailable")]
    ResumeUnavailable,
    /// The provider stopped before completing the model response.
    #[error("model response is incomplete: {reason}")]
    IncompleteResponse {
        /// Provider-specific reason generation stopped.
        reason: String,
    },
    /// The successful provider response body exceeds the adapter safety limit.
    #[error("model response body exceeds the size limit")]
    ResponseBodyTooLarge,
    /// The provider cannot represent the requested output schema.
    #[error("provider cannot satisfy this output schema: {reason}")]
    UnsupportedOutputSchema {
        /// Provider-specific reason the schema cannot be represented.
        reason: String,
    },
    /// The decoded provider response content exceeds the harness safety limit.
    #[error("model response content exceeds the size limit")]
    ResponseContentTooLarge,
    /// The provider returned malformed JSON for a structured request.
    #[error("model returned invalid JSON: {reason}")]
    InvalidJson {
        /// JSON parser diagnostic without the raw response body.
        reason: String,
    },
    /// The returned JSON does not conform to the requested schema.
    #[error("model output violates the schema at {path}: {reason}")]
    SchemaViolation {
        /// Bounded JSON Pointer-like path to the invalid value, or `$` for the
        /// root.
        path: String,
        /// Validator diagnostic for the failed constraint.
        reason: String,
    },
    /// The provider returned tool calls without any call entries.
    #[error("model returned no tool call")]
    MissingToolCall,
    /// The provider tool-call identifier is blank or exceeds its byte limit.
    #[error("model returned a blank or oversized tool call identifier")]
    InvalidToolCallId,
    /// The provider returned more than the single supported call.
    #[error("model returned multiple tool calls")]
    MultipleToolCalls,
    /// The provider returned multiple tool calls with the same identifier.
    #[error("model returned duplicate tool call identifier: {id}")]
    DuplicateToolCallId {
        /// Bounded duplicate identifier returned by the provider.
        id: String,
    },
    /// A terminal response also contained native tool calls.
    #[error("model terminal response contained tool calls")]
    TerminalResponseWithToolCalls,
    /// The provider returned an unsupported tool-call type.
    #[error("model requested unsupported tool type: {kind}")]
    UnsupportedToolType {
        /// Provider tool type that is not a native function.
        kind: String,
    },
    /// The provider returned an unsupported or unadvertised native function.
    #[error("model requested unsupported tool: {name}")]
    UnsupportedToolName {
        /// Native function name that was not advertised for the request.
        name: String,
    },
    /// The provider returned malformed or invalid native function arguments.
    #[error("model returned invalid tool arguments: {reason}")]
    InvalidToolArguments {
        /// Bounded parser or validation diagnostic.
        reason: String,
    },
}

/// Stable, low-cardinality classification for a [`ModelError`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelErrorType {
    /// Request construction or another unclassified client-side failure.
    Request,
    /// Network transport failed before a provider response was decoded.
    Transport,
    /// The provider returned an unsuccessful HTTP response.
    Provider,
    /// The provider returned a malformed response envelope.
    InvalidProviderResponse,
    /// The provider returned an unusable or incomplete successful response.
    InvalidResponse,
    /// The provider cannot satisfy the requested output contract.
    UnsupportedOutput,
    /// The response exceeded a configured safety bound.
    ResponseTooLarge,
    /// Terminal output failed JSON parsing or local schema validation.
    InvalidOutput,
    /// A native tool call was missing, malformed, or unsupported.
    InvalidToolCall,
}

impl ModelErrorType {
    /// Returns the stable value intended for telemetry attributes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => telemetry::ERROR_REQUEST,
            Self::Transport => telemetry::ERROR_TRANSPORT,
            Self::Provider => telemetry::ERROR_PROVIDER,
            Self::InvalidProviderResponse => telemetry::ERROR_INVALID_PROVIDER_RESPONSE,
            Self::InvalidResponse => telemetry::ERROR_INVALID_RESPONSE,
            Self::UnsupportedOutput => telemetry::ERROR_UNSUPPORTED_OUTPUT,
            Self::ResponseTooLarge => telemetry::ERROR_RESPONSE_TOO_LARGE,
            Self::InvalidOutput => telemetry::ERROR_INVALID_OUTPUT,
            Self::InvalidToolCall => telemetry::ERROR_INVALID_TOOL_CALL,
        }
    }
}

#[derive(Debug, Error)]
#[error("{source}")]
struct ClassifiedRequestError {
    error_type: ModelErrorType,
    http_status: Option<u16>,
    #[source]
    source: Box<dyn Error + Send + Sync>,
}

#[derive(Debug, Error)]
#[error("{provider} returned HTTP {status}: {body}")]
struct ProviderRequestError {
    body: String,
    provider: &'static str,
    #[source]
    source: reqwest::Error,
    status: reqwest::StatusCode,
}

impl ModelError {
    /// Wraps a provider transport or response-decoding failure.
    pub fn request(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Request(Box::new(error))
    }

    /// Returns a stable, low-cardinality classification for this failure.
    pub fn error_type(&self) -> ModelErrorType {
        match self {
            Self::Request(source) => {
                if source.downcast_ref::<ProviderRequestError>().is_some() {
                    ModelErrorType::Provider
                } else {
                    source
                        .downcast_ref::<ClassifiedRequestError>()
                        .map_or(ModelErrorType::Request, |error| error.error_type)
                }
            }
            Self::InvalidResponse | Self::IncompleteResponse { .. } => {
                ModelErrorType::InvalidResponse
            }
            Self::ResumeUnavailable => ModelErrorType::Provider,
            Self::ResponseBodyTooLarge | Self::ResponseContentTooLarge => {
                ModelErrorType::ResponseTooLarge
            }
            Self::UnsupportedOutputSchema { .. } => ModelErrorType::UnsupportedOutput,
            Self::InvalidJson { .. } | Self::SchemaViolation { .. } => {
                ModelErrorType::InvalidOutput
            }
            Self::MissingToolCall
            | Self::InvalidToolCallId
            | Self::MultipleToolCalls
            | Self::DuplicateToolCallId { .. }
            | Self::TerminalResponseWithToolCalls
            | Self::UnsupportedToolType { .. }
            | Self::UnsupportedToolName { .. }
            | Self::InvalidToolArguments { .. } => ModelErrorType::InvalidToolCall,
        }
    }

    /// Returns the provider HTTP status associated with this failure, when
    /// available.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::Request(source) => source
                .downcast_ref::<ProviderRequestError>()
                .map(|error| error.status.as_u16())
                .or_else(|| {
                    source
                        .downcast_ref::<ClassifiedRequestError>()
                        .and_then(|error| error.http_status)
                }),
            _ => None,
        }
    }

    pub(crate) fn provider_request(
        provider: &'static str,
        body: String,
        source: reqwest::Error,
        status: reqwest::StatusCode,
    ) -> Self {
        Self::Request(Box::new(ProviderRequestError {
            body,
            provider,
            source,
            status,
        }))
    }

    pub(crate) fn classified_request(
        error_type: ModelErrorType,
        http_status: Option<u16>,
        source: Box<dyn Error + Send + Sync>,
    ) -> Self {
        Self::Request(Box::new(ClassifiedRequestError {
            error_type,
            http_status,
            source,
        }))
    }
}

pub(crate) fn ensure_unique_tool_call_ids(calls: &[tool::ToolCall]) -> Result<(), ModelError> {
    let mut call_ids = HashSet::with_capacity(calls.len());
    for call in calls {
        if !call_ids.insert(call.id()) {
            return Err(ModelError::DuplicateToolCallId {
                id: bounded_diagnostic(call.id()),
            });
        }
    }

    Ok(())
}

impl From<OutputValidationError> for ModelError {
    fn from(error: OutputValidationError) -> Self {
        match error {
            OutputValidationError::InvalidJson(reason) => Self::InvalidJson { reason },
            OutputValidationError::SchemaViolation { path, reason } => {
                Self::SchemaViolation { path, reason }
            }
            OutputValidationError::TooLarge => Self::ResponseContentTooLarge,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::tool::{ReadArguments, ToolCall};

    struct ResponseOnlyModel;

    #[async_trait]
    impl Model for ResponseOnlyModel {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
            Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({ "name": "Ada" }),
            )))
        }
    }

    struct MetadataModel;

    #[async_trait]
    impl Model for MetadataModel {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
            Ok(ModelCompletion::new(
                CompletionMetadata::new(
                    "stop".to_string(),
                    Some("response-id".to_string()),
                    None,
                    None,
                    None,
                ),
                ModelResponse::Output(json!({ "name": "Ada" })),
            ))
        }
    }

    fn test_request() -> ModelRequest {
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("fixture schema should be valid");

        ModelRequest::new("prompt", schema)
    }

    #[tokio::test]
    async fn response_only_model_returns_completion_without_metadata() {
        // Arrange
        let model = ResponseOnlyModel;

        // Act
        let model_metadata = Model::metadata(&model);
        let completion = model
            .complete(test_request())
            .await
            .expect("response-only model should complete");

        // Assert
        assert!(model_metadata.is_none());
        assert_eq!(
            completion.response().output(),
            Some(&json!({ "name": "Ada" }))
        );
        assert!(completion.metadata().is_none());
    }

    #[tokio::test]
    async fn model_completion_exposes_optional_metadata() {
        // Arrange
        let model = MetadataModel;

        // Act
        let model_metadata = Model::metadata(&model);
        let completion = Model::complete(&model, test_request())
            .await
            .expect("metadata model should complete through Model");

        // Assert
        assert!(model_metadata.is_none());
        assert_eq!(
            completion.response().output(),
            Some(&json!({ "name": "Ada" }))
        );
        assert_eq!(
            completion
                .metadata()
                .and_then(CompletionMetadata::response_id),
            Some("response-id")
        );
    }

    #[test]
    fn client_exposes_provider_and_model() {
        // Arrange
        let client = ModelClient::qwen(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: "https://example.com".to_string(),
            model: "qwen-plus".to_string(),
        })
        .expect("fixture configuration should be valid");

        // Act
        let metadata = client.metadata();
        let trait_metadata = Model::metadata(&client)
            .expect("model client should expose configured metadata through the trait");

        // Assert
        assert_eq!(metadata.provider(), "alibaba_cloud");
        assert_eq!(metadata.model(), "qwen-plus");
        assert_eq!(
            metadata,
            &ModelMetadata::new("alibaba_cloud", "qwen-plus").expect("metadata should be valid")
        );
        assert_eq!(&trait_metadata, metadata);
    }

    #[tokio::test]
    async fn client_observes_success_unless_request_is_already_observed() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let client = ModelClient::qwen(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            model: "qwen-plus".to_string(),
        })
        .expect("fixture configuration should be valid")
        .with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });
        let schema = OutputSchema::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        }))
        .expect("fixture schema should be valid");
        let mut observed_request = ModelRequest::new("prompt", schema.clone());
        observed_request.mark_lifecycle_observed();

        // Act
        client
            .complete(ModelRequest::new("prompt", schema))
            .await
            .expect("request should succeed");
        client
            .complete(observed_request)
            .await
            .expect("externally observed request should succeed");

        // Assert
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].kind(),
            crate::LifecycleEventKind::ModelRequestStarted { .. }
        ));
        assert!(matches!(
            events[1].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn client_returns_multiple_tool_calls() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": "",
                        "tool_calls": [
                            {
                                "id": "call_manifest",
                                "type": "function",
                                "function": {
                                    "name": "read",
                                    "arguments": r#"{"path":"Cargo.toml"}"#
                                }
                            },
                            {
                                "id": "call_readme",
                                "type": "function",
                                "function": {
                                    "name": "read",
                                    "arguments": r#"{"path":"README.md"}"#
                                }
                            }
                        ]
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = ModelClient::qwen(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            model: "qwen-plus".to_string(),
        })
        .expect("fixture configuration should be valid");
        let request = test_request().with_tool(tool::ToolDefinition::read());

        // Act
        let completion = client
            .complete(request)
            .await
            .expect("batched tool response should complete");

        // Assert
        assert_eq!(
            completion
                .metadata()
                .expect("provider completion should include metadata")
                .finish_reason(),
            "tool_calls"
        );
        assert_eq!(
            completion
                .response()
                .calls()
                .iter()
                .map(tool::ToolCall::id)
                .collect::<Vec<_>>(),
            ["call_manifest", "call_readme"]
        );
    }

    #[tokio::test]
    async fn client_observes_classified_failure() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("offline"))
            .expect(1)
            .mount(&server)
            .await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let client = ModelClient::qwen(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            model: "qwen-plus".to_string(),
        })
        .expect("fixture configuration should be valid")
        .with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("fixture schema should be valid");

        // Act
        let error = client
            .complete(ModelRequest::new("prompt", schema))
            .await
            .expect_err("provider failure should be returned");

        // Assert
        assert_eq!(error.error_type(), ModelErrorType::Provider);
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1].kind(),
            crate::LifecycleEventKind::ModelRequestFailed {
                error_type: ModelErrorType::Provider,
                http_status: Some(503),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn client_supports_dynamic_model_dispatch() {
        // Arrange
        let model: Box<dyn Model> = Box::new(
            ModelClient::qwen(QwenConfig {
                api_key: "test-key".to_string(),
                base_url: "https://example.com".to_string(),
                model: "qwen-plus".to_string(),
            })
            .expect("fixture configuration should be valid"),
        );
        let schema = OutputSchema::new(json!({ "type": "array" })).expect("schema should be valid");

        // Act
        let error = model
            .complete(ModelRequest::new("return a list", schema))
            .await
            .expect_err("Qwen should reject a non-object schema");

        // Assert
        assert!(matches!(error, ModelError::UnsupportedOutputSchema { .. }));
    }

    #[test]
    fn metadata_rejects_empty_provider() {
        // Arrange and Act
        let error =
            ModelMetadata::new("  ", "stub-large").expect_err("empty provider should be rejected");

        // Assert
        assert_eq!(error, ModelMetadataError::EmptyProvider);
        assert_eq!(error.to_string(), "model provider must not be empty");
    }

    #[test]
    fn metadata_rejects_empty_model() {
        // Arrange and Act
        let error =
            ModelMetadata::new("stub_provider", "  ").expect_err("empty model should be rejected");

        // Assert
        assert_eq!(error, ModelMetadataError::EmptyModel);
        assert_eq!(error.to_string(), "model identifier must not be empty");
    }

    #[test]
    fn request_contains_prompt_and_schema() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema.clone());

        // Assert
        assert_eq!(request.prompt(), "hello");
        assert_eq!(request.schema(), &schema);
        assert_eq!(request.tools(), []);
    }

    #[test]
    fn request_explicitly_advertises_read() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema).with_tool(tool::ToolDefinition::read());

        // Assert
        assert_eq!(request.tools(), &[tool::ToolDefinition::read()]);
        assert!(request.advertises_tool("read"));
        assert!(!request.advertises_tool("write"));
    }

    #[test]
    fn request_deduplicates_native_tools() {
        // Arrange
        let schema =
            OutputSchema::new(json!({ "type": "object" })).expect("schema should be valid");

        // Act
        let request = ModelRequest::new("hello", schema)
            .with_tool(tool::ToolDefinition::read())
            .with_tool(tool::ToolDefinition::read());

        // Assert
        assert_eq!(request.tools(), &[tool::ToolDefinition::read()]);
    }

    #[test]
    fn response_exposes_terminal_output() {
        // Arrange
        let value = json!({ "name": "Ada" });

        // Act
        let response = ModelResponse::from_output(value.clone());

        // Assert
        assert_eq!(response.output(), Some(&value));
        assert!(response.call().is_none());
        assert_eq!(response.calls(), []);
    }

    #[test]
    fn response_exposes_multiple_tool_calls() {
        // Arrange
        let calls = vec![
            tool::ToolCall::read(
                "call_one".to_string(),
                serde_json::from_value(json!({"path": "Cargo.toml"}))
                    .expect("read arguments should be valid"),
                None,
            ),
            tool::ToolCall::read(
                "call_two".to_string(),
                serde_json::from_value(json!({"path": "README.md"}))
                    .expect("read arguments should be valid"),
                None,
            ),
        ];

        // Act
        let response = ModelResponse::tool_calls(calls);

        // Assert
        assert!(response.output().is_none());
        assert!(response.call().is_none());
        assert_eq!(
            response
                .calls()
                .iter()
                .map(tool::ToolCall::id)
                .collect::<Vec<_>>(),
            ["call_one", "call_two"]
        );
    }

    #[test]
    fn batched_tool_message_retained_bytes_sums_each_call() {
        // Arrange
        let calls = vec![
            tool::ToolCall::read(
                "call_manifest".to_string(),
                serde_json::from_value(json!({"path": "Cargo.toml"}))
                    .expect("read arguments should be valid"),
                Some("reasoning".to_string()),
            ),
            tool::ToolCall::read(
                "call_readme".to_string(),
                serde_json::from_value(json!({"path": "README.md"}))
                    .expect("read arguments should be valid"),
                None,
            ),
        ];
        let message = ModelMessage::AssistantToolCalls(calls);
        let expected = "call_manifest".len()
            + "read".len()
            + r#"{"path":"Cargo.toml"}"#.len()
            + "reasoning".len()
            + "call_readme".len()
            + "read".len()
            + r#"{"path":"README.md"}"#.len();

        // Act
        let retained_bytes = message.retained_bytes();

        // Assert
        assert_eq!(retained_bytes, expected);
    }

    #[test]
    fn completion_exposes_normalized_metadata_and_response() {
        // Arrange
        let usage = CompletionUsage::new(Some(5), Some(8), Some(13), Some(21), Some(3), Some(34));
        let metadata = CompletionMetadata::new(
            "stop".to_string(),
            Some("response-1".to_string()),
            Some("provider-model".to_string()),
            Some("fingerprint-1".to_string()),
            Some(usage),
        );
        let response = ModelResponse::from_output(json!({ "name": "Ada" }));
        let completion = ModelCompletion::new(metadata, response.clone())
            .with_provider_session_id("provider-session-1");

        // Act
        let completion_metadata = completion
            .metadata()
            .expect("completion should include metadata");
        let completion_response = completion.response();

        // Assert
        assert_eq!(completion_metadata.finish_reason(), "stop");
        assert_eq!(completion_metadata.response_id(), Some("response-1"));
        assert_eq!(completion_metadata.response_model(), Some("provider-model"));
        assert_eq!(
            completion_metadata.system_fingerprint(),
            Some("fingerprint-1")
        );
        assert_eq!(completion_metadata.usage(), Some(&usage));
        assert_eq!(completion.provider_session_id(), Some("provider-session-1"));
        assert_eq!(usage.cache_hit_tokens(), Some(5));
        assert_eq!(usage.cache_miss_tokens(), Some(8));
        assert_eq!(usage.input_tokens(), Some(13));
        assert_eq!(usage.output_tokens(), Some(21));
        assert_eq!(usage.reasoning_tokens(), Some(3));
        assert_eq!(usage.total_tokens(), Some(34));
        assert_eq!(completion_response, &response);
        assert_eq!(completion.into_response(), response);
    }

    #[test]
    fn completion_metadata_preserves_absent_provider_fields() {
        // Arrange
        let metadata = CompletionMetadata::new("stop".to_string(), None, None, None, None);

        // Act and Assert
        assert_eq!(metadata.response_id(), None);
        assert_eq!(metadata.response_model(), None);
        assert_eq!(metadata.system_fingerprint(), None);
        assert_eq!(metadata.usage(), None);
    }

    #[test]
    fn response_debug_redacts_provider_reasoning() {
        // Arrange
        let secret_reasoning = "private reasoning from repository context";
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "path": "Cargo.toml"
        }))
        .expect("read arguments should be valid");
        let response = ModelResponse::tool_call(ToolCall::read(
            "call_read".to_string(),
            arguments,
            Some(secret_reasoning.to_string()),
        ));

        // Act
        let debug_output = format!("{response:?}");

        // Assert
        assert_eq!(response.calls()[0].id(), "call_read");
        assert!(debug_output.contains("call_read"));
        assert!(debug_output.contains("[REDACTED]"));
        assert!(!debug_output.contains(secret_reasoning));
    }

    #[test]
    fn invalid_response_error_has_user_facing_message() {
        // Arrange and Act
        let message = ModelError::InvalidResponse.to_string();

        // Assert
        assert_eq!(message, "model returned no response content");
    }

    #[test]
    fn incomplete_response_error_includes_reason() {
        // Arrange and Act
        let message = ModelError::IncompleteResponse {
            reason: "length".to_string(),
        }
        .to_string();

        // Assert
        assert_eq!(message, "model response is incomplete: length");
    }

    #[test]
    fn request_error_includes_source_message() {
        // Arrange
        let source = io::Error::other("connection refused");

        // Act
        let message = ModelError::request(source).to_string();

        // Assert
        assert_eq!(message, "model request failed: connection refused");
    }

    #[test]
    fn classifies_model_errors_with_stable_telemetry_values() {
        // Arrange
        let errors = [
            (
                ModelError::request(io::Error::other("request")),
                ModelErrorType::Request,
            ),
            (ModelError::InvalidResponse, ModelErrorType::InvalidResponse),
            (
                ModelError::IncompleteResponse {
                    reason: "length".to_string(),
                },
                ModelErrorType::InvalidResponse,
            ),
            (ModelError::ResumeUnavailable, ModelErrorType::Provider),
            (
                ModelError::ResponseBodyTooLarge,
                ModelErrorType::ResponseTooLarge,
            ),
            (
                ModelError::ResponseContentTooLarge,
                ModelErrorType::ResponseTooLarge,
            ),
            (
                ModelError::UnsupportedOutputSchema {
                    reason: "object required".to_string(),
                },
                ModelErrorType::UnsupportedOutput,
            ),
            (
                ModelError::InvalidJson {
                    reason: "invalid".to_string(),
                },
                ModelErrorType::InvalidOutput,
            ),
            (
                ModelError::SchemaViolation {
                    path: "$".to_string(),
                    reason: "invalid".to_string(),
                },
                ModelErrorType::InvalidOutput,
            ),
            (ModelError::MissingToolCall, ModelErrorType::InvalidToolCall),
            (
                ModelError::MultipleToolCalls,
                ModelErrorType::InvalidToolCall,
            ),
            (
                ModelError::TerminalResponseWithToolCalls,
                ModelErrorType::InvalidToolCall,
            ),
            (
                ModelError::UnsupportedToolType {
                    kind: "custom".to_string(),
                },
                ModelErrorType::InvalidToolCall,
            ),
            (
                ModelError::UnsupportedToolName {
                    name: "write".to_string(),
                },
                ModelErrorType::InvalidToolCall,
            ),
            (
                ModelError::InvalidToolArguments {
                    reason: "invalid".to_string(),
                },
                ModelErrorType::InvalidToolCall,
            ),
        ];

        // Act
        let classifications =
            errors.map(|(error, expected)| (error.error_type(), expected, error.http_status()));

        // Assert
        assert!(
            classifications
                .into_iter()
                .all(|(actual, expected, status)| actual == expected && status.is_none())
        );
        assert_eq!(ModelErrorType::Request.as_str(), "request_error");
        assert_eq!(ModelErrorType::Transport.as_str(), "transport_error");
        assert_eq!(ModelErrorType::Provider.as_str(), "provider_error");
        assert_eq!(
            ModelErrorType::InvalidProviderResponse.as_str(),
            "invalid_provider_response"
        );
        assert_eq!(ModelErrorType::InvalidResponse.as_str(), "invalid_response");
        assert_eq!(
            ModelErrorType::UnsupportedOutput.as_str(),
            "unsupported_output"
        );
        assert_eq!(
            ModelErrorType::ResponseTooLarge.as_str(),
            "response_too_large"
        );
        assert_eq!(ModelErrorType::InvalidOutput.as_str(), "invalid_output");
        assert_eq!(
            ModelErrorType::InvalidToolCall.as_str(),
            "invalid_tool_call"
        );
    }

    #[test]
    fn classifies_duplicate_tool_call_id_as_invalid_tool_call() {
        // Arrange
        let error = ModelError::DuplicateToolCallId {
            id: "duplicate_call".to_string(),
        };

        // Act
        let error_type = error.error_type();

        // Assert
        assert_eq!(error_type, ModelErrorType::InvalidToolCall);
        assert_eq!(error.http_status(), None);
    }

    #[test]
    fn classified_request_retains_source_type_and_status() {
        // Arrange
        let error = ModelError::classified_request(
            ModelErrorType::Transport,
            Some(503),
            io::Error::other("connection reset").into(),
        );

        // Act
        let source = std::error::Error::source(&error)
            .and_then(std::error::Error::source)
            .expect("classified request should retain its original source");

        // Assert
        assert_eq!(error.error_type(), ModelErrorType::Transport);
        assert_eq!(error.http_status(), Some(503));
        assert_eq!(source.to_string(), "connection reset");
    }

    #[test]
    fn unsupported_schema_error_includes_reason() {
        // Arrange and Act
        let message = ModelError::UnsupportedOutputSchema {
            reason: "top-level object required".to_string(),
        }
        .to_string();

        // Assert
        assert_eq!(
            message,
            "provider cannot satisfy this output schema: top-level object required"
        );
    }

    #[test]
    fn oversized_response_body_error_has_user_facing_message() {
        // Arrange and Act
        let message = ModelError::ResponseBodyTooLarge.to_string();

        // Assert
        assert_eq!(message, "model response body exceeds the size limit");
    }

    #[test]
    fn converts_invalid_json_error() {
        // Arrange
        let error = OutputValidationError::InvalidJson("expected value".to_string());

        // Act
        let error = ModelError::from(error);

        // Assert
        assert_eq!(
            error.to_string(),
            "model returned invalid JSON: expected value"
        );
    }

    #[test]
    fn converts_schema_violation_error() {
        // Arrange
        let error = OutputValidationError::SchemaViolation {
            path: "/name".to_string(),
            reason: "wrong type".to_string(),
        };

        // Act
        let error = ModelError::from(error);

        // Assert
        assert_eq!(
            error.to_string(),
            "model output violates the schema at /name: wrong type"
        );
    }

    #[test]
    fn converts_oversized_content_error() {
        // Arrange and Act
        let error = ModelError::from(OutputValidationError::TooLarge);

        // Assert
        assert_eq!(
            error.to_string(),
            "model response content exceeds the size limit"
        );
    }
}
