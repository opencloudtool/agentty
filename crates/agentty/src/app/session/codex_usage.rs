//! Codex account usage-limit loading through the `codex app-server` protocol.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, Lines};

use crate::app::SessionManager;
use crate::domain::session::{CodexUsageLimitWindow, CodexUsageLimits};

const RATE_LIMITS_REQUEST_ID: &str = "rate-limits-read";
const INITIALIZE_REQUEST_ID: &str = "init";
const CODEX_APP_SERVER_TIMEOUT: Duration = Duration::from_secs(2);

/// App-server response envelope used for parsing line-delimited JSON.
#[derive(Deserialize)]
struct CodexAppServerEnvelope {
    id: Option<String>,
    result: Option<CodexRateLimitsReadResult>,
}

/// Payload returned by `account/rateLimits/read`.
#[derive(Deserialize)]
struct CodexRateLimitsReadResult {
    #[serde(rename = "rateLimits", alias = "rate_limits")]
    rate_limits: Option<CodexRateLimitSnapshot>,
    #[serde(rename = "rateLimitsByLimitId", alias = "rate_limits_by_limit_id")]
    rate_limits_by_limit_id: Option<HashMap<String, CodexRateLimitSnapshot>>,
}

/// Account-level Codex rate-limit snapshot payload.
#[derive(Deserialize)]
struct CodexRateLimitSnapshot {
    #[serde(rename = "limitId", alias = "limit_id")]
    limit_id: Option<String>,
    primary: Option<CodexRateLimitWindowPayload>,
    secondary: Option<CodexRateLimitWindowPayload>,
}

/// One window payload in the app-server rate-limit response.
#[derive(Deserialize)]
struct CodexRateLimitWindowPayload {
    #[serde(rename = "resetsAt", alias = "resets_at")]
    resets_at: Option<CodexNumericField>,
    #[serde(rename = "usedPercent", alias = "used_percent")]
    used_percent: Option<CodexNumericField>,
    #[serde(rename = "windowDurationMins", alias = "window_minutes")]
    window_duration_mins: Option<CodexNumericField>,
}

/// Flexible numeric field that accepts integer, float, or numeric string
/// values from Codex payloads.
#[derive(Deserialize)]
#[serde(untagged)]
enum CodexNumericField {
    Float(f64),
    Integer(i64),
    String(String),
}

impl SessionManager {
    /// Loads account-level Codex usage limits via `codex app-server`.
    pub(crate) async fn load_codex_usage_limits() -> Option<CodexUsageLimits> {
        let mut child = tokio::process::Command::new("codex")
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stdout_payload_lines = Vec::new();

        if write_codex_app_server_stdin(
            &mut stdin,
            r#"{"method":"initialize","id":"init","params":{"clientInfo":{"name":"agentty","title":"agentty","version":"0.0.0"},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":null}}}"#,
        )
        .await
        .is_err()
        {
            return None;
        }

        wait_for_codex_app_server_response(
            &mut stdout_lines,
            &mut stdout_payload_lines,
            INITIALIZE_REQUEST_ID,
        )
        .await?;

        if write_codex_app_server_stdin(&mut stdin, r#"{"method":"initialized"}"#)
            .await
            .is_err()
        {
            return None;
        }

        if write_codex_app_server_stdin(
            &mut stdin,
            r#"{"method":"account/rateLimits/read","id":"rate-limits-read","params":{}}"#,
        )
        .await
        .is_err()
        {
            return None;
        }

        wait_for_codex_app_server_response(
            &mut stdout_lines,
            &mut stdout_payload_lines,
            RATE_LIMITS_REQUEST_ID,
        )
        .await?;

        drop(stdin);

        if tokio::time::timeout(CODEX_APP_SERVER_TIMEOUT, child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        let stdout = stdout_payload_lines.join("\n");

        parse_codex_usage_limits_response(&stdout)
    }
}

/// Writes one JSON-RPC line to the Codex app-server `stdin` stream.
async fn write_codex_app_server_stdin(
    stdin: &mut tokio::process::ChildStdin,
    payload: &str,
) -> std::io::Result<()> {
    stdin.write_all(payload.as_bytes()).await?;
    stdin.write_all(b"\n").await?;

    stdin.flush().await
}

/// Reads app-server stdout lines until a response matching the requested id.
///
/// Every line is appended to `stdout_payload_lines` for downstream parsing.
async fn wait_for_codex_app_server_response<R>(
    stdout_lines: &mut Lines<BufReader<R>>,
    stdout_payload_lines: &mut Vec<String>,
    response_id: &str,
) -> Option<()>
where
    R: AsyncRead + Unpin,
{
    tokio::time::timeout(CODEX_APP_SERVER_TIMEOUT, async {
        loop {
            let next_stdout_line = stdout_lines.next_line().await.ok()?;
            let stdout_line = next_stdout_line?;
            let matches_response = codex_app_server_response_matches_id(&stdout_line, response_id);

            stdout_payload_lines.push(stdout_line);

            if matches_response {
                return Some(());
            }
        }
    })
    .await
    .ok()?
}

/// Returns whether one app-server JSON line matches the provided response id.
fn codex_app_server_response_matches_id(stdout_line: &str, response_id: &str) -> bool {
    let Ok(envelope) = serde_json::from_str::<CodexAppServerEnvelope>(stdout_line) else {
        return false;
    };

    envelope.id.as_deref() == Some(response_id)
}

/// Parses Codex app-server stdout and extracts account usage limits.
fn parse_codex_usage_limits_response(stdout: &str) -> Option<CodexUsageLimits> {
    for line in stdout.lines() {
        let Ok(envelope) = serde_json::from_str::<CodexAppServerEnvelope>(line) else {
            continue;
        };
        if envelope.id.as_deref() != Some(RATE_LIMITS_REQUEST_ID) {
            continue;
        }

        let CodexRateLimitsReadResult {
            rate_limits,
            rate_limits_by_limit_id,
        } = envelope.result?;
        let snapshot = codex_rate_limit_snapshot(rate_limits, rate_limits_by_limit_id)?;
        let primary = parse_codex_limit_window(snapshot.primary);
        let secondary = parse_codex_limit_window(snapshot.secondary);
        if primary.is_none() && secondary.is_none() {
            continue;
        }

        return Some(CodexUsageLimits { primary, secondary });
    }

    None
}

/// Picks the Codex bucket from a rate-limit response payload.
fn codex_rate_limit_snapshot(
    rate_limits: Option<CodexRateLimitSnapshot>,
    rate_limits_by_limit_id: Option<HashMap<String, CodexRateLimitSnapshot>>,
) -> Option<CodexRateLimitSnapshot> {
    let codex_snapshot = rate_limits_by_limit_id.and_then(|limits| {
        for (limit_key, snapshot) in limits {
            let key_contains_codex = limit_key.to_ascii_lowercase().contains("codex");
            let id_contains_codex = snapshot
                .limit_id
                .as_deref()
                .is_some_and(|limit_id| limit_id.to_ascii_lowercase().contains("codex"));

            if key_contains_codex || id_contains_codex {
                return Some(snapshot);
            }
        }

        None
    });
    if codex_snapshot.is_some() {
        return codex_snapshot;
    }

    rate_limits
}

/// Converts one app-server window payload to the domain usage window type.
fn parse_codex_limit_window(
    window: Option<CodexRateLimitWindowPayload>,
) -> Option<CodexUsageLimitWindow> {
    let window = window?;
    let used_percent_value =
        parse_codex_numeric_field_i64(window.used_percent.as_ref())?.clamp(0, 100);
    let used_percent = u8::try_from(used_percent_value).ok()?;
    let resets_at = parse_codex_numeric_field_i64(window.resets_at.as_ref());
    let window_minutes = window
        .window_duration_mins
        .as_ref()
        .and_then(|window_minutes_value| parse_codex_numeric_field_i64(Some(window_minutes_value)))
        .and_then(|window_minutes_value| u32::try_from(window_minutes_value).ok())
        .filter(|minutes| *minutes > 0);

    Some(CodexUsageLimitWindow {
        resets_at,
        used_percent,
        window_minutes,
    })
}

/// Converts one Codex numeric payload field to `i64`.
fn parse_codex_numeric_field_i64(field: Option<&CodexNumericField>) -> Option<i64> {
    let field = field?;

    match field {
        CodexNumericField::Float(value) => parse_f64_as_i64(*value),
        CodexNumericField::Integer(value) => Some(*value),
        CodexNumericField::String(value) => parse_numeric_string_as_i64(value),
    }
}

/// Parses one numeric string into `i64`, supporting both integer and float
/// formats.
fn parse_numeric_string_as_i64(value: &str) -> Option<i64> {
    if let Ok(integer_value) = value.parse::<i64>() {
        return Some(integer_value);
    }

    let float_value = value.parse::<f64>().ok()?;

    parse_f64_as_i64(float_value)
}

/// Converts one finite `f64` to the nearest `i64` when in range.
fn parse_f64_as_i64(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }

    serde_json::Number::from_f64(value.round()).and_then(|rounded_value| rounded_value.as_i64())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tokio::io::BufReader;

    use super::*;

    #[test]
    fn test_parse_codex_usage_limits_response_extracts_primary_and_secondary_windows() {
        // Arrange
        let stdout = [
            r#"{"id":"init","result":{"userAgent":"agentty/0.0.0"}}"#,
            r#"{"id":"rate-limits-read","result":{"rateLimits":{"primary":{"usedPercent":30,"windowDurationMins":300,"resetsAt":1},"secondary":{"usedPercent":26,"windowDurationMins":10080,"resetsAt":2}}}}"#,
        ]
        .join("\n");

        // Act
        let limits = parse_codex_usage_limits_response(&stdout);

        // Assert
        assert_eq!(
            limits,
            Some(CodexUsageLimits {
                primary: Some(CodexUsageLimitWindow {
                    resets_at: Some(1),
                    used_percent: 30,
                    window_minutes: Some(300),
                }),
                secondary: Some(CodexUsageLimitWindow {
                    resets_at: Some(2),
                    used_percent: 26,
                    window_minutes: Some(10_080),
                }),
            })
        );
    }

    #[test]
    fn test_parse_codex_usage_limits_response_keeps_primary_when_secondary_missing() {
        // Arrange
        let stdout = r#"{"id":"rate-limits-read","result":{"rateLimits":{"primary":{"usedPercent":30,"windowDurationMins":300,"resetsAt":1}}}}"#;

        // Act
        let limits = parse_codex_usage_limits_response(stdout);

        // Assert
        assert_eq!(
            limits,
            Some(CodexUsageLimits {
                primary: Some(CodexUsageLimitWindow {
                    resets_at: Some(1),
                    used_percent: 30,
                    window_minutes: Some(300),
                }),
                secondary: None,
            })
        );
    }

    #[test]
    fn test_parse_codex_usage_limits_response_uses_codex_bucket_from_multi_limit_payload() {
        // Arrange
        let stdout = r#"{"id":"rate-limits-read","result":{"rateLimits":{"primary":{"usedPercent":40,"windowDurationMins":300,"resetsAt":1}},"rateLimitsByLimitId":{"chatgpt":{"limitId":"chatgpt","primary":{"usedPercent":10,"windowDurationMins":60,"resetsAt":1}},"codex":{"limitId":"codex","primary":{"usedPercent":55,"windowDurationMins":300,"resetsAt":8}}}}}"#;

        // Act
        let limits = parse_codex_usage_limits_response(stdout);

        // Assert
        assert_eq!(
            limits,
            Some(CodexUsageLimits {
                primary: Some(CodexUsageLimitWindow {
                    resets_at: Some(8),
                    used_percent: 55,
                    window_minutes: Some(300),
                }),
                secondary: None,
            })
        );
    }

    #[test]
    fn test_parse_codex_usage_limits_response_allows_nullable_window_fields() {
        // Arrange
        let stdout = r#"{"id":"rate-limits-read","result":{"rateLimits":{"primary":{"usedPercent":30,"windowDurationMins":null,"resetsAt":null}}}}"#;

        // Act
        let limits = parse_codex_usage_limits_response(stdout);

        // Assert
        assert_eq!(
            limits,
            Some(CodexUsageLimits {
                primary: Some(CodexUsageLimitWindow {
                    resets_at: None,
                    used_percent: 30,
                    window_minutes: None,
                }),
                secondary: None,
            })
        );
    }

    #[test]
    fn test_parse_codex_usage_limits_response_accepts_snake_case_float_payloads() {
        // Arrange
        let stdout = r#"{"id":"rate-limits-read","result":{"rate_limits":{"primary":{"used_percent":30.0,"window_minutes":300.0,"resets_at":1.0},"secondary":{"used_percent":"26.0","window_minutes":"10080","resets_at":"2"}}}}"#;

        // Act
        let limits = parse_codex_usage_limits_response(stdout);

        // Assert
        assert_eq!(
            limits,
            Some(CodexUsageLimits {
                primary: Some(CodexUsageLimitWindow {
                    resets_at: Some(1),
                    used_percent: 30,
                    window_minutes: Some(300),
                }),
                secondary: Some(CodexUsageLimitWindow {
                    resets_at: Some(2),
                    used_percent: 26,
                    window_minutes: Some(10_080),
                }),
            })
        );
    }

    #[test]
    fn test_parse_codex_usage_limits_response_matches_codex_like_limit_ids() {
        // Arrange
        let stdout = r#"{"id":"rate-limits-read","result":{"rateLimitsByLimitId":{"chatgpt":{"limitId":"chatgpt","primary":{"usedPercent":10,"windowDurationMins":60,"resetsAt":1}},"codex_pro":{"limitId":"codex-pro","primary":{"usedPercent":55,"windowDurationMins":300,"resetsAt":8}}}}}"#;

        // Act
        let limits = parse_codex_usage_limits_response(stdout);

        // Assert
        assert_eq!(
            limits,
            Some(CodexUsageLimits {
                primary: Some(CodexUsageLimitWindow {
                    resets_at: Some(8),
                    used_percent: 55,
                    window_minutes: Some(300),
                }),
                secondary: None,
            })
        );
    }

    #[test]
    fn test_codex_app_server_response_matches_id_returns_true_for_matching_json_line() {
        // Arrange
        let stdout_line =
            r#"{"id":"rate-limits-read","result":{"rateLimits":{"primary":{"usedPercent":30}}}}"#;

        // Act
        let matches_response =
            codex_app_server_response_matches_id(stdout_line, RATE_LIMITS_REQUEST_ID);

        // Assert
        assert!(matches_response);
    }

    #[test]
    fn test_codex_app_server_response_matches_id_returns_false_for_mismatched_json_line() {
        // Arrange
        let stdout_line = r#"{"id":"init","result":{"userAgent":"agentty/0.0.0"}}"#;

        // Act
        let matches_response =
            codex_app_server_response_matches_id(stdout_line, RATE_LIMITS_REQUEST_ID);

        // Assert
        assert!(!matches_response);
    }

    #[tokio::test]
    async fn test_wait_for_codex_app_server_response_returns_some_when_id_is_seen() {
        // Arrange
        let stdout = [
            r#"{"id":"init","result":{"userAgent":"agentty/0.0.0"}}"#,
            r#"{"id":"rate-limits-read","result":{"rateLimits":{"primary":{"usedPercent":30}}}}"#,
        ]
        .join("\n");
        let reader = BufReader::new(Cursor::new(stdout));
        let mut stdout_lines = reader.lines();
        let mut stdout_payload_lines = Vec::new();

        // Act
        let result = wait_for_codex_app_server_response(
            &mut stdout_lines,
            &mut stdout_payload_lines,
            RATE_LIMITS_REQUEST_ID,
        )
        .await;

        // Assert
        assert_eq!(result, Some(()));
        assert_eq!(stdout_payload_lines.len(), 2);
    }

    #[tokio::test]
    async fn test_wait_for_codex_app_server_response_returns_none_when_id_is_missing() {
        // Arrange
        let stdout = r#"{"id":"init","result":{"userAgent":"agentty/0.0.0"}}"#;
        let reader = BufReader::new(Cursor::new(stdout));
        let mut stdout_lines = reader.lines();
        let mut stdout_payload_lines = Vec::new();

        // Act
        let result = wait_for_codex_app_server_response(
            &mut stdout_lines,
            &mut stdout_payload_lines,
            RATE_LIMITS_REQUEST_ID,
        )
        .await;

        // Assert
        assert_eq!(result, None);
        assert_eq!(stdout_payload_lines.len(), 1);
    }
}
