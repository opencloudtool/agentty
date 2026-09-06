//! External-consumer coverage for the `ag-harness` model traits.

use std::error::Error;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ag_harness::{
    CompletionMetadata, CompletionUsage, FileSystem, Harness, LifecycleEventKind, LifecycleMetrics,
    LifecycleObserverSet, LifecycleTraceObserver, Model, ModelCompletion, ModelConfiguration,
    ModelError, ModelMessage, ModelMetadata, ModelProvider, ModelRequest, ModelResponse,
    ModelResponseType, OutputSchema, OutputSchemaError, Repository, RepositoryError, SessionError,
    SessionInfo, Tool, ToolCall, TurnError,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncRead;

struct ExternalToolModel {
    batched: bool,
    requests: Arc<Mutex<Vec<Vec<ModelMessage>>>>,
}

#[async_trait]
impl Model for ExternalToolModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests
            .lock()
            .map_err(|_| ModelError::request(io::Error::other("request recorder poisoned")))?
            .push(request.messages().to_vec());

        let response = match request.messages().last() {
            Some(ModelMessage::User(prompt)) if prompt == "Read the name" => {
                let call = ToolCall::from_json(
                    "read-name".to_string(),
                    "read",
                    r#"{"path":"name.txt"}"#,
                    Some("provider replay context".to_string()),
                )?;
                if self.batched {
                    let second = ToolCall::from_json(
                        "read-again".to_string(),
                        "read",
                        r#"{"path":"name.txt"}"#,
                        None,
                    )?;

                    return Ok(ModelCompletion::from_response(ModelResponse::ToolCalls(
                        vec![call, second],
                    )));
                }

                Ok(ModelResponse::ToolCall(call))
            }
            Some(ModelMessage::ToolResult { content, .. }) => {
                let result: serde_json::Value =
                    serde_json::from_str(content).map_err(ModelError::request)?;
                let name = result["content"]
                    .as_str()
                    .ok_or(ModelError::InvalidResponse)?;

                Ok(ModelResponse::Output(json!({ "name": name.trim() })))
            }
            Some(ModelMessage::User(_)) => {
                let previous = request
                    .messages()
                    .iter()
                    .rev()
                    .find_map(|message| {
                        if let ModelMessage::Assistant(output) = message {
                            return Some(output);
                        }

                        None
                    })
                    .ok_or(ModelError::InvalidResponse)?;

                Ok(ModelResponse::Output(
                    serde_json::from_str(previous).map_err(ModelError::request)?,
                ))
            }
            _ => Err(ModelError::InvalidResponse),
        }?;

        Ok(ModelCompletion::from_response(response))
    }
}

struct NameFileSystem;

#[async_trait]
impl FileSystem for NameFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        assert!(root.is_absolute());
        assert_eq!(path, Path::new("name.txt"));

        Ok(Box::new(Cursor::new(b"Ada\n")))
    }

    async fn replace_beneath(
        &self,
        _root: &Path,
        _path: &Path,
        _expected: Option<Vec<u8>>,
        _content: Vec<u8>,
    ) -> io::Result<()> {
        Err(io::Error::other("read-only fixture"))
    }
}

#[test]
fn external_repository_configuration_rejects_relative_git_executable() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let repository = tempfile::tempdir()?;

    // Act
    let error = Repository::new(repository.path(), "git")
        .expect_err("relative Git executable should be rejected");

    // Assert
    assert!(matches!(
        error,
        RepositoryError::GitExecutableNotAbsolute { .. }
    ));

    Ok(())
}

#[tokio::test]
async fn external_model_reads_tool_results_and_retains_chat_history() -> Result<(), Box<dyn Error>>
{
    for batched in [false, true] {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = ExternalToolModel {
            batched,
            requests: Arc::clone(&requests),
        };
        let directory = tempfile::tempdir()?;
        let repository = Repository::new(directory.path(), std::env::current_exe()?)?;
        let harness = Harness::new(model)
            .database(directory.path().join("harness.db"))
            .repository(repository)
            .file_system(NameFileSystem)
            .allow(Tool::Read);
        let mut chat = harness
            .session(format!("external-{batched}"), request()?.schema().clone())
            .system_prompt("Extract names")
            .create()
            .await?;

        // Act
        let first = chat.send("Read the name").await?;
        let second = chat.send("Recall the name").await?;

        // Assert
        assert_eq!(first.output(), &json!({ "name": "Ada" }));
        assert_eq!(second.output(), first.output());
        assert_eq!(
            first.report().tool_calls().len(),
            if batched { 2 } else { 1 }
        );
        assert_eq!(second.report().tool_calls().len(), 0);
        let requests = requests
            .lock()
            .expect("request recorder should not be poisoned");
        assert_eq!(requests.len(), 3);
        assert!(
            matches!(requests[0].as_slice(), [ModelMessage::System(system), ModelMessage::User(prompt)]
            if system == "Extract names" && prompt == "Read the name")
        );
        let calls = match &requests[1][2] {
            ModelMessage::AssistantToolCall(call) if !batched => std::slice::from_ref(call),
            ModelMessage::AssistantToolCalls(calls) if batched => calls.as_slice(),
            message => return Err(format!("unexpected assistant message: {message:?}").into()),
        };
        assert_eq!(requests[1].len(), 3 + calls.len());
        assert_eq!(
            calls[0].reasoning_content(),
            Some("provider replay context")
        );
        assert_eq!(calls[0].arguments_json()?, r#"{"path":"name.txt"}"#);
        for (call, result) in calls.iter().zip(&requests[1][3..]) {
            assert!(
                matches!(result, ModelMessage::ToolResult { call_id, content, name }
                if call_id == call.id() && name == "read" && content.contains("Ada"))
            );
        }
        assert_eq!(&requests[2][..requests[1].len()], requests[1].as_slice());
        assert!(matches!(&requests[2][requests[1].len()..],
            [ModelMessage::Assistant(output), ModelMessage::User(prompt)]
                if serde_json::from_str::<serde_json::Value>(output)? == *first.output()
                    && prompt == "Recall the name"));
    }

    Ok(())
}

#[test]
fn external_adapter_enforces_tool_call_identifier_limits() -> Result<(), Box<dyn Error>> {
    for (name, arguments) in [
        ("read", r#"{"path":"name.txt"}"#),
        ("write", r#"{"path":"name.txt","patch":"patch"}"#),
    ] {
        // Arrange
        let invalid_identifiers = [
            String::new(),
            " \t\n".to_string(),
            "x".repeat(1025),
            "é".repeat(513),
        ];
        let boundary_id = "é".repeat(512);

        // Act
        let rejected = invalid_identifiers.map(|id| ToolCall::from_json(id, name, arguments, None));
        let accepted = ToolCall::from_json(boundary_id.clone(), name, arguments, None)?;

        // Assert
        assert!(
            rejected
                .iter()
                .all(|result| matches!(result, Err(ModelError::InvalidToolCallId)))
        );
        assert_eq!(accepted.id(), boundary_id);
    }

    Ok(())
}

#[test]
fn external_adapter_constructs_a_validated_write_call() -> Result<(), Box<dyn Error>> {
    // Arrange
    let arguments = json!({
        "path": "name.txt",
        "patch": "--- a/name.txt\n+++ b/name.txt\n@@ -1 +1 @@\n-Ada\n+Grace\n"
    });

    // Act
    let call = ToolCall::from_json(
        "write-name".to_string(),
        "write",
        &arguments.to_string(),
        None,
    )?;
    let invalid = ToolCall::from_json(
        "unsafe".to_string(),
        "write",
        r#"{"path":"../secret","patch":"patch"}"#,
        None,
    );

    // Assert
    assert_eq!(
        call.write_arguments().expect("write arguments").path(),
        "name.txt"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&call.arguments_json()?)?,
        arguments
    );
    assert!(matches!(
        invalid,
        Err(ModelError::InvalidToolArguments { .. })
    ));

    Ok(())
}

#[tokio::test]
async fn external_tool_call_still_requires_harness_permission() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ExternalToolModel {
        batched: false,
        requests: Arc::new(Mutex::new(Vec::new())),
    });

    // Act
    let result = harness
        .run_once("Read the name", request()?.schema().clone())
        .await;

    // Assert
    assert!(matches!(result, Err(TurnError::ToolDenied { name }) if name == "read"));

    Ok(())
}

#[test]
fn external_consumer_can_inspect_resume_fallback_outcome() {
    // Arrange
    let response_type = ModelResponseType::ResumeUnavailable;

    // Act
    let response_type = response_type.to_string();

    // Assert
    assert_eq!(response_type, "resume unavailable");
}

struct ExternalModel;

#[async_trait]
impl Model for ExternalModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({
                "name": "Ada"
            }),
        )))
    }
}

struct ExternalMetadataModel;

#[async_trait]
impl Model for ExternalMetadataModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        ModelMetadata::new("external_provider", "external-model").ok()
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let usage = CompletionUsage::new(None, None, Some(4), Some(2), None, Some(6));
        let metadata = CompletionMetadata::new(
            "stop".to_string(),
            Some("external-response".to_string()),
            Some("external-model".to_string()),
            None,
            Some(usage),
        );

        Ok(ModelCompletion::new(
            metadata,
            ModelResponse::Output(json!({ "name": "Ada" })),
        ))
    }
}

fn assert_observer<Observer: ag_harness::LifecycleObserver>(_observer: Observer) {}

#[test]
fn external_consumer_configures_every_catalog_provider() {
    // Arrange and Act
    let clients = ModelProvider::all()
        .iter()
        .map(|provider| {
            ModelConfiguration::new(*provider, provider.known_models()[0])
                .base_url("https://models.example/v1")
                .client_from_environment(|_| Ok("test-key".to_string()))
                .expect("catalog provider should construct a client")
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(clients.len(), ModelProvider::all().len());
    for (client, provider) in clients.iter().zip(ModelProvider::all()) {
        assert_eq!(client.metadata().model(), provider.known_models()[0]);
    }
}

#[test]
fn external_consumer_constructs_lifecycle_trace_observer() {
    // Arrange & Act
    let observer = LifecycleTraceObserver::new();

    // Assert
    assert_observer(observer);
}

fn request() -> Result<ModelRequest, OutputSchemaError> {
    Ok(ModelRequest::new(
        "extract the name",
        OutputSchema::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        }))?,
    ))
}

#[tokio::test]
async fn external_response_only_provider_implements_model() -> Result<(), Box<dyn Error>> {
    // Arrange
    fn assert_model<ModelType: Model>() {}
    let model = ExternalModel;

    // Act
    let completion = model.complete(request()?).await?;

    // Assert
    assert_model::<ExternalModel>();
    assert_eq!(completion.output(), Some(&json!({ "name": "Ada" })));
    assert!(completion.metadata().is_none());

    Ok(())
}

#[tokio::test]
async fn external_consumer_creates_and_reopens_persistent_session() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let harness = Harness::new(ExternalModel).database(directory.path().join("harness.db"));

    // Act
    let mut session = harness
        .session("external-session", request()?.schema().clone())
        .system_prompt("Extract names")
        .create()
        .await?;
    let first = session.send("Ada").await?;
    drop(session);
    let reopened = harness.resume("external-session").await?;

    // Assert
    assert_eq!(first.output(), &json!({ "name": "Ada" }));
    assert_eq!(reopened.id(), "external-session");

    Ok(())
}

#[tokio::test]
async fn durable_session_requires_explicit_storage() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ExternalModel);

    // Act
    let error = harness
        .session("external-session", request()?.schema().clone())
        .create()
        .await
        .err()
        .expect("missing storage should fail");

    // Assert
    assert!(matches!(error, SessionError::StorageRequired));

    Ok(())
}

#[tokio::test]
async fn session_info_exposes_stored_model_identity() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("harness.db");
    let harness = Harness::new(ExternalMetadataModel).database(&database);
    let session = harness
        .session("external-session", request()?.schema().clone())
        .create()
        .await?;
    drop(session);

    // Act
    let info = SessionInfo::load(&database, "external-session").await?;

    // Assert
    assert_eq!(info.provider(), Some("external_provider"));
    assert_eq!(info.model(), Some("external-model"));

    Ok(())
}

#[tokio::test]
async fn external_provider_constructs_metadata_completion_through_dynamic_dispatch()
-> Result<(), Box<dyn Error>> {
    // Arrange
    fn assert_model<ModelType: Model>() {}
    let model: Box<dyn Model> = Box::new(ExternalMetadataModel);

    // Act
    let configured_metadata = model
        .metadata()
        .expect("external provider should expose configured identity");
    let completion = model.complete(request()?).await?;

    // Assert
    assert_model::<ExternalMetadataModel>();
    assert_eq!(configured_metadata.provider(), "external_provider");
    assert_eq!(configured_metadata.model(), "external-model");
    assert_eq!(
        completion.response().output(),
        Some(&json!({ "name": "Ada" }))
    );
    assert_eq!(
        completion
            .metadata()
            .expect("completion should include metadata")
            .response_id(),
        Some("external-response")
    );
    assert_eq!(
        completion
            .metadata()
            .expect("completion should include metadata")
            .usage()
            .and_then(|usage| usage.total_tokens()),
        Some(6)
    );

    Ok(())
}

#[tokio::test]
async fn external_metadata_provider_reaches_harness_lifecycle() -> Result<(), Box<dyn Error>> {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(ExternalMetadataModel).with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });

    // Act
    let output = harness
        .run_once("extract the name", request()?.schema().clone())
        .await?;

    // Assert
    assert_eq!(output.output(), &json!({ "name": "Ada" }));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        LifecycleEventKind::ModelRequestStarted {
            model: Some(model),
            ..
        } if model.provider() == "external_provider" && model.model() == "external-model"
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        LifecycleEventKind::ModelRequestCompleted {
            completion: Some(metadata),
            ..
        } if metadata.response_id() == Some("external-response")
            && metadata.usage().and_then(|usage| usage.total_tokens()) == Some(6)
    )));

    Ok(())
}

#[tokio::test]
async fn external_observer_set_fans_out_lifecycle_events() -> Result<(), Box<dyn Error>> {
    // Arrange
    let first_events = Arc::new(Mutex::new(Vec::new()));
    let observed_first_events = Arc::clone(&first_events);
    let second_events = Arc::new(Mutex::new(Vec::new()));
    let observed_second_events = Arc::clone(&second_events);
    let observers = LifecycleObserverSet::new(move |event| {
        observed_first_events
            .lock()
            .expect("first event recorder should not be poisoned")
            .push(event);
    })
    .with_observer(move |event| {
        observed_second_events
            .lock()
            .expect("second event recorder should not be poisoned")
            .push(event);
    })
    .with_observer(LifecycleMetrics::new());
    let harness = Harness::new(ExternalMetadataModel).with_lifecycle_observer(observers);

    // Act
    let output = harness
        .run_once("extract the name", request()?.schema().clone())
        .await?;

    // Assert
    assert_eq!(output.output(), &json!({ "name": "Ada" }));
    let first_events = first_events
        .lock()
        .expect("first event recorder should not be poisoned");
    let second_events = second_events
        .lock()
        .expect("second event recorder should not be poisoned");
    assert!(!first_events.is_empty());
    assert_eq!(*first_events, *second_events);

    Ok(())
}
