use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Array, Context, KeyValue, StringValue, Value, global};

use crate::lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, LifecycleOperationGuard,
};
use crate::model::{CompletionMetadata, ModelMetadata};
use crate::telemetry;

const TOOL_CALL_ID_ATTRIBUTE_LIMIT_BYTES: usize = 128;

/// Projects one ordered harness lifecycle stream to OpenTelemetry `GenAI`
/// spans.
///
/// Install an OpenTelemetry tracer provider before operations start, then
/// attach one observer to a [`crate::Harness`] or [`crate::ModelClient`].
/// Applications retain ownership of exporter configuration, flushing, and
/// shutdown.
pub struct LifecycleTraceObserver {
    state: Mutex<TraceState>,
}

impl LifecycleTraceObserver {
    /// Creates an empty lifecycle trace projection.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TraceState::default()),
        }
    }
}

impl Default for LifecycleTraceObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleObserver for LifecycleTraceObserver {
    fn observe(&self, event: LifecycleEvent) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(event.kind());
    }

    fn enter_operation(
        &self,
        operation_id: LifecycleId,
    ) -> Option<Box<dyn LifecycleOperationGuard>> {
        let context = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .operation_context(operation_id)?;

        Some(Box::new(TraceOperationGuard {
            _guard: context.attach(),
        }))
    }
}

struct TraceOperationGuard {
    _guard: opentelemetry::ContextGuard,
}

impl LifecycleOperationGuard for TraceOperationGuard {}

#[derive(Default)]
struct TraceState {
    model_spans: HashMap<LifecycleId, Context>,
    pending_tools: HashMap<LifecycleId, PendingTool>,
    tool_spans: HashMap<LifecycleId, Context>,
    turn_spans: HashMap<LifecycleId, Context>,
}

impl TraceState {
    fn operation_context(&self, operation_id: LifecycleId) -> Option<Context> {
        self.model_spans
            .get(&operation_id)
            .or_else(|| self.tool_spans.get(&operation_id))
            .cloned()
    }

    fn observe(&mut self, event: &LifecycleEventKind) {
        match event {
            LifecycleEventKind::TurnStarted { turn_id } => self.start_turn(*turn_id),
            LifecycleEventKind::TurnCompleted { turn_id, .. } => {
                finish_span(self.turn_spans.remove(turn_id), None, Vec::new());
            }
            LifecycleEventKind::TurnFailed {
                error_type,
                turn_id,
                ..
            } => {
                finish_span(
                    self.turn_spans.remove(turn_id),
                    Some(error_type.as_str().to_string()),
                    Vec::new(),
                );
            }
            LifecycleEventKind::ModelRequestStarted {
                model,
                model_call_id,
                turn_id,
                ..
            } => self.start_model(*model_call_id, model.as_ref(), *turn_id),
            LifecycleEventKind::ModelRequestCompleted {
                completion,
                model_call_id,
                ..
            } => finish_span(
                self.model_spans.remove(model_call_id),
                None,
                completion_attributes(completion.as_ref()),
            ),
            LifecycleEventKind::ModelRequestFailed {
                error_type,
                http_status,
                model_call_id,
                ..
            } => finish_span(
                self.model_spans.remove(model_call_id),
                Some(http_status.map_or_else(
                    || error_type.as_str().to_string(),
                    |status| status.to_string(),
                )),
                Vec::new(),
            ),
            LifecycleEventKind::ModelRequestCancelled { model_call_id, .. } => finish_span(
                self.model_spans.remove(model_call_id),
                Some(telemetry::ERROR_CANCELLED.to_string()),
                Vec::new(),
            ),
            LifecycleEventKind::ToolRequested {
                provider_call_id,
                tool_call_id,
                tool_name,
                turn_id,
            } => {
                self.pending_tools.insert(
                    *tool_call_id,
                    PendingTool {
                        name: tool_name.clone(),
                        provider_call_id: provider_call_id.clone(),
                        turn_id: *turn_id,
                    },
                );
            }
            LifecycleEventKind::ToolStarted {
                tool_call_id,
                turn_id,
            } => self.start_tool(*tool_call_id, *turn_id),
            LifecycleEventKind::ToolCompleted { tool_call_id, .. } => {
                finish_span(self.tool_spans.remove(tool_call_id), None, Vec::new());
            }
            LifecycleEventKind::ToolDenied { tool_call_id, .. } => {
                self.pending_tools.remove(tool_call_id);
            }
            LifecycleEventKind::ToolFailed {
                error_type,
                tool_call_id,
                ..
            } => {
                self.pending_tools.remove(tool_call_id);
                finish_span(
                    self.tool_spans.remove(tool_call_id),
                    Some(error_type.as_str().to_string()),
                    Vec::new(),
                );
            }
        }
    }

    fn start_turn(&mut self, turn_id: LifecycleId) {
        let context = start_span(
            telemetry::OPERATION_INVOKE_AGENT,
            SpanKind::Internal,
            vec![
                KeyValue::new(
                    telemetry::ATTRIBUTE_OPERATION_NAME,
                    telemetry::OPERATION_INVOKE_AGENT,
                ),
                KeyValue::new(telemetry::ATTRIBUTE_OUTPUT_TYPE, telemetry::OUTPUT_JSON),
            ],
            None,
        );
        self.turn_spans.insert(turn_id, context);
    }

    fn start_model(
        &mut self,
        model_call_id: LifecycleId,
        model: Option<&ModelMetadata>,
        turn_id: Option<LifecycleId>,
    ) {
        let Some(model) = model else {
            return;
        };
        let name = format!("{} {}", telemetry::OPERATION_CHAT, model.model());
        let attributes = vec![
            KeyValue::new(
                telemetry::ATTRIBUTE_OPERATION_NAME,
                telemetry::OPERATION_CHAT,
            ),
            KeyValue::new(telemetry::ATTRIBUTE_PROVIDER_NAME, model.provider()),
            KeyValue::new(
                telemetry::ATTRIBUTE_REQUEST_MODEL,
                model.model().to_string(),
            ),
            KeyValue::new(telemetry::ATTRIBUTE_OUTPUT_TYPE, telemetry::OUTPUT_JSON),
        ];
        let context = match turn_id {
            Some(turn_id) => {
                let Some(parent) = self.turn_spans.get(&turn_id) else {
                    return;
                };
                start_span(name, SpanKind::Client, attributes, Some(parent))
            }
            None => start_span(name, SpanKind::Client, attributes, None),
        };
        self.model_spans.insert(model_call_id, context);
    }

    fn start_tool(&mut self, tool_call_id: LifecycleId, turn_id: LifecycleId) {
        let Some(tool) = self.pending_tools.remove(&tool_call_id) else {
            return;
        };
        debug_assert_eq!(tool.turn_id, turn_id);
        let name = format!("{} {}", telemetry::OPERATION_EXECUTE_TOOL, tool.name);
        let mut attributes = vec![
            KeyValue::new(
                telemetry::ATTRIBUTE_OPERATION_NAME,
                telemetry::OPERATION_EXECUTE_TOOL,
            ),
            KeyValue::new(telemetry::ATTRIBUTE_TOOL_NAME, tool.name),
            KeyValue::new(
                telemetry::ATTRIBUTE_TOOL_TYPE,
                telemetry::TOOL_TYPE_FUNCTION,
            ),
        ];
        if tool.provider_call_id.len() <= TOOL_CALL_ID_ATTRIBUTE_LIMIT_BYTES {
            attributes.push(KeyValue::new(
                telemetry::ATTRIBUTE_TOOL_CALL_ID,
                tool.provider_call_id,
            ));
        }
        let Some(parent) = self.turn_spans.get(&turn_id) else {
            return;
        };
        let context = start_span(name, SpanKind::Internal, attributes, Some(parent));
        self.tool_spans.insert(tool_call_id, context);
    }
}

struct PendingTool {
    name: String,
    provider_call_id: String,
    turn_id: LifecycleId,
}

fn start_span(
    name: impl Into<std::borrow::Cow<'static, str>>,
    kind: SpanKind,
    attributes: Vec<KeyValue>,
    parent: Option<&Context>,
) -> Context {
    let tracer = global::tracer(telemetry::INSTRUMENTATION_SCOPE);
    let builder = tracer
        .span_builder(name)
        .with_kind(kind)
        .with_attributes(attributes);
    let parent = parent.cloned().unwrap_or_else(Context::current);
    let span = builder.start_with_context(&tracer, &parent);

    parent.with_span(span)
}

fn finish_span(context: Option<Context>, error_type: Option<String>, attributes: Vec<KeyValue>) {
    let Some(context) = context else {
        return;
    };
    let span = context.span();
    span.set_attributes(attributes);
    if let Some(error_type) = error_type {
        span.set_attribute(KeyValue::new(telemetry::ATTRIBUTE_ERROR_TYPE, error_type));
        span.set_status(Status::error(""));
    }
    span.end();
}

fn completion_attributes(completion: Option<&CompletionMetadata>) -> Vec<KeyValue> {
    let Some(completion) = completion else {
        return Vec::new();
    };
    let mut attributes = vec![KeyValue::new(
        telemetry::ATTRIBUTE_RESPONSE_FINISH_REASONS,
        Value::Array(Array::String(vec![StringValue::from(
            completion.finish_reason().to_string(),
        )])),
    )];
    if let Some(response_id) = completion.response_id() {
        attributes.push(KeyValue::new(
            telemetry::ATTRIBUTE_RESPONSE_ID,
            response_id.to_string(),
        ));
    }
    if let Some(response_model) = completion.response_model() {
        attributes.push(KeyValue::new(
            telemetry::ATTRIBUTE_RESPONSE_MODEL,
            response_model.to_string(),
        ));
    }
    if let Some(usage) = completion.usage() {
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_CACHE_READ_INPUT_TOKENS,
            usage.cache_hit_tokens(),
        );
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_INPUT_TOKENS,
            usage.input_tokens(),
        );
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_OUTPUT_TOKENS,
            usage.output_tokens(),
        );
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_REASONING_OUTPUT_TOKENS,
            usage.reasoning_tokens(),
        );
    }

    attributes
}

fn push_token_attribute(attributes: &mut Vec<KeyValue>, key: &'static str, value: Option<u64>) {
    let Some(value) = value.and_then(|value| i64::try_from(value).ok()) else {
        return;
    };
    attributes.push(KeyValue::new(key, value));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use async_trait::async_trait;
    use opentelemetry::baggage::BaggageExt as _;
    use opentelemetry::global;
    use opentelemetry::trace::{FutureExt as _, Span as _, SpanId, SpanKind, Status};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use serde_json::json;
    use tokio::sync::Mutex as TestMutex;

    use super::*;
    use crate::file_system::MockFileSystem;
    use crate::harness::Harness;
    use crate::lifecycle::{LifecycleEmitter, ToolErrorType, TurnErrorType};
    use crate::model::{
        CompletionUsage, Model, ModelCompletion, ModelError, ModelErrorType, ModelRequest,
        ModelResponse,
    };
    use crate::schema_contract::OutputSchema;
    use crate::tool::{ReadArguments, Tool, ToolCall};

    static TRACE_PROVIDER_LOCK: TestMutex<()> = TestMutex::const_new(());

    struct NestedSpanModel {
        call_count: AtomicUsize,
        observed_baggage: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait]
    impl Model for NestedSpanModel {
        fn metadata(&self) -> Option<ModelMetadata> {
            Some(
                ModelMetadata::new("test_provider", "nested-model")
                    .expect("fixture metadata should be valid"),
            )
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
            record_baggage(&self.observed_baggage);
            nested_span("mock.model.child");
            if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                let arguments = serde_json::from_value::<ReadArguments>(json!({
                    "path": "Cargo.toml",
                    "limit": 1
                }))
                .expect("read arguments should be valid");

                return Ok(ModelCompletion::from_response(ModelResponse::ToolCall(
                    ToolCall::read("mock-call".to_string(), arguments, None),
                )));
            }

            Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({
                    "summary": "workspace"
                }),
            )))
        }
    }

    fn nested_span(name: &'static str) {
        let tracer = global::tracer("ag-harness-test");
        let mut span = tracer.start(name);
        span.end();
    }

    fn record_baggage(observed_baggage: &StdMutex<Vec<String>>) {
        let baggage_value = Context::current()
            .baggage()
            .get("workflow.id")
            .expect("application baggage should be propagated")
            .to_string();
        observed_baggage
            .lock()
            .expect("baggage recorder should not be poisoned")
            .push(baggage_value);
    }

    fn attributes(span: &SpanData) -> BTreeMap<&str, String> {
        span.attributes
            .iter()
            .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
            .collect()
    }

    fn find_span<'a>(spans: &'a [SpanData], name: &str, error_type: Option<&str>) -> &'a SpanData {
        spans
            .iter()
            .find(|span| {
                span.name == name
                    && attributes(span)
                        .get(telemetry::ATTRIBUTE_ERROR_TYPE)
                        .map(String::as_str)
                        == error_type
            })
            .expect("expected span should be exported")
    }

    fn full_completion() -> CompletionMetadata {
        CompletionMetadata::new(
            "stop".to_string(),
            Some("response-id".to_string()),
            Some("response-model".to_string()),
            Some("unexported-fingerprint".to_string()),
            Some(CompletionUsage::new(
                Some(2),
                Some(3),
                Some(5),
                Some(8),
                Some(1),
                Some(13),
            )),
        )
    }

    fn project_successes(emitter: &LifecycleEmitter, metadata: &ModelMetadata) {
        let successful_turn = emitter.start_turn().expect("observer should enable turns");
        let successful_turn_id = successful_turn.id();
        emitter
            .start_model_request(Some(metadata.clone()), 0, Some(successful_turn_id))
            .expect("observer should enable model calls")
            .completed(Some(full_completion()), crate::ModelResponseType::ToolCall);
        let mut successful_tool = emitter
            .request_tool(
                "successful-call".to_string(),
                "read".to_string(),
                Some(successful_turn_id),
            )
            .expect("turn should enable tools");
        successful_tool.started();
        successful_tool.completed();
        successful_turn.completed();

        let first_interleaved_turn = emitter.start_turn().expect("observer should enable turns");
        let first_interleaved_turn_id = first_interleaved_turn.id();
        let second_interleaved_turn = emitter.start_turn().expect("observer should enable turns");
        let second_interleaved_turn_id = second_interleaved_turn.id();
        emitter
            .start_model_request(
                Some(
                    ModelMetadata::new("test_provider", "interleaved-one")
                        .expect("fixture metadata should be valid"),
                ),
                0,
                Some(first_interleaved_turn_id),
            )
            .expect("observer should enable model calls")
            .completed(None, crate::ModelResponseType::ToolCall);
        emitter
            .start_model_request(
                Some(
                    ModelMetadata::new("test_provider", "interleaved-two")
                        .expect("fixture metadata should be valid"),
                ),
                0,
                Some(second_interleaved_turn_id),
            )
            .expect("observer should enable model calls")
            .completed(None, crate::ModelResponseType::ToolCall);
        let mut first_interleaved_tool = emitter
            .request_tool(
                "first-call".to_string(),
                "first".to_string(),
                Some(first_interleaved_turn_id),
            )
            .expect("turn should enable tools");
        first_interleaved_tool.started();
        let mut second_interleaved_tool = emitter
            .request_tool(
                "second-call".to_string(),
                "second".to_string(),
                Some(second_interleaved_turn_id),
            )
            .expect("turn should enable tools");
        second_interleaved_tool.started();
        second_interleaved_tool.completed();
        first_interleaved_tool.completed();
        second_interleaved_turn.completed();
        first_interleaved_turn.completed();
    }

    fn project_failures(emitter: &LifecycleEmitter, metadata: &ModelMetadata) {
        let failed_turn = emitter.start_turn().expect("observer should enable turns");
        let failed_turn_id = failed_turn.id();
        emitter
            .start_model_request(Some(metadata.clone()), 0, Some(failed_turn_id))
            .expect("observer should enable model calls")
            .failed(ModelErrorType::Transport, None);
        failed_turn.failed(TurnErrorType::Model(ModelErrorType::Transport));

        emitter
            .start_model_request(Some(metadata.clone()), 0, None)
            .expect("observer should enable standalone calls");
        emitter
            .start_model_request(Some(metadata.clone()), 0, None)
            .expect("observer should enable standalone calls")
            .failed(ModelErrorType::Provider, Some(429));

        let denied_turn = emitter.start_turn().expect("observer should enable turns");
        let denied_turn_id = denied_turn.id();
        emitter
            .request_tool(
                "denied-call".to_string(),
                "write".to_string(),
                Some(denied_turn_id),
            )
            .expect("turn should enable tools")
            .denied();
        denied_turn.failed(TurnErrorType::ToolDenied);

        let tool_failure_turn = emitter.start_turn().expect("observer should enable turns");
        let tool_failure_turn_id = tool_failure_turn.id();
        let mut failed_tool = emitter
            .request_tool(
                "failed-call".to_string(),
                "read".to_string(),
                Some(tool_failure_turn_id),
            )
            .expect("turn should enable tools");
        failed_tool.started();
        failed_tool.failed(ToolErrorType::Execution);
        tool_failure_turn.failed(TurnErrorType::Tool);

        let cancelled_turn = emitter.start_turn().expect("observer should enable turns");
        drop(cancelled_turn);

        emitter
            .start_model_request(Some(metadata.clone()), 0, None)
            .expect("observer should enable standalone calls")
            .completed(None, crate::ModelResponseType::Output);
        emitter
            .start_model_request(None, 0, None)
            .expect("observer should enable unknown model calls")
            .completed(None, crate::ModelResponseType::Output);

        let limited_turn = emitter.start_turn().expect("observer should enable turns");
        let limited_turn_id = limited_turn.id();
        emitter
            .request_tool(
                "limited-call".to_string(),
                "read".to_string(),
                Some(limited_turn_id),
            )
            .expect("turn should enable tools")
            .failed(ToolErrorType::CallLimit);
        limited_turn.failed(TurnErrorType::ToolCallLimit);

        let cancelled_tool_turn = emitter.start_turn().expect("observer should enable turns");
        let cancelled_tool_turn_id = cancelled_tool_turn.id();
        let mut cancelled_tool = emitter
            .request_tool(
                "cancelled-call".to_string(),
                "write".to_string(),
                Some(cancelled_tool_turn_id),
            )
            .expect("turn should enable tools");
        cancelled_tool.started();
        drop(cancelled_tool);
        cancelled_tool_turn.failed(TurnErrorType::Tool);
    }

    fn project_optional_metadata(emitter: &LifecycleEmitter, metadata: ModelMetadata) {
        emitter
            .start_model_request(Some(metadata), 0, None)
            .expect("observer should enable standalone calls")
            .completed(
                Some(CompletionMetadata::new(
                    "stop".to_string(),
                    None,
                    None,
                    None,
                    Some(CompletionUsage::new(
                        None,
                        None,
                        Some(u64::MAX),
                        None,
                        None,
                        None,
                    )),
                )),
                crate::ModelResponseType::Output,
            );
    }

    fn assert_success_spans(spans: &[SpanData]) {
        assert!(spans.iter().all(|span| {
            span.instrumentation_scope.name() == telemetry::INSTRUMENTATION_SCOPE
                && span.end_time >= span.start_time
                && !format!("{span:?}").contains("unexported-fingerprint")
        }));

        let successful_agent = find_span(spans, "invoke_agent", None);
        assert_eq!(successful_agent.span_kind, SpanKind::Internal);
        assert_eq!(successful_agent.status, Status::Unset);
        assert_eq!(
            attributes(successful_agent),
            BTreeMap::from([
                (
                    telemetry::ATTRIBUTE_OPERATION_NAME,
                    "invoke_agent".to_string()
                ),
                (telemetry::ATTRIBUTE_OUTPUT_TYPE, "json".to_string()),
            ])
        );
        let successful_model = find_span(spans, "chat test-model", None);
        assert_eq!(successful_model.span_kind, SpanKind::Client);
        assert_eq!(
            successful_model.parent_span_id,
            successful_agent.span_context.span_id()
        );
        let model_attributes = attributes(successful_model);
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_PROVIDER_NAME),
            Some(&"test_provider".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_RESPONSE_FINISH_REASONS),
            Some(&"[\"stop\"]".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_RESPONSE_ID),
            Some(&"response-id".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_RESPONSE_MODEL),
            Some(&"response-model".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_USAGE_CACHE_READ_INPUT_TOKENS),
            Some(&"2".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_USAGE_INPUT_TOKENS),
            Some(&"5".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_USAGE_OUTPUT_TOKENS),
            Some(&"8".to_string())
        );
        assert_eq!(
            model_attributes.get(telemetry::ATTRIBUTE_USAGE_REASONING_OUTPUT_TOKENS),
            Some(&"1".to_string())
        );
        let successful_tool = find_span(spans, "execute_tool read", None);
        assert_eq!(successful_tool.span_kind, SpanKind::Internal);
        assert_eq!(
            successful_tool.parent_span_id,
            successful_agent.span_context.span_id()
        );
        assert_eq!(
            attributes(successful_tool),
            BTreeMap::from([
                (
                    telemetry::ATTRIBUTE_OPERATION_NAME,
                    "execute_tool".to_string()
                ),
                (
                    telemetry::ATTRIBUTE_TOOL_CALL_ID,
                    "successful-call".to_string()
                ),
                (telemetry::ATTRIBUTE_TOOL_NAME, "read".to_string()),
                (telemetry::ATTRIBUTE_TOOL_TYPE, "function".to_string()),
            ])
        );
    }

    fn assert_interleaved_spans(spans: &[SpanData]) {
        let first_interleaved_model = find_span(spans, "chat interleaved-one", None);
        let second_interleaved_model = find_span(spans, "chat interleaved-two", None);
        let first_interleaved_tool = find_span(spans, "execute_tool first", None);
        let second_interleaved_tool = find_span(spans, "execute_tool second", None);
        assert_eq!(
            first_interleaved_model.parent_span_id,
            first_interleaved_tool.parent_span_id
        );
        assert_eq!(
            second_interleaved_model.parent_span_id,
            second_interleaved_tool.parent_span_id
        );
        assert_ne!(
            first_interleaved_model.parent_span_id,
            second_interleaved_model.parent_span_id
        );
        assert!(
            spans
                .iter()
                .filter(|span| span.name == "invoke_agent")
                .any(|span| span.span_context.span_id() == first_interleaved_model.parent_span_id)
        );
        assert!(
            spans
                .iter()
                .filter(|span| span.name == "invoke_agent")
                .any(|span| span.span_context.span_id() == second_interleaved_model.parent_span_id)
        );
    }

    fn assert_failure_spans(spans: &[SpanData]) {
        let failed_model = find_span(spans, "chat test-model", Some("transport_error"));
        assert_eq!(failed_model.status, Status::error(""));
        find_span(spans, "chat test-model", Some("429"));
        let standalone_cancelled =
            find_span(spans, "chat test-model", Some(telemetry::ERROR_CANCELLED));
        assert_eq!(standalone_cancelled.parent_span_id, SpanId::INVALID);
        let denied_agent = find_span(spans, "invoke_agent", Some("tool_denied"));
        assert_eq!(denied_agent.status, Status::error(""));
        assert!(!spans.iter().any(|span| {
            span.name == "execute_tool write"
                && !attributes(span).contains_key(telemetry::ATTRIBUTE_ERROR_TYPE)
        }));
        let failed_tool = find_span(spans, "execute_tool read", Some("tool_execution_error"));
        assert_eq!(failed_tool.status, Status::error(""));
        find_span(spans, "invoke_agent", Some("cancelled"));
        find_span(spans, "invoke_agent", Some("tool_call_limit"));
        find_span(spans, "execute_tool write", Some("cancelled"));
    }

    fn assert_optional_metadata(spans: &[SpanData]) {
        let oversized_usage = spans
            .iter()
            .find(|span| {
                span.name == "chat test-model"
                    && attributes(span).contains_key(telemetry::ATTRIBUTE_RESPONSE_FINISH_REASONS)
                    && !attributes(span).contains_key(telemetry::ATTRIBUTE_RESPONSE_ID)
            })
            .expect("completion without optional identity should be exported");
        assert!(!attributes(oversized_usage).contains_key(telemetry::ATTRIBUTE_USAGE_INPUT_TOKENS));
    }

    #[tokio::test]
    async fn projects_correlated_semantic_spans_for_every_lifecycle_outcome() {
        // Arrange
        let _trace_provider_guard = TRACE_PROVIDER_LOCK.lock().await;
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        let emitter = LifecycleEmitter::new(LifecycleTraceObserver::default());
        let metadata = ModelMetadata::new("test_provider", "test-model")
            .expect("fixture metadata should be valid");

        // Act
        project_successes(&emitter, &metadata);
        project_failures(&emitter, &metadata);
        project_optional_metadata(&emitter, metadata);
        provider
            .force_flush()
            .expect("finished spans should flush to memory");
        let spans = exporter
            .get_finished_spans()
            .expect("finished spans should be readable");

        // Assert
        assert_eq!(spans.len(), 22);
        assert_success_spans(&spans);
        assert_interleaved_spans(&spans);
        assert_failure_spans(&spans);
        assert_optional_metadata(&spans);

        provider.shutdown().expect("test provider should shut down");
    }

    #[tokio::test]
    async fn propagates_model_and_tool_contexts_to_nested_spans() {
        // Arrange
        let _trace_provider_guard = TRACE_PROVIDER_LOCK.lock().await;
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        let schema = OutputSchema::new(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }))
        .expect("schema should be valid");
        let observed_baggage = Arc::new(StdMutex::new(Vec::new()));
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().returning(|path| {
            Ok(if path == Path::new("repo") {
                PathBuf::from("/repo")
            } else {
                path.to_path_buf()
            })
        });
        let tool_baggage = Arc::clone(&observed_baggage);
        file_system
            .expect_open_beneath()
            .once()
            .return_once(move |_, _| {
                record_baggage(&tool_baggage);
                nested_span("mock.tool.child");

                Ok(Box::new(Cursor::new(b"[workspace]\n".to_vec())))
            });
        file_system.expect_replace_beneath().never();
        let harness = Harness::new(NestedSpanModel {
            call_count: AtomicUsize::new(0),
            observed_baggage: Arc::clone(&observed_baggage),
        })
        .file_system(file_system)
        .repository(crate::Repository::fixture("repo"))
        .allow(Tool::Read)
        .with_lifecycle_observer(LifecycleTraceObserver::new());

        // Act
        let application_context =
            Context::current().with_baggage([KeyValue::new("workflow.id", "workflow-42")]);
        let output = harness
            .run_once("inspect the manifest", schema)
            .with_context(application_context)
            .await
            .expect("mock tool round trip should succeed");
        provider
            .force_flush()
            .expect("finished spans should flush to memory");
        let spans = exporter
            .get_finished_spans()
            .expect("finished spans should be readable");

        // Assert
        assert_eq!(output.output(), &json!({ "summary": "workspace" }));
        let model_span_ids = spans
            .iter()
            .filter(|span| span.name == "chat nested-model")
            .map(|span| span.span_context.span_id())
            .collect::<Vec<_>>();
        let nested_model_parents = spans
            .iter()
            .filter(|span| span.name == "mock.model.child")
            .map(|span| span.parent_span_id)
            .collect::<Vec<_>>();
        assert_eq!(model_span_ids.len(), 2);
        assert_eq!(nested_model_parents.len(), 2);
        assert!(
            nested_model_parents
                .iter()
                .all(|parent| model_span_ids.contains(parent))
        );
        assert!(
            model_span_ids
                .iter()
                .all(|span_id| nested_model_parents.contains(span_id))
        );
        let tool_span = find_span(&spans, "execute_tool read", None);
        let nested_tool = find_span(&spans, "mock.tool.child", None);
        assert_eq!(nested_tool.parent_span_id, tool_span.span_context.span_id());
        assert_eq!(
            attributes(tool_span).get(telemetry::ATTRIBUTE_TOOL_CALL_ID),
            Some(&"mock-call".to_string())
        );
        assert_eq!(
            *observed_baggage
                .lock()
                .expect("baggage recorder should not be poisoned"),
            vec!["workflow-42", "workflow-42", "workflow-42"]
        );

        provider.shutdown().expect("test provider should shut down");
    }

    #[tokio::test]
    async fn omits_oversized_tool_call_id_attribute() {
        // Arrange
        let _trace_provider_guard = TRACE_PROVIDER_LOCK.lock().await;
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        let emitter = LifecycleEmitter::new(LifecycleTraceObserver::new());
        let turn = emitter.start_turn().expect("observer should enable turns");
        let turn_id = turn.id();

        // Act
        let mut tool = emitter
            .request_tool(
                "x".repeat(TOOL_CALL_ID_ATTRIBUTE_LIMIT_BYTES + 1),
                "read".to_string(),
                Some(turn_id),
            )
            .expect("turn should enable tools");
        tool.started();
        tool.completed();
        turn.completed();
        provider
            .force_flush()
            .expect("finished spans should flush to memory");
        let spans = exporter
            .get_finished_spans()
            .expect("finished spans should be readable");

        // Assert
        let tool_span = find_span(&spans, "execute_tool read", None);
        assert!(!attributes(tool_span).contains_key(telemetry::ATTRIBUTE_TOOL_CALL_ID));

        provider.shutdown().expect("test provider should shut down");
    }

    #[test]
    fn lifecycle_error_types_are_bounded_and_documentable() {
        // Arrange
        let turn_errors = [
            TurnErrorType::Cancelled,
            TurnErrorType::Model(ModelErrorType::InvalidOutput),
            TurnErrorType::Tool,
            TurnErrorType::ToolDenied,
            TurnErrorType::ToolCallLimit,
            TurnErrorType::RepositoryRequired,
        ];
        let tool_errors = [
            ToolErrorType::Cancelled,
            ToolErrorType::CallLimit,
            ToolErrorType::Execution,
        ];

        // Act
        let turn_values = turn_errors.map(TurnErrorType::as_str);
        let tool_values = tool_errors.map(ToolErrorType::as_str);

        // Assert
        assert_eq!(
            turn_values,
            [
                "cancelled",
                "invalid_output",
                "tool_execution_error",
                "tool_denied",
                "tool_call_limit",
                "repository_required",
            ]
        );
        assert_eq!(
            tool_values,
            ["cancelled", "tool_call_limit", "tool_execution_error"]
        );
    }

    #[tokio::test]
    async fn trace_observer_recovers_after_its_state_mutex_is_poisoned() {
        // Arrange
        let _trace_provider_guard = TRACE_PROVIDER_LOCK.lock().await;
        let observer = LifecycleTraceObserver::new();
        let _ = std::panic::catch_unwind(|| {
            let _state = observer
                .state
                .lock()
                .expect("fresh observer state should lock");
            std::panic::resume_unwind(Box::new("poison trace observer state"));
        });
        let emitter = LifecycleEmitter::new(observer);

        // Act
        let turn = emitter.start_turn().expect("observer should recover");
        turn.completed();

        // Assert
        assert!(emitter.is_enabled());
    }

    #[test]
    fn missing_correlations_do_not_create_partial_spans() {
        // Arrange
        let mut state = TraceState::default();
        let emitter = LifecycleEmitter::new(|_event: LifecycleEvent| {});
        let missing_turn = emitter.start_turn().expect("observer should enable turns");
        let missing_id = missing_turn.id();
        missing_turn.completed();
        state.pending_tools.insert(
            missing_id,
            PendingTool {
                name: "read".to_string(),
                provider_call_id: "missing-call".to_string(),
                turn_id: missing_id,
            },
        );
        let metadata = ModelMetadata::new("test_provider", "missing-parent")
            .expect("fixture metadata should be valid");

        // Act
        state.start_tool(missing_id, missing_id);
        state.start_tool(missing_id, missing_id);
        state.start_model(missing_id, Some(&metadata), Some(missing_id));
        finish_span(
            None,
            Some(telemetry::ERROR_TOOL_EXECUTION.to_string()),
            Vec::new(),
        );
        let absent_tool = LifecycleEmitter::default().request_tool(
            "absent-call".to_string(),
            "read".to_string(),
            None,
        );

        // Assert
        assert!(state.tool_spans.is_empty());
        assert!(state.model_spans.is_empty());
        assert!(absent_tool.is_none());
    }
}
