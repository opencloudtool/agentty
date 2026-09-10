use super::*;

pub(super) fn model() -> crate::model::MockModel {
    let mut model = crate::model::MockModel::new();
    model.expect_metadata().return_const(None);

    model
}

pub(super) fn object_schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("schema should be valid")
}

pub(super) fn read_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
    Harness::new(model)
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(file_system)
}

pub(super) fn write_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
    Harness::new(model)
        .repository(Repository::fixture("repo"))
        .allow(Tool::Write)
        .file_system(file_system)
}

pub(super) fn read_call(id: &str) -> ToolCall {
    read_call_with_path(id, "Cargo.toml")
}

pub(super) fn read_call_with_path(id: &str, path: &str) -> ToolCall {
    let arguments = serde_json::from_value::<ReadArguments>(json!({
        "action": "file",
        "path": path,
        "limit": 1
    }))
    .expect("read arguments should be valid");

    ToolCall::read(id.to_string(), arguments, None)
}

pub(super) fn response_without_metadata(response: ModelResponse) -> crate::ModelCompletion {
    crate::ModelCompletion::from_response(response)
}

pub(super) async fn send_with_resumed_session(
    harness: Arc<Harness>,
    prompt: &'static str,
) -> Result<TurnOutcome, SessionError> {
    let mut session = harness
        .resume("session-a")
        .await
        .expect("session should resume");

    session.send(prompt).await
}

async fn stored_turn_state(database: &Database) -> (String, Option<String>) {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, error_type FROM session_turn ORDER BY turn_position DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored turn state should load")
}

pub(super) async fn wait_for_stored_turn_state(
    database: &Database,
    expected: &(String, Option<String>),
) -> (String, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        let state = stored_turn_state(database).await;
        if &state == expected || Instant::now() >= deadline {
            return state;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) fn write_call(id: &str, patch: &str) -> ToolCall {
    let arguments = serde_json::from_value::<WriteArguments>(json!({
        "path": "src/lib.rs",
        "patch": patch
    }))
    .expect("write arguments should be valid");

    ToolCall::write(id.to_string(), arguments, None)
}

pub(super) fn readable_file_system() -> MockFileSystem {
    readable_file_system_with(b"[workspace]\nmember = true\n".to_vec())
}

pub(super) fn readable_file_system_with(content: Vec<u8>) -> MockFileSystem {
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
        .returning(|_| Ok(PathBuf::from("/repo/Cargo.toml")));
    file_system
        .expect_open_beneath()
        .times(1)
        .return_once(move |_, _| Ok(Box::new(Cursor::new(content))));

    file_system
}
