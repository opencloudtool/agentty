use super::*;

#[tokio::test]
async fn rejects_schema_invalid_output_from_injected_model() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": 42
        }))))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(model).with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("schema-invalid custom output should fail");

    // Assert
    assert!(matches!(
        error,
        TurnError::Model(ModelError::SchemaViolation { path, .. }) if path == "/summary"
    ));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert_eq!(events.len(), 4);
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        crate::LifecycleEventKind::ModelRequestFailed {
            error_type: crate::ModelErrorType::InvalidOutput,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        crate::LifecycleEventKind::TurnFailed {
            error_type: TurnErrorType::Model(crate::ModelErrorType::InvalidOutput),
            ..
        }
    )));
}

#[tokio::test]
async fn report_describes_model_requests_and_repository_reads() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |_| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call_with_path("call_read", "Cargo\n.toml\u{1b}"),
            )));
        }

        Ok(response_with_metadata(ModelResponse::Output(json!({
            "summary": "workspace"
        }))))
    });
    let harness = read_harness(model, readable_file_system());

    // Act
    let outcome = harness
        .run_once("inspect", object_schema())
        .await
        .expect("reported turn should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({"summary": "workspace"}));
    assert_eq!(outcome.report().model_requests().len(), 2);
    let final_request = &outcome.report().model_requests()[1];
    assert_eq!(
        final_request.response_type(),
        crate::ModelResponseType::Output
    );
    let metadata = final_request
        .completion()
        .expect("metadata should be present");
    assert_eq!(metadata.finish_reason(), "stop\u{fffd}forged");
    assert_eq!(metadata.response_id(), Some("response\u{fffd}-1"));
    assert_eq!(metadata.response_model(), Some("reported\u{fffd}model"));
    assert_eq!(metadata.system_fingerprint(), Some("finger\u{fffd}print"));
    assert_eq!(
        metadata.usage().and_then(|usage| usage.total_tokens()),
        Some(16)
    );
    assert_eq!(outcome.report().tool_calls().len(), 1);
    let activity = &outcome.report().tool_calls()[0];
    assert_eq!(activity.name(), "read");
    assert_eq!(activity.path(), "Cargo\u{fffd}.toml\u{fffd}");
    assert!(activity.duration() <= outcome.report().duration());
}

#[tokio::test]
async fn emits_correlated_lifecycle_for_read_tool_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        assert!(request.lifecycle_observed());
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("provider-call-id"),
            )));
        }

        Ok(response_without_metadata(ModelResponse::Output(
            json!({ "summary": "workspace" }),
        )))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness =
        read_harness(model, readable_file_system()).with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });

    // Act
    let output = harness
        .run_once("sensitive prompt", object_schema())
        .await
        .expect("tool round trip should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "workspace" }));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert_eq!(events.len(), 9);
    assert_eq!(
        events
            .iter()
            .map(crate::LifecycleEvent::sequence)
            .collect::<Vec<_>>(),
        (0..9).collect::<Vec<_>>()
    );
    assert_read_tool_lifecycle(&events);
    let event_debug = format!("{events:?}");
    assert!(!event_debug.contains("sensitive prompt"));
    assert!(!event_debug.contains("[workspace]"));
}

#[tokio::test]
async fn returns_typed_read_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("call_read"),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
    let harness = read_harness(model, file_system).with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("filesystem failure should end the turn");

    // Assert
    assert!(matches!(&error, TurnError::Read(_)));
    assert_eq!(error.error_type(), TurnErrorType::Tool);
}

#[tokio::test]
async fn returns_typed_model_failure() {
    // Arrange
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .returning(|_| Err(ModelError::request(io::Error::other("offline"))));
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    let harness = Harness::new(model).with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("model failure should end the turn");

    // Assert
    assert!(matches!(&error, TurnError::Model(ModelError::Request(_))));
    assert_eq!(
        error.error_type(),
        TurnErrorType::Model(crate::ModelErrorType::Request)
    );
}

fn response_with_metadata(response: ModelResponse) -> crate::ModelCompletion {
    crate::ModelCompletion::new(
        crate::CompletionMetadata::new(
            "stop\nforged".to_string(),
            Some("response\u{1b}-1".to_string()),
            Some("reported\nmodel".to_string()),
            Some("finger\tprint".to_string()),
            Some(crate::CompletionUsage::new(
                None,
                None,
                Some(12),
                Some(4),
                None,
                Some(16),
            )),
        ),
        response,
    )
}

fn turn_started_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
    match event.kind() {
        crate::LifecycleEventKind::TurnStarted { turn_id } => Some(*turn_id),
        _ => None,
    }
}

fn model_started_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
    match event.kind() {
        crate::LifecycleEventKind::ModelRequestStarted { model_call_id, .. } => {
            Some(*model_call_id)
        }
        _ => None,
    }
}

fn tool_requested_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
    match event.kind() {
        crate::LifecycleEventKind::ToolRequested { tool_call_id, .. } => Some(*tool_call_id),
        _ => None,
    }
}

fn assert_read_tool_lifecycle(events: &[crate::LifecycleEvent]) {
    let turn_id = turn_started_id(&events[0]).expect("first event should start the turn");
    let first_model_call_id =
        model_started_id(&events[1]).expect("second event should start the model request");
    assert!(matches!(
        events[1].kind(),
        crate::LifecycleEventKind::ModelRequestStarted {
            model: None,
            request_index: 0,
            turn_id: Some(event_turn_id),
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[2].kind(),
        crate::LifecycleEventKind::ModelRequestCompleted {
            completion: None,
            model_call_id,
            response_type: crate::ModelResponseType::ToolCall,
            turn_id: Some(event_turn_id),
            ..
        } if *model_call_id == first_model_call_id && *event_turn_id == turn_id
    ));
    let tool_call_id = tool_requested_id(&events[3]).expect("fourth event should request the tool");
    assert!(matches!(
        events[3].kind(),
        crate::LifecycleEventKind::ToolRequested {
            provider_call_id,
            tool_name,
            turn_id: event_turn_id,
            ..
        } if provider_call_id == "provider-call-id"
            && tool_name == "read"
            && *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[4].kind(),
        crate::LifecycleEventKind::ToolStarted {
            tool_call_id: event_tool_call_id,
            turn_id: event_turn_id,
        } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[5].kind(),
        crate::LifecycleEventKind::ToolCompleted {
            tool_call_id: event_tool_call_id,
            turn_id: event_turn_id,
            ..
        } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[6].kind(),
        crate::LifecycleEventKind::ModelRequestStarted {
            request_index: 1,
            turn_id: Some(event_turn_id),
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[7].kind(),
        crate::LifecycleEventKind::ModelRequestCompleted {
            completion: None,
            response_type: crate::ModelResponseType::Output,
            turn_id: Some(event_turn_id),
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[8].kind(),
        crate::LifecycleEventKind::TurnCompleted {
            turn_id: event_turn_id,
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(turn_started_id(&events[1]).is_none());
    assert!(model_started_id(&events[0]).is_none());
    assert!(tool_requested_id(&events[0]).is_none());
}
