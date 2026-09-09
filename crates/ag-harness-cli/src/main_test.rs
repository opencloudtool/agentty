use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

struct FixedModel(Value);

#[async_trait]
impl ag_harness::Model for FixedModel {
    async fn complete(
        &self,
        _request: ag_harness::ModelRequest,
    ) -> Result<ag_harness::ModelCompletion, ag_harness::ModelError> {
        Ok(ag_harness::ModelCompletion::from_response(
            ag_harness::ModelResponse::Output(self.0.clone()),
        ))
    }
}

struct FailOnceModel {
    requests: AtomicUsize,
}

#[async_trait]
impl ag_harness::Model for FailOnceModel {
    async fn complete(
        &self,
        _request: ag_harness::ModelRequest,
    ) -> Result<ag_harness::ModelCompletion, ag_harness::ModelError> {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ag_harness::ModelError::InvalidResponse);
        }

        Ok(ag_harness::ModelCompletion::from_response(
            ag_harness::ModelResponse::Output(json!({"message": "recovered"})),
        ))
    }
}

fn provider_response(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"message": message}).to_string()}
        }]
    }))
}

fn run_arguments(command: Command) -> Option<RunArgs> {
    match command {
        Command::Run(args) => Some(args),
        Command::Resume(_) => None,
    }
}

fn resume_arguments(command: Command) -> Option<ResumeArgs> {
    match command {
        Command::Resume(args) => Some(args),
        Command::Run(_) => None,
    }
}

fn with_repository_controlled_git(mut cli: Cli) -> Result<Cli, io::Error> {
    let git_executable = std::env::current_exe()?;
    let repository_root = git_executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("test executable should have a parent"))?;
    cli.git_executable = Some(git_executable);
    match &mut cli.command {
        Command::Run(arguments) => arguments.read_dir = repository_root,
        Command::Resume(arguments) => arguments.read_dir = repository_root,
    }

    Ok(cli)
}

fn parse_cli<I, T>(arguments: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    arguments.splice(
        1..1,
        [
            OsString::from("--git-executable"),
            test_git_executable().into_os_string(),
        ],
    );

    Cli::try_parse_from(arguments)
}

fn test_git_executable() -> PathBuf {
    let executable_name = format!("git{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH");
    assert!(path.is_some(), "test PATH should be configured");
    let executables = path
        .iter()
        .flat_map(|path| std::env::split_paths(path))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name))
        .filter_map(|candidate| candidate.canonicalize().ok())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert!(
        !executables.is_empty(),
        "trusted Git executable should be available on PATH"
    );

    executables[0].clone()
}

#[test]
fn cli_accepts_chat_with_or_without_an_initial_prompt() {
    // Arrange and Act
    let without_prompt =
        parse_cli(["ag-harness", "run", "muse-custom"]).expect("chat arguments should parse");
    let with_prompt = parse_cli([
        "ag-harness",
        "run",
        "muse-custom",
        "Summarize this change",
        "--provider",
        "qwen",
        "--base-url",
        "https://models.example/v1",
        "--read-dir",
        "repo",
        "--allow-write",
    ])
    .expect("an initial prompt should parse");
    let blank_prompt = parse_cli(["ag-harness", "run", "muse-custom", "  "])
        .expect_err("a blank initial prompt should be rejected");
    let unknown_provider = parse_cli(["ag-harness", "run", "muse-custom", "--provider", "unknown"])
        .expect_err("an unknown provider should be rejected");

    // Assert
    let without_prompt =
        run_arguments(without_prompt.command).expect("run command should contain run arguments");
    assert_eq!(without_prompt.prompt, None);
    assert!(!without_prompt.allow_write);
    assert_eq!(without_prompt.provider, ModelProvider::Muse);
    assert_eq!(without_prompt.read_dir, PathBuf::from("."));
    let with_prompt =
        run_arguments(with_prompt.command).expect("run command should contain run arguments");
    assert_eq!(with_prompt.model, "muse-custom");
    assert_eq!(with_prompt.prompt.as_deref(), Some("Summarize this change"));
    assert_eq!(with_prompt.provider, ModelProvider::Qwen);
    assert_eq!(
        with_prompt.base_url.as_deref(),
        Some("https://models.example/v1")
    );
    assert_eq!(with_prompt.read_dir, PathBuf::from("repo"));
    assert!(with_prompt.allow_write);
    assert!(
        blank_prompt
            .to_string()
            .contains("prompt must contain a non-whitespace character")
    );
    assert!(
        unknown_provider
            .to_string()
            .contains("invalid value 'unknown'")
    );
}

#[test]
fn cli_accepts_every_catalog_provider() {
    // Arrange and Act
    let providers = ModelProvider::all()
        .iter()
        .map(|provider| {
            parse_cli([
                "ag-harness",
                "run",
                "model-id",
                "--provider",
                provider.as_str(),
            ])
            .expect("catalog provider should parse")
        })
        .collect::<Vec<_>>();

    // Assert
    for (cli, expected) in providers.into_iter().zip(ModelProvider::all()) {
        let args = run_arguments(cli.command).expect("run command should contain arguments");
        assert_eq!(args.provider, *expected);
    }
}

#[test]
fn cli_accepts_resume_and_database_override() {
    // Arrange and Act
    let cli = parse_cli([
        "ag-harness",
        "--database",
        "state.db",
        "resume",
        "session-a",
        "continue",
        "--allow-write",
    ])
    .expect("resume arguments should parse");

    // Assert
    assert_eq!(cli.database, Some(PathBuf::from("state.db")));
    let args = resume_arguments(cli.command).expect("resume command should contain arguments");
    assert_eq!(args.session, "session-a");
    assert_eq!(args.prompt.as_deref(), Some("continue"));
    assert!(args.allow_write);

    let resume_probe =
        parse_cli(["ag-harness", "resume", "session-b"]).expect("resume arguments should parse");
    let run_probe = parse_cli(["ag-harness", "run", "model"]).expect("run arguments should parse");
    assert!(run_arguments(resume_probe.command).is_none());
    assert!(resume_arguments(run_probe.command).is_none());
}

#[test]
fn database_path_uses_explicit_root_or_home_and_rejects_missing_storage_root() {
    // Arrange and Act
    let explicit_environment_calls = std::cell::Cell::new(0);
    let mut explicit_environment = |_: &str| {
        explicit_environment_calls.set(explicit_environment_calls.get() + 1);

        Err(env::VarError::NotPresent)
    };
    let explicit = database_path(
        Some(PathBuf::from("explicit.db")),
        &mut explicit_environment,
    );
    let rooted_variables = std::collections::HashMap::from([("AG_HARNESS_ROOT", "/state/harness")]);
    let rooted = database_path(None, &mut |name| {
        rooted_variables
            .get(name)
            .map(ToString::to_string)
            .ok_or(env::VarError::NotPresent)
    });
    let home_variables = std::collections::HashMap::from([("HOME", "/home/user")]);
    let home = database_path(None, &mut |name| {
        home_variables
            .get(name)
            .map(ToString::to_string)
            .ok_or(env::VarError::NotPresent)
    });
    let missing = database_path(None, &mut |_| Err(env::VarError::NotPresent));
    let empty_home = database_path(None, &mut |name| match name {
        "HOME" => Ok(String::new()),
        _ => Err(env::VarError::NotPresent),
    });

    // Assert
    assert_eq!(
        explicit.expect("explicit database should resolve"),
        PathBuf::from("explicit.db")
    );
    assert_eq!(explicit_environment_calls.get(), 0);
    assert!(explicit_environment("unused").is_err());
    assert_eq!(explicit_environment_calls.get(), 1);
    assert_eq!(
        rooted.expect("rooted database should resolve"),
        PathBuf::from("/state/harness/db/harness.db")
    );
    assert_eq!(
        home.expect("home database should resolve"),
        PathBuf::from("/home/user/.ag-harness/db/harness.db")
    );
    assert!(matches!(missing, Err(CliError::DatabaseLocation)));
    assert!(matches!(empty_home, Err(CliError::DatabaseLocation)));
}

#[test]
fn chat_mode_accounts_for_both_terminal_streams_and_initial_prompt() {
    // Arrange
    let with_prompt =
        parse_cli(["ag-harness", "run", "muse", "hello"]).expect("chat arguments should parse");
    let without_prompt =
        parse_cli(["ag-harness", "run", "muse"]).expect("chat arguments should parse");
    let resume = parse_cli(["ag-harness", "resume", "session-a", "continue"])
        .expect("resume arguments should parse");

    // Act and Assert
    assert_eq!(
        ChatMode::detect(&with_prompt, true, true),
        ChatMode::Interactive
    );
    assert_eq!(
        ChatMode::detect(&with_prompt, true, false),
        ChatMode::OneShot
    );
    assert_eq!(
        ChatMode::detect(&with_prompt, false, false),
        ChatMode::NonInteractive
    );
    assert_eq!(
        ChatMode::detect(&without_prompt, true, false),
        ChatMode::NonInteractive
    );
    assert_eq!(ChatMode::detect(&resume, true, false), ChatMode::OneShot);
}

#[test]
fn stored_model_identity_maps_supported_providers_and_rejects_incomplete_identity() {
    // Arrange and Act
    let muse = stored_model_identity_parts(Some("meta"), Some("muse-model"));
    let kimi = stored_model_identity_parts(Some("moonshot_ai"), Some("kimi-model"));
    let qwen = stored_model_identity_parts(Some("alibaba_cloud"), Some("qwen-model"));
    let unknown = stored_model_identity_parts(Some("unknown"), Some("model"));
    let missing_model = stored_model_identity_parts(Some("meta"), None);

    // Assert
    assert!(matches!(muse, Ok((ModelProvider::Muse, model)) if model == "muse-model"));
    assert!(matches!(kimi, Ok((ModelProvider::Kimi, model)) if model == "kimi-model"));
    assert!(matches!(qwen, Ok((ModelProvider::Qwen, model)) if model == "qwen-model"));
    assert!(matches!(unknown, Err(CliError::MissingModelIdentity)));
    assert!(matches!(missing_model, Err(CliError::MissingModelIdentity)));
}

#[test]
fn exit_reporting_sanitizes_errors_and_preserves_success() {
    // Arrange
    let mut success_output = Vec::new();
    let mut error_output = Vec::new();
    let error = CliError::Turn(ag_harness::TurnError::Model(
        ag_harness::ModelError::IncompleteResponse {
            reason: "stop\u{1b}]52;c;Y2xpcGJvYXJk\u{7}".to_string(),
        },
    ));

    // Act
    let success = report_exit(Ok(()), &mut success_output);
    let failure = report_exit(Err(error), &mut error_output);

    // Assert
    assert_eq!(success, ExitCode::SUCCESS);
    assert_eq!(failure, ExitCode::FAILURE);
    assert_eq!(success_output, [] as [u8; 0]);
    assert!(!error_output.contains(&0x1b));
    assert!(!error_output.contains(&0x07));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_advertises_repository_reads_by_default() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""content":"Hello","role":"user""#))
        .respond_with(provider_response("hello"))
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let database = storage.path().join("harness.db");
    let cli = parse_cli([
        "ag-harness",
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        &server.uri(),
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("chat arguments should parse");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    execute(
        cli,
        |_| Ok("test-key".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("chat with default repository reads should succeed");

    // Assert
    assert!(
        String::from_utf8(output)
            .expect("chat output should be UTF-8")
            .contains("assistant> hello\n---\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_resumes_a_saved_session_with_its_model_identity_and_history() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""content":"second","role":"user""#))
        .and(body_string_contains(r#""content":"first","role":"user""#))
        .respond_with(provider_response("second answer"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""content":"first","role":"user""#))
        .respond_with(provider_response("first answer"))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let database = storage.path().join("harness.db");
    let run = parse_cli([
        "ag-harness",
        "--database",
        &database.to_string_lossy(),
        "run",
        "muse-test",
        "first",
        "--session",
        "session-a",
        "--base-url",
        &server.uri(),
    ])
    .expect("run arguments should parse");
    let resume = parse_cli([
        "ag-harness",
        "--database",
        &database.to_string_lossy(),
        "resume",
        "session-a",
        "second",
        "--base-url",
        &server.uri(),
    ])
    .expect("resume arguments should parse");
    let invalid_resume = with_repository_controlled_git(
        parse_cli([
            "ag-harness",
            "--database",
            &database.to_string_lossy(),
            "resume",
            "session-a",
            "third",
            "--base-url",
            &server.uri(),
        ])
        .expect("invalid resume fixture should parse"),
    )
    .expect("repository-controlled Git fixture should resolve");
    let mut first_output = Vec::new();
    let mut second_output = Vec::new();

    // Act
    execute(
        run,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        &mut first_output,
        ChatMode::OneShot,
    )
    .await
    .expect("first process should create the session");
    execute(
        resume,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        &mut second_output,
        ChatMode::OneShot,
    )
    .await
    .expect("second process should resume the session");
    let invalid_error = execute(
        invalid_resume,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        Vec::new(),
        ChatMode::OneShot,
    )
    .await
    .expect_err("repository-controlled Git should reject resume");

    // Assert
    let first_output = String::from_utf8(first_output).expect("output should be UTF-8");
    let second_output = String::from_utf8(second_output).expect("output should be UTF-8");
    assert!(first_output.contains("session: session-a\n"));
    assert!(first_output.contains("assistant> first answer\n"));
    assert!(second_output.contains("session: session-a\n"));
    assert!(second_output.contains("assistant> second answer\n"));
    assert!(matches!(
        invalid_error,
        CliError::Repository(ag_harness::RepositoryError::GitExecutableInsideRepository { .. })
    ));
}

#[tokio::test]
async fn execute_reports_missing_provider_credentials_before_creating_a_session() {
    // Arrange
    let cli = parse_cli([
        "ag-harness",
        "--database",
        "unused.db",
        "run",
        "muse-test",
        "Hello",
    ])
    .expect("run arguments should parse");
    let mut output = Vec::new();

    // Act
    let error = execute(
        cli,
        |_| Err(env::VarError::NotPresent),
        BufReader::new(&b""[..]),
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect_err("missing credentials should fail");

    // Assert
    assert!(matches!(
        error,
        CliError::ModelConfiguration(ModelConfigurationError::ApiKey { .. })
    ));
    assert_eq!(output, [] as [u8; 0]);
}

#[test]
fn cli_accepts_the_default_git_executable() {
    // Arrange
    let arguments = [
        "ag-harness",
        "--database",
        "unused.db",
        "run",
        "muse-test",
        "Hello",
    ];

    // Act
    let cli = Cli::try_parse_from(arguments)
        .expect("missing Git executable override should use the default");

    // Assert
    assert_eq!(cli.git_executable, None);
}

#[test]
#[cfg(unix)]
fn git_executable_default_skips_a_non_executable_file() {
    // Arrange
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let executable_name = format!("git{}", env::consts::EXE_SUFFIX);
    let inert = storage.path().join(executable_name);
    std::fs::write(&inert, "not executable").expect("inert Git fixture should be written");
    let trusted_git = test_git_executable();
    let trusted_directory = trusted_git
        .parent()
        .expect("trusted Git executable should have a parent");
    let path =
        env::join_paths([storage.path(), trusted_directory]).expect("test PATH should be valid");
    let root = env::current_dir().expect("current directory should resolve");
    let expected = Repository::new(&root, trusted_git)
        .expect("trusted Git executable should configure the repository");

    // Act
    let actual = repository_from_path(&root, Some(path.as_os_str()));

    // Assert
    assert_eq!(
        actual.expect("non-executable Git should be skipped"),
        expected
    );
}

#[test]
fn git_executable_default_uses_the_process_path() {
    // Arrange
    let root = env::current_dir().expect("current directory should resolve");
    let expected = Repository::new(&root, test_git_executable())
        .expect("trusted Git executable should configure the repository");

    // Act
    let actual = repository_or_default(root, None)
        .expect("test PATH should contain a trusted Git executable");

    // Assert
    assert_eq!(actual, expected);
}

#[test]
fn git_executable_default_requires_git_on_path() {
    // Arrange
    let root = env::current_dir().expect("current directory should resolve");

    // Act
    let error = repository_from_path(&root, None)
        .expect_err("missing PATH should not produce a Git executable");

    // Assert
    assert!(matches!(error, CliError::GitExecutableNotFound));
}

#[test]
fn git_executable_default_preserves_repository_root_errors() {
    // Arrange
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let missing_root = storage.path().join("missing-root");
    let path = env::var_os("PATH").expect("test PATH should be configured");

    // Act
    let error = repository_from_path(&missing_root, Some(path.as_os_str()))
        .expect_err("missing repository root should fail");

    // Assert
    assert!(matches!(
        error,
        CliError::Repository(ag_harness::RepositoryError::Root { .. })
    ));
}

#[tokio::test]
async fn execute_rejects_repository_controlled_git_for_new_sessions() {
    // Arrange
    let cli = with_repository_controlled_git(
        parse_cli([
            "ag-harness",
            "--database",
            "unused.db",
            "run",
            "muse-test",
            "Hello",
        ])
        .expect("run arguments should parse"),
    )
    .expect("repository-controlled Git fixture should resolve");

    // Act
    let error = execute(
        cli,
        |_| Ok("test-key".to_string()),
        BufReader::new(&b""[..]),
        Vec::new(),
        ChatMode::OneShot,
    )
    .await
    .expect_err("repository-controlled Git should reject a new session");

    // Assert
    assert!(matches!(
        error,
        CliError::Repository(ag_harness::RepositoryError::GitExecutableInsideRepository { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_advertises_writes_only_when_explicitly_enabled() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(READ_WRITE_SYSTEM_PROMPT))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""name":"write""#))
        .respond_with(provider_response("ready"))
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let database = storage.path().join("harness.db");
    let cli = parse_cli([
        "ag-harness",
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        &server.uri(),
        "--allow-write",
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("write-enabled chat arguments should parse");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    execute(
        cli,
        |_| Ok("test-key".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("write-enabled chat should succeed");

    // Assert
    assert!(
        String::from_utf8(output)
            .expect("chat output should be UTF-8")
            .contains("assistant> ready\n---\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_advertises_read_only_with_an_explicit_directory() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""content":"Hello","role":"user""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-read",
                        "type": "function",
                        "function": {
                            "name": "read",
                            "arguments": r#"{"path":"input.txt"}"#
                        }
                    }]
                }
            }]
        })))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(r#""tool_call_id":"call-read""#))
        .respond_with(provider_response("hello"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let repository = tempfile::TempDir::new().expect("temporary repository should exist");
    let database = repository.path().join("harness.db");
    std::fs::write(repository.path().join("input.txt"), "contents")
        .expect("read fixture should be written");
    let cli = parse_cli([
        "ag-harness",
        "run",
        "muse-test",
        "Hello",
        "--base-url",
        &server.uri(),
        "--read-dir",
        &repository.path().to_string_lossy(),
        "--database",
        &database.to_string_lossy(),
    ])
    .expect("chat arguments should parse");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    execute(
        cli,
        |_| Ok("test-key".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("chat with explicit read access should succeed");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.contains("assistant> hello\n---\n"));
    assert!(output.contains("tools:\n  read input.txt (lines 1-1;"));
}

#[test]
fn cli_configuration_errors_preserve_cli_specific_guidance() {
    // Arrange
    let base_url = ModelConfigurationError::BaseUrl {
        name: "KIMI_BASE_URL",
    };
    let api_key = ModelConfigurationError::ApiKey {
        name: "MODEL_API_KEY",
    };

    // Act
    let base_url = CliError::from(base_url);
    let api_key = CliError::from(api_key);

    // Assert
    assert_eq!(
        base_url.to_string(),
        "--base-url or KIMI_BASE_URL is required"
    );
    assert_eq!(api_key.to_string(), "MODEL_API_KEY is unavailable");
}

#[test]
fn chat_schema_requires_one_message_string() {
    // Arrange and Act
    let schema = chat_schema().expect("chat schema should compile");

    // Assert
    assert_eq!(schema.value()["required"], json!(["message"]));
    assert_eq!(schema.value()["additionalProperties"], json!(false));
}

#[test]
fn line_endings_are_trimmed_without_changing_prompt_content() {
    // Arrange
    let mut unix = "hello\n".to_string();
    let mut windows = "hello\r\n".to_string();
    let mut unchanged = "hello".to_string();

    // Act
    trim_line_ending(&mut unix);
    trim_line_ending(&mut windows);
    trim_line_ending(&mut unchanged);

    // Assert
    assert_eq!(unix, "hello");
    assert_eq!(windows, "hello");
    assert_eq!(unchanged, "hello");
}

#[test]
fn durations_have_compact_terminal_formatting() {
    // Arrange and Act
    let short = format_duration(std::time::Duration::ZERO);
    let measured = format_duration(std::time::Duration::from_millis(12));

    // Assert
    assert_eq!(short, "<1 ms");
    assert_eq!(measured, "12 ms");
}

#[tokio::test]
async fn interactive_chat_prints_prompts_and_handles_blank_input() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let repository = Repository::new(env!("CARGO_MANIFEST_DIR"), test_git_executable())
        .expect("repository fixture should be valid");
    let harness = Harness::new(FixedModel(json!({"message": "hello"})))
        .database(directory.path().join("harness.db"))
        .repository(repository)
        .allow(Tool::Read);
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"\nquestion\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::Interactive,
    )
    .await
    .expect("interactive chat should finish at EOF");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(
        output.starts_with("Chat with test-model. Ctrl-D to exit.\n>>> >>> assistant> hello\n")
    );
    assert!(output.contains("output; test-model; unavailable;"));
    assert!(output.contains("tokens unavailable"));
    assert!(output.ends_with("tools: none\n>>> "));
}

#[tokio::test]
async fn interactive_chat_continues_after_a_failed_turn() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\nretry\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::Interactive,
    )
    .await
    .expect("interactive chat should recover and finish at EOF");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.contains("error: model returned no response content\n"));
    assert!(output.contains(">>> assistant> recovered\n---\n"));
    assert!(output.ends_with("tools: none\n>>> "));
}

#[tokio::test]
async fn noninteractive_chat_reports_a_failure_before_retrying() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\nretry\n"[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
    )
    .await
    .expect_err("a recovered chat should retain its failed exit status");

    // Assert
    assert!(matches!(error, CliError::ChatTurnsFailed));
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert!(output.starts_with("error: model returned no response content\n"));
    assert!(output.contains("assistant> recovered\n---\n"));
}

#[tokio::test]
async fn noninteractive_chat_returns_the_last_failure_at_eof() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"first\n"[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        None,
        input,
        &mut output,
        ChatMode::NonInteractive,
    )
    .await
    .expect_err("the final failed turn should be returned at EOF");

    // Assert
    assert!(matches!(error, CliError::ChatTurnsFailed));
    assert_eq!(
        String::from_utf8(output).expect("chat output should be UTF-8"),
        "error: model returned no response content\n"
    );
}

#[tokio::test]
async fn chat_rejects_model_output_that_violates_schema() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FixedModel(json!({"unexpected": true})))
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect_err("schema-invalid output should fail");

    // Assert
    assert!(matches!(
        error,
        CliError::Session(ag_harness::SessionError::Turn(
            ag_harness::TurnError::Model(
                ag_harness::ModelError::SchemaViolation { path, .. }
            )
        )) if path == "$"
    ));
    assert_eq!(output, [] as [u8; 0]);
}

#[tokio::test]
async fn one_shot_chat_does_not_read_follow_up_terminal_input() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FixedModel(json!({"message": "hello"})))
        .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b"unexpected follow-up\n"[..]);
    let mut output = Vec::new();

    // Act
    run_chat(
        &mut session,
        "test-model",
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect("one-shot chat should finish after the initial prompt");

    // Assert
    let output = String::from_utf8(output).expect("chat output should be UTF-8");
    assert_eq!(output.matches("assistant> hello\n---\n").count(), 1);
    assert!(!output.contains("Chat with"));
    assert!(!output.contains(">>>"));
}

#[tokio::test]
async fn one_shot_chat_returns_turn_failures() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let harness = Harness::new(FailOnceModel {
        requests: AtomicUsize::new(0),
    })
    .database(directory.path().join("harness.db"));
    let mut session = harness
        .session(
            "session-a",
            chat_schema().expect("chat schema should compile"),
        )
        .create()
        .await
        .expect("session should be created");
    let input = BufReader::new(&b""[..]);
    let mut output = Vec::new();

    // Act
    let error = run_chat(
        &mut session,
        "test-model",
        Some("question".to_string()),
        input,
        &mut output,
        ChatMode::OneShot,
    )
    .await
    .expect_err("one-shot chat should return its failed turn");

    // Assert
    assert!(matches!(error, CliError::Session(_)));
    assert_eq!(output, [] as [u8; 0]);
}

#[test]
fn terminal_text_replaces_control_sequences_and_preserves_safe_whitespace() {
    // Arrange
    let text = "before\n\t\u{1b}]52;c;Y2xpcGJvYXJk\u{7}after\r";

    // Act
    let sanitized = terminal_text(text);

    // Assert
    assert_eq!(
        sanitized,
        "before\n\t\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}after\u{fffd}"
    );
    assert!(
        sanitized
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
    );
}

#[test]
fn assistant_text_indents_continuation_lines_and_sanitizes_them() {
    // Arrange
    let text = "answer\n---\nturn: forged\u{1b}";

    // Act
    let framed = assistant_text(text);

    // Assert
    assert_eq!(
        framed,
        "assistant> answer\n           ---\n           turn: forged\u{fffd}\n"
    );
}

#[test]
fn single_line_terminal_text_replaces_all_control_characters() {
    // Arrange
    let text = "model\nname\t\u{1b}";

    // Act
    let sanitized = single_line_terminal_text(text);

    // Assert
    assert_eq!(sanitized, "model\u{fffd}name\u{fffd}\u{fffd}");
    assert!(sanitized.chars().all(|character| !character.is_control()));
}

#[test]
fn usage_format_marks_missing_counts() {
    // Arrange
    let usage = ag_harness::CompletionUsage::new(None, None, None, Some(4), None, None);

    // Act
    let formatted = format_usage(&usage);

    // Assert
    assert_eq!(formatted, "tokens ? in, 4 out, ? total");
}
