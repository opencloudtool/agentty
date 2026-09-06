//! Manually executed, real-model compatibility benchmark.

mod summary;

use std::collections::BTreeSet;
use std::io::Write as _;
use std::time::{Duration, Instant};
use std::{env, fmt};

use ag_harness::{
    Harness, KimiConfig, LifecycleMetrics, LifecycleObserverSet, LifecycleTraceObserver,
    MUSE_SPARK_1_3, ModelClient, ModelRequestActivity, ModelResponseType, MuseConfig, OutputSchema,
    QwenConfig, Tool, ToolActivity, TurnOutcome,
};
use opentelemetry::global;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{Value, json};

type DynError = Box<dyn std::error::Error + Send + Sync>;

const MODEL_API_BASE_URL: &str = "https://api.meta.ai/v1";
const KIMI_CASE_INTERVAL: Duration = Duration::from_secs(60);
const PERSISTENT_TURNS_PER_CASE: usize = 2;

type CaseFuture = std::pin::Pin<Box<dyn Future<Output = Result<CaseMeasurement, DynError>>>>;
type CaseRunner = fn(Provider) -> CaseFuture;

const CASES: [(&str, CaseRunner); 5] = [
    ("structured", structured),
    ("parallel_read", parallel_read),
    ("read_recovery", read_recovery),
    ("write", write),
    ("persistent_memory", persistent_memory),
];

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let repetitions = env::var("AG_HARNESS_BENCHMARK_REPETITIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let provider_filter = env::var("AG_HARNESS_BENCHMARK_PROVIDER").ok();
    let providers = Provider::ALL
        .into_iter()
        .filter(|provider| {
            provider_filter
                .as_deref()
                .is_none_or(|filter| provider.as_str() == filter)
        })
        .collect::<Vec<_>>();
    if providers.is_empty() {
        return Err("benchmark provider filter did not match kimi, muse, or qwen".into());
    }
    let case_filter = env::var("AG_HARNESS_BENCHMARK_CASE").ok();
    let cases = CASES
        .into_iter()
        .filter(|(case, _)| case_filter.as_deref().is_none_or(|filter| *case == filter))
        .collect::<Vec<_>>();
    if cases.is_empty() {
        return Err("benchmark case filter did not match a known case".into());
    }
    let telemetry = TelemetryCapture::install();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut last_kimi_case = None;
    let mut passed = 0;
    let mut total = 0;

    for repetition in 1..=repetitions {
        for provider in providers.iter().copied() {
            for &(case, run) in &cases {
                provider.wait_for_case_slot(last_kimi_case).await;
                let result = run_case(provider, repetition, case, run).await;
                if matches!(provider, Provider::Kimi) {
                    last_kimi_case = Some(Instant::now());
                }
                passed += usize::from(result.passed);
                total += 1;
                writeln!(stdout, "{result}")?;
                stdout.flush()?;
            }
        }
    }

    let telemetry = telemetry.finish()?;
    writeln!(stdout, "{telemetry}")?;
    writeln!(stdout, "SUMMARY passed={passed} total={total}")?;
    stdout.flush()?;
    let persistent_turns = repetitions
        * providers.len()
        * usize::from(cases.iter().any(|(case, _)| *case == "persistent_memory"))
        * PERSISTENT_TURNS_PER_CASE;
    telemetry.ensure_complete(persistent_turns)?;
    summary::ensure_all_passed(passed, total)?;

    Ok(())
}

async fn run_case(
    provider: Provider,
    repetition: usize,
    case: &'static str,
    run: fn(Provider) -> CaseFuture,
) -> ResultLine {
    let started_at = Instant::now();
    let result = run(provider).await;
    let (detail, measurement, passed) = match result {
        Ok(measurement) => (None, measurement, true),
        Err(error) => (Some(error.to_string()), CaseMeasurement::default(), false),
    };

    ResultLine {
        case,
        detail,
        duration: started_at.elapsed(),
        measurement,
        passed,
        provider,
        repetition,
    }
}

fn structured(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let schema = schema(json!({
            "type": "object",
            "properties": {
                "person": {
                    "type": "object",
                    "properties": {
                        "active": { "type": "boolean", "const": true },
                        "name": { "type": "string", "const": "Ada" },
                        "score": { "type": "integer", "const": 17 }
                    },
                    "required": ["active", "name", "score"],
                    "additionalProperties": false
                },
                "tags": {
                    "type": "array",
                    "prefixItems": [
                        { "type": "string", "const": "rust" },
                        { "type": "string", "const": "agent" }
                    ],
                    "items": false,
                    "minItems": 2,
                    "maxItems": 2
                }
            },
            "required": ["person", "tags"],
            "additionalProperties": false
        }))?;
        let outcome = Harness::new(provider.client()?)
            .run_once(
                "Extract this record exactly: Ada has score 17, is active, and has tags rust then \
                 agent.",
                schema,
            )
            .await?;
        if outcome.output()["person"]["name"] != "Ada" {
            return Err("structured output had the wrong name".into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn parallel_read(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let repository = tempfile::tempdir()?;
        std::fs::write(repository.path().join("alpha.txt"), "first=amber\n")?;
        std::fs::write(repository.path().join("beta.txt"), "second=17\n")?;
        let schema = string_value_schema("code")?;
        let outcome = Harness::new(provider.client()?)
            .repository(repository.path())
            .allow(Tool::Read)
            .run_once(
                "In one response, call read twice using exactly the paths alpha.txt and beta.txt \
                 without a ./ prefix. Combine their values as first-second.",
                schema,
            )
            .await?;
        let mut read_paths = match outcome.report().tool_calls() {
            [
                ToolActivity::Read {
                    path: first_path, ..
                },
                ToolActivity::Read {
                    path: second_path, ..
                },
            ] => [first_path.as_str(), second_path.as_str()],
            activities => {
                return Err(format!(
                    "expected exactly two successful reads, observed activities={activities:?}"
                )
                .into());
            }
        };
        read_paths.sort_unstable();
        let response_types = outcome
            .report()
            .model_requests()
            .iter()
            .map(ModelRequestActivity::response_type)
            .collect::<Vec<_>>();
        if read_paths != ["alpha.txt", "beta.txt"]
            || response_types != [ModelResponseType::ToolCall, ModelResponseType::Output]
            || outcome.output()["code"] != "amber-17"
        {
            return Err(format!(
                "expected one exact read batch and amber-17, observed paths={read_paths:?} \
                 responses={response_types:?}"
            )
            .into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn read_recovery(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let repository = tempfile::tempdir()?;
        std::fs::write(repository.path().join("fallback.txt"), "code=violet-29\n")?;
        let schema = string_value_schema("code")?;
        let outcome = Harness::new(provider.client()?)
            .repository(repository.path())
            .allow(Tool::Read)
            .run_once(
                "First read missing.txt. When that is rejected, recover by reading fallback.txt \
                 and return its code.",
                schema,
            )
            .await?;
        let rejected = outcome
            .report()
            .tool_calls()
            .iter()
            .any(|activity| matches!(activity, ToolActivity::ReadRejected { .. }));
        let recovered = outcome
            .report()
            .tool_calls()
            .iter()
            .any(|activity| matches!(activity, ToolActivity::Read { path, .. } if path == "fallback.txt"));
        if !rejected || !recovered || outcome.output()["code"] != "violet-29" {
            return Err("model did not follow the rejected-read recovery trajectory".into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn write(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let repository = tempfile::tempdir()?;
        let target = repository.path().join("status.txt");
        std::fs::write(&target, "status=pending\n")?;
        let schema = schema(json!({
            "type": "object",
            "properties": { "changed": { "type": "boolean", "const": true } },
            "required": ["changed"],
            "additionalProperties": false
        }))?;
        let outcome = Harness::new(provider.client()?)
            .repository(repository.path())
            .allow(Tool::Write)
            .run_once(
                "Use the write tool to change status.txt from status=pending to status=complete.",
                schema,
            )
            .await?;
        let wrote = outcome.report().tool_calls().iter().any(
            |activity| matches!(activity, ToolActivity::Write { path, .. } if path == "status.txt"),
        );
        if !wrote || std::fs::read_to_string(target)? != "status=complete\n" {
            return Err("model did not produce the verified file edit".into());
        }

        Ok(CaseMeasurement::from_outcomes([&outcome]))
    })
}

fn persistent_memory(provider: Provider) -> CaseFuture {
    Box::pin(async move {
        let schema = schema(json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string" }
            },
            "required": ["answer"],
            "additionalProperties": false
        }))?;
        let directory = tempfile::tempdir()?;
        let database_path = directory.path().join("sessions.db");
        let harness = instrumented_harness(provider)?.database(&database_path);
        let create_started = Instant::now();
        let mut session = harness
            .session("benchmark-session", schema)
            .create()
            .await?;
        let create_duration = create_started.elapsed();
        let stored = session
            .send("Remember the code cobalt-41 and answer only with stored.")
            .await?;
        drop(session);
        drop(harness);

        let harness = instrumented_harness(provider)?.database(&database_path);
        let reopen_started = Instant::now();
        let mut session = harness.resume("benchmark-session").await?;
        let reopen_duration = reopen_started.elapsed();
        let recalled = session
            .send("What exact code did I ask you to remember?")
            .await?;
        if stored.output()["answer"] != "stored"
            || recalled.output()["answer"] != "cobalt-41"
            || session.id() != "benchmark-session"
        {
            return Err("reopened session did not retain the exact code across turns".into());
        }

        Ok(CaseMeasurement::from_outcomes([&stored, &recalled])
            .with_storage_duration(create_duration + reopen_duration))
    })
}

fn instrumented_harness(provider: Provider) -> Result<Harness, DynError> {
    let observers = LifecycleObserverSet::new(LifecycleMetrics::new())
        .with_observer(LifecycleTraceObserver::new());

    Ok(Harness::new(provider.client()?).with_lifecycle_observer(observers))
}

fn string_value_schema(property: &str) -> Result<OutputSchema, DynError> {
    schema(json!({
        "type": "object",
        "properties": { property: { "type": "string" } },
        "required": [property],
        "additionalProperties": false
    }))
}

fn schema(value: Value) -> Result<OutputSchema, DynError> {
    OutputSchema::new(value).map_err(Into::into)
}

#[derive(Clone, Copy)]
enum Provider {
    Kimi,
    Muse,
    Qwen,
}

impl Provider {
    const ALL: [Self; 3] = [Self::Kimi, Self::Muse, Self::Qwen];

    fn client(self) -> Result<ModelClient, DynError> {
        match self {
            Self::Kimi => Ok(ModelClient::kimi(KimiConfig {
                api_key: env::var("KIMI_API_KEY")?,
                base_url: env::var("KIMI_BASE_URL")?,
                model: env::var("KIMI_MODEL")?,
            })?),
            Self::Muse => Ok(ModelClient::muse(MuseConfig {
                api_key: env::var("MODEL_API_KEY")?,
                base_url: env::var("MODEL_API_BASE_URL")
                    .unwrap_or_else(|_| MODEL_API_BASE_URL.to_string()),
                model: env::var("MODEL_API_MODEL").unwrap_or_else(|_| MUSE_SPARK_1_3.to_string()),
            })?),
            Self::Qwen => Ok(ModelClient::qwen(QwenConfig {
                api_key: env::var("DASHSCOPE_API_KEY")?,
                base_url: env::var("DASHSCOPE_BASE_URL")?,
                model: "qwen-plus".to_string(),
            })?),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Kimi => "kimi",
            Self::Muse => "muse",
            Self::Qwen => "qwen",
        }
    }

    async fn wait_for_case_slot(self, last_kimi_case: Option<Instant>) {
        if !matches!(self, Self::Kimi) {
            return;
        }
        let Some(last_kimi_case) = last_kimi_case else {
            return;
        };
        let elapsed = last_kimi_case.elapsed();
        if let Some(delay) = KIMI_CASE_INTERVAL.checked_sub(elapsed) {
            tokio::time::sleep(delay).await;
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

struct ResultLine {
    case: &'static str,
    detail: Option<String>,
    duration: Duration,
    measurement: CaseMeasurement,
    passed: bool,
    provider: Provider,
    repetition: usize,
}

impl fmt::Display for ResultLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "RESULT provider={} case={} repetition={} passed={} duration_ms={} model_requests={} \
             model_duration_ms={} turn_duration_ms={} storage_duration_ms={} tool_calls={} \
             total_tokens={}",
            self.provider,
            self.case,
            self.repetition,
            self.passed,
            self.duration.as_millis(),
            self.measurement.model_requests,
            self.measurement.model_duration.as_millis(),
            self.measurement.turn_duration.as_millis(),
            self.measurement.storage_duration.as_millis(),
            self.measurement.tool_calls,
            self.measurement
                .total_tokens
                .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string())
        )?;
        if let Some(detail) = &self.detail {
            write!(formatter, " detail={}", summary::sanitize_detail(detail))?;
        }

        Ok(())
    }
}

#[derive(Default)]
struct CaseMeasurement {
    model_duration: Duration,
    model_requests: usize,
    storage_duration: Duration,
    tool_calls: usize,
    total_tokens: Option<u64>,
    turn_duration: Duration,
}

impl CaseMeasurement {
    fn from_outcomes<'a>(outcomes: impl IntoIterator<Item = &'a TurnOutcome>) -> Self {
        let mut measurement = Self::default();
        let mut reported_tokens = false;
        let mut total_tokens = 0_u64;

        for outcome in outcomes {
            measurement.turn_duration += outcome.report().duration();
            measurement.model_requests += outcome.report().model_requests().len();
            measurement.tool_calls += outcome.report().tool_calls().len();
            for request in outcome.report().model_requests() {
                measurement.model_duration += request.duration();
                if let Some(tokens) = request
                    .completion()
                    .and_then(|completion| completion.usage())
                    .and_then(|usage| usage.total_tokens())
                {
                    reported_tokens = true;
                    total_tokens = total_tokens.saturating_add(tokens);
                }
            }
        }
        measurement.total_tokens = reported_tokens.then_some(total_tokens);

        measurement
    }

    fn with_storage_duration(mut self, storage_duration: Duration) -> Self {
        self.storage_duration = storage_duration;

        self
    }
}

struct TelemetryCapture {
    metric_exporter: InMemoryMetricExporter,
    meter_provider: SdkMeterProvider,
    span_exporter: InMemorySpanExporter,
    tracer_provider: SdkTracerProvider,
}

impl TelemetryCapture {
    fn install() -> Self {
        let metric_exporter = InMemoryMetricExporter::default();
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter.clone())
            .build();
        global::set_meter_provider(meter_provider.clone());
        let span_exporter = InMemorySpanExporter::default();
        let tracer_provider = SdkTracerProvider::builder()
            .with_simple_exporter(span_exporter.clone())
            .build();
        global::set_tracer_provider(tracer_provider.clone());

        Self {
            metric_exporter,
            meter_provider,
            span_exporter,
            tracer_provider,
        }
    }

    fn finish(self) -> Result<TelemetryMeasurement, DynError> {
        self.meter_provider.force_flush()?;
        self.tracer_provider.force_flush()?;
        let metric_names = self
            .metric_exporter
            .get_finished_metrics()?
            .iter()
            .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .map(|metric| metric.name().to_string())
            .collect();
        let spans = self.span_exporter.get_finished_spans()?;
        self.tracer_provider.shutdown()?;
        self.meter_provider.shutdown()?;

        Ok(TelemetryMeasurement {
            metric_names,
            spans,
        })
    }
}

struct TelemetryMeasurement {
    metric_names: BTreeSet<String>,
    spans: Vec<SpanData>,
}

impl TelemetryMeasurement {
    fn ensure_complete(&self, expected_turns: usize) -> Result<(), DynError> {
        let mut allowed_metrics = BTreeSet::from([
            "gen_ai.client.operation.duration".to_string(),
            "gen_ai.client.token.usage".to_string(),
        ]);
        let mut required_metrics = BTreeSet::from(["gen_ai.client.operation.duration".to_string()]);
        if expected_turns > 0 {
            required_metrics.extend([
                "gen_ai.client.token.usage".to_string(),
                "gen_ai.invoke_agent.duration".to_string(),
                "gen_ai.invoke_agent.inference_calls".to_string(),
                "gen_ai.invoke_agent.tool_calls".to_string(),
            ]);
            allowed_metrics.clone_from(&required_metrics);
        }
        if !required_metrics.is_subset(&self.metric_names)
            || !self.metric_names.is_subset(&allowed_metrics)
        {
            return Err(format!(
                "telemetry metrics differ: required={required_metrics:?} \
                 allowed={allowed_metrics:?} actual={:?}",
                self.metric_names,
            )
            .into());
        }
        let agent_spans = self.agent_spans();
        let model_spans = self.model_spans();
        if agent_spans.len() != expected_turns || model_spans.len() != expected_turns {
            return Err(format!(
                "telemetry spans differ: expected={expected_turns} agent={} model={}",
                agent_spans.len(),
                model_spans.len()
            )
            .into());
        }
        for agent_span in &agent_spans {
            if agent_span.parent_span_id != SpanId::INVALID {
                return Err("invoke-agent telemetry span is not a root span".into());
            }
            let child_count = model_spans
                .iter()
                .filter(|model_span| Self::is_direct_model_child(model_span, agent_span))
                .count();
            if child_count != 1 {
                return Err(format!(
                    "invoke-agent telemetry span has {child_count} direct model children"
                )
                .into());
            }
        }
        for model_span in &model_spans {
            let parent_count = agent_spans
                .iter()
                .filter(|agent_span| Self::is_direct_model_child(model_span, agent_span))
                .count();
            if parent_count != 1 {
                return Err(format!(
                    "model telemetry span has {parent_count} direct invoke-agent parents"
                )
                .into());
            }
        }

        Ok(())
    }

    fn agent_spans(&self) -> Vec<&SpanData> {
        self.spans
            .iter()
            .filter(|span| span.name == "invoke_agent")
            .collect()
    }

    fn model_spans(&self) -> Vec<&SpanData> {
        self.spans
            .iter()
            .filter(|span| span.name.starts_with("chat "))
            .collect()
    }

    fn is_direct_model_child(model_span: &SpanData, agent_span: &SpanData) -> bool {
        model_span.span_context.trace_id() == agent_span.span_context.trace_id()
            && model_span.parent_span_id == agent_span.span_context.span_id()
    }
}

impl fmt::Display for TelemetryMeasurement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let agent_spans = self.agent_spans().len();
        let model_spans = self.model_spans().len();

        write!(
            formatter,
            "TELEMETRY metrics={} agent_spans={} model_spans={}",
            self.metric_names.len(),
            agent_spans,
            model_spans
        )
    }
}
