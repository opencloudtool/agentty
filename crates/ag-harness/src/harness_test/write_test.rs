use super::*;

#[tokio::test]
async fn completes_write_tool_round_trip() {
    // Arrange
    let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(request.tools(), &[ToolDefinition::write()]);

            return Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call("call_write", patch),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            }
                if call_id == "call_write"
                    && name == "write"
                    && serde_json::from_str::<Value>(content).is_ok_and(|value| {
                        value == json!({
                            "bytes_written": 4,
                            "path": "src/lib.rs",
                            "status": "applied"
                        })
                    })
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "updated"
        }))))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system
        .expect_replace_beneath()
        .times(1)
        .withf(|_, _, expected, content| {
            expected.as_deref() == Some(b"old\n".as_slice()) && content == b"new\n"
        })
        .returning(|_, _, _, _| Ok(()));
    let harness = write_harness(model, file_system);

    // Act
    let output = harness
        .run_once("update the file", object_schema())
        .await
        .expect("write round trip should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "updated" }));
}

#[tokio::test]
async fn returns_correctable_write_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call("call_write", "not a unified diff"),
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
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system.expect_replace_beneath().times(0);
    let harness = write_harness(model, file_system);

    // Act
    let output = harness
        .run_once("update", object_schema())
        .await
        .expect("model should recover from rejected patch");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "recovered" }));
}

#[tokio::test]
async fn returns_terminal_write_boundary_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            write_call(
                "call_write",
                "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
            ),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
    let harness = write_harness(model, file_system);

    // Act
    let error = harness
        .run_once("update", object_schema())
        .await
        .expect_err("write boundary failure should end turn");

    // Assert
    assert!(matches!(&error, TurnError::Write(_)));
    assert_eq!(error.error_type(), TurnErrorType::Tool);
}
