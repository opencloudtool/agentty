//! Session loading and derived snapshot attributes from persisted rows.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::session_folder;
use crate::app::SessionManager;
use crate::app::settings::SettingName;
use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::project::Project;
use crate::domain::session::{
    AllTimeModelUsage, CodexUsageLimitWindow, CodexUsageLimits, DailyActivity, Session,
    SessionHandles, SessionSize, SessionStats, Status,
};
use crate::infra::db::Database;
use crate::infra::git::GitClient;

const RATE_LIMITS_REQUEST_ID: &str = "rate-limits-read";
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
    #[serde(rename = "rateLimits")]
    rate_limits: Option<CodexRateLimitSnapshot>,
    #[serde(rename = "rateLimitsByLimitId")]
    rate_limits_by_limit_id: Option<HashMap<String, CodexRateLimitSnapshot>>,
}

/// Account-level Codex rate-limit snapshot payload.
#[derive(Deserialize)]
struct CodexRateLimitSnapshot {
    #[serde(rename = "limitId")]
    limit_id: Option<String>,
    primary: Option<CodexRateLimitWindowPayload>,
    secondary: Option<CodexRateLimitWindowPayload>,
}

/// One window payload in the app-server rate-limit response.
#[derive(Deserialize)]
struct CodexRateLimitWindowPayload {
    #[serde(rename = "resetsAt")]
    resets_at: Option<i64>,
    #[serde(rename = "usedPercent")]
    used_percent: Option<i64>,
    #[serde(rename = "windowDurationMins")]
    window_duration_mins: Option<i64>,
}

impl SessionManager {
    /// Loads session models from the database and reuses live handles when
    /// possible.
    ///
    /// Existing handles are updated in place to preserve `Arc` identity so
    /// that background workers holding cloned references continue to work.
    /// New handles are inserted for sessions that don't have entries yet.
    ///
    /// Returns both loaded sessions and aggregated daily activity counts.
    pub(crate) async fn load_sessions(
        base: &Path,
        db: &Database,
        projects: &[Project],
        handles: &mut HashMap<String, SessionHandles>,
        git_client: Arc<dyn GitClient>,
    ) -> (Vec<Session>, Vec<DailyActivity>) {
        const SECONDS_PER_DAY: i64 = 86_400;

        let project_names: HashMap<i64, String> = projects
            .iter()
            .filter_map(|project| {
                let name = project.path.file_name()?.to_string_lossy().to_string();
                Some((project.id, name))
            })
            .collect();

        let db_rows = db.load_sessions().await.unwrap_or_default();
        let mut activity_by_day: HashMap<i64, u32> = HashMap::new();
        let mut sessions: Vec<Session> = Vec::new();

        for row in db_rows {
            let folder = session_folder(base, &row.id);
            let status = row.status.parse::<Status>().unwrap_or(Status::Done);
            let persisted_size = row.size.parse::<SessionSize>().unwrap_or_default();
            let is_terminal_status = matches!(status, Status::Done | Status::Canceled);
            if !folder.is_dir() && !is_terminal_status {
                continue;
            }

            let session_model = row
                .model
                .parse::<AgentModel>()
                .unwrap_or_else(|_| AgentKind::Gemini.default_model());
            let project_name = row
                .project_id
                .and_then(|id| project_names.get(&id))
                .cloned()
                .unwrap_or_default();

            if let Some(existing) = handles.get(&row.id) {
                if let Ok(mut output_buffer) = existing.output.lock() {
                    output_buffer.clone_from(&row.output);
                }
                if let Ok(mut status_value) = existing.status.lock() {
                    *status_value = status;
                }
            } else {
                handles.insert(
                    row.id.clone(),
                    SessionHandles::new(row.output.clone(), status),
                );
            }

            let size = if is_terminal_status {
                persisted_size
            } else {
                let computed_size =
                    Self::session_size_for_folder(git_client.as_ref(), &folder, &row.base_branch)
                        .await;
                let _ = db
                    .update_session_size(&row.id, &computed_size.to_string())
                    .await;

                computed_size
            };

            let created_day_key = row.created_at.div_euclid(SECONDS_PER_DAY);
            let day_entry = activity_by_day.entry(created_day_key).or_insert(0);
            *day_entry = day_entry.saturating_add(1);

            sessions.push(Session {
                base_branch: row.base_branch,
                created_at: row.created_at,
                folder,
                id: row.id,
                model: session_model,
                output: row.output,
                permission_mode: row.permission_mode.parse().unwrap_or_default(),
                project_name,
                prompt: row.prompt,
                size,
                stats: SessionStats {
                    input_tokens: row.input_tokens.cast_unsigned(),
                    output_tokens: row.output_tokens.cast_unsigned(),
                },
                status,
                summary: row.summary,
                title: row.title,
                updated_at: row.updated_at,
            });
        }

        let mut stats_activity: Vec<DailyActivity> = activity_by_day
            .into_iter()
            .map(|(day_key, session_count)| DailyActivity {
                day_key,
                session_count,
            })
            .collect();
        stats_activity.sort_by_key(|activity| activity.day_key);

        (sessions, stats_activity)
    }

    /// Loads all-time model usage aggregates from persisted token usage rows.
    pub(crate) async fn load_all_time_model_usage(db: &Database) -> Vec<AllTimeModelUsage> {
        db.load_all_time_model_usage()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| AllTimeModelUsage {
                input_tokens: row.input_tokens.cast_unsigned(),
                model: row.model,
                output_tokens: row.output_tokens.cast_unsigned(),
                session_count: row.session_count.cast_unsigned(),
            })
            .collect()
    }

    /// Loads the persisted longest session duration in seconds.
    pub(crate) async fn load_longest_session_duration_seconds(db: &Database) -> u64 {
        db.get_setting(SettingName::LongestSessionDurationSeconds.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default()
    }

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
        let request_input = [
            r#"{"method":"initialize","id":"init","params":{"clientInfo":{"name":"agentty","title":"agentty","version":"0.0.0"},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":null}}}"#,
            r#"{"method":"initialized"}"#,
            r#"{"method":"account/rateLimits/read","id":"rate-limits-read","params":{}}"#,
        ]
        .join("\n");
        if stdin.write_all(request_input.as_bytes()).await.is_err() {
            return None;
        }

        drop(stdin);

        let output = tokio::time::timeout(CODEX_APP_SERVER_TIMEOUT, child.wait_with_output())
            .await
            .ok()?
            .ok()?;
        let stdout = String::from_utf8(output.stdout).ok()?;

        parse_codex_usage_limits_response(&stdout)
    }

    async fn session_size_for_folder(
        git_client: &dyn GitClient,
        folder: &Path,
        base_branch: &str,
    ) -> SessionSize {
        if !folder.is_dir() {
            return SessionSize::Xs;
        }

        let folder = folder.to_path_buf();
        let base_branch = base_branch.to_string();
        let diff = git_client
            .diff(folder, base_branch)
            .await
            .ok()
            .unwrap_or_default();

        SessionSize::from_diff(&diff)
    }
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
            if limit_key == "codex" || snapshot.limit_id.as_deref() == Some("codex") {
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
    let used_percent_value = window.used_percent?.clamp(0, 100);
    let used_percent = u8::try_from(used_percent_value).ok()?;
    let window_minutes = window
        .window_duration_mins
        .and_then(|window_minutes_value| u32::try_from(window_minutes_value).ok())
        .filter(|minutes| *minutes > 0);

    Some(CodexUsageLimitWindow {
        resets_at: window.resets_at,
        used_percent,
        window_minutes,
    })
}

#[cfg(test)]
mod tests {
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
}
