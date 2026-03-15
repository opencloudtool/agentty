//! App-wide background task helpers for status polling, version checks, and
//! app-server turns.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use askama::Template;
use tokio::sync::mpsc;

use crate::app::{AppEvent, UpdateStatus};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::infra::agent;
use crate::infra::git::GitClient;
use crate::version;

/// Stateless helpers for app-scoped background pollers and app-server
/// session execution.
pub(super) struct TaskService;

/// Inputs needed to generate review assist text in the background.
pub(super) struct FocusedReviewAssistTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Hash of the diff that triggered this review, threaded back in the
    /// completion event so the reducer can store it without re-reading cache.
    pub(super) diff_hash: u64,
    pub(super) focused_review_diff: String,
    pub(super) review_model: AgentModel,
    pub(super) session_folder: PathBuf,
    pub(super) session_id: String,
    pub(super) session_summary: Option<String>,
}

/// Askama view model for rendering review assist prompts.
#[derive(Template)]
#[template(path = "review_assist_prompt.md", escape = "none")]
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

    /// Spawns a one-shot background check for newer `agentty` versions on
    /// npmjs, optionally followed by an automatic `npm i -g agentty@latest`
    /// update.
    ///
    /// The task emits [`AppEvent::VersionAvailabilityUpdated`] with
    /// `Some("vX.Y.Z")` only when a newer version is detected. When
    /// `auto_update` is `true` and a newer version exists, the task
    /// subsequently emits [`AppEvent::UpdateStatusChanged`] with
    /// `InProgress`, then `Complete` or `Failed` depending on the npm
    /// install outcome.
    ///
    /// In tests, it emits an immediate `None` update instead of spawning the
    /// network check so test runs stay deterministic and offline.
    pub(super) fn spawn_version_check_task(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        auto_update: bool,
    ) {
        #[cfg(test)]
        {
            let _ = auto_update;
            let _ = app_event_tx.send(Self::version_availability_event(None));
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let latest_version_tag = version::latest_npm_version_tag().await;
            let version_event = Self::version_availability_event(latest_version_tag);

            let newer_version = match &version_event {
                AppEvent::VersionAvailabilityUpdated {
                    latest_available_version: Some(version),
                } => Some(version.clone()),
                _ => None,
            };

            let _ = app_event_tx.send(version_event);

            if let Some(newer_version) = newer_version
                && auto_update
            {
                Self::run_background_update(&app_event_tx, &newer_version).await;
            }
        });
    }

    /// Runs `npm i -g agentty@latest` in a background blocking task and
    /// emits update progress events.
    #[cfg(not(test))]
    async fn run_background_update(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        newer_version: &str,
    ) {
        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: newer_version.to_string(),
            },
        });

        let update_result = tokio::task::spawn_blocking(move || {
            let update_runner = version::RealUpdateRunner;
            version::run_npm_update_sync(&update_runner)
        })
        .await;

        let update_status = match update_result {
            Ok(Ok(_)) => UpdateStatus::Complete {
                version: newer_version.to_string(),
            },
            Ok(Err(_)) | Err(_) => UpdateStatus::Failed {
                version: newer_version.to_string(),
            },
        };

        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged { update_status });
    }

    /// Spawns one background review assist generation task and emits
    /// an event with either final review text or a failure description.
    pub(super) fn spawn_focused_review_assist_task(input: FocusedReviewAssistTaskInput) {
        let FocusedReviewAssistTaskInput {
            app_event_tx,
            diff_hash,
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

            let app_event =
                Self::focused_review_app_event(diff_hash, focused_review_result, session_id);
            let _ = app_event_tx.send(app_event);
        });
    }

    /// Generates review assist text by running one model command with
    /// read-only review constraints and parsing the final assistant response
    /// content.
    async fn focused_review_assist_text(
        session_folder: &Path,
        review_model: AgentModel,
        focused_review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, String> {
        Self::focused_review_assist_text_with_submitter(
            session_folder,
            review_model,
            focused_review_diff,
            session_summary,
            |review_folder, review_model, focused_review_prompt| {
                Box::pin(async move {
                    agent::submit_one_shot(agent::OneShotRequest {
                        child_pid: None,
                        folder: review_folder,
                        model: review_model,
                        prompt: focused_review_prompt,
                        protocol_profile: agent::ProtocolRequestProfile::UtilityPrompt,
                        reasoning_level: ReasoningLevel::default(),
                    })
                    .await
                })
            },
        )
        .await
    }

    /// Converts a raw version lookup result into the reducer event consumed by
    /// app state.
    fn version_availability_event(latest_version_tag: Option<String>) -> AppEvent {
        let latest_available_version = latest_version_tag.filter(|latest_version| {
            version::is_newer_than_current_version(env!("CARGO_PKG_VERSION"), latest_version)
        });

        AppEvent::VersionAvailabilityUpdated {
            latest_available_version,
        }
    }

    /// Generates review assist text using an injected one-shot submitter so
    /// failure paths can be tested without subprocess execution.
    async fn focused_review_assist_text_with_submitter<Submitter>(
        session_folder: &Path,
        review_model: AgentModel,
        focused_review_diff: &str,
        session_summary: Option<&str>,
        submitter: Submitter,
    ) -> Result<String, String>
    where
        Submitter: for<'submit> FnOnce(
            &'submit Path,
            AgentModel,
            &'submit str,
        ) -> Pin<
            Box<dyn Future<Output = Result<agent::AgentResponse, String>> + Send + 'submit>,
        >,
    {
        let focused_review_prompt =
            Self::focused_review_assist_prompt(focused_review_diff, session_summary)?;
        let agent_response =
            submitter(session_folder, review_model, &focused_review_prompt).await?;

        Self::focused_review_output_text(&agent_response)
    }

    /// Builds the final reducer event for one review-assist task outcome.
    fn focused_review_app_event(
        diff_hash: u64,
        focused_review_result: Result<String, String>,
        session_id: String,
    ) -> AppEvent {
        match focused_review_result {
            Ok(review_text) => AppEvent::FocusedReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            },
            Err(error) => AppEvent::FocusedReviewPreparationFailed {
                diff_hash,
                error,
                session_id,
            },
        }
    }

    /// Extracts one non-empty review string from the agent response payload.
    fn focused_review_output_text(agent_response: &agent::AgentResponse) -> Result<String, String> {
        let focused_review_text = agent_response.to_display_text();
        let focused_review_text = focused_review_text.trim();
        if focused_review_text.is_empty() {
            return Err("Review assist returned empty output".to_string());
        }

        Ok(focused_review_text.to_string())
    }

    /// Renders the review assist prompt from the markdown template.
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
            .map_err(|error| format!("Failed to render `review_assist_prompt.md`: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::agent::protocol::AgentResponse;

    #[tokio::test]
    /// Ensures test-mode version checks still emit one reducer event without
    /// touching the network.
    async fn spawn_version_check_task_emits_none_update_in_tests() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_version_check_task(&app_event_tx, true);
        let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
            .await
            .expect("timed out waiting for version-check event")
            .expect("version-check task should emit one event");

        // Assert
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            }
        );
    }

    #[tokio::test]
    /// Ensures the `--no-update` flag (`auto_update=false`) still emits a
    /// version availability event without triggering an update.
    async fn spawn_version_check_task_with_no_update_emits_version_event() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_version_check_task(&app_event_tx, false);
        let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
            .await
            .expect("timed out waiting for version-check event")
            .expect("version-check task should emit one event");

        // Assert — still emits version check, no update events
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            }
        );
    }

    #[test]
    /// Verifies version availability keeps only tags newer than the current
    /// crate version.
    fn version_availability_event_keeps_newer_version_tags() {
        // Arrange
        let latest_version_tag = Some("v999.0.0".to_string());

        // Act
        let app_event = TaskService::version_availability_event(latest_version_tag);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: Some("v999.0.0".to_string()),
            }
        );
    }

    #[test]
    /// Verifies version availability suppresses current-version tags so the
    /// UI only announces true upgrades.
    fn version_availability_event_ignores_current_version_tag() {
        // Arrange
        let latest_version_tag = Some(format!("v{}", env!("CARGO_PKG_VERSION")));

        // Act
        let app_event = TaskService::version_availability_event(latest_version_tag);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            }
        );
    }

    #[tokio::test]
    /// Ensures review assist surfaces one-shot submission failures without
    /// invoking a real subprocess.
    async fn focused_review_assist_text_with_submitter_returns_submit_error() {
        // Arrange
        let session_folder = Path::new("/tmp/review-assist-submit-error");
        let review_model = AgentModel::ClaudeSonnet46;
        let focused_review_diff = "diff --git a/src/lib.rs b/src/lib.rs";

        // Act
        let result = TaskService::focused_review_assist_text_with_submitter(
            session_folder,
            review_model,
            focused_review_diff,
            None,
            |_, _, _| Box::pin(async { Err("submit failed".to_string()) }),
        )
        .await;

        // Assert
        let error = result.expect_err("submit failure should be returned");
        assert_eq!(error, "submit failed");
    }

    #[test]
    /// Verifies review-assist event mapping preserves successful review text.
    fn focused_review_app_event_maps_successful_review_output() {
        // Arrange
        let diff_hash = 7;
        let focused_review_result = Ok("Flagged one missing error branch.".to_string());
        let session_id = "session-7".to_string();

        // Act
        let app_event =
            TaskService::focused_review_app_event(diff_hash, focused_review_result, session_id);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::FocusedReviewPrepared {
                diff_hash: 7,
                review_text: "Flagged one missing error branch.".to_string(),
                session_id: "session-7".to_string(),
            }
        );
    }

    #[test]
    /// Verifies review-assist event mapping preserves failure details for the
    /// reducer and view-mode status text.
    fn focused_review_app_event_maps_failure_output() {
        // Arrange
        let diff_hash = 9;
        let focused_review_result = Err("empty response".to_string());
        let session_id = "session-9".to_string();

        // Act
        let app_event =
            TaskService::focused_review_app_event(diff_hash, focused_review_result, session_id);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::FocusedReviewPreparationFailed {
                diff_hash: 9,
                error: "empty response".to_string(),
                session_id: "session-9".to_string(),
            }
        );
    }

    #[test]
    /// Verifies review output text is trimmed before it is stored in app
    /// state.
    fn focused_review_output_text_trims_agent_response_text() {
        // Arrange
        let agent_response = AgentResponse::plain("  Review looks good.  \n");

        // Act
        let review_text = TaskService::focused_review_output_text(&agent_response)
            .expect("non-empty output should be accepted");

        // Assert
        assert_eq!(review_text, "Review looks good.");
    }

    #[test]
    /// Verifies whitespace-only review output is rejected so users see a clear
    /// error instead of a blank review pane.
    fn focused_review_output_text_rejects_blank_agent_response_text() {
        // Arrange
        let agent_response = AgentResponse::plain(" \n\t ");

        // Act
        let result = TaskService::focused_review_output_text(&agent_response);

        // Assert
        let error = result.expect_err("blank output should be rejected");
        assert_eq!(error, "Review assist returned empty output");
    }

    #[test]
    /// Ensures review prompt rendering includes read-only constraints
    /// while keeping internet and non-editing verification options available.
    fn test_focused_review_assist_prompt_enforces_read_only_constraints() {
        // Arrange
        let focused_review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let session_summary = Some("Refactor parser error mapping.");

        // Act
        let prompt =
            TaskService::focused_review_assist_prompt(focused_review_diff, session_summary)
                .expect("review prompt should render");

        // Assert
        assert!(prompt.contains("You are in read-only review mode."));
        assert!(prompt.contains("Do not create, modify, rename, or delete files."));
        assert!(prompt.contains("You may browse the internet when needed."));
        assert!(prompt.contains("You may run non-editing CLI commands"));
    }

    #[test]
    /// Verifies that structured `AgentResponse` JSON is unwrapped to plain
    /// display text for focused review rendering.
    fn test_structured_agent_response_is_unwrapped_to_display_text() {
        // Arrange
        let structured_json = r#"{"messages":[{"type":"answer","text":"Review looks good."}]}"#;

        // Act
        let agent_response = agent::protocol::parse_agent_response(structured_json);
        let display_text = agent_response.to_display_text();

        // Assert
        assert_eq!(display_text.trim(), "Review looks good.");
    }

    #[test]
    /// Verifies that `UpdateStatusChanged` events for in-progress, complete,
    /// and failed states can be constructed and compared.
    fn update_status_changed_event_roundtrips_all_variants() {
        // Arrange / Act
        let in_progress = AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: "v1.0.0".to_string(),
            },
        };
        let complete = AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::Complete {
                version: "v1.0.0".to_string(),
            },
        };
        let failed = AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::Failed {
                version: "v1.0.0".to_string(),
            },
        };

        // Assert
        assert_ne!(in_progress, complete);
        assert_ne!(complete, failed);
        assert_ne!(in_progress, failed);
    }
}
