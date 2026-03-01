//! App-wide background task helpers for status polling, version checks, and
//! app-server turns.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use askama::Template;
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::agent::AgentModel;
use crate::infra::git::GitClient;

/// Poll interval for account-level Codex usage limits snapshots.
#[cfg(not(test))]
const CODEX_USAGE_LIMITS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Stateless helpers for app-scoped background pollers and app-server
/// session execution.
pub(super) struct TaskService;

/// Inputs needed to generate focused-review assist text in the background.
pub(super) struct FocusedReviewAssistTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) focused_review_diff: String,
    pub(super) review_model: AgentModel,
    pub(super) session_folder: PathBuf,
    pub(super) session_id: String,
    pub(super) session_summary: Option<String>,
}

/// Askama view model for rendering focused-review assist prompts.
#[derive(Template)]
#[template(path = "focused_review_assist_prompt.md", escape = "none")]
struct FocusedReviewAssistPromptTemplate<'a> {
    focused_review_diff: &'a str,
    session_summary: &'a str,
}

impl TaskService {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    ///
    /// The task emits [`AppEvent::GitStatusUpdated`] snapshots instead of
    /// mutating app state directly.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        cancel: Arc<AtomicBool>,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
    ) {
        let dir = working_dir.to_path_buf();
        tokio::spawn(async move {
            let repo_root = git_client
                .find_git_repo_root(dir.clone())
                .await
                .unwrap_or(dir);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }

                {
                    let root = repo_root.clone();
                    let _ = git_client.fetch_remote(root).await;
                }

                let status = {
                    let root = repo_root.clone();
                    git_client.get_ahead_behind(root).await.ok()
                };
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = app_event_tx.send(AppEvent::GitStatusUpdated { status });
                for _ in 0..30 {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }

    /// Spawns a background loop that periodically refreshes Codex usage
    /// limits.
    ///
    /// The task emits [`AppEvent::CodexUsageLimitsUpdated`] snapshots instead
    /// of mutating app state directly.
    ///
    /// In tests, this function is a no-op so test runs stay deterministic and
    /// offline.
    pub(super) fn spawn_codex_usage_limits_task(app_event_tx: &mpsc::UnboundedSender<AppEvent>) {
        #[cfg(test)]
        {
            let _ = app_event_tx;
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let mut refresh_tick = tokio::time::interval(CODEX_USAGE_LIMITS_REFRESH_INTERVAL);
            refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            refresh_tick.tick().await;

            loop {
                let codex_usage_limits =
                    crate::app::SessionManager::load_codex_usage_limits().await;
                let _ = app_event_tx.send(AppEvent::CodexUsageLimitsUpdated { codex_usage_limits });

                refresh_tick.tick().await;
            }
        });
    }

    /// Spawns a one-shot background check for newer `agentty` versions on
    /// npmjs.
    ///
    /// The task emits [`AppEvent::VersionAvailabilityUpdated`] with
    /// `Some("vX.Y.Z")` only when a newer version is detected.
    ///
    /// In tests, it emits an immediate `None` update instead of spawning the
    /// network check so test runs stay deterministic and offline.
    pub(super) fn spawn_version_check_task(app_event_tx: &mpsc::UnboundedSender<AppEvent>) {
        #[cfg(test)]
        {
            let _ = app_event_tx.send(AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            });
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let latest_available_version =
                crate::version::latest_npm_version_tag()
                    .await
                    .filter(|latest_version| {
                        crate::version::is_newer_than_current_version(
                            env!("CARGO_PKG_VERSION"),
                            latest_version,
                        )
                    });

            let _ = app_event_tx.send(AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            });
        });
    }

    /// Spawns one background focused-review assist generation task and emits
    /// an event with either final review text or a failure description.
    pub(super) fn spawn_focused_review_assist_task(input: FocusedReviewAssistTaskInput) {
        let FocusedReviewAssistTaskInput {
            app_event_tx,
            focused_review_diff,
            review_model,
            session_folder,
            session_id,
            session_summary,
        } = input;

        tokio::spawn(async move {
            let focused_review_result = Self::focused_review_assist_text(
                &session_folder,
                review_model,
                &focused_review_diff,
                session_summary.as_deref(),
            )
            .await;

            let app_event = match focused_review_result {
                Ok(review_text) => AppEvent::FocusedReviewPrepared {
                    review_text,
                    session_id,
                },
                Err(error) => AppEvent::FocusedReviewPreparationFailed { error, session_id },
            };
            let _ = app_event_tx.send(app_event);
        });
    }

    /// Generates focused-review assist text by running one model command with
    /// read-only review constraints and parsing the final assistant response
    /// content.
    async fn focused_review_assist_text(
        session_folder: &Path,
        review_model: AgentModel,
        focused_review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, String> {
        let focused_review_prompt =
            Self::focused_review_assist_prompt(focused_review_diff, session_summary)?;
        let backend = crate::infra::agent::create_backend(review_model.kind());
        let command = backend
            .build_command(crate::infra::agent::BuildCommandRequest {
                folder: session_folder,
                mode: crate::infra::agent::AgentCommandMode::Resume {
                    prompt: &focused_review_prompt,
                    session_output: None,
                },
                model: review_model.as_str(),
            })
            .map_err(|error| error.to_string())?;
        let mut tokio_command = tokio::process::Command::from(command);
        tokio_command.stdin(std::process::Stdio::null());
        let command_output = tokio_command
            .output()
            .await
            .map_err(|error| format!("Failed to execute focused review assist command: {error}"))?;

        let stdout_text = String::from_utf8_lossy(&command_output.stdout).into_owned();
        let stderr_text = String::from_utf8_lossy(&command_output.stderr).into_owned();
        if !command_output.status.success() {
            return Err(Self::format_focused_review_assist_exit_error(
                command_output.status.code(),
                &stdout_text,
                &stderr_text,
            ));
        }

        let parsed =
            crate::infra::agent::parse_response(review_model.kind(), &stdout_text, &stderr_text);
        let focused_review_text = parsed.content.trim();
        if focused_review_text.is_empty() {
            return Err("Focused review assist returned empty output".to_string());
        }

        Ok(focused_review_text.to_string())
    }

    /// Renders the focused-review assist prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error when Askama template rendering fails.
    fn focused_review_assist_prompt(
        focused_review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, String> {
        let template = FocusedReviewAssistPromptTemplate {
            focused_review_diff: focused_review_diff.trim(),
            session_summary: session_summary.map_or("", str::trim),
        };

        template
            .render()
            .map_err(|error| format!("Failed to render `focused_review_assist_prompt.md`: {error}"))
    }

    /// Formats a focused-review assist process failure with normalized output
    /// details for UI display.
    fn format_focused_review_assist_exit_error(
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> String {
        let exit_code = exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
        let output_detail = Self::focused_review_assist_output_detail(stdout, stderr);

        format!("Focused review assist failed with exit code {exit_code}: {output_detail}")
    }

    /// Formats stdout/stderr details for focused-review assist failures.
    fn focused_review_assist_output_detail(stdout: &str, stderr: &str) -> String {
        let trimmed_stdout = stdout.trim();
        let trimmed_stderr = stderr.trim();
        if !trimmed_stderr.is_empty() && !trimmed_stdout.is_empty() {
            return format!("stderr: {trimmed_stderr}; stdout: {trimmed_stdout}");
        }
        if !trimmed_stderr.is_empty() {
            return format!("stderr: {trimmed_stderr}");
        }
        if !trimmed_stdout.is_empty() {
            return format!("stdout: {trimmed_stdout}");
        }

        "no stdout or stderr output".to_string()
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    /// Ensures focused-review prompt rendering includes read-only constraints
    /// while keeping internet and non-editing verification options available.
    fn test_focused_review_assist_prompt_enforces_read_only_constraints() {
        // Arrange
        let focused_review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let session_summary = Some("Refactor parser error mapping.");

        // Act
        let prompt =
            TaskService::focused_review_assist_prompt(focused_review_diff, session_summary)
                .expect("focused review prompt should render");

        // Assert
        assert!(prompt.contains("You are in read-only review mode."));
        assert!(prompt.contains("Do not create, modify, rename, or delete files."));
        assert!(prompt.contains("You may browse the internet when needed."));
        assert!(prompt.contains("You may run non-editing CLI commands"));
    }

    #[test]
    /// Verifies Codex usage refresh is disabled in test builds.
    fn test_spawn_codex_usage_limits_task_is_noop_in_tests() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_codex_usage_limits_task(&app_event_tx);

        // Assert
        assert!(app_event_rx.try_recv().is_err());
    }
}
