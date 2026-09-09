use super::*;

#[tokio::test]
async fn completes_read_tool_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(request.tools(), &[ToolDefinition::read()]);

            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )));
        }
        assert_eq!(request.messages().len(), 3);
        assert!(matches!(
            &request.messages()[0],
            ModelMessage::User(prompt) if prompt == "inspect the manifest"
        ));
        assert!(matches!(
            &request.messages()[1],
            ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
        ));
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            }
                if call_id == "call_read"
                    && name == "read"
                    && serde_json::from_str::<Value>(content)
                        .is_ok_and(|value| value["content"] == "[workspace]")
        ));

        Ok(response_without_metadata(ModelResponse::Output(
            json!({ "summary": "workspace" }),
        )))
    });
    let harness = read_harness(model, readable_file_system());

    // Act
    let output = harness
        .run_once("inspect the manifest", object_schema())
        .await
        .expect("tool round trip should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "workspace" }));
}

#[tokio::test]
async fn completes_repository_inspection_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                inspection_call(
                    "call_list",
                    json!({ "action": "list", "path": "Cargo.toml" }),
                ),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { content, .. }
                if serde_json::from_str::<Value>(content)
                    .is_ok_and(|value| value["result"] == json!(["Cargo.toml"]))
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "listed"
        }))))
    });
    let harness = Harness::new(model)
        .repository(Repository::fixture(env!("CARGO_MANIFEST_DIR")))
        .allow(Tool::Read);

    // Act
    let outcome = harness
        .run_once("list the manifest", object_schema())
        .await
        .expect("repository inspection should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "listed" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadInspection {
            action: ReadAction::List,
            summary,
            ..
        } if summary == "Cargo.toml"
    ));
}

#[tokio::test]
async fn returns_encoded_file_result_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )));
        }
        assert!(request.messages().iter().any(|message| {
            matches!(
                message,
                ModelMessage::ToolResult { content, .. }
                    if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                        value["status"] == "rejected"
                            && value["error"]
                                .as_str()
                                .is_some_and(|error| error.contains("exceeds the read size limit"))
                    })
            )
        }));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "recovered"
        }))))
    });
    let content = "\u{1}".repeat(16 * 1024).into_bytes();
    let harness = read_harness(model, readable_file_system_with(content));

    // Act
    let outcome = harness
        .run_once("read the manifest", object_schema())
        .await
        .expect("model should recover from an encoded-size rejection");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadRejected { path, .. } if path == "Cargo.toml"
    ));
}

#[tokio::test]
async fn returns_schema_valid_read_argument_rejections_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model
        .expect_complete()
        .times(2)
        .returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                    inspection_call("call_file", json!({})),
                    inspection_call("call_search", json!({ "action": "search" })),
                ])));
            }
            assert!(request.messages().iter().any(|message| {
                matches!(
                    message,
                    ModelMessage::ToolResult { content, .. }
                        if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                            value["status"] == "rejected"
                                && value["error"] == "file requires a path and accepts only offset and limit"
                        })
                )
            }));
            assert!(request.messages().iter().any(|message| {
                matches!(
                    message,
                    ModelMessage::ToolResult { content, .. }
                        if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                            value["status"] == "rejected"
                                && value["error"] == "search requires a query and accepts only an optional path and limit"
                        })
                )
            }));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
    let harness = read_harness(model, MockFileSystem::new());

    // Act
    let outcome = harness
        .run_once("read and search the repository", object_schema())
        .await
        .expect("model should recover from schema-portable argument rejection");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadRejected { path, .. } if path == "read"
    ));
    assert!(matches!(
        &outcome.report().tool_calls()[1],
        ToolActivity::ReadInspectionRejected {
            action: ReadAction::Search,
            summary,
            ..
        } if summary == "read"
    ));
}

#[tokio::test]
async fn returns_correctable_repository_inspection_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                inspection_call(
                    "call_show",
                    json!({
                        "action": "show",
                        "path": "definitely-missing-review-file",
                        "side": "head"
                    }),
                ),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { content, .. }
                if serde_json::from_str::<Value>(content)
                    .is_ok_and(|value| value["status"] == "rejected")
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "recovered"
        }))))
    });
    let harness = Harness::new(model)
        .repository(Repository::fixture(env!("CARGO_MANIFEST_DIR")))
        .allow(Tool::Read);

    // Act
    let outcome = harness
        .run_once("show the missing file", object_schema())
        .await
        .expect("model should recover from a rejected inspection");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadInspectionRejected {
            action: ReadAction::Show,
            summary,
            ..
        } if summary == "definitely-missing-review-file"
    ));
}

#[tokio::test]
async fn returns_repository_inspection_boundary_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            inspection_call("call_list", json!({ "action": "list" })),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::other("repository unavailable")));
    file_system.expect_open_beneath().times(0);
    let harness = read_harness(model, file_system);

    // Act
    let error = harness
        .run_once("list files", object_schema())
        .await
        .expect_err("repository boundary failure should end the turn");

    // Assert
    assert!(matches!(
        error,
        TurnError::Read(ReadError::RepositoryRoot { .. })
    ));
}

#[tokio::test]
async fn completes_multiple_read_tools_from_one_model_response() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                read_call("call_one"),
                read_call("call_two"),
            ])));
        }
        assert!(matches!(
            &request.messages()[1],
            ModelMessage::AssistantToolCalls(calls)
                if calls.iter().map(ToolCall::id).collect::<Vec<_>>()
                    == ["call_one", "call_two"]
        ));
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { call_id, .. } if call_id == "call_one"
        ));
        assert!(matches!(
            &request.messages()[3],
            ModelMessage::ToolResult { call_id, .. } if call_id == "call_two"
        ));

        Ok(response_without_metadata(ModelResponse::Output(
            json!({ "summary": "workspace" }),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(4)
        .returning(|path| {
            if path == std::path::Path::new("repo") {
                Ok(PathBuf::from("/repo"))
            } else {
                Ok(PathBuf::from("/repo/Cargo.toml"))
            }
        });
    file_system
        .expect_open_beneath()
        .times(2)
        .returning(|_, _| {
            Ok(Box::new(Cursor::new(
                b"[workspace]\nmember = true\n".to_vec(),
            )))
        });
    let harness = read_harness(model, file_system);

    // Act
    let outcome = harness
        .run_once("inspect two files", object_schema())
        .await
        .expect("parallel tool round trip should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "workspace" }));
    assert_eq!(outcome.report().tool_calls().len(), 2);
}

#[tokio::test]
async fn returns_correctable_read_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { content, .. }
                if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                    value["path"] == "Cargo.toml" && value["status"] == "rejected"
                })
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "recovered"
        }))))
    });
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")));
    file_system.expect_open_beneath().times(0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = read_harness(model, file_system).with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });

    // Act
    let outcome = harness
        .run_once("inspect", object_schema())
        .await
        .expect("model should recover from a rejected read path");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert_eq!(outcome.report().tool_calls().len(), 1);
    let activity = &outcome.report().tool_calls()[0];
    assert_eq!(activity.name(), "read");
    assert_eq!(activity.path(), "Cargo.toml");
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert!(matches!(
        events[5].kind(),
        crate::LifecycleEventKind::ToolFailed {
            error_type: ToolErrorType::Execution,
            ..
        }
    ));
    assert!(matches!(
        events[8].kind(),
        crate::LifecycleEventKind::TurnCompleted { .. }
    ));
}

fn inspection_call(id: &str, arguments: Value) -> ToolCall {
    let arguments = serde_json::from_value::<ReadArguments>(arguments)
        .expect("inspection arguments should be valid");

    ToolCall::read(id.to_string(), arguments, None)
}
