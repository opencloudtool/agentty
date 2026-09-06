//! Integration coverage for `ag-harness` OpenTelemetry projections.
#![cfg(test)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_harness::{
    Harness, LifecycleMetrics, LifecycleObserverSet, LifecycleTraceObserver, Model, ModelClient,
    ModelCompletion, ModelError, ModelMetadata, ModelRequest, OutputSchema, Tool, ToolDefinition,
};
use async_trait::async_trait;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_proto::tonic::common::v1::{
    AnyValue as ProtoAnyValue, KeyValue as ProtoKeyValue, any_value,
};
use opentelemetry_proto::tonic::metrics::v1::{
    AggregationTemporality, Histogram as ProtoHistogram, HistogramDataPoint as ProtoHistogramPoint,
    Metric as ProtoMetric, metric,
};
use opentelemetry_proto::tonic::trace::v1::span::SpanKind as ProtoSpanKind;
use opentelemetry_proto::tonic::trace::v1::status::StatusCode as ProtoStatusCode;
use opentelemetry_proto::tonic::trace::v1::{
    ResourceSpans as ProtoResourceSpans, Span as ProtoSpan,
};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::data::{
    AggregatedMetrics, HistogramDataPoint, Metric as SdkMetric, MetricData,
};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider, Temporality,
};
use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider};
use serde_json::json;
use support::otlp::{OtlpCollector, OtlpPayload, OtlpRequest};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const OTLP_CONTRACT_FIXTURE_ENV: &str = "AG_HARNESS_RUN_OTLP_CONTRACT_FIXTURE";
const OTLP_CONTRACT_TEST: &str = "exports_otlp_metric_contract_and_flushes_on_shutdown";
const OTLP_LIFECYCLE_FIXTURE_ENV: &str = "AG_HARNESS_RUN_OTLP_LIFECYCLE_FIXTURE";
const OTLP_LIFECYCLE_TEST: &str = "exports_otlp_lifecycle_contract_and_flushes_on_shutdown";
const OTLP_MODEL: &str = "otlp-lifecycle-model";
const OTLP_RESPONSE_MODEL: &str = "otlp-lifecycle-response-model";

static TELEMETRY_TEST_LOCK: Mutex<()> = Mutex::const_new(());

struct PendingResponse {
    started: Arc<Notify>,
}

impl Respond for PendingResponse {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.started.notify_one();

        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(10))
            .set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":"Ada"}"#}
                }]
            }))
    }
}

#[derive(Clone)]
struct SequenceResponder {
    next_response: Arc<AtomicUsize>,
    pending_response: usize,
    pending_started: Arc<Notify>,
    responses: Arc<[ResponseTemplate]>,
}

impl Respond for SequenceResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let response_index = self.next_response.fetch_add(1, Ordering::SeqCst);
        if response_index == self.pending_response {
            self.pending_started.notify_one();
        }

        self.responses
            .get(response_index)
            .expect("fixture response should exist")
            .clone()
    }
}

struct PolicyDenialModel {
    client: ModelClient,
}

#[async_trait]
impl Model for PolicyDenialModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Some(self.client.metadata().clone())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.client
            .complete(request.with_tool(ToolDefinition::write()))
            .await
    }
}

fn client(server: &MockServer, model: &str) -> ag_harness::ModelClient {
    ag_harness::ModelClient::qwen(ag_harness::QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: model.to_string(),
    })
    .expect("fixture configuration should be valid")
}

fn request(prompt: &str) -> ag_harness::ModelRequest {
    ag_harness::ModelRequest::new(
        prompt,
        ag_harness::OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        }))
        .expect("fixture schema should be valid"),
    )
}

async fn mount_success_response(server: &MockServer, response_model: Option<&str>, usage: bool) {
    let mut response = json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": r#"{"name":"sensitive-response-content-sentinel"}"#}
        }],
        "id": "sensitive-response-id",
        "system_fingerprint": "sensitive-system-fingerprint"
    });
    if let Some(response_model) = response_model {
        response["model"] = json!(response_model);
    }
    if usage {
        response["usage"] = json!({
            "completion_tokens": 7,
            "prompt_tokens": 23,
            "total_tokens": 30
        });
    }

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_invalid_output_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": r#"{"unexpected":true}"#}
            }],
            "model": "invalid-output-response",
            "usage": {
                "completion_tokens": 3,
                "prompt_tokens": 11,
                "total_tokens": 14
            }
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_incomplete_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": r#"{"name":"Ada"}"#}
            }],
            "usage": {
                "completion_tokens": 5,
                "prompt_tokens": 13,
                "total_tokens": 18
            }
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn metric_attributes<T>(point: &HistogramDataPoint<T>) -> Vec<(&str, String)> {
    let mut attributes = point
        .attributes()
        .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
        .collect::<Vec<_>>();
    attributes.sort_unstable();

    attributes
}

async fn wait_for_provider<ResultType: std::fmt::Debug>(
    started: &Notify,
    request_task: &mut tokio::task::JoinHandle<ResultType>,
) -> Result<(), String> {
    tokio::select! {
        () = started.notified() => Ok(()),
        result = request_task => {
            Err(format!("pending request completed before cancellation: {result:?}"))
        }
        () = tokio::time::sleep(Duration::from_secs(5)) => {
            Err("pending request did not reach the provider before the timeout".to_string())
        }
    }
}

fn assert_duration_metric(metrics: &[&SdkMetric]) {
    let duration = metrics
        .iter()
        .find(|metric| metric.name() == "gen_ai.client.operation.duration")
        .expect("duration metric should be exported");
    assert_eq!(duration.description(), "GenAI operation duration.");
    assert_eq!(duration.unit(), "s");
    assert!(matches!(
        duration.data(),
        AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 6);
            assert!(points.iter().all(|point| point.count() == 1));
            assert!(points.iter().all(|point| point.bounds().eq([
                0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24,
                20.48, 40.96, 81.92,
            ])));
            let mut attributes = points
                .iter()
                .map(|point| metric_attributes(point))
                .collect::<Vec<_>>();
            attributes.sort_unstable();
            assert_eq!(
                attributes,
                [
                    vec![
                        ("error.type", "503".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "failed".to_string()),
                    ],
                    vec![
                        ("error.type", "cancelled".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "cancelled".to_string()),
                    ],
                    vec![
                        ("error.type", "invalid_output".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "invalid-output".to_string()),
                    ],
                    vec![
                        ("error.type", "invalid_response".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "incomplete".to_string()),
                    ],
                    vec![
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "missing-usage".to_string()),
                    ],
                    vec![
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                        ("gen_ai.request.model", "qwen-plus".to_string()),
                    ],
                ]
            );

            true
        }
    ));
}

fn assert_token_usage_metric(metrics: &[&SdkMetric]) {
    let token_usage = metrics
        .iter()
        .find(|metric| metric.name() == "gen_ai.client.token.usage")
        .expect("token-usage metric should be exported");
    assert_eq!(
        token_usage.description(),
        "Number of input and output tokens used."
    );
    assert_eq!(token_usage.unit(), "{token}");
    assert!(matches!(
        token_usage.data(),
        AggregatedMetrics::U64(MetricData::Histogram(histogram)) if {
            let points = histogram.data_points().collect::<Vec<_>>();
            assert_eq!(points.len(), 6);
            assert!(points.iter().all(|point| point.count() == 1));
            assert!(points.iter().all(|point| point.bounds().eq([
                1.0, 4.0, 16.0, 64.0, 256.0, 1_024.0, 4_096.0, 16_384.0, 65_536.0,
                262_144.0, 1_048_576.0, 4_194_304.0, 16_777_216.0, 67_108_864.0,
            ])));
            let mut points = points
                .iter()
                .map(|point| (metric_attributes(point), point.sum()))
                .collect::<Vec<_>>();
            points.sort_unstable();
            assert_eq!(
                points,
                [
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "incomplete".to_string()),
                            ("gen_ai.token.type", "input".to_string()),
                        ],
                        13,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "incomplete".to_string()),
                            ("gen_ai.token.type", "output".to_string()),
                        ],
                        5,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "invalid-output".to_string()),
                            ("gen_ai.token.type", "input".to_string()),
                        ],
                        11,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "invalid-output".to_string()),
                            ("gen_ai.token.type", "output".to_string()),
                        ],
                        3,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "qwen-plus".to_string()),
                            ("gen_ai.token.type", "input".to_string()),
                        ],
                        23,
                    ),
                    (
                        vec![
                            ("gen_ai.operation.name", "chat".to_string()),
                            ("gen_ai.provider.name", "alibaba_cloud".to_string()),
                            ("gen_ai.request.model", "qwen-plus".to_string()),
                            ("gen_ai.token.type", "output".to_string()),
                        ],
                        7,
                    ),
                ]
            );

            true
        }
    ));
}

fn proto_string_attributes(attributes: &[ProtoKeyValue]) -> Vec<(&str, &str)> {
    let mut attributes = attributes
        .iter()
        .map(|attribute| {
            let value = attribute
                .value
                .as_ref()
                .and_then(|value| value.value.as_ref())
                .expect("OTLP attribute should have a value");
            let any_value::Value::StringValue(value) = value else {
                unreachable!("OTLP contract attributes should be strings");
            };

            (attribute.key.as_str(), value.as_str())
        })
        .collect::<Vec<_>>();
    attributes.sort_unstable();

    attributes
}

fn proto_histogram(metric: &ProtoMetric) -> &ProtoHistogram {
    let Some(metric::Data::Histogram(histogram)) = metric.data.as_ref() else {
        unreachable!("GenAI metric should export as a histogram");
    };

    histogram
}

fn assert_histogram_point(point: &ProtoHistogramPoint, boundaries: &[f64]) {
    assert_eq!(point.count, 1);
    assert_histogram_shape(point, boundaries);
}

fn assert_histogram_shape(point: &ProtoHistogramPoint, boundaries: &[f64]) {
    assert_eq!(point.explicit_bounds, boundaries);
    assert_eq!(point.bucket_counts.len(), boundaries.len() + 1);
    assert_eq!(point.bucket_counts.iter().sum::<u64>(), point.count);
    assert!(point.start_time_unix_nano > 0);
    assert!(point.time_unix_nano >= point.start_time_unix_nano);
}

fn assert_otlp_duration(metric: &ProtoMetric, expected_models: &[&str]) {
    assert_eq!(metric.name, "gen_ai.client.operation.duration");
    assert_eq!(metric.description, "GenAI operation duration.");
    assert_eq!(metric.unit, "s");
    assert_eq!(metric.metadata.len(), 0);
    let histogram = proto_histogram(metric);
    assert_eq!(
        histogram.aggregation_temporality,
        AggregationTemporality::Cumulative as i32
    );
    assert_eq!(histogram.data_points.len(), expected_models.len());
    let mut attributes = histogram
        .data_points
        .iter()
        .map(|point| proto_string_attributes(&point.attributes))
        .collect::<Vec<_>>();
    attributes.sort_unstable();
    let mut expected_attributes = expected_models
        .iter()
        .map(|model| {
            vec![
                ("gen_ai.operation.name", "chat"),
                ("gen_ai.provider.name", "alibaba_cloud"),
                ("gen_ai.request.model", *model),
            ]
        })
        .collect::<Vec<_>>();
    expected_attributes.sort_unstable();
    assert_eq!(
        attributes, expected_attributes,
        "duration points should have the expected model identities"
    );
    for point in &histogram.data_points {
        assert_histogram_point(
            point,
            &[
                0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96,
                81.92,
            ],
        );
        assert!(point.sum.is_some_and(|sum| sum >= 0.0));
    }
}

fn assert_otlp_token_usage(metric: &ProtoMetric, expected_models: &[&str]) {
    assert_eq!(metric.name, "gen_ai.client.token.usage");
    assert_eq!(
        metric.description,
        "Number of input and output tokens used."
    );
    assert_eq!(metric.unit, "{token}");
    assert_eq!(metric.metadata.len(), 0);
    let histogram = proto_histogram(metric);
    assert_eq!(
        histogram.aggregation_temporality,
        AggregationTemporality::Cumulative as i32
    );
    assert_eq!(histogram.data_points.len(), expected_models.len() * 2);
    let mut points = histogram
        .data_points
        .iter()
        .map(|point| (proto_string_attributes(&point.attributes), point))
        .collect::<Vec<_>>();
    points.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut expected_points = expected_models
        .iter()
        .flat_map(|model| {
            [
                (
                    vec![
                        ("gen_ai.operation.name", "chat"),
                        ("gen_ai.provider.name", "alibaba_cloud"),
                        ("gen_ai.request.model", *model),
                        ("gen_ai.token.type", "input"),
                    ],
                    Some(23.0),
                ),
                (
                    vec![
                        ("gen_ai.operation.name", "chat"),
                        ("gen_ai.provider.name", "alibaba_cloud"),
                        ("gen_ai.request.model", *model),
                        ("gen_ai.token.type", "output"),
                    ],
                    Some(7.0),
                ),
            ]
        })
        .collect::<Vec<_>>();
    expected_points.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        points
            .iter()
            .map(|(attributes, point)| (attributes.clone(), point.sum))
            .collect::<Vec<_>>(),
        expected_points
    );
    for (_, point) in points {
        assert_histogram_point(
            point,
            &[
                1.0,
                4.0,
                16.0,
                64.0,
                256.0,
                1_024.0,
                4_096.0,
                16_384.0,
                65_536.0,
                262_144.0,
                1_048_576.0,
                4_194_304.0,
                16_777_216.0,
                67_108_864.0,
            ],
        );
    }
}

fn assert_otlp_metric_request(request: &OtlpRequest, expected_models: &[&str]) {
    request.assert_protobuf();
    assert_eq!(request.resource_count, 1);
    let OtlpPayload::Metrics(payload) = &request.payload else {
        unreachable!("metric exporter should only send metric payloads");
    };
    assert_eq!(payload.resource_metrics.len(), 1);
    let resource_metrics = &payload.resource_metrics[0];
    assert_eq!(resource_metrics.schema_url, "");
    let resource = resource_metrics
        .resource
        .as_ref()
        .expect("OTLP resource should be present");
    assert_eq!(
        proto_string_attributes(&resource.attributes),
        [
            ("deployment.environment.name", "test"),
            ("service.name", "ag-harness-otlp-contract-test"),
        ]
    );
    assert_eq!(resource.dropped_attributes_count, 0);
    assert_eq!(resource.entity_refs.len(), 0);
    assert_eq!(resource_metrics.scope_metrics.len(), 1);
    let scope_metrics = &resource_metrics.scope_metrics[0];
    assert_eq!(scope_metrics.schema_url, "");
    let scope = scope_metrics
        .scope
        .as_ref()
        .expect("OTLP instrumentation scope should be present");
    assert_eq!(scope.name, "ag-harness");
    assert_eq!(scope.version, "");
    assert_eq!(scope.attributes.len(), 0);
    assert_eq!(scope.dropped_attributes_count, 0);
    assert_eq!(scope_metrics.metrics.len(), 2);
    let mut metrics = scope_metrics.metrics.iter().collect::<Vec<_>>();
    metrics.sort_unstable_by_key(|metric| metric.name.as_str());
    assert_otlp_duration(metrics[0], expected_models);
    assert_otlp_token_usage(metrics[1], expected_models);
}

fn assert_secrets_absent(request: &OtlpRequest) {
    for secret in [
        "test-key",
        "sensitive-prompt-sentinel",
        "sensitive-response-content-sentinel",
        "sensitive-response-id",
        "sensitive-system-fingerprint",
        "shutdown-sensitive-prompt",
    ] {
        assert!(
            !request
                .body
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "OTLP payload should not contain fixture secrets"
        );
    }
}

fn proto_attribute<'a>(attributes: &'a [ProtoKeyValue], key: &str) -> Option<&'a ProtoAnyValue> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref())
}

fn proto_string_attribute<'a>(attributes: &'a [ProtoKeyValue], key: &str) -> Option<&'a str> {
    let any_value::Value::StringValue(value) = proto_attribute(attributes, key)?.value.as_ref()?
    else {
        return None;
    };

    Some(value)
}

fn proto_integer_attribute(attributes: &[ProtoKeyValue], key: &str) -> Option<i64> {
    let any_value::Value::IntValue(value) = proto_attribute(attributes, key)?.value.as_ref()?
    else {
        return None;
    };

    Some(*value)
}

fn proto_string_array_attribute<'a>(
    attributes: &'a [ProtoKeyValue],
    key: &str,
) -> Option<Vec<&'a str>> {
    let any_value::Value::ArrayValue(values) = proto_attribute(attributes, key)?.value.as_ref()?
    else {
        return None;
    };

    values
        .values
        .iter()
        .map(|value| match value.value.as_ref()? {
            any_value::Value::StringValue(value) => Some(value.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_contract_resource(attributes: &[ProtoKeyValue]) {
    assert_eq!(
        proto_string_attributes(attributes),
        [
            ("deployment.environment.name", "test"),
            ("service.name", "ag-harness-otlp-lifecycle-test"),
        ]
    );
}

fn assert_metric_definition<'a>(
    metric: &'a ProtoMetric,
    name: &str,
    description: &str,
    unit: &str,
    boundaries: &[f64],
) -> &'a ProtoHistogram {
    assert_eq!(metric.name, name);
    assert_eq!(metric.description, description);
    assert_eq!(metric.unit, unit);
    assert_eq!(metric.metadata.len(), 0);
    let histogram = proto_histogram(metric);
    assert_eq!(
        histogram.aggregation_temporality,
        AggregationTemporality::Cumulative as i32
    );
    for point in &histogram.data_points {
        assert_histogram_shape(point, boundaries);
    }

    histogram
}

fn point_error_counts(histogram: &ProtoHistogram) -> BTreeMap<Option<&str>, u64> {
    histogram
        .data_points
        .iter()
        .map(|point| {
            (
                proto_string_attribute(&point.attributes, "error.type"),
                point.count,
            )
        })
        .collect()
}

fn assert_lifecycle_metric_request(
    request: &OtlpRequest,
    expected_model_calls: u32,
    expected_tokenized_calls: u32,
    expected_turns: u32,
) {
    request.assert_protobuf();
    assert_eq!(request.resource_count, 1);
    let OtlpPayload::Metrics(payload) = &request.payload else {
        unreachable!("metric exporter should only send metric payloads");
    };
    assert_eq!(payload.resource_metrics.len(), 1);
    let resource_metrics = &payload.resource_metrics[0];
    assert_eq!(resource_metrics.schema_url, "");
    let resource = resource_metrics
        .resource
        .as_ref()
        .expect("OTLP metric resource should be present");
    assert_contract_resource(&resource.attributes);
    assert_eq!(resource.dropped_attributes_count, 0);
    assert_eq!(resource.entity_refs.len(), 0);
    assert_eq!(resource_metrics.scope_metrics.len(), 1);
    let scope_metrics = &resource_metrics.scope_metrics[0];
    assert_eq!(scope_metrics.schema_url, "");
    let scope = scope_metrics
        .scope
        .as_ref()
        .expect("OTLP metric scope should be present");
    assert_eq!(scope.name, "ag-harness");
    assert_eq!(scope.version, "");
    assert_eq!(scope.attributes.len(), 0);
    assert_eq!(scope.dropped_attributes_count, 0);
    assert_eq!(scope_metrics.metrics.len(), 6);
    let metrics = scope_metrics
        .metrics
        .iter()
        .map(|metric| (metric.name.as_str(), metric))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        metrics.keys().copied().collect::<Vec<_>>(),
        [
            "gen_ai.client.operation.duration",
            "gen_ai.client.token.usage",
            "gen_ai.execute_tool.duration",
            "gen_ai.invoke_agent.duration",
            "gen_ai.invoke_agent.inference_calls",
            "gen_ai.invoke_agent.tool_calls",
        ]
    );

    assert_client_metric_contract(&metrics, expected_model_calls, expected_tokenized_calls);
    assert_tool_metric_contract(&metrics);
    assert_agent_metric_contract(&metrics, expected_model_calls, expected_turns);
}

fn assert_client_metric_contract(
    metrics: &BTreeMap<&str, &ProtoMetric>,
    expected_model_calls: u32,
    expected_tokenized_calls: u32,
) {
    let client_duration = assert_metric_definition(
        metrics["gen_ai.client.operation.duration"],
        "gen_ai.client.operation.duration",
        "GenAI operation duration.",
        "s",
        &[
            0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
        ],
    );
    assert_eq!(
        client_duration
            .data_points
            .iter()
            .map(|point| point.count)
            .sum::<u64>(),
        u64::from(expected_model_calls),
        "model-client duration should be recorded exactly once per request"
    );
    for point in &client_duration.data_points {
        assert_eq!(
            proto_string_attribute(&point.attributes, "gen_ai.operation.name"),
            Some("chat")
        );
        assert_eq!(
            proto_string_attribute(&point.attributes, "gen_ai.provider.name"),
            Some("alibaba_cloud")
        );
        assert_eq!(
            proto_string_attribute(&point.attributes, "gen_ai.request.model"),
            Some(OTLP_MODEL)
        );
    }

    let token_usage = assert_metric_definition(
        metrics["gen_ai.client.token.usage"],
        "gen_ai.client.token.usage",
        "Number of input and output tokens used.",
        "{token}",
        &[
            1.0,
            4.0,
            16.0,
            64.0,
            256.0,
            1_024.0,
            4_096.0,
            16_384.0,
            65_536.0,
            262_144.0,
            1_048_576.0,
            4_194_304.0,
            16_777_216.0,
            67_108_864.0,
        ],
    );
    assert_eq!(token_usage.data_points.len(), 2);
    assert!(
        token_usage
            .data_points
            .iter()
            .all(|point| point.count == u64::from(expected_tokenized_calls))
    );
    assert_eq!(
        token_usage
            .data_points
            .iter()
            .map(|point| {
                proto_string_attribute(&point.attributes, "gen_ai.token.type")
                    .expect("token point should identify its token type")
            })
            .collect::<std::collections::BTreeSet<_>>(),
        ["input", "output"].into_iter().collect()
    );
}

fn assert_tool_metric_contract(metrics: &BTreeMap<&str, &ProtoMetric>) {
    let tool_duration = assert_metric_definition(
        metrics["gen_ai.execute_tool.duration"],
        "gen_ai.execute_tool.duration",
        "The duration of a single tool execution.",
        "s",
        &[
            0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
        ],
    );
    assert_eq!(
        tool_duration
            .data_points
            .iter()
            .map(|point| point.count)
            .sum::<u64>(),
        2
    );
    assert_eq!(
        point_error_counts(tool_duration),
        [(None, 1), (Some("tool_execution_error"), 1)]
            .into_iter()
            .collect()
    );
    for point in &tool_duration.data_points {
        assert_eq!(
            proto_string_attribute(&point.attributes, "gen_ai.tool.name"),
            Some("read")
        );
        assert_eq!(
            proto_string_attribute(&point.attributes, "gen_ai.tool.type"),
            Some("function")
        );
    }
}

fn assert_agent_metric_contract(
    metrics: &BTreeMap<&str, &ProtoMetric>,
    expected_model_calls: u32,
    expected_turns: u32,
) {
    let agent_duration = assert_metric_definition(
        metrics["gen_ai.invoke_agent.duration"],
        "gen_ai.invoke_agent.duration",
        "The end-to-end duration of a single in-process agent invocation, from the moment the \
         invocation starts until the agent emits the last chunk of its final response or \
         terminates with an error.",
        "s",
        &[
            0.1, 0.2, 0.4, 0.8, 1.6, 3.2, 6.4, 12.8, 25.6, 51.2, 102.4, 204.8, 409.6,
        ],
    );
    assert_eq!(
        agent_duration
            .data_points
            .iter()
            .map(|point| point.count)
            .sum::<u64>(),
        u64::from(expected_turns)
    );

    let inference_calls = assert_metric_definition(
        metrics["gen_ai.invoke_agent.inference_calls"],
        "gen_ai.invoke_agent.inference_calls",
        "The number of inference (model) calls a GenAI agent makes during a single invocation.",
        "{inference_call}",
        &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0],
    );
    assert_eq!(inference_calls.data_points.len(), 1);
    assert_eq!(
        inference_calls.data_points[0].count,
        u64::from(expected_turns)
    );
    assert_eq!(
        inference_calls.data_points[0].sum,
        Some(f64::from(expected_model_calls))
    );
    let two_call_turns = expected_model_calls - expected_turns;
    assert_eq!(
        inference_calls.data_points[0].bucket_counts,
        [
            u64::from(expected_turns - two_call_turns),
            u64::from(two_call_turns),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]
    );

    let tool_calls = assert_metric_definition(
        metrics["gen_ai.invoke_agent.tool_calls"],
        "gen_ai.invoke_agent.tool_calls",
        "The number of tool calls a GenAI agent makes during a single invocation.",
        "{tool_call}",
        &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0],
    );
    assert_eq!(tool_calls.data_points.len(), 1);
    assert_eq!(tool_calls.data_points[0].count, u64::from(expected_turns));
    assert_eq!(tool_calls.data_points[0].sum, Some(3.0));
    assert_eq!(
        tool_calls.data_points[0].bucket_counts,
        [u64::from(expected_turns), 0, 0, 0, 0, 0, 0, 0, 0]
    );
}

fn span_error_counts<'a>(
    spans: impl Iterator<Item = &'a ProtoSpan>,
) -> BTreeMap<Option<&'a str>, usize> {
    let mut counts = BTreeMap::new();
    for span in spans {
        *counts
            .entry(proto_string_attribute(&span.attributes, "error.type"))
            .or_default() += 1;
    }

    counts
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ChildSpanSignature<'a> {
    error_type: Option<&'a str>,
    finish_reasons: Option<Vec<&'a str>>,
    input_tokens: Option<i64>,
    name: &'a str,
    output_tokens: Option<i64>,
    response_id: Option<&'a str>,
    response_model: Option<&'a str>,
    tool_call_id: Option<&'a str>,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TurnTraceSignature<'a> {
    children: Vec<ChildSpanSignature<'a>>,
    error_type: Option<&'a str>,
}

fn child_span_signature(span: &ProtoSpan) -> ChildSpanSignature<'_> {
    ChildSpanSignature {
        error_type: proto_string_attribute(&span.attributes, "error.type"),
        finish_reasons: proto_string_array_attribute(
            &span.attributes,
            "gen_ai.response.finish_reasons",
        ),
        input_tokens: proto_integer_attribute(&span.attributes, "gen_ai.usage.input_tokens"),
        name: &span.name,
        output_tokens: proto_integer_attribute(&span.attributes, "gen_ai.usage.output_tokens"),
        response_id: proto_string_attribute(&span.attributes, "gen_ai.response.id"),
        response_model: proto_string_attribute(&span.attributes, "gen_ai.response.model"),
        tool_call_id: proto_string_attribute(&span.attributes, "gen_ai.tool.call.id"),
    }
}

fn expected_model_child(
    error_type: Option<&'static str>,
    finish_reason: Option<&'static str>,
    response_id: Option<&'static str>,
) -> ChildSpanSignature<'static> {
    ChildSpanSignature {
        error_type,
        finish_reasons: finish_reason.map(|reason| vec![reason]),
        input_tokens: finish_reason.map(|_| 23),
        name: "chat otlp-lifecycle-model",
        output_tokens: finish_reason.map(|_| 7),
        response_id,
        response_model: finish_reason.map(|_| OTLP_RESPONSE_MODEL),
        tool_call_id: None,
    }
}

fn expected_tool_child(
    error_type: Option<&'static str>,
    tool_call_id: Option<&'static str>,
) -> ChildSpanSignature<'static> {
    ChildSpanSignature {
        error_type,
        finish_reasons: None,
        input_tokens: None,
        name: "execute_tool read",
        output_tokens: None,
        response_id: None,
        response_model: None,
        tool_call_id,
    }
}

fn turn_trace_signatures<'a>(spans: &[&'a ProtoSpan]) -> BTreeSet<TurnTraceSignature<'a>> {
    let mut traces = BTreeMap::<&[u8], Vec<&ProtoSpan>>::new();
    for span in spans.iter().copied() {
        traces.entry(&span.trace_id).or_default().push(span);
    }

    traces
        .into_values()
        .map(|trace| {
            let roots = trace
                .iter()
                .copied()
                .filter(|span| span.parent_span_id.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(roots.len(), 1);
            let root = roots[0];
            let mut children = trace
                .iter()
                .copied()
                .filter(|span| !span.parent_span_id.is_empty())
                .map(|span| {
                    assert_eq!(span.parent_span_id, root.span_id);

                    child_span_signature(span)
                })
                .collect::<Vec<_>>();
            children.sort_unstable();

            TurnTraceSignature {
                children,
                error_type: proto_string_attribute(&root.attributes, "error.type"),
            }
        })
        .collect()
}

fn expected_initial_trace_signatures() -> BTreeSet<TurnTraceSignature<'static>> {
    [
        TurnTraceSignature {
            children: vec![
                expected_model_child(None, Some("tool_calls"), Some("response-1")),
                expected_model_child(None, Some("stop"), Some("response-2")),
                expected_tool_child(None, Some("safe-read-call")),
            ],
            error_type: None,
        },
        TurnTraceSignature {
            children: vec![expected_model_child(Some("503"), None, None)],
            error_type: Some("provider_error"),
        },
        TurnTraceSignature {
            children: vec![expected_model_child(Some("invalid_output"), None, None)],
            error_type: Some("invalid_output"),
        },
        TurnTraceSignature {
            children: vec![expected_model_child(
                None,
                Some("tool_calls"),
                Some("response-5"),
            )],
            error_type: Some("tool_denied"),
        },
        TurnTraceSignature {
            children: vec![
                expected_model_child(None, Some("tool_calls"), Some("response-6")),
                expected_model_child(None, Some("stop"), Some("response-7")),
                expected_tool_child(Some("tool_execution_error"), None),
            ],
            error_type: None,
        },
        TurnTraceSignature {
            children: vec![expected_model_child(Some("cancelled"), None, None)],
            error_type: Some("cancelled"),
        },
    ]
    .into_iter()
    .map(|mut trace| {
        trace.children.sort_unstable();

        trace
    })
    .collect()
}

fn trace_spans(resource_spans: &ProtoResourceSpans) -> Vec<&ProtoSpan> {
    assert_eq!(resource_spans.schema_url, "");
    let resource = resource_spans
        .resource
        .as_ref()
        .expect("OTLP trace resource should be present");
    assert_contract_resource(&resource.attributes);
    assert_eq!(resource.dropped_attributes_count, 0);
    assert_eq!(resource.entity_refs.len(), 0);
    assert_eq!(resource_spans.scope_spans.len(), 1);
    let scope_spans = &resource_spans.scope_spans[0];
    assert_eq!(scope_spans.schema_url, "");
    let scope = scope_spans
        .scope
        .as_ref()
        .expect("OTLP trace scope should be present");
    assert_eq!(scope.name, "ag-harness");
    assert_eq!(scope.version, "");
    assert_eq!(scope.attributes.len(), 0);
    assert_eq!(scope.dropped_attributes_count, 0);

    scope_spans.spans.iter().collect()
}

fn assert_trace_shape(spans: &[&ProtoSpan], expected_turns: usize, expected_model_calls: usize) {
    assert_eq!(
        spans
            .iter()
            .filter(|span| span.name == "invoke_agent")
            .count(),
        expected_turns
    );
    assert_eq!(
        spans
            .iter()
            .filter(|span| span.name == format!("chat {OTLP_MODEL}"))
            .count(),
        expected_model_calls
    );
    for span in spans {
        assert_span_semantic_contract(span);
        assert_eq!(span.trace_id.len(), 16);
        assert_eq!(span.span_id.len(), 8);
        assert!(span.trace_id.iter().any(|byte| *byte != 0));
        assert!(span.span_id.iter().any(|byte| *byte != 0));
        assert!(span.start_time_unix_nano > 0);
        assert!(span.end_time_unix_nano >= span.start_time_unix_nano);
        assert_eq!(span.dropped_attributes_count, 0);
        assert_eq!(span.dropped_events_count, 0);
        assert_eq!(span.dropped_links_count, 0);
        assert_eq!(span.events.len(), 0);
        assert_eq!(span.links.len(), 0);

        let error_type = proto_string_attribute(&span.attributes, "error.type");
        let status_code = span.status.as_ref().map_or(0, |status| status.code);
        assert_eq!(
            status_code,
            if error_type.is_some() {
                ProtoStatusCode::Error as i32
            } else {
                ProtoStatusCode::Unset as i32
            }
        );
    }

    let turn_spans = spans
        .iter()
        .copied()
        .filter(|span| span.name == "invoke_agent")
        .collect::<Vec<_>>();
    assert!(turn_spans.iter().all(|span| {
        span.parent_span_id.is_empty() && span.kind == ProtoSpanKind::Internal as i32
    }));
    for child in spans
        .iter()
        .copied()
        .filter(|span| span.name != "invoke_agent")
    {
        assert!(turn_spans.iter().any(|parent| {
            parent.trace_id == child.trace_id && parent.span_id == child.parent_span_id
        }));
    }
}

fn assert_span_semantic_contract(span: &ProtoSpan) {
    let (expected_kind, expected_attributes): (_, &[(&str, &str)]) = match span.name.as_str() {
        "invoke_agent" => (
            ProtoSpanKind::Internal,
            &[
                ("gen_ai.operation.name", "invoke_agent"),
                ("gen_ai.output.type", "json"),
            ],
        ),
        name if name == format!("chat {OTLP_MODEL}") => (
            ProtoSpanKind::Client,
            &[
                ("gen_ai.operation.name", "chat"),
                ("gen_ai.output.type", "json"),
                ("gen_ai.provider.name", "alibaba_cloud"),
                ("gen_ai.request.model", OTLP_MODEL),
            ],
        ),
        "execute_tool read" => (
            ProtoSpanKind::Internal,
            &[
                ("gen_ai.operation.name", "execute_tool"),
                ("gen_ai.tool.name", "read"),
                ("gen_ai.tool.type", "function"),
            ],
        ),
        name => unreachable!("unexpected lifecycle span {name:?}"),
    };
    assert_eq!(span.kind, expected_kind as i32);
    for (key, value) in expected_attributes {
        assert_eq!(proto_string_attribute(&span.attributes, key), Some(*value));
    }
    assert_exact_span_attribute_keys(span);
}

fn assert_exact_span_attribute_keys(span: &ProtoSpan) {
    let mut expected_keys = match span.name.as_str() {
        "invoke_agent" => vec!["gen_ai.operation.name", "gen_ai.output.type"],
        name if name == format!("chat {OTLP_MODEL}") => vec![
            "gen_ai.operation.name",
            "gen_ai.output.type",
            "gen_ai.provider.name",
            "gen_ai.request.model",
        ],
        "execute_tool read" => vec![
            "gen_ai.operation.name",
            "gen_ai.tool.name",
            "gen_ai.tool.type",
        ],
        name => unreachable!("unexpected lifecycle span {name:?}"),
    };
    if proto_string_attribute(&span.attributes, "error.type").is_some() {
        expected_keys.push("error.type");
    }
    if span.name == format!("chat {OTLP_MODEL}")
        && proto_string_attribute(&span.attributes, "error.type").is_none()
    {
        expected_keys.extend([
            "gen_ai.response.finish_reasons",
            "gen_ai.response.id",
            "gen_ai.response.model",
            "gen_ai.usage.input_tokens",
            "gen_ai.usage.output_tokens",
        ]);
    }
    if proto_string_attribute(&span.attributes, "gen_ai.tool.call.id").is_some() {
        expected_keys.push("gen_ai.tool.call.id");
    }
    let actual_keys = span
        .attributes
        .iter()
        .map(|attribute| attribute.key.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_keys.len(), span.attributes.len());
    assert_eq!(actual_keys, expected_keys.into_iter().collect());
}

fn assert_lifecycle_trace_request(
    request: &OtlpRequest,
    expected_turns: usize,
    expected_model_calls: usize,
) -> Vec<&ProtoSpan> {
    request.assert_protobuf();
    assert_eq!(request.resource_count, 1);
    let OtlpPayload::Traces(payload) = &request.payload else {
        unreachable!("trace exporter should only send trace payloads");
    };
    assert_eq!(payload.resource_spans.len(), 1);
    let spans = trace_spans(&payload.resource_spans[0]);
    assert_trace_shape(&spans, expected_turns, expected_model_calls);

    spans
}

fn assert_lifecycle_secrets_absent(request: &OtlpRequest) {
    for secret in [
        "lifecycle-test-key",
        "sensitive-user-prompt",
        "sensitive-response-content",
        "sensitive-provider-error-body",
        "sensitive-invalid-output",
        "sensitive-patch-content",
        "sensitive-missing-path",
        "sensitive-file-content",
        "sensitive-system-fingerprint",
        "sensitive-cancelled-prompt",
        "sensitive-shutdown-prompt",
        "sensitive-post-shutdown-prompt",
    ] {
        assert!(
            !request
                .body
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "OTLP payload should not contain fixture secrets"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn records_client_metrics_after_late_provider_installation_for_all_outcomes() {
    // Arrange
    let _guard = TELEMETRY_TEST_LOCK.lock().await;
    let unconfigured_server = MockServer::start().await;
    mount_success_response(&unconfigured_server, None, true).await;

    // Act
    client(&unconfigured_server, "before-installation")
        .complete(request("request before telemetry installation"))
        .await
        .expect("request before telemetry installation should succeed");

    // Arrange
    let exporter = InMemoryMetricExporter::default();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());
    let success_server = MockServer::start().await;
    mount_success_response(&success_server, Some("qwen-plus-2026-08-16"), true).await;
    let missing_usage_server = MockServer::start().await;
    mount_success_response(&missing_usage_server, None, false).await;
    let invalid_output_server = MockServer::start().await;
    mount_invalid_output_response(&invalid_output_server).await;
    let incomplete_server = MockServer::start().await;
    mount_incomplete_response(&incomplete_server).await;
    let failure_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("offline"))
        .expect(1)
        .mount(&failure_server)
        .await;
    let pending_server = MockServer::start().await;
    let pending_started = Arc::new(Notify::new());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(PendingResponse {
            started: Arc::clone(&pending_started),
        })
        .expect(1)
        .mount(&pending_server)
        .await;

    // Act
    client(&success_server, "qwen-plus")
        .complete(request("successful request"))
        .await
        .expect("instrumented request should succeed");
    client(&missing_usage_server, "missing-usage")
        .complete(request("request without provider usage"))
        .await
        .expect("instrumented request without usage should succeed");
    let invalid_output = client(&invalid_output_server, "invalid-output")
        .complete(request("request with invalid output"))
        .await
        .expect_err("invalid provider output should be returned");
    let incomplete = client(&incomplete_server, "incomplete")
        .complete(request("incomplete request"))
        .await
        .expect_err("incomplete provider response should be returned");
    let failure = client(&failure_server, "failed")
        .complete(request("failing request"))
        .await
        .expect_err("instrumented provider failure should be returned");
    let pending_client = client(&pending_server, "cancelled");
    let mut pending_request =
        tokio::spawn(async move { pending_client.complete(request("cancelled request")).await });
    wait_for_provider(&pending_started, &mut pending_request)
        .await
        .expect("pending request should reach the provider");
    pending_request.abort();
    let cancellation = pending_request
        .await
        .expect_err("aborted request should be cancelled");
    meter_provider
        .force_flush()
        .expect("client metrics should flush");

    // Assert
    assert!(matches!(failure, ag_harness::ModelError::Request(_)));
    assert!(matches!(
        invalid_output,
        ag_harness::ModelError::SchemaViolation { .. }
    ));
    assert!(matches!(
        incomplete,
        ag_harness::ModelError::IncompleteResponse { .. }
    ));
    assert!(cancellation.is_cancelled());
    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("client metrics should be exported");
    let metrics = resource_metrics
        .iter()
        .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .collect::<Vec<_>>();
    assert_eq!(metrics.len(), 2);
    assert_duration_metric(&metrics);
    assert_token_usage_metric(&metrics);
}

async fn run_otlp_metric_contract_fixture() {
    // Arrange
    let _guard = TELEMETRY_TEST_LOCK.lock().await;
    let collector = OtlpCollector::start().await;
    let exporter = MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(collector.metrics_endpoint())
        .with_temporality(Temporality::Cumulative)
        .build()
        .expect("OTLP metric exporter should build");
    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_hours(1))
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(
            Resource::builder_empty()
                .with_service_name("ag-harness-otlp-contract-test")
                .with_attribute(KeyValue::new("deployment.environment.name", "test"))
                .build(),
        )
        .build();
    global::set_meter_provider(meter_provider.clone());
    let server = MockServer::start().await;
    mount_success_response(&server, Some("sensitive-response-model"), true).await;

    // Act
    client(&server, "otlp-contract-model")
        .complete(request("sensitive-prompt-sentinel"))
        .await
        .expect("instrumented request should succeed");
    meter_provider
        .force_flush()
        .expect("OTLP metric payload should force flush");

    // Assert
    let flushed_requests = collector
        .requests()
        .await
        .expect("flushed OTLP requests should decode");
    assert_eq!(flushed_requests.len(), 1);
    assert_otlp_metric_request(&flushed_requests[0], &["otlp-contract-model"]);
    assert_secrets_absent(&flushed_requests[0]);

    // Arrange
    let shutdown_server = MockServer::start().await;
    mount_success_response(&shutdown_server, None, true).await;

    // Act
    client(&shutdown_server, "shutdown-contract-model")
        .complete(request("shutdown-sensitive-prompt"))
        .await
        .expect("request before telemetry shutdown should succeed");
    meter_provider
        .shutdown()
        .expect("OTLP metric provider should shut down");

    // Assert
    let shutdown_requests = collector
        .requests()
        .await
        .expect("shutdown OTLP requests should decode");
    assert_eq!(shutdown_requests.len(), 2);
    assert_otlp_metric_request(&shutdown_requests[0], &["otlp-contract-model"]);
    assert_otlp_metric_request(
        &shutdown_requests[1],
        &["otlp-contract-model", "shutdown-contract-model"],
    );
    assert_secrets_absent(&shutdown_requests[0]);
    assert_secrets_absent(&shutdown_requests[1]);

    // Arrange
    let post_shutdown_server = MockServer::start().await;
    mount_success_response(&post_shutdown_server, None, true).await;

    // Act
    client(&post_shutdown_server, "post-shutdown-model")
        .complete(request("post-shutdown-sensitive-prompt"))
        .await
        .expect("request after telemetry shutdown should succeed");

    // Assert
    assert_eq!(
        collector
            .requests()
            .await
            .expect("post-shutdown OTLP requests should decode")
            .len(),
        2
    );
}

fn lifecycle_schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" }
        },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("lifecycle fixture schema should be valid")
}

fn lifecycle_response(
    id: &str,
    finish_reason: &str,
    message: &serde_json::Value,
) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": finish_reason,
            "message": message
        }],
        "id": id,
        "model": OTLP_RESPONSE_MODEL,
        "system_fingerprint": "sensitive-system-fingerprint",
        "usage": {
            "completion_tokens": 7,
            "prompt_tokens": 23,
            "total_tokens": 30
        }
    }))
}

fn lifecycle_responses() -> Vec<ResponseTemplate> {
    vec![
        lifecycle_response(
            "response-1",
            "tool_calls",
            &json!({
                "content": null,
                "tool_calls": [{
                    "id": "safe-read-call",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"fixture.txt"}"#
                    }
                }]
            }),
        ),
        lifecycle_response(
            "response-2",
            "stop",
            &json!({"content": r#"{"summary":"sensitive-response-content"}"#}),
        ),
        ResponseTemplate::new(503).set_body_string("sensitive-provider-error-body"),
        lifecycle_response(
            "response-4",
            "stop",
            &json!({"content": r#"{"unexpected":"sensitive-invalid-output"}"#}),
        ),
        lifecycle_response(
            "response-5",
            "tool_calls",
            &json!({
                "content": null,
                "tool_calls": [{
                    "id": "denied-write-call",
                    "type": "function",
                    "function": {
                        "name": "write",
                        "arguments": r#"{"path":"fixture.txt","patch":"--- a/fixture.txt\n+++ b/fixture.txt\n@@ -1 +1 @@\n-old\n+sensitive-patch-content\n"}"#
                    }
                }]
            }),
        ),
        lifecycle_response(
            "response-6",
            "tool_calls",
            &json!({
                "content": null,
                "tool_calls": [{
                    "id": "x".repeat(129),
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": r#"{"path":"sensitive-missing-path"}"#
                    }
                }]
            }),
        ),
        lifecycle_response(
            "response-7",
            "stop",
            &json!({"content": r#"{"summary":"recovered-response"}"#}),
        ),
        lifecycle_response(
            "response-8",
            "stop",
            &json!({"content": r#"{"summary":"cancelled-response"}"#}),
        )
        .set_delay(Duration::from_secs(10)),
        lifecycle_response(
            "response-9",
            "stop",
            &json!({"content": r#"{"summary":"shutdown-response"}"#}),
        ),
        lifecycle_response(
            "response-10",
            "stop",
            &json!({"content": r#"{"summary":"post-shutdown-response"}"#}),
        ),
    ]
}

fn lifecycle_resource() -> Resource {
    Resource::builder_empty()
        .with_service_name("ag-harness-otlp-lifecycle-test")
        .with_attribute(KeyValue::new("deployment.environment.name", "test"))
        .build()
}

fn lifecycle_tracer_provider(trace_exporter: SpanExporter) -> SdkTracerProvider {
    let trace_processor = BatchSpanProcessor::builder(trace_exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_scheduled_delay(Duration::from_hours(1))
                .build(),
        )
        .build();

    SdkTracerProvider::builder()
        .with_span_processor(trace_processor)
        .with_resource(lifecycle_resource())
        .build()
}

async fn mount_lifecycle_responses(server: &MockServer, pending_started: Arc<Notify>) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SequenceResponder {
            next_response: Arc::new(AtomicUsize::new(0)),
            pending_response: 7,
            pending_started,
            responses: lifecycle_responses().into(),
        })
        .expect(10)
        .mount(server)
        .await;
}

fn lifecycle_harness(server: &MockServer, repository: &std::path::Path) -> Harness {
    let client = ModelClient::qwen(ag_harness::QwenConfig {
        api_key: "lifecycle-test-key".to_string(),
        base_url: server.uri(),
        model: OTLP_MODEL.to_string(),
    })
    .expect("lifecycle fixture configuration should be valid");
    let observers = LifecycleObserverSet::new(LifecycleMetrics::new())
        .with_observer(LifecycleTraceObserver::new());

    Harness::new(PolicyDenialModel { client })
        .repository(repository)
        .allow(Tool::Read)
        .with_lifecycle_observer(observers)
}

async fn exercise_lifecycle_outcomes(harness: &Arc<Harness>, pending_started: &Notify) {
    let success = harness
        .run_once("sensitive-user-prompt", lifecycle_schema())
        .await
        .expect("tool round trip should succeed");
    assert_eq!(
        success.output(),
        &json!({"summary": "sensitive-response-content"})
    );

    let provider_failure = harness
        .run_once("provider failure", lifecycle_schema())
        .await
        .expect_err("provider failure should fail the turn");
    assert!(provider_failure.to_string().contains("503"));

    harness
        .run_once("invalid output", lifecycle_schema())
        .await
        .expect_err("invalid output should fail the turn");

    let denial = harness
        .run_once("denied tool", lifecycle_schema())
        .await
        .expect_err("denied tool should fail the turn");
    assert_eq!(denial.to_string(), "tool `write` is denied by policy");

    let recovered = harness
        .run_once("failed tool", lifecycle_schema())
        .await
        .expect("the model should recover from the rejected read path");
    assert_eq!(
        recovered.output(),
        &json!({"summary": "recovered-response"})
    );

    let harness = Arc::clone(harness);
    let mut cancelled_turn = tokio::spawn(async move {
        harness
            .run_once("sensitive-cancelled-prompt", lifecycle_schema())
            .await
    });
    wait_for_provider(pending_started, &mut cancelled_turn)
        .await
        .expect("pending lifecycle turn should reach the provider");
    cancelled_turn.abort();
    assert!(
        cancelled_turn
            .await
            .expect_err("aborted lifecycle turn should be cancelled")
            .is_cancelled()
    );
}

fn signal_requests(requests: &[OtlpRequest]) -> (Vec<&OtlpRequest>, Vec<&OtlpRequest>) {
    requests
        .iter()
        .partition(|request| matches!(request.payload, OtlpPayload::Metrics(_)))
}

fn assert_initial_lifecycle_metrics(request: &OtlpRequest) {
    assert_lifecycle_metric_request(request, 8, 6, 6);
    let OtlpPayload::Metrics(payload) = &request.payload else {
        unreachable!("initial lifecycle metrics should be present");
    };
    let metrics = payload.resource_metrics[0].scope_metrics[0]
        .metrics
        .iter()
        .map(|metric| (metric.name.as_str(), metric))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        point_error_counts(proto_histogram(metrics["gen_ai.client.operation.duration"])),
        [
            (None, 5),
            (Some("503"), 1),
            (Some("cancelled"), 1),
            (Some("invalid_output"), 1),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        point_error_counts(proto_histogram(metrics["gen_ai.invoke_agent.duration"])),
        [
            (None, 2),
            (Some("cancelled"), 1),
            (Some("invalid_output"), 1),
            (Some("tool_denied"), 1),
            (Some("provider_error"), 1),
        ]
        .into_iter()
        .collect()
    );
}

fn assert_initial_lifecycle_traces(request: &OtlpRequest) {
    let spans = assert_lifecycle_trace_request(request, 6, 8);
    assert_eq!(spans.len(), 16);
    assert_eq!(
        turn_trace_signatures(&spans),
        expected_initial_trace_signatures()
    );
    assert_eq!(
        span_error_counts(
            spans
                .iter()
                .copied()
                .filter(|span| span.name == "invoke_agent")
        ),
        [
            (None, 2),
            (Some("cancelled"), 1),
            (Some("invalid_output"), 1),
            (Some("tool_denied"), 1),
            (Some("provider_error"), 1),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        span_error_counts(
            spans
                .iter()
                .copied()
                .filter(|span| span.name == format!("chat {OTLP_MODEL}"))
        ),
        [
            (None, 5),
            (Some("503"), 1),
            (Some("cancelled"), 1),
            (Some("invalid_output"), 1),
        ]
        .into_iter()
        .collect()
    );
    assert_initial_tool_spans(&spans);
    assert_completed_model_span(&spans);
}

fn assert_initial_tool_spans(spans: &[&ProtoSpan]) {
    let tool_spans = spans
        .iter()
        .copied()
        .filter(|span| span.name == "execute_tool read")
        .collect::<Vec<_>>();
    assert_eq!(tool_spans.len(), 2);
    assert!(
        tool_spans
            .iter()
            .all(|span| span.kind == ProtoSpanKind::Internal as i32)
    );
    assert_eq!(
        span_error_counts(tool_spans.iter().copied()),
        [(None, 1), (Some("tool_execution_error"), 1)]
            .into_iter()
            .collect()
    );
    let successful_tool = tool_spans
        .iter()
        .find(|span| proto_string_attribute(&span.attributes, "error.type").is_none())
        .expect("successful tool span should be exported");
    assert_eq!(
        proto_string_attribute(&successful_tool.attributes, "gen_ai.tool.call.id"),
        Some("safe-read-call")
    );
    let failed_tool = tool_spans
        .iter()
        .find(|span| {
            proto_string_attribute(&span.attributes, "error.type") == Some("tool_execution_error")
        })
        .expect("failed tool span should be exported");
    assert_eq!(
        proto_string_attribute(&failed_tool.attributes, "gen_ai.tool.call.id"),
        None,
        "oversized provider call identifiers should be omitted"
    );
    assert!(!spans.iter().any(|span| span.name == "execute_tool write"));
}

fn assert_completed_model_span(spans: &[&ProtoSpan]) {
    let completed_model = spans
        .iter()
        .find(|span| {
            proto_string_attribute(&span.attributes, "gen_ai.response.id") == Some("response-2")
        })
        .expect("completed model span should retain response metadata");
    assert_eq!(completed_model.kind, ProtoSpanKind::Client as i32);
    assert_eq!(
        proto_string_array_attribute(
            &completed_model.attributes,
            "gen_ai.response.finish_reasons"
        ),
        Some(vec!["stop"])
    );
    assert_eq!(
        proto_integer_attribute(&completed_model.attributes, "gen_ai.usage.input_tokens"),
        Some(23)
    );
    assert_eq!(
        proto_integer_attribute(&completed_model.attributes, "gen_ai.usage.output_tokens"),
        Some(7)
    );
    for span in spans {
        assert!(!span.attributes.iter().any(|attribute| {
            attribute.key.contains("prompt")
                || attribute.key.contains("message")
                || attribute.key.contains("arguments")
                || attribute.key.contains("result")
                || attribute.key.contains("lifecycle")
        }));
    }
}

async fn run_otlp_lifecycle_contract_fixture() {
    // Arrange
    let _guard = TELEMETRY_TEST_LOCK.lock().await;
    let collector = OtlpCollector::start().await;
    let metric_exporter = MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(collector.metrics_endpoint())
        .with_temporality(Temporality::Cumulative)
        .build()
        .expect("OTLP lifecycle metric exporter should build");
    let metric_reader = PeriodicReader::builder(metric_exporter)
        .with_interval(Duration::from_hours(1))
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(metric_reader)
        .with_resource(lifecycle_resource())
        .build();
    global::set_meter_provider(meter_provider.clone());
    let trace_exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(collector.traces_endpoint())
        .build()
        .expect("OTLP lifecycle trace exporter should build");
    let tracer_provider = lifecycle_tracer_provider(trace_exporter);
    global::set_tracer_provider(tracer_provider.clone());
    let server = MockServer::start().await;
    let pending_started = Arc::new(Notify::new());
    mount_lifecycle_responses(&server, Arc::clone(&pending_started)).await;
    let repository = tempfile::tempdir().expect("lifecycle repository should be created");
    std::fs::write(
        repository.path().join("fixture.txt"),
        "sensitive-file-content",
    )
    .expect("lifecycle fixture file should be written");
    let harness = Arc::new(lifecycle_harness(&server, repository.path()));

    // Act
    exercise_lifecycle_outcomes(&harness, &pending_started).await;
    meter_provider
        .force_flush()
        .expect("lifecycle metrics should force flush");
    tracer_provider
        .force_flush()
        .expect("lifecycle traces should force flush");

    // Assert
    let flushed_requests = collector
        .requests()
        .await
        .expect("flushed lifecycle OTLP requests should decode");
    let (metric_requests, trace_requests) = signal_requests(&flushed_requests);
    assert_eq!(metric_requests.len(), 1);
    assert_eq!(trace_requests.len(), 1);
    assert_initial_lifecycle_metrics(metric_requests[0]);
    assert_initial_lifecycle_traces(trace_requests[0]);
    for request in &flushed_requests {
        assert_lifecycle_secrets_absent(request);
    }

    // Arrange and Act
    harness
        .run_once("sensitive-shutdown-prompt", lifecycle_schema())
        .await
        .expect("turn before telemetry shutdown should succeed");
    tracer_provider
        .shutdown()
        .expect("lifecycle trace provider should shut down");
    meter_provider
        .shutdown()
        .expect("lifecycle metric provider should shut down");

    // Assert
    let shutdown_requests = collector
        .requests()
        .await
        .expect("shutdown lifecycle OTLP requests should decode");
    let (metric_requests, trace_requests) = signal_requests(&shutdown_requests);
    assert_eq!(metric_requests.len(), 2);
    assert_eq!(trace_requests.len(), 2);
    assert_lifecycle_metric_request(metric_requests[1], 9, 7, 7);
    let shutdown_spans = assert_lifecycle_trace_request(trace_requests[1], 1, 1);
    assert_eq!(shutdown_spans.len(), 2);
    for request in &shutdown_requests {
        assert_lifecycle_secrets_absent(request);
    }

    // Arrange and Act
    harness
        .run_once("sensitive-post-shutdown-prompt", lifecycle_schema())
        .await
        .expect("turn after telemetry shutdown should still succeed");

    // Assert
    assert_eq!(
        collector
            .requests()
            .await
            .expect("post-shutdown lifecycle OTLP requests should decode")
            .len(),
        4
    );
}

async fn spawn_otlp_metric_contract_fixture() -> Output {
    let executable = std::env::current_exe().expect("test executable should be available");

    Command::new(executable)
        .arg(OTLP_CONTRACT_TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(OTLP_CONTRACT_FIXTURE_ENV, "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT")
        .env_remove("OTEL_EXPORTER_OTLP_TIMEOUT")
        .env_remove("OTEL_METRIC_EXPORT_INTERVAL")
        .env("OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE", "delta")
        .output()
        .await
        .expect("OTLP contract fixture process should run")
}

async fn spawn_otlp_lifecycle_contract_fixture() -> Output {
    let executable = std::env::current_exe().expect("test executable should be available");

    Command::new(executable)
        .arg(OTLP_LIFECYCLE_TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(OTLP_LIFECYCLE_FIXTURE_ENV, "1")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_HEADERS")
        .env_remove("OTEL_EXPORTER_OTLP_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_COMPRESSION")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT")
        .env_remove("OTEL_EXPORTER_OTLP_TIMEOUT")
        .env_remove("OTEL_METRIC_EXPORT_INTERVAL")
        .env_remove("OTEL_BSP_EXPORT_TIMEOUT")
        .env_remove("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")
        .env_remove("OTEL_BSP_MAX_QUEUE_SIZE")
        .env_remove("OTEL_BSP_SCHEDULE_DELAY")
        .env("OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE", "delta")
        .output()
        .await
        .expect("OTLP lifecycle contract fixture process should run")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_otlp_metric_contract_and_flushes_on_shutdown() {
    if std::env::var_os(OTLP_CONTRACT_FIXTURE_ENV).is_some() {
        run_otlp_metric_contract_fixture().await;

        return;
    }

    // Arrange & Act
    let output = spawn_otlp_metric_contract_fixture().await;

    // Assert
    assert!(
        output.status.success(),
        "OTLP contract fixture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_otlp_lifecycle_contract_and_flushes_on_shutdown() {
    if std::env::var_os(OTLP_LIFECYCLE_FIXTURE_ENV).is_some() {
        run_otlp_lifecycle_contract_fixture().await;

        return;
    }

    // Arrange and Act
    let output = spawn_otlp_lifecycle_contract_fixture().await;

    // Assert
    assert!(
        output.status.success(),
        "OTLP lifecycle contract fixture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
