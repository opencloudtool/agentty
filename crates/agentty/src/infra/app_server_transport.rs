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

/// Default timeout for initialization handshakes and session creation.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Default timeout for a single prompt turn.
pub const TURN_TIMEOUT: Duration = Duration::from_mins(5);

/// Writes one JSON-RPC payload as a newline-delimited line to `stdin`.
///
/// # Errors
///
/// Returns an error when the write or flush to stdin fails.
pub async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    payload: &Value,
) -> Result<(), String> {
    let serialized_payload = payload.to_string();

    stdin
        .write_all(serialized_payload.as_bytes())
        .await
        .map_err(|error| format!("Failed writing to app-server stdin: {error}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| format!("Failed writing newline to app-server stdin: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("Failed flushing app-server stdin: {error}"))
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
) -> Result<String, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(STARTUP_TIMEOUT, async {
        loop {
            let stdout_line = stdout_lines
                .next_line()
                .await
                .map_err(|error| format!("Failed reading app-server stdout: {error}"))?
                .ok_or_else(|| {
                    "App-server terminated before sending expected response".to_string()
                })?;

            let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                continue;
            };
            if response_id_matches(&response_value, response_id) {
                return Ok(stdout_line);
            }
        }
    })
    .await
    .map_err(|_| {
        format!(
            "Timed out waiting for app-server response `{response_id}` after {} seconds",
            STARTUP_TIMEOUT.as_secs()
        )
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
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
