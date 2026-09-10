use super::*;

#[tokio::test]
async fn rejects_disabled_write_call() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|request| {
        assert_eq!(request.tools(), []);

        Ok(response_without_metadata(ModelResponse::ToolCall(
            write_call(
                "call_denied",
                "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
            ),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    file_system.expect_replace_beneath().times(0);
    let harness = Harness::new(model);

    // Act
    let error = harness
        .run_once("update", object_schema())
        .await
        .expect_err("denied write should fail");

    // Assert
    assert!(matches!(
        error,
        TurnError::ToolDenied { name } if name == "write"
    ));
}

#[tokio::test]
async fn rejects_disabled_read_call() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|request| {
        assert_eq!(request.tools(), []);

        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("call_denied"),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    let harness = Harness::new(model).with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("denied tool should fail");

    // Assert
    assert!(matches!(
        &error,
        TurnError::ToolDenied { name } if name == "read"
    ));
    assert_eq!(error.error_type(), TurnErrorType::ToolDenied);
}

#[tokio::test]
async fn enforces_tool_call_limit() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(2).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("call_read"),
        )))
    });
    let harness = read_harness(model, readable_file_system())
        .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"))
        .with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("second tool call should exceed the limit");

    // Assert
    assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
    assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
}

#[tokio::test]
async fn enforces_tool_call_limit_within_one_model_response() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
            read_call("call_one"),
            read_call("call_two"),
        ])))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    let harness = read_harness(model, file_system)
        .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("second batched tool call should exceed the limit");

    // Assert
    assert!(matches!(error, TurnError::ToolCallLimit { limit: 1 }));
}

#[tokio::test]
async fn rejects_batched_writes_before_any_write_executes() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
            write_call(
                "call_one",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
            ),
            write_call(
                "call_two",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
            ),
        ])))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    file_system.expect_replace_beneath().times(0);
    let harness = write_harness(model, file_system)
        .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

    // Act
    let error = harness
        .run_once("update twice", object_schema())
        .await
        .expect_err("oversized batch should fail before writing");

    // Assert
    assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
    assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
}

#[tokio::test]
async fn rejects_duplicate_batched_call_ids_before_any_write_executes() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
            write_call(
                "duplicate_call",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
            ),
            write_call(
                "duplicate_call",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
            ),
        ])))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    file_system.expect_replace_beneath().times(0);
    let harness = write_harness(model, file_system);

    // Act
    let error = harness
        .run_once("update twice", object_schema())
        .await
        .expect_err("duplicate call identifiers should fail before writing");

    // Assert
    assert!(matches!(
        &error,
        TurnError::Model(ModelError::DuplicateToolCallId { id }) if id == "duplicate_call"
    ));
    assert_eq!(
        error.error_type(),
        TurnErrorType::Model(crate::ModelErrorType::InvalidToolCall)
    );
}

#[tokio::test]
async fn rejects_empty_tool_call_batch() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(
            Vec::new(),
        )))
    });
    let harness = Harness::new(model);

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("empty tool batch should fail immediately");

    // Assert
    assert!(matches!(
        &error,
        TurnError::Model(ModelError::MissingToolCall)
    ));
    assert_eq!(
        error.error_type(),
        TurnErrorType::Model(crate::ModelErrorType::InvalidToolCall)
    );
}
