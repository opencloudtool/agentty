//! Shared stdio JSON-RPC transport utilities for app-server protocols.
//!
//! Provides low-level helpers for NDJSON-over-stdio communication used by
//! persistent app-server backends (e.g., Codex app-server, Gemini ACP).
//! Each helper is protocol-agnostic — it operates on raw JSON values and
//! async stdio handles without knowledge of specific method names or event
//! shapes.

use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader, Lines};

/// Typed error returned by shared app-server transport operations.
///
/// Covers the low-level stdio communication failures that can occur when
/// writing JSON-RPC payloads to a child process or reading responses from
/// its stdout stream.
#[derive(Debug, thiserror::Error)]
pub enum AppServerTransportError {
    /// An IO error occurred during app-server stdio communication.
    #[error("{context}: {source}")]
    Io {
        /// Human-readable description of the operation that failed.
        context: String,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// The app-server process terminated before sending the expected response.
    #[error("App-server terminated before sending expected response")]
    ProcessTerminated,

    /// Timed out waiting for a JSON-RPC response from the app-server.
    #[error(
        "Timed out waiting for app-server response `{response_id}` after {timeout_seconds} seconds"
    )]
    Timeout {
        /// The JSON-RPC request identifier that was being awaited.
        response_id: String,
        /// Number of seconds elapsed before the timeout fired.
        timeout_seconds: u64,
    },
}

/// Default timeout for initialization handshakes and session creation.
///
/// Gemini ACP cold starts can take materially longer than a typical
/// request/response round trip while the runtime initializes tools and model
/// state, so the shared startup window stays measured in minutes rather than
/// seconds to avoid aborting healthy app-server bootstraps.
pub const STARTUP_TIMEOUT: Duration = Duration::from_mins(5);

/// Default timeout for a single prompt turn.
///
/// App-server turns may legitimately run for long periods while agents plan,
/// execute tools, and compact context, so the shared turn window is aligned
/// with the long-running Codex behavior instead of the shorter bootstrap
/// timeout.
pub const TURN_TIMEOUT: Duration = Duration::from_hours(4);

/// Writes one JSON-RPC payload as a newline-delimited line to `stdin`.
///
/// # Errors
///
/// Returns an error when the write or flush to stdin fails.
pub async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    payload: &Value,
) -> Result<(), AppServerTransportError> {
    let serialized_payload = payload.to_string();

    stdin
        .write_all(serialized_payload.as_bytes())
        .await
        .map_err(|source| AppServerTransportError::Io {
            context: "Failed writing to app-server stdin".to_string(),
            source,
        })?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|source| AppServerTransportError::Io {
            context: "Failed writing newline to app-server stdin".to_string(),
            source,
        })?;
    stdin
        .flush()
        .await
        .map_err(|source| AppServerTransportError::Io {
            context: "Failed flushing app-server stdin".to_string(),
            source,
        })
}

/// Reads stdout lines until a JSON-RPC response carrying `response_id` arrives.
///
/// Non-matching lines (notifications, other responses) are silently skipped.
/// Times out after [`STARTUP_TIMEOUT`].
///
/// # Errors
///
/// Returns an error when the read times out or the child process terminates
/// before a matching response is received.
pub async fn wait_for_response_line<R>(
    stdout_lines: &mut Lines<BufReader<R>>,
    response_id: &str,
) -> Result<String, AppServerTransportError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(STARTUP_TIMEOUT, async {
        loop {
            let stdout_line = stdout_lines
                .next_line()
                .await
                .map_err(|source| AppServerTransportError::Io {
                    context: "Failed reading app-server stdout".to_string(),
                    source,
                })?
                .ok_or(AppServerTransportError::ProcessTerminated)?;

            let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                continue;
            };
            if response_id_matches(&response_value, response_id) {
                return Ok(stdout_line);
            }
        }
    })
    .await
    .map_err(|_| AppServerTransportError::Timeout {
        response_id: response_id.to_string(),
        timeout_seconds: STARTUP_TIMEOUT.as_secs(),
    })?
}

/// Returns whether a JSON-RPC response line carries the expected `id`.
pub fn response_id_matches(response_value: &Value, response_id: &str) -> bool {
    response_value
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|line_id| line_id == response_id)
}

/// Extracts a top-level `error.message` string from a JSON-RPC error response.
pub fn extract_json_error_message(response_value: &Value) -> Option<String> {
    response_value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Gracefully shuts down a child process by closing stdin, waiting briefly,
/// then killing if the process has not exited.
pub async fn shutdown_child(child: &mut tokio::process::Child) {
    // Closing stdin signals the child to exit cleanly.
    drop(child.stdin.take());

    if tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .is_err()
    {
        // Best-effort: process may have already exited.
        let _ = child.kill().await;
        // Best-effort: process may have already exited.
        let _ = child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use tokio::io::AsyncBufReadExt;

    use super::*;

    /// Spawns a simple echo process that mirrors stdin to stdout for transport
    /// write tests.
    fn spawn_cat_process() -> tokio::process::Child {
        let mut command = tokio::process::Command::new("cat");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        command.spawn().expect("failed to spawn `cat`")
    }

    #[test]
    fn response_id_matches_returns_true_for_matching_string_id() {
        // Arrange
        let response_value = serde_json::json!({"id": "init-123", "result": {}});

        // Act / Assert
        assert!(response_id_matches(&response_value, "init-123"));
    }

    #[test]
    fn response_id_matches_returns_false_for_different_id() {
        // Arrange
        let response_value = serde_json::json!({"id": "init-123", "result": {}});

        // Act / Assert
        assert!(!response_id_matches(&response_value, "init-456"));
    }

    #[test]
    fn response_id_matches_returns_false_when_id_is_missing() {
        // Arrange
        let response_value = serde_json::json!({"method": "session/update", "params": {}});

        // Act / Assert
        assert!(!response_id_matches(&response_value, "init-123"));
    }

    #[test]
    fn response_id_matches_returns_false_for_integer_id() {
        // Arrange
        let response_value = serde_json::json!({"id": 1, "result": {}});

        // Act / Assert
        assert!(!response_id_matches(&response_value, "1"));
    }

    #[test]
    fn extract_json_error_message_returns_message_string() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "req-1",
            "error": {"code": -32600, "message": "Invalid request"}
        });

        // Act
        let message = extract_json_error_message(&response_value);

        // Assert
        assert_eq!(message, Some("Invalid request".to_string()));
    }

    #[test]
    fn extract_json_error_message_returns_none_without_error() {
        // Arrange
        let response_value = serde_json::json!({"id": "req-1", "result": {}});

        // Act
        let message = extract_json_error_message(&response_value);

        // Assert
        assert_eq!(message, None);
    }

    #[test]
    fn extract_json_error_message_returns_none_without_message_field() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "req-1",
            "error": {"code": -32600}
        });

        // Act
        let message = extract_json_error_message(&response_value);

        // Assert
        assert_eq!(message, None);
    }

    /// Verifies `write_json_line()` serializes one compact JSON line followed
    /// by a newline.
    #[tokio::test]
    async fn write_json_line_writes_serialized_payload_with_newline() {
        // Arrange
        let mut child = spawn_cat_process();
        let mut stdin = child.stdin.take().expect("`cat` stdin should be piped");
        let stdout = child.stdout.take().expect("`cat` stdout should be piped");
        let payload = serde_json::json!({
            "id": "req-1",
            "method": "initialize",
            "params": {"value": 1}
        });

        // Act
        write_json_line(&mut stdin, &payload)
            .await
            .expect("write should succeed");
        drop(stdin);
        let echoed_line = BufReader::new(stdout)
            .lines()
            .next_line()
            .await
            .expect("stdout read should succeed")
            .expect("echoed payload line should exist");

        // Assert
        assert_eq!(echoed_line, payload.to_string());
        shutdown_child(&mut child).await;
    }

    /// Verifies `wait_for_response_line()` skips unrelated or invalid lines
    /// until the matching response id arrives.
    #[tokio::test]
    async fn wait_for_response_line_skips_invalid_and_non_matching_lines() {
        // Arrange
        let (reader, mut writer) = tokio::io::duplex(512);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(
                    b"not-json\n{\"id\":\"other\",\"result\":{}}\n{\"id\":\"req-1\",\"result\":{\"ok\":true}}\n",
                )
                .await
                .expect("test writer should succeed");
        });
        let mut stdout_lines = BufReader::new(reader).lines();

        // Act
        let response_line = wait_for_response_line(&mut stdout_lines, "req-1")
            .await
            .expect("matching response should be returned");

        // Assert
        assert_eq!(response_line, "{\"id\":\"req-1\",\"result\":{\"ok\":true}}");
        writer_task.await.expect("writer task should finish");
    }

    /// Verifies `wait_for_response_line()` reports early process termination
    /// when the stream ends before the expected response arrives.
    #[tokio::test]
    async fn wait_for_response_line_returns_error_when_stream_ends() {
        // Arrange
        let (reader, mut writer) = tokio::io::duplex(256);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(b"{\"id\":\"other\",\"result\":{}}\n")
                .await
                .expect("test writer should succeed");
            drop(writer);
        });
        let mut stdout_lines = BufReader::new(reader).lines();

        // Act
        let response_result = wait_for_response_line(&mut stdout_lines, "req-1").await;

        // Assert
        assert!(
            matches!(
                response_result,
                Err(AppServerTransportError::ProcessTerminated)
            ),
            "expected ProcessTerminated, got: {response_result:?}"
        );
        writer_task.await.expect("writer task should finish");
    }

    #[test]
    fn io_error_display_includes_context_and_source() {
        // Arrange
        let error = AppServerTransportError::Io {
            context: "Failed writing to app-server stdin".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed"),
        };

        // Act
        let display = error.to_string();

        // Assert
        assert_eq!(display, "Failed writing to app-server stdin: pipe closed");
    }

    #[test]
    fn process_terminated_display_message() {
        // Arrange
        let error = AppServerTransportError::ProcessTerminated;

        // Act / Assert
        assert_eq!(
            error.to_string(),
            "App-server terminated before sending expected response"
        );
    }

    #[test]
    fn timeout_display_includes_response_id_and_seconds() {
        // Arrange
        let error = AppServerTransportError::Timeout {
            response_id: "init-123".to_string(),
            timeout_seconds: 300,
        };

        // Act / Assert
        assert_eq!(
            error.to_string(),
            "Timed out waiting for app-server response `init-123` after 300 seconds"
        );
    }

    /// Verifies `shutdown_child()` closes stdin and waits for a cooperative
    /// child process to exit.
    #[tokio::test]
    async fn shutdown_child_exits_cleanly_after_closing_stdin() {
        // Arrange
        let mut child = spawn_cat_process();

        // Act
        shutdown_child(&mut child).await;
        let exit_status = child.wait().await.expect("child wait should succeed");

        // Assert
        assert!(exit_status.success());
    }
}
