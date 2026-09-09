//! Integration coverage for metadata-only lifecycle observation.
#![cfg(test)]

use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::{
    LifecycleEvent, LifecycleEventKind, LifecycleObserver, ModelErrorType, ModelResponseType,
    TurnErrorType,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone, Default)]
struct EventRecorder {
    events: Arc<Mutex<Vec<LifecycleEvent>>>,
}

impl EventRecorder {
    fn events(&self) -> Vec<LifecycleEvent> {
        self.events
            .lock()
            .expect("event recorder should not be poisoned")
            .clone()
    }
}

impl LifecycleObserver for EventRecorder {
    fn observe(&self, event: LifecycleEvent) {
        self.events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    }
}

struct PendingResponse {
    started: Arc<Notify>,
}

impl Respond for PendingResponse {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.started.notify_one();

        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(10))
            .set_body_json(success_body())
    }
}

struct GatedModel {
    release: Arc<Notify>,
    started: Arc<Notify>,
}

#[async_trait]
impl ag_harness::Model for GatedModel {
    async fn complete(
        &self,
        _request: ag_harness::ModelRequest,
    ) -> Result<ag_harness::ModelCompletion, ag_harness::ModelError> {
        self.started.notify_one();
        self.release.notified().await;

        Ok(ag_harness::ModelCompletion::from_response(
            ag_harness::ModelResponse::Output(json!({"name": "done"})),
        ))
    }
}

fn success_body() -> serde_json::Value {
    json!({
        "id": "safe-response-id",
        "model": "returned-model",
        "system_fingerprint": "safe-fingerprint",
        "usage": {
            "completion_tokens": 2,
            "completion_tokens_details": {"reasoning_tokens": 1},
            "prompt_cache_hit_tokens": 1,
            "prompt_cache_miss_tokens": 2,
            "prompt_tokens": 3,
            "total_tokens": 5
        },
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": r#"{"name":"SECRET_OUTPUT"}"#}
        }]
    })
}

fn client(server: &MockServer, model: &str, recorder: EventRecorder) -> ag_harness::ModelClient {
    ag_harness::ModelClient::qwen(ag_harness::QwenConfig {
        api_key: "SECRET_API_KEY".to_string(),
        base_url: server.uri(),
        model: model.to_string(),
    })
    .expect("fixture configuration should be valid")
    .with_lifecycle_observer(recorder)
}

fn request() -> ag_harness::ModelRequest {
    ag_harness::ModelRequest::new(
        "SECRET_PROMPT",
        ag_harness::OutputSchema::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"],
            "additionalProperties": false
        }))
        .expect("fixture schema should be valid"),
    )
}

async fn wait_for_provider<Output: Debug>(
    started: &Notify,
    request_task: &mut tokio::task::JoinHandle<Output>,
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

fn assert_sequence(events: &[LifecycleEvent]) {
    assert_eq!(
        events
            .iter()
            .map(LifecycleEvent::sequence)
            .collect::<Vec<_>>(),
        (0..events.len() as u64).collect::<Vec<_>>()
    );
}

fn assert_durable_terminal(events: &[LifecycleEvent], cancel: bool, persistence_wait: Duration) {
    assert_sequence(events);
    assert_eq!(events.len(), 4);
    let turn_id = match events[0].kind() {
        LifecycleEventKind::TurnStarted { turn_id } => Some(turn_id),
        _ => None,
    }
    .expect("first event should start the turn");
    if cancel {
        assert!(matches!(
            events[3].kind(),
            LifecycleEventKind::TurnFailed {
                error_type: TurnErrorType::Cancelled,
                turn_id: failed_id,
                ..
            } if failed_id == turn_id
        ));
    } else {
        assert!(matches!(
            events[3].kind(),
            LifecycleEventKind::TurnCompleted {
                duration,
                turn_id: completed_id,
            } if completed_id == turn_id && *duration >= persistence_wait
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn observes_one_terminal_event_for_success_failure_and_cancellation() {
    // Arrange
    let success_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
        .expect(1)
        .mount(&success_server)
        .await;
    let failure_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("SECRET_FAILURE_BODY"))
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
    let success_events = EventRecorder::default();
    let failure_events = EventRecorder::default();
    let cancellation_events = EventRecorder::default();

    // Act
    client(&success_server, "successful", success_events.clone())
        .complete(request())
        .await
        .expect("successful request should complete");
    let failure = client(&failure_server, "failed", failure_events.clone())
        .complete(request())
        .await
        .expect_err("provider failure should be returned");
    let pending_client = client(&pending_server, "cancelled", cancellation_events.clone());
    let mut pending_request = tokio::spawn(async move { pending_client.complete(request()).await });
    wait_for_provider(&pending_started, &mut pending_request)
        .await
        .expect("pending request should reach the provider");
    pending_request.abort();
    let cancellation = pending_request
        .await
        .expect_err("aborted request should be cancelled");

    // Assert
    assert!(matches!(failure, ag_harness::ModelError::Request(_)));
    assert!(cancellation.is_cancelled());
    let success_events = success_events.events();
    let failure_events = failure_events.events();
    let cancellation_events = cancellation_events.events();
    for events in [&success_events, &failure_events, &cancellation_events] {
        assert_eq!(events.len(), 2);
        assert_sequence(events);
        assert!(matches!(
            events[0].kind(),
            LifecycleEventKind::ModelRequestStarted {
                request_index: 0,
                turn_id: None,
                ..
            }
        ));
    }
    assert!(matches!(
        success_events[1].kind(),
        LifecycleEventKind::ModelRequestCompleted {
            completion,
            response_type: ModelResponseType::Output,
            turn_id: None,
            ..
        } if completion.as_ref().is_some_and(|completion| {
            completion.finish_reason() == "stop"
                && completion.response_id() == Some("safe-response-id")
        })
    ));
    assert!(matches!(
        failure_events[1].kind(),
        LifecycleEventKind::ModelRequestFailed {
            error_type: ModelErrorType::Provider,
            http_status: Some(503),
            turn_id: None,
            ..
        }
    ));
    assert!(matches!(
        cancellation_events[1].kind(),
        LifecycleEventKind::ModelRequestCancelled { turn_id: None, .. }
    ));
    let event_debug = format!("{success_events:?}{failure_events:?}{cancellation_events:?}");
    for secret in [
        "SECRET_API_KEY",
        "SECRET_PROMPT",
        "SECRET_OUTPUT",
        "SECRET_FAILURE_BODY",
    ] {
        assert!(!event_debug.contains(secret));
    }
}

#[tokio::test]
async fn harness_owns_model_events_without_duplicate_model_observation() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
        .expect(1)
        .mount(&server)
        .await;
    let model_events = EventRecorder::default();
    let harness_events = EventRecorder::default();
    let harness = ag_harness::Harness::new(client(&server, "harness-model", model_events.clone()))
        .with_lifecycle_observer(harness_events.clone());

    // Act
    let output = harness
        .run_once("SECRET_PROMPT", request().schema().clone())
        .await
        .expect("harness request should complete");

    // Assert
    assert_eq!(output.output(), &json!({"name": "SECRET_OUTPUT"}));
    assert_eq!(model_events.events(), [] as [ag_harness::LifecycleEvent; 0]);
    let harness_events = harness_events.events();
    assert_sequence(&harness_events);
    assert_eq!(harness_events.len(), 4);
    assert!(matches!(
        harness_events[0].kind(),
        LifecycleEventKind::TurnStarted { .. }
    ));
    assert!(matches!(
        harness_events[1].kind(),
        LifecycleEventKind::ModelRequestStarted {
            model: Some(model),
            turn_id: Some(_),
            ..
        } if model.provider() == "alibaba_cloud" && model.model() == "harness-model"
    ));
    assert!(matches!(
        harness_events[2].kind(),
        LifecycleEventKind::ModelRequestCompleted {
            completion,
            response_type: ModelResponseType::Output,
            turn_id: Some(_),
            ..
        } if completion.as_ref().is_some_and(|completion| {
            completion.finish_reason() == "stop"
                && completion.response_id() == Some("safe-response-id")
                && completion.response_model() == Some("returned-model")
                && completion.system_fingerprint() == Some("safe-fingerprint")
                && completion.usage().is_some_and(|usage| {
                    usage.cache_hit_tokens() == Some(1)
                        && usage.cache_miss_tokens() == Some(2)
                        && usage.input_tokens() == Some(3)
                        && usage.output_tokens() == Some(2)
                        && usage.reasoning_tokens() == Some(1)
                        && usage.total_tokens() == Some(5)
                })
        })
    ));
    assert!(matches!(
        harness_events[3].kind(),
        LifecycleEventKind::TurnCompleted { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_harness_turn_closes_model_and_turn_lifecycles_once() {
    // Arrange
    let server = MockServer::start().await;
    let request_started = Arc::new(Notify::new());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(PendingResponse {
            started: Arc::clone(&request_started),
        })
        .expect(1)
        .mount(&server)
        .await;
    let model_events = EventRecorder::default();
    let harness_events = EventRecorder::default();
    let harness = ag_harness::Harness::new(client(&server, "cancelled-turn", model_events.clone()))
        .with_lifecycle_observer(harness_events.clone());

    // Act
    let mut turn = tokio::spawn(async move {
        harness
            .run_once("SECRET_PROMPT", request().schema().clone())
            .await
    });
    wait_for_provider(&request_started, &mut turn)
        .await
        .expect("pending request should reach the provider");
    turn.abort();
    let cancellation = turn.await.expect_err("aborted turn should be cancelled");

    // Assert
    assert!(cancellation.is_cancelled());
    assert_eq!(model_events.events(), [] as [ag_harness::LifecycleEvent; 0]);
    let harness_events = harness_events.events();
    assert_sequence(&harness_events);
    assert_eq!(harness_events.len(), 4);
    assert!(matches!(
        harness_events[0].kind(),
        LifecycleEventKind::TurnStarted { .. }
    ));
    assert!(matches!(
        harness_events[1].kind(),
        LifecycleEventKind::ModelRequestStarted { .. }
    ));
    assert!(matches!(
        harness_events[2].kind(),
        LifecycleEventKind::ModelRequestCancelled { .. }
    ));
    assert!(matches!(
        harness_events[3].kind(),
        LifecycleEventKind::TurnFailed {
            error_type: TurnErrorType::Cancelled,
            ..
        }
    ));
}

#[tokio::test]
async fn durable_lifecycle_and_duration_include_completion_persistence() {
    for cancel in [false, true] {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let model_completed = Arc::new(Notify::new());
        let events = EventRecorder::default();
        let observed_events = events.clone();
        let observed_completion = Arc::clone(&model_completed);
        let harness = Arc::new(
            ag_harness::Harness::new(GatedModel {
                release: Arc::clone(&release),
                started: Arc::clone(&started),
            })
            .database(&database_path)
            .with_lifecycle_observer(move |event: LifecycleEvent| {
                if matches!(
                    event.kind(),
                    LifecycleEventKind::ModelRequestCompleted { .. }
                ) {
                    observed_completion.notify_one();
                }
                observed_events.observe(event);
            }),
        );
        harness
            .session("durable", request().schema().clone())
            .create()
            .await
            .expect("session should be created");
        let database = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&database_path),
        )
        .await
        .expect("inspection database should open");
        let mut turn = tokio::spawn(async move {
            let mut session = harness
                .resume("durable")
                .await
                .expect("session should resume");
            session.send("complete").await
        });

        // Act
        wait_for_provider(&started, &mut turn)
            .await
            .expect("model should start");
        let writer = database
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("writer lock should be acquired");
        release.notify_one();
        wait_for_provider(&model_completed, &mut turn)
            .await
            .expect("model should complete");
        let persistence_started = std::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let persistence_wait = persistence_started.elapsed();

        // Assert
        assert_eq!(
            events.events().len(),
            3,
            "turn must stay active while its commit is blocked"
        );
        assert!(!turn.is_finished());
        if cancel {
            turn.abort();
            assert!(
                turn.await
                    .expect_err("turn should be cancelled")
                    .is_cancelled()
            );
            writer
                .rollback()
                .await
                .expect("writer lock should be released");
        } else {
            writer
                .commit()
                .await
                .expect("writer lock should be released");
            let outcome = turn
                .await
                .expect("turn task should finish")
                .expect("durable send should succeed");
            assert!(outcome.report().duration() >= persistence_wait);
            let status: String = sqlx::query_scalar("SELECT status FROM session_turn")
                .fetch_one(&database)
                .await
                .expect("turn state should load");
            assert_eq!(status, "completed");
        }
        assert_durable_terminal(&events.events(), cancel, persistence_wait);
        database.close().await;
    }
}
