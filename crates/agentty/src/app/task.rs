//! App-wide background task helpers for status polling, version checks, and
//! app-server turns.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use askama::Template;
use tokio::sync::mpsc;

use crate::app::error::AppError;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppEvent, UpdateStatus};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::infra::agent;
use crate::infra::git::GitClient;
use crate::version;

/// Stateless helpers for app-scoped background pollers and app-server
/// session execution.
pub(super) struct TaskService;

/// Per-session git-status polling target for one active session branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SessionGitStatusTarget {
    /// Base branch the session branch should be compared against, for example
    /// `main`.
    pub(super) base_branch: String,
    /// Local branch name tracked for the session, for example
    /// `agentty/1234abcd`.
    pub(super) branch_name: String,
    /// Stable session identifier used as the reducer map key.
    pub(super) session_id: String,
}

/// Inputs needed to generate review assist text in the background.
pub(super) struct ReviewAssistTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Hash of the diff that triggered this review, threaded back in the
    /// completion event so the reducer can store it without re-reading cache.
    pub(super) diff_hash: u64,
    pub(super) review_diff: String,
    pub(super) review_model: AgentModel,
    pub(super) session_folder: PathBuf,
    pub(super) session_id: String,
    pub(super) session_summary: Option<String>,
}

/// Askama view model for rendering review assist prompts.
#[derive(Template)]
#[template(path = "review_assist_prompt.md", escape = "none")]
struct ReviewAssistPromptTemplate<'a> {
    review_diff: &'a str,
    session_summary: &'a str,
}

impl TaskService {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    ///
    /// The task emits one combined [`AppEvent::GitStatusUpdated`] payload for
    /// the active project branch plus all active session branches instead of
    /// mutating app state directly. Project status stays upstream-based, while
    /// session status carries both the base-branch comparison and any tracked
    /// remote comparison for each session branch.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        project_branch_name: String,
        session_git_status_targets: Vec<SessionGitStatusTarget>,
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
                    // Best-effort: background fetch failure is non-critical.
                    let _ = git_client.fetch_remote(root).await;
                }

                let branch_tracking_statuses = {
                    let root = repo_root.clone();
                    git_client
                        .branch_tracking_statuses(root)
                        .await
                        .unwrap_or_default()
                };
                let status = branch_tracking_statuses
                    .get(&project_branch_name)
                    .copied()
                    .flatten();
                let session_git_statuses = Self::session_git_statuses(
                    &branch_tracking_statuses,
                    &repo_root,
                    &session_git_status_targets,
                    git_client.as_ref(),
                );
                let session_git_statuses = session_git_statuses.await;
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                // Fire-and-forget: receiver may be dropped during shutdown.
                let _ = app_event_tx.send(AppEvent::GitStatusUpdated {
                    session_statuses: session_git_statuses,
                    status,
                });
                for _ in 0..30 {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }

    /// Resolves ahead/behind snapshots for all tracked session branches by
    /// combining each branch's base-branch comparison with any tracked-remote
    /// snapshot already available from the repo-wide status query.
    async fn session_git_statuses(
        branch_tracking_statuses: &HashMap<String, Option<(u32, u32)>>,
        repo_root: &Path,
        session_git_status_targets: &[SessionGitStatusTarget],
        git_client: &dyn GitClient,
    ) -> HashMap<String, SessionGitStatus> {
        let mut session_git_statuses = HashMap::with_capacity(session_git_status_targets.len());

        for session_git_status_target in session_git_status_targets {
            let base_status = git_client
                .get_ref_ahead_behind(
                    repo_root.to_path_buf(),
                    session_git_status_target.branch_name.clone(),
                    session_git_status_target.base_branch.clone(),
                )
                .await
                .ok();
            let remote_status = branch_tracking_statuses
                .get(&session_git_status_target.branch_name)
                .copied()
                .flatten();
            session_git_statuses.insert(
                session_git_status_target.session_id.clone(),
                SessionGitStatus {
                    base_status,
                    remote_status,
                },
            );
        }

        session_git_statuses
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
            // Fire-and-forget: receiver may be dropped during shutdown.
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

            // Fire-and-forget: receiver may be dropped during shutdown.
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
        // Fire-and-forget: receiver may be dropped during shutdown.
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

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged { update_status });
    }

    /// Spawns one background review assist generation task and emits
    /// an event with either final review text or a failure description.
    pub(super) fn spawn_review_assist_task(input: ReviewAssistTaskInput) {
        let ReviewAssistTaskInput {
            app_event_tx,
            diff_hash,
            review_diff,
            review_model,
            session_folder,
            session_id,
            session_summary,
        } = input;

        tokio::spawn(async move {
            let review_result = Self::review_assist_text(
                &session_folder,
                review_model,
                &review_diff,
                session_summary.as_deref(),
            )
            .await;

            let app_event = Self::review_app_event(diff_hash, review_result, session_id);
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(app_event);
        });
    }

    /// Generates review assist text by running one model command with
    /// read-only review constraints and parsing the final assistant response
    /// content.
    async fn review_assist_text(
        session_folder: &Path,
        review_model: AgentModel,
        review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, AppError> {
        Self::review_assist_text_with_submitter(
            session_folder,
            review_model,
            review_diff,
            session_summary,
            |review_folder, review_model, review_prompt| {
                Box::pin(async move {
                    agent::submit_one_shot(agent::OneShotRequest {
                        child_pid: None,
                        folder: review_folder,
                        model: review_model,
                        prompt: review_prompt,
                        request_kind: crate::infra::channel::AgentRequestKind::UtilityPrompt,
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
    async fn review_assist_text_with_submitter<Submitter>(
        session_folder: &Path,
        review_model: AgentModel,
        review_diff: &str,
        session_summary: Option<&str>,
        submitter: Submitter,
    ) -> Result<String, AppError>
    where
        Submitter: for<'submit> FnOnce(
            &'submit Path,
            AgentModel,
            &'submit str,
        ) -> Pin<
            Box<dyn Future<Output = Result<agent::AgentResponse, String>> + Send + 'submit>,
        >,
    {
        let review_prompt = Self::review_assist_prompt(review_diff, session_summary)?;
        let agent_response = submitter(session_folder, review_model, &review_prompt)
            .await
            .map_err(AppError::Workflow)?;

        Self::review_output_text(&agent_response)
    }

    /// Builds the final reducer event for one review-assist task outcome.
    ///
    /// Converts the typed [`AppError`] to a display string at the event
    /// boundary because [`AppEvent`] requires `Clone` + `Eq`, which
    /// [`AppError`] cannot satisfy due to non-cloneable inner IO errors.
    fn review_app_event(
        diff_hash: u64,
        review_result: Result<String, AppError>,
        session_id: String,
    ) -> AppEvent {
        match review_result {
            Ok(review_text) => AppEvent::ReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            },
            Err(error) => AppEvent::ReviewPreparationFailed {
                diff_hash,
                error: error.to_string(),
                session_id,
            },
        }
    }

    /// Extracts one non-empty review string from the agent response payload.
    fn review_output_text(agent_response: &agent::AgentResponse) -> Result<String, AppError> {
        let review_text = agent_response.to_display_text();
        let review_text = review_text.trim();
        if review_text.is_empty() {
            return Err(AppError::Workflow(
                "Review assist returned empty output".to_string(),
            ));
        }

        Ok(review_text.to_string())
    }

    /// Renders the review assist prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error when Askama template rendering fails.
    fn review_assist_prompt(
        review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, AppError> {
        let template = ReviewAssistPromptTemplate {
            review_diff: review_diff.trim(),
            session_summary: session_summary.map_or("", str::trim),
        };

        template.render().map_err(|error| {
            AppError::Workflow(format!(
                "Failed to render `review_assist_prompt.md`: {error}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::infra::agent::protocol::AgentResponse;
    use crate::infra::git::{GitError, MockGitClient};

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
    /// Ensures review assist surfaces one-shot submission failures as
    /// [`AppError::Workflow`] without invoking a real subprocess.
    async fn review_assist_text_with_submitter_returns_workflow_error_on_submit_failure() {
        // Arrange
        let session_folder = Path::new("/tmp/review-assist-submit-error");
        let review_model = AgentModel::ClaudeSonnet46;
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";

        // Act
        let result = TaskService::review_assist_text_with_submitter(
            session_folder,
            review_model,
            review_diff,
            None,
            |_, _, _| Box::pin(async { Err("submit failed".to_string()) }),
        )
        .await;

        // Assert
        let error = result.expect_err("submit failure should be returned");
        assert!(
            matches!(error, AppError::Workflow(_)),
            "expected AppError::Workflow, got: {error:?}"
        );
        assert_eq!(error.to_string(), "submit failed");
    }

    #[tokio::test]
    /// Verifies session git-status selection maps branch comparisons back to
    /// session ids.
    async fn session_git_statuses_collects_all_target_statuses() {
        // Arrange
        let repo_root = Path::new("/tmp/task-service-session-statuses");
        let branch_tracking_statuses = HashMap::from([
            ("agentty/session-a".to_string(), Some((7, 0))),
            ("agentty/session-b".to_string(), Some((0, 4))),
        ]);
        let session_git_status_targets = vec![
            SessionGitStatusTarget {
                base_branch: "main".to_string(),
                branch_name: "agentty/session-a".to_string(),
                session_id: "session-a".to_string(),
            },
            SessionGitStatusTarget {
                base_branch: "develop".to_string(),
                branch_name: "agentty/session-b".to_string(),
                session_id: "session-b".to_string(),
            },
        ];
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_get_ref_ahead_behind()
            .times(2)
            .returning(|_, left_ref, right_ref| {
                Box::pin(async move {
                    match (left_ref.as_str(), right_ref.as_str()) {
                        ("agentty/session-a", "main") => Ok((2, 1)),
                        ("agentty/session-b", "develop") => Ok((0, 0)),
                        _ => Err(GitError::OutputParse("unexpected ref pair".to_string())),
                    }
                })
            });

        // Act
        let statuses = TaskService::session_git_statuses(
            &branch_tracking_statuses,
            repo_root,
            &session_git_status_targets,
            &mock_git_client,
        )
        .await;

        // Assert
        assert_eq!(
            statuses.get("session-a"),
            Some(&SessionGitStatus {
                base_status: Some((2, 1)),
                remote_status: Some((7, 0)),
            })
        );
        assert_eq!(
            statuses.get("session-b"),
            Some(&SessionGitStatus {
                base_status: Some((0, 0)),
                remote_status: Some((0, 4)),
            })
        );
    }

    #[tokio::test]
    /// Verifies session branches without tracked status degrade to `None`
    /// without affecting the rest of the snapshot.
    async fn session_git_statuses_keeps_failed_targets_as_none() {
        // Arrange
        let repo_root = Path::new("/tmp/task-service-session-statuses-error");
        let branch_tracking_statuses = HashMap::new();
        let session_git_status_targets = vec![SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "agentty/session-a".to_string(),
            session_id: "session-a".to_string(),
        }];
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_get_ref_ahead_behind()
            .once()
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(GitError::OutputParse(
                        "failed to compare session branch".to_string(),
                    ))
                })
            });

        // Act
        let statuses = TaskService::session_git_statuses(
            &branch_tracking_statuses,
            repo_root,
            &session_git_status_targets,
            &mock_git_client,
        )
        .await;

        // Assert
        assert_eq!(
            statuses.get("session-a"),
            Some(&SessionGitStatus {
                base_status: None,
                remote_status: None,
            })
        );
    }

    #[test]
    /// Verifies review-assist event mapping preserves successful review text.
    fn review_app_event_maps_successful_review_output() {
        // Arrange
        let diff_hash = 7;
        let review_result = Ok("Flagged one missing error branch.".to_string());
        let session_id = "session-7".to_string();

        // Act
        let app_event = TaskService::review_app_event(diff_hash, review_result, session_id);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::ReviewPrepared {
                diff_hash: 7,
                review_text: "Flagged one missing error branch.".to_string(),
                session_id: "session-7".to_string(),
            }
        );
    }

    #[test]
    /// Verifies review-assist event mapping preserves failure details for the
    /// reducer and view-mode status text.
    fn review_app_event_maps_failure_output() {
        // Arrange
        let diff_hash = 9;
        let review_result = Err(AppError::Workflow("empty response".to_string()));
        let session_id = "session-9".to_string();

        // Act
        let app_event = TaskService::review_app_event(diff_hash, review_result, session_id);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::ReviewPreparationFailed {
                diff_hash: 9,
                error: "empty response".to_string(),
                session_id: "session-9".to_string(),
            }
        );
    }

    #[test]
    /// Verifies review output text is trimmed before it is stored in app
    /// state.
    fn review_output_text_trims_agent_response_text() {
        // Arrange
        let agent_response = AgentResponse::plain("  Review looks good.  \n");

        // Act
        let review_text = TaskService::review_output_text(&agent_response)
            .expect("non-empty output should be accepted");

        // Assert
        assert_eq!(review_text, "Review looks good.");
    }

    #[test]
    /// Verifies whitespace-only review output is rejected as
    /// [`AppError::Workflow`] so users see a clear error instead of a blank
    /// review pane.
    fn review_output_text_rejects_blank_agent_response_text() {
        // Arrange
        let agent_response = AgentResponse::plain(" \n\t ");

        // Act
        let result = TaskService::review_output_text(&agent_response);

        // Assert
        let error = result.expect_err("blank output should be rejected");
        assert!(
            matches!(error, AppError::Workflow(_)),
            "expected AppError::Workflow, got: {error:?}"
        );
        assert_eq!(error.to_string(), "Review assist returned empty output");
    }

    #[test]
    /// Ensures review prompt rendering includes read-only constraints
    /// while keeping internet and non-editing verification options available.
    fn test_review_assist_prompt_enforces_read_only_constraints() {
        // Arrange
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let session_summary = Some("Refactor parser error mapping.");

        // Act
        let prompt = TaskService::review_assist_prompt(review_diff, session_summary)
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
        let structured_json = r#"{"answer":"Review looks good.","questions":[],"summary":null}"#;

        // Act
        let agent_response = agent::protocol::parse_agent_response_strict(structured_json)
            .expect("structured response should parse");
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
