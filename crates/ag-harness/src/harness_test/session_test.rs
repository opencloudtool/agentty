use super::*;

#[tokio::test]
async fn session_retains_successful_conversation_history() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(
                request.messages(),
                &[ModelMessage::User("first question".to_string())]
            );

            return Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first answer"
            }))));
        }
        assert_eq!(
            request.messages(),
            &[
                ModelMessage::User("first question".to_string()),
                ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                ModelMessage::User("second question".to_string()),
            ]
        );

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "second answer"
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
    let first = session
        .send("first question")
        .await
        .expect("first chat turn should succeed");
    let second = session
        .send("second question")
        .await
        .expect("second chat turn should succeed");

    // Assert
    assert_eq!(first.output(), &json!({"summary": "first answer"}));
    assert_eq!(second.output(), &json!({"summary": "second answer"}));
    assert_eq!(second.report().model_requests().len(), 1);
    assert!(second.report().duration() >= second.report().model_requests()[0].duration());
}

#[tokio::test]
async fn stale_session_handles_acquire_current_canonical_state() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            assert_eq!(
                request.messages(),
                &[ModelMessage::User("first question".to_string())]
            );
            assert_eq!(request.provider_session_id(), None);

            return Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first answer"
            })))
            .with_provider_session_id("native-session"));
        }
        assert_eq!(
            request.messages(),
            &[
                ModelMessage::User("first question".to_string()),
                ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                ModelMessage::User("second question".to_string()),
            ]
        );
        assert_eq!(request.provider_session_id(), Some("native-session"));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "second answer"
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut first = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    let mut stale = harness
        .resume("session-a")
        .await
        .expect("second handle should resume before the first turn");

    // Act
    first
        .send("first question")
        .await
        .expect("first handle should complete its turn");
    let outcome = stale
        .send("second question")
        .await
        .expect("stale handle should refresh before its turn");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "second answer" }));
}

#[tokio::test]
async fn session_sends_the_system_prompt_on_every_turn() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ModelMessage::System("read-only instructions".to_string()),
                ModelMessage::User("first".to_string()),
            ]
        } else {
            vec![
                ModelMessage::System("read-only instructions".to_string()),
                ModelMessage::User("first".to_string()),
                ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                ModelMessage::User("second".to_string()),
            ]
        };
        assert_eq!(request.messages(), expected);

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": if expected.len() == 2 { "one" } else { "two" }
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .system_prompt("read-only instructions")
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("first")
        .await
        .expect("first chat turn should succeed");
    let second = session
        .send("second")
        .await
        .expect("second chat turn should succeed");

    // Assert
    assert_eq!(second.output(), &json!({"summary": "two"}));
}

#[test]
fn chat_history_evicts_complete_tool_turns() {
    // Arrange
    let tool_turn = vec![
        ModelMessage::User("inspect".to_string()),
        ModelMessage::AssistantToolCall(read_call("call_read")),
        ModelMessage::ToolResult {
            call_id: "call_read".to_string(),
            content: "file contents".to_string(),
            name: "read".to_string(),
        },
        ModelMessage::Assistant(r#"{"summary":"old"}"#.to_string()),
    ];
    let latest_turn = vec![
        ModelMessage::User("latest".to_string()),
        ModelMessage::Assistant(r#"{"summary":"new"}"#.to_string()),
    ];
    let max_bytes = retained_bytes(&tool_turn).max(retained_bytes(&latest_turn));
    let mut history = SessionHistory::new(max_bytes);

    // Act
    history.push(tool_turn);
    history.push(latest_turn.clone());

    // Assert
    assert_eq!(history.messages(), latest_turn);
    assert!(history.bytes <= max_bytes);
}

#[tokio::test]
async fn session_applies_the_configured_history_budget() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(3).returning(move |request| {
        match call_count.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert_eq!(
                    request.messages(),
                    &[ModelMessage::User("first".to_string())]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "xxxxxxxxxxxxxxxxxxxx"
                }))))
            }
            1 => {
                assert_eq!(request.messages().len(), 3);

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "two"
                }))))
            }
            _ => {
                assert_eq!(
                    request.messages(),
                    &[
                        ModelMessage::User("second".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"two"}"#.to_string()),
                        ModelMessage::User("third".to_string()),
                    ]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "three"
                }))))
            }
        }
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model)
        .database(directory.path().join("harness.db"))
        .max_history_bytes(NonZeroUsize::new(50).expect("history budget should be nonzero"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("first")
        .await
        .expect("first chat turn should succeed");
    session
        .send("second")
        .await
        .expect("second chat turn should succeed");
    let third = session
        .send("third")
        .await
        .expect("third chat turn should succeed");

    // Assert
    assert_eq!(third.output(), &json!({"summary": "three"}));
}

#[tokio::test]
async fn sequential_resumed_handles_reload_completed_canonical_history() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        let expected = if call_index == 0 {
            vec![ModelMessage::User("first".to_string())]
        } else {
            vec![
                ModelMessage::User("first".to_string()),
                ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                ModelMessage::User("second".to_string()),
            ]
        };
        assert_eq!(request.messages(), expected);

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": if call_index == 0 { "one" } else { "two" }
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut first_handle = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    let mut stale_handle = harness
        .resume("session-a")
        .await
        .expect("second handle should resume before the first turn");

    // Act
    let first = first_handle
        .send("first")
        .await
        .expect("first handle should complete its turn");
    let second = stale_handle
        .send("second")
        .await
        .expect("stale handle should reload the completed turn");

    // Assert
    assert_eq!(first.output(), &json!({"summary": "one"}));
    assert_eq!(second.output(), &json!({"summary": "two"}));
}

#[tokio::test]
async fn completion_persistence_failure_interrupts_the_session_turn() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    });
    let harness = Arc::new(Harness::new(model).database(&database_path));
    let session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    drop(session);
    let database = harness.open_database().await.expect("database should open");
    sqlx::query(
        r"
CREATE TRIGGER reject_turn_completion
BEFORE UPDATE OF status ON session_turn
WHEN NEW.status = 'completed'
BEGIN
SELECT RAISE(ABORT, 'injected completion persistence failure');
END
",
    )
    .execute(database.pool())
    .await
    .expect("completion failure trigger should be created");

    // Act
    let error = send_with_resumed_session(Arc::clone(&harness), "complete").await;
    let database = Database::open(&database_path)
        .await
        .expect("database should reopen");
    let expected_state = ("interrupted".to_string(), Some("interrupted".to_string()));
    let state = wait_for_stored_turn_state(&database, &expected_state).await;

    // Assert
    assert!(
        error
            .expect_err("completion persistence should fail")
            .to_string()
            .contains("injected completion persistence failure")
    );
    assert_eq!(state, expected_state);
}

#[tokio::test]
async fn session_error_preserves_model_and_failure_persistence_errors() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .returning(|_| Err(ModelError::InvalidResponse));
    let harness = Harness::new(model).database(&database_path);
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    sqlx::query(
        r"
CREATE TRIGGER reject_turn_failure
BEFORE UPDATE OF status ON session_turn
WHEN NEW.status = 'failed'
BEGIN
SELECT RAISE(ABORT, 'injected persistence failure');
END
",
    )
    .execute(session.database.pool())
    .await
    .expect("failure trigger should be created");

    // Act
    let error = session
        .send("fail twice")
        .await
        .expect_err("model and persistence failures should be returned");

    // Assert
    assert!(matches!(&error, SessionError::TurnPersistence { .. }));
    if let SessionError::TurnPersistence { turn, persistence } = error {
        assert!(matches!(
            turn,
            TurnError::Model(ModelError::InvalidResponse)
        ));
        assert!(
            persistence
                .to_string()
                .contains("injected persistence failure")
        );
    }
}

#[tokio::test]
async fn session_rejects_overlapping_turns_for_the_same_id() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(SlowModel).database(directory.path().join("harness.db"));
    let mut first = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    let mut second = harness
        .resume("session-a")
        .await
        .expect("session should resume");

    // Act
    let (first_result, second_result) = tokio::join!(first.send("first"), second.send("second"));

    // Assert
    let results = [first_result, second_result];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(SessionError::Busy { .. })))
            .count(),
        1
    );
}

#[tokio::test]
async fn different_sessions_run_concurrently() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(SlowModel).database(directory.path().join("harness.db"));
    let mut first = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("first session should be created");
    let mut second = harness
        .session("session-b", object_schema())
        .create()
        .await
        .expect("second session should be created");

    // Act
    let (first_result, second_result) = tokio::join!(first.send("first"), second.send("second"));

    // Assert
    assert!(first_result.is_ok());
    assert!(second_result.is_ok());
}

struct SlowModel;

#[async_trait]
impl Model for SlowModel {
    async fn complete(&self, _request: ModelRequest) -> Result<crate::ModelCompletion, ModelError> {
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}
