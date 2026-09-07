//! Process-level coverage for the `ag-harness` command-line interface.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::Duration;

use ag_harness::{ModelProvider, ToolDefinition};
use assert_cmd::cargo::cargo_bin;
use serde_json::json;
use testty::session::PtySessionBuilder;
use wiremock::matchers::{bearer_token, body_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const READ_ONLY_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a read-only repository harness. The read tool supports file, list, ",
    "search, diff, and show actions. For change review, call diff first, then use search, file, ",
    "list, or show for evidence. Call the tool immediately and use its result before answering. ",
    "Never narrate, promise, or defer a future tool call. Never claim that you created, ",
    "modified, deleted, or executed files or commands because filesystem mutation and command ",
    "execution are unavailable. If asked to perform an unsupported action, state that it is ",
    "unsupported."
);
const READ_WRITE_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a repository harness with read and write tools. The read tool supports ",
    "file, list, search, diff, and show actions. For change review, call diff first. When a user ",
    "asks about repository contents, call read immediately and use its result before answering. ",
    "When a user asks to create or modify a file, call the write tool ",
    "immediately in the same response. Never narrate, promise, or defer a future tool call. Only ",
    "claim that a file was created or modified after the write tool succeeds. File deletion and ",
    "command execution are unavailable."
);

fn chat_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "message": {"type": "string"}
        },
        "required": ["message"],
        "additionalProperties": false
    })
}

fn structured_output_instruction() -> String {
    format!(
        "Return only one JSON object. The object must validate against this JSON Schema. Do not \
         include Markdown fences or any other text.\n\nJSON Schema:\n{}",
        chat_schema()
    )
}

fn read_tool() -> serde_json::Value {
    let definition = ToolDefinition::read();

    json!({
        "type": "function",
        "function": {
            "description": definition.description(),
            "name": definition.name(),
            "parameters": definition.parameters()
        }
    })
}

fn response(message: &str, input_tokens: u64, output_tokens: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"message": message}).to_string()}
        }],
        "id": "response-test",
        "model": "muse-reported",
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    }))
}

fn harness_command() -> std::io::Result<(tempfile::TempDir, Command)> {
    let storage = tempfile::tempdir()?;
    let mut command = Command::new(cargo_bin!("ag-harness"));
    command
        .arg("--git-executable")
        .arg(test_git_executable())
        .env("AG_HARNESS_ROOT", storage.path());

    Ok((storage, command))
}

fn test_git_executable() -> std::path::PathBuf {
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
fn help_describes_the_chat_interface() {
    // Arrange
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command.arg("--help").output().expect("CLI help should run");

    // Assert
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("Chats with models through a repository harness"));
    assert!(stdout.contains("Usage: ag-harness [OPTIONS] <--git-executable <FILE>> <COMMAND>"));
    assert!(stdout.contains("--git-executable <FILE>"));
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("Starts a new durable session"));
    assert!(stdout.contains("Resumes a durable session"));
    assert!(stdout.contains("Supported models"));
    for provider in ModelProvider::all() {
        assert!(stdout.contains(provider.as_str()));
        for model in provider.known_models() {
            assert!(stdout.contains(model));
        }
    }
    assert!(stdout.contains("Credentials:"));
    for provider in ModelProvider::all() {
        assert!(stdout.contains(provider.api_key_environment()));
    }
    assert!(!stdout.contains("Get started:"));
    assert!(!stdout.contains("Examples:"));
}

#[test]
fn missing_git_executable_is_a_clap_error() {
    // Arrange
    let mut command = Command::new(cargo_bin!("ag-harness"));

    // Act
    let output = command
        .args(["run", "muse-custom"])
        .output()
        .expect("CLI argument validation should run");

    // Assert
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("argument error should be UTF-8");
    assert!(stderr.contains("required arguments were not provided"));
    assert!(stderr.contains("--git-executable <FILE>"));
}

#[test]
fn run_help_describes_optional_initial_prompt() {
    // Arrange
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command
        .args(["run", "--help"])
        .output()
        .expect("run help should execute");

    // Assert
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(
        stdout
            .contains("Usage: ag-harness <--git-executable <FILE>> run [OPTIONS] <MODEL> [PROMPT]")
    );
    assert!(stdout.contains("Optional first prompt"));
    assert!(stdout.contains("--base-url <URL>"));
    for provider in ModelProvider::all() {
        assert!(stdout.contains(provider.base_url_environment()));
    }
    assert!(stdout.contains("Credentials:"));
    for provider in ModelProvider::all() {
        assert!(stdout.contains(provider.api_key_environment()));
    }
    assert!(stdout.contains("--provider <PROVIDER>"));
    assert!(stdout.contains("[default: muse]"));
    let provider_values = ModelProvider::all()
        .iter()
        .map(|provider| provider.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    assert!(stdout.contains(&format!("[possible values: {provider_values}]")));
    assert!(stdout.contains("--read-dir <DIR>"));
    assert!(stdout.contains("Repository directory available to enabled tools"));
    assert!(stdout.contains("[default: .]"));
    assert!(stdout.contains("--allow-write"));
    assert!(stdout.contains("Enables repository writes through the write tool"));
    assert!(!stdout.contains("Chat behavior:"));
    assert!(!stdout.contains("--schema"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_flags_select_kimi_and_qwen_wire_formats() {
    // Arrange and Act
    for provider in [ModelProvider::Kimi, ModelProvider::Qwen] {
        let model = provider.known_models()[0];
        let server = MockServer::start().await;
        let expected_request = json!({
            "messages": [
                {"content": structured_output_instruction(), "role": "system"},
                {"content": READ_ONLY_SYSTEM_PROMPT, "role": "system"},
                {"content": "Hello", "role": "user"}
            ],
            "model": model,
            "tools": [read_tool()]
        });
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(bearer_token("test-key"))
            .respond_with(response("provider response", 4, 2))
            .expect(1)
            .mount(&server)
            .await;
        let (_storage, mut command) = harness_command().expect("temporary storage should exist");
        let output = command
            .args(["run", model, "Hello", "--provider", provider.as_str()])
            .env(provider.api_key_environment(), "test-key")
            .env(provider.base_url_environment(), server.uri())
            .output()
            .expect("provider request should run");

        // Assert
        assert!(
            output.status.success(),
            "{provider} CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("assistant> provider response"));
        let requests = server
            .received_requests()
            .await
            .expect("provider request should be recorded");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .body_json::<serde_json::Value>()
                .expect("provider request should contain JSON"),
            expected_request
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_prompt_prints_answer_and_model_metadata() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(bearer_token("test-key"))
        .and(body_json(json!({
            "messages": [
                {"content": READ_ONLY_SYSTEM_PROMPT, "role": "system"},
                {"content": "Hello", "role": "user"}
            ],
            "model": "muse-test",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "ag_harness_output",
                    "schema": chat_schema()
                }
            },
            "tools": [read_tool()]
        })))
        .respond_with(response("Hi there", 9, 3))
        .expect(1)
        .mount(&server)
        .await;
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command
        .args(["run", "muse-test", "Hello", "--base-url", &server.uri()])
        .env("MODEL_API_KEY", "test-key")
        .output()
        .expect("CLI request should run");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.starts_with("session: "));
    assert!(stdout.contains("assistant> Hi there\n---\n"));
    assert!(stdout.contains("model calls: 1\n"));
    assert!(stdout.contains("output; muse-reported; stop;"));
    assert!(stdout.contains("tokens 9 in, 3 out, 12 total"));
    assert!(stdout.ends_with("tools: none\n"));
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_output_and_metadata_cannot_spoof_terminal_framing() {
    // Arrange
    let server = MockServer::start().await;
    let escape = "\u{1b}]52;c;Y2xpcGJvYXJk\u{7}";
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": json!({
                        "message": format!(
                            "before{escape}after\n---\nturn: forged\nmodel calls: forged\ntools: forged"
                        )
                    }).to_string()
                }
            }],
            "model": format!("muse{escape}\nmodel calls: forged"),
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command
        .args(["run", "muse-test", "Hello", "--base-url", &server.uri()])
        .env("MODEL_API_KEY", "test-key")
        .output()
        .expect("CLI request should run");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.contains(&0x1b));
    assert!(!output.stdout.contains(&0x07));
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(
        stdout.contains(
            "assistant> before�]52;c;Y2xpcGJvYXJk�after\n           ---\n           turn: \
             forged\n           model calls: forged\n           tools: forged\n---\n"
        )
    );
    assert!(stdout.contains("output; muse�]52;c;Y2xpcGJvYXJk��model calls: forged; stop;"));
    assert_eq!(stdout.matches("\nturn: ").count(), 1);
    assert_eq!(stdout.matches("\nmodel calls: ").count(), 1);
    assert_eq!(stdout.matches("\ntools:").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdin_prompts_share_conversation_history() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"first question","role":"user""#,
        ))
        .respond_with(response("first answer", 4, 2))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"{\"message\":\"first answer\"}","role":"assistant""#,
        ))
        .and(body_string_contains(
            r#""content":"second question","role":"user""#,
        ))
        .respond_with(response("second answer", 10, 2))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let mut child = Command::new(cargo_bin!("ag-harness"))
        .arg("--git-executable")
        .arg(test_git_executable())
        .args([
            "run",
            "muse-test",
            "first question",
            "--base-url",
            &server.uri(),
        ])
        .env("MODEL_API_KEY", "test-key")
        .env("AG_HARNESS_ROOT", storage.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI chat should start");

    // Act
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(b"second question\n")
        .expect("second prompt should be written");
    let output = child.wait_with_output().expect("CLI chat should finish");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("assistant> first answer\n---\n"));
    assert!(stdout.contains("assistant> second answer\n---\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_restores_history_from_the_default_database() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"first question","role":"user""#,
        ))
        .respond_with(response("first answer", 4, 2))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"{\"message\":\"first answer\"}","role":"assistant""#,
        ))
        .and(body_string_contains(
            r#""content":"second question","role":"user""#,
        ))
        .respond_with(response("second answer", 8, 2))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let mut first = Command::new(cargo_bin!("ag-harness"));
    first
        .arg("--git-executable")
        .arg(test_git_executable())
        .args([
            "run",
            "muse-test",
            "first question",
            "--session",
            "cli-resume",
            "--base-url",
            &server.uri(),
        ])
        .env("MODEL_API_KEY", "test-key")
        .env("AG_HARNESS_ROOT", storage.path());

    // Act
    let first_output = first.output().expect("first CLI request should run");
    let mut second = Command::new(cargo_bin!("ag-harness"));
    let second_output = second
        .arg("--git-executable")
        .arg(test_git_executable())
        .args([
            "resume",
            "cli-resume",
            "second question",
            "--base-url",
            &server.uri(),
        ])
        .env("MODEL_API_KEY", "test-key")
        .env("AG_HARNESS_ROOT", storage.path())
        .output()
        .expect("resumed CLI request should run");

    // Assert
    assert!(first_output.status.success());
    assert!(
        second_output.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&second_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second_output.stdout).contains("assistant> second answer\n---\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdin_chat_emits_failure_before_retry_and_exits_unsuccessfully() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(
            r#""content":"first question","role":"user""#,
        ))
        .respond_with(ResponseTemplate::new(500).set_body_string("temporary failure"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "messages": [
                {"content": READ_ONLY_SYSTEM_PROMPT, "role": "system"},
                {"content": "retry question", "role": "user"}
            ],
            "model": "muse-test",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "ag_harness_output",
                    "schema": chat_schema()
                }
            },
            "tools": [read_tool()]
        })))
        .respond_with(response("recovered answer", 5, 2))
        .expect(1)
        .mount(&server)
        .await;
    let storage = tempfile::tempdir().expect("temporary storage should exist");
    let mut child = Command::new(cargo_bin!("ag-harness"))
        .arg("--git-executable")
        .arg(test_git_executable())
        .args(["run", "muse-test", "--base-url", &server.uri()])
        .env("MODEL_API_KEY", "test-key")
        .env("AG_HARNESS_ROOT", storage.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI chat should start");

    // Act
    let mut stdin = child.stdin.take().expect("stdin should be piped");
    let stdout = child.stdout.take().expect("stdout should be piped");
    let mut stdout = BufReader::new(stdout);
    stdin
        .write_all(b"first question\n")
        .expect("first prompt should be written");
    stdin.flush().expect("first prompt should be flushed");
    let mut announcement = String::new();
    stdout
        .read_line(&mut announcement)
        .expect("session identifier should be emitted");
    let mut failure = String::new();
    stdout
        .read_line(&mut failure)
        .expect("failed turn should be emitted while stdin remains open");
    stdin
        .write_all(b"retry question\n")
        .expect("retry prompt should be written");
    drop(stdin);
    let mut recovered = String::new();
    stdout
        .read_to_string(&mut recovered)
        .expect("retry output should be readable");
    let status = child.wait().expect("CLI chat should finish");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr should be piped")
        .read_to_string(&mut stderr)
        .expect("stderr should be readable");

    // Assert
    assert!(announcement.starts_with("session: "));
    assert!(announcement.ends_with('\n'));
    assert!(failure.contains("error: model request failed:"));
    assert!(recovered.contains("assistant> recovered answer\n---\n"));
    assert!(!status.success());
    assert_eq!(stderr, "one or more chat turns failed\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_tool_reports_the_file_without_printing_its_contents() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "messages": [
                {"content": READ_ONLY_SYSTEM_PROMPT, "role": "system"},
                {"content": "Inspect the manifest", "role": "user"}
            ],
            "model": "muse-test",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "ag_harness_output",
                    "schema": chat_schema()
                }
            },
            "tools": [read_tool()]
        })))
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
                                "arguments": r#"{"path":"Cargo.toml","limit":2}"#
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
        .respond_with(response("It is a Rust workspace.", 20, 5))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command
        .args([
            "run",
            "muse-test",
            "Inspect the manifest",
            "--base-url",
            &server.uri(),
        ])
        .env("MODEL_API_KEY", "test-key")
        .output()
        .expect("CLI tool round trip should run");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("model calls: 2\n"));
    assert!(stdout.contains("tools:\n  read Cargo.toml (lines 1-2, truncated;"));
    assert!(!stdout.contains("[workspace]"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_write_enables_the_tool_and_creates_an_empty_file() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(READ_WRITE_SYSTEM_PROMPT))
        .and(body_string_contains(r#""name":"read""#))
        .and(body_string_contains(r#""name":"write""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call-write",
                        "type": "function",
                        "function": {
                            "name": "write",
                            "arguments": serde_json::json!({
                                "path": "t.py",
                                "patch": "--- /dev/null\n+++ b/t.py\n"
                            }).to_string()
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
        .and(body_string_contains(r#""tool_call_id":"call-write""#))
        .respond_with(response("Created t.py.", 20, 5))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let repository = tempfile::TempDir::new().expect("temporary repository should exist");
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command
        .args([
            "run",
            "muse-test",
            "Create an empty t.py file",
            "--base-url",
            &server.uri(),
            "--read-dir",
            &repository.path().to_string_lossy(),
            "--allow-write",
        ])
        .env("MODEL_API_KEY", "test-key")
        .output()
        .expect("CLI write-tool round trip should run");

    // Assert
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(repository.path().join("t.py")).expect("empty file should be created"),
        [] as [u8; 0]
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.contains("assistant> Created t.py.\n---\n"));
    assert!(stdout.contains("tools:\n  write t.py (0 bytes;"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirected_output_with_terminal_stdin_is_one_shot() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "messages": [
                {"content": READ_ONLY_SYSTEM_PROMPT, "role": "system"},
                {"content": "Hello", "role": "user"}
            ],
            "model": "muse-test",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "ag_harness_output",
                    "schema": chat_schema()
                }
            },
            "tools": [read_tool()]
        })))
        .respond_with(response("one-shot answer", 4, 2))
        .expect(1)
        .mount(&server)
        .await;
    let temp_dir = tempfile::TempDir::new().expect("temporary directory should be created");
    let output_path = temp_dir.path().join("stdout.txt");
    let command = r#"exec "$AG_HARNESS_BIN" --git-executable "$AG_HARNESS_GIT" run muse-test Hello --base-url "$MODEL_BASE_URL" > "$MODEL_OUTPUT_PATH""#;
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args(["-c", command])
        .env(
            "AG_HARNESS_BIN",
            cargo_bin!("ag-harness").to_string_lossy().into_owned(),
        )
        .env(
            "AG_HARNESS_GIT",
            test_git_executable().to_string_lossy().into_owned(),
        )
        .env("MODEL_API_KEY", "test-key")
        .env("MODEL_BASE_URL", server.uri())
        .env(
            "AG_HARNESS_ROOT",
            temp_dir
                .path()
                .join("harness-root")
                .to_string_lossy()
                .into_owned(),
        )
        .env(
            "MODEL_OUTPUT_PATH",
            output_path.to_string_lossy().into_owned(),
        )
        .spawn()
        .expect("PTY command should start");

    // Act
    let succeeded = session
        .wait_for_exit(Duration::from_secs(10))
        .expect("one-shot command should exit without terminal EOF");

    // Assert
    assert!(succeeded);
    let stdout = fs::read_to_string(output_path).expect("redirected output should be readable");
    assert!(stdout.starts_with("session: "));
    assert!(stdout.contains("assistant> one-shot answer\n---\n"));
    assert!(!stdout.contains("Chat with"));
    assert!(!stdout.contains(">>>"));
}

#[cfg(unix)]
#[test]
fn redirected_output_with_blank_prompt_exits_with_an_error() {
    // Arrange
    let temp_dir = tempfile::TempDir::new().expect("temporary directory should be created");
    let output_path = temp_dir.path().join("stdout.txt");
    let error_path = temp_dir.path().join("stderr.txt");
    let command = r#"exec "$AG_HARNESS_BIN" run muse-test "   " > "$MODEL_OUTPUT_PATH" 2> "$MODEL_ERROR_PATH""#;
    let mut session = PtySessionBuilder::new("/bin/sh")
        .args(["-c", command])
        .env(
            "AG_HARNESS_BIN",
            cargo_bin!("ag-harness").to_string_lossy().into_owned(),
        )
        .env(
            "MODEL_OUTPUT_PATH",
            output_path.to_string_lossy().into_owned(),
        )
        .env(
            "MODEL_ERROR_PATH",
            error_path.to_string_lossy().into_owned(),
        )
        .spawn()
        .expect("PTY command should start");

    // Act
    let succeeded = session
        .wait_for_exit(Duration::from_secs(5))
        .expect("blank one-shot prompt should exit without terminal input");

    // Assert
    assert!(!succeeded);
    assert_eq!(
        fs::read(output_path).expect("redirected output should be readable"),
        [] as [u8; 0]
    );
    let stderr = fs::read_to_string(error_path).expect("redirected error should be readable");
    assert!(stderr.contains("prompt must contain a non-whitespace character"));
}

#[test]
fn missing_api_key_fails_without_model_output() {
    // Arrange
    let (_storage, mut command) = harness_command().expect("temporary storage should exist");

    // Act
    let output = command
        .args(["run", "muse-test"])
        .env_remove("MODEL_API_KEY")
        .output()
        .expect("CLI failure should run");

    // Assert
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(output.stderr).expect("error should be UTF-8"),
        "MODEL_API_KEY is unavailable\n"
    );
}
