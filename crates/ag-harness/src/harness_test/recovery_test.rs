use super::*;

#[tokio::test]
async fn session_exposes_applied_writes_after_failure_and_reopen() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call(
                    "write-call",
                    "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                ),
            )))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(ModelError::InvalidResponse));
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|request| {
            assert_eq!(request.messages().len(), 2);
            assert!(
                matches!(&request.messages()[0], ModelMessage::System(context)
            if context.contains("write-call") && context.contains("applied"))
            );
            assert!(request.provider_session_id().is_none());

            Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "recovered"}),
            )))
        });
    let directory = tempdir().expect("repository");
    let harness = Harness::new(model)
        .database(directory.path().join("harness.db"))
        .repository(Repository::fixture(directory.path()))
        .allow(Tool::Write);
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let error = session
        .send("write and fail")
        .await
        .expect_err("model failure");
    let before = session.writes().await.expect("partial execution");
    drop(session);
    let mut reopened = harness.resume("session-a").await.expect("reopen");
    let after = reopened.writes().await.expect("durable execution");
    let retry = reopened.send("retry").await.expect("retry");
    let history = reopened
        .database
        .load_session("session-a")
        .await
        .expect("history");

    // Assert
    assert!(matches!(
        error,
        SessionError::Turn(TurnError::Model(ModelError::InvalidResponse))
    ));
    assert_eq!(before, after);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].status, crate::WriteStatus::Applied);
    assert_eq!(
        tokio::fs::read(directory.path().join("src/lib.rs"))
            .await
            .expect("file"),
        b"new\n"
    );
    assert_eq!(retry.output(), &json!({"summary": "recovered"}));
    assert_eq!(
        history.turns,
        vec![vec![
            ModelMessage::User("retry".to_string()),
            ModelMessage::Assistant(r#"{"summary":"recovered"}"#.to_string()),
        ]]
    );
}

#[test]
fn write_diagnostics_bounds_context_and_keeps_latest_records_in_order() {
    // Arrange
    let record = WriteRecord {
        call_id: "write-call".to_string(),
        expected_hash: None,
        id: 0,
        path: "file.txt".to_string(),
        recovery: None,
        repository_root: PathBuf::from("/private/host-user/secret-project"),
        resulting_hash: "0".repeat(64),
        status: crate::WriteStatus::Applied,
        turn_position: 0,
    };
    let mut records = vec![record; 100];
    for (position, record) in records.iter_mut().enumerate() {
        record.turn_position = i64::try_from(position).expect("position");
        record.id = record.turn_position;
    }

    // Act
    let (context, acknowledged_writes) = write_diagnostics(&records);

    // Assert
    assert!(context.len() < MAX_WRITE_DIAGNOSTIC_BYTES + 512);
    assert!(!context.contains("repository_root"));
    assert!(!context.contains("host-user"));
    assert!(!context.contains("secret-project"));
    assert!(!acknowledged_writes.contains(&0));
    assert!(acknowledged_writes.contains(&99));
    let (_, encoded) = context.split_once(": [").expect("records");
    let encoded: Vec<Value> =
        serde_json::from_str(&format!("[{encoded}")).expect("diagnostic JSON");
    assert_eq!(acknowledged_writes.len(), encoded.len());
    assert!(!context.contains(r#""turn_position":0"#));
    let penultimate = context
        .find(r#""turn_position":98"#)
        .expect("penultimate write");
    let last = context.find(r#""turn_position":99"#).expect("latest write");
    assert!(penultimate < last);
}

#[tokio::test]
async fn session_does_not_replay_a_failed_turn() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(ModelError::InvalidResponse));
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|request| {
            assert_eq!(
                request.messages(),
                &[ModelMessage::User("retry".to_string())]
            );

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    let error = session
        .send("failed question")
        .await
        .expect_err("the first turn should fail");
    let recovered = session
        .send("retry")
        .await
        .expect("the next turn should start from clean history");

    // Assert
    assert!(matches!(
        error,
        SessionError::Turn(TurnError::Model(ModelError::InvalidResponse))
    ));
    assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
}

#[tokio::test]
async fn session_invalidates_provider_resume_state_after_a_failed_turn() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id().is_none())
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first"
            })))
            .with_provider_session_id("native-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("native-session"))
        .returning(|_| {
            Err(ModelError::SchemaViolation {
                path: "/summary".to_string(),
                reason: "required property is missing".to_string(),
            })
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| {
            request.provider_session_id().is_none()
                && request.messages()
                    == [
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                        ModelMessage::User("retry".to_string()),
                    ]
        })
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    session
        .send("first")
        .await
        .expect("first turn should succeed");

    // Act
    let error = session
        .send("failed")
        .await
        .expect_err("second turn should fail");
    drop(session);
    let mut resumed = harness
        .resume("session-a")
        .await
        .expect("session should reopen");
    let recovered = resumed
        .send("retry")
        .await
        .expect("replayed turn should succeed");

    // Assert
    assert!(matches!(
        &error,
        SessionError::Turn(TurnError::Model(ModelError::SchemaViolation { path, reason }))
            if path == "/summary" && reason == "required property is missing"
    ));
    assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
}

#[tokio::test]
async fn session_accounts_for_resume_fallback_and_persists_its_continuation() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(resume_fallback_model())
        .database(directory.path().join("harness.db"))
        .with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    session
        .send("first")
        .await
        .expect("first turn should succeed");
    drop(session);
    let mut session = harness
        .resume("session-a")
        .await
        .expect("session should resume");

    // Act
    let outcome = session
        .send("second")
        .await
        .expect("history replay should succeed");
    drop(session);
    let mut session = harness
        .resume("session-a")
        .await
        .expect("session should resume with replacement continuation");
    let third = session
        .send("third")
        .await
        .expect("replacement continuation should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({"summary": "second"}));
    assert_eq!(outcome.report().model_requests().len(), 2);
    assert_eq!(
        outcome.report().model_requests()[0].response_type(),
        crate::ModelResponseType::ResumeUnavailable
    );
    assert_eq!(
        outcome.report().model_requests()[1].response_type(),
        crate::ModelResponseType::Output
    );
    assert_eq!(third.output(), &json!({"summary": "third"}));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert!(matches!(
        events[5].kind(),
        crate::LifecycleEventKind::ModelRequestStarted {
            request_index: 0,
            ..
        }
    ));
    assert!(matches!(
        events[6].kind(),
        crate::LifecycleEventKind::ModelRequestFailed {
            error_type: crate::ModelErrorType::Provider,
            ..
        }
    ));
    assert!(matches!(
        events[7].kind(),
        crate::LifecycleEventKind::ModelRequestStarted {
            request_index: 1,
            ..
        }
    ));
    assert!(matches!(
        events[8].kind(),
        crate::LifecycleEventKind::ModelRequestCompleted {
            response_type: crate::ModelResponseType::Output,
            ..
        }
    ));
}

#[tokio::test]
async fn session_preserves_structured_failure_after_native_resume_fallback() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first"
            })))
            .with_provider_session_id("native-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("native-session"))
        .returning(|_| Err(ModelError::ResumeUnavailable));
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id().is_none())
        .returning(|_| Err(ModelError::ResponseBodyTooLarge));
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    session
        .send("first")
        .await
        .expect("first turn should succeed");

    // Act
    let error = session
        .send("second")
        .await
        .expect_err("history replay should fail");

    // Assert
    assert!(matches!(
        &error,
        SessionError::Turn(TurnError::Model(ModelError::ResponseBodyTooLarge))
    ));
}

fn resume_fallback_model() -> crate::model::MockModel {
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id().is_none())
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first"
            })))
            .with_provider_session_id("native-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("native-session"))
        .returning(|_| Err(ModelError::ResumeUnavailable));
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| {
            request.provider_session_id().is_none()
                && request.messages()
                    == [
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                        ModelMessage::User("second".to_string()),
                    ]
        })
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "second"
            })))
            .with_provider_session_id("replacement-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("replacement-session"))
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "third"
            }))))
        });

    model
}
