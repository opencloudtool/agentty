//! Shared app dependency container for managers and background workflows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent::{AppServerClient, OneShotClient, RealOneShotClient};
use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tracing::{debug, warn};

use crate::app::AppEvent;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentCliInfo, AgentKind};
use crate::domain::session::SessionId;
use crate::infra::clipboard_image::{ClipboardImageClient, RealClipboardImageClient};
use crate::infra::clock::Clock;
use crate::infra::fs::FsClient;
use crate::infra::personality::{PersonalityCatalogClient, RealPersonalityCatalogClient};

/// Shared per-app session redraw version counters keyed by session id.
pub(crate) type SessionUpdateVersionMap = Arc<Mutex<HashMap<SessionId, u64>>>;

/// Maximum graceful-shutdown wait shared by all background cleanup tasks.
const CLEANUP_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// External clients and cached machine-scoped availability injected into
/// [`AppServices`].
pub(crate) struct AppServiceDeps {
    /// Shared provider-owned app-server client override used by tests and
    /// injected environments.
    pub(crate) app_server_client_override: Option<Arc<dyn AppServerClient>>,
    /// Cached locally runnable backends used to scope model selection.
    pub(crate) available_agent_kinds: Vec<AgentKind>,
    /// Optional clipboard image client override used by tests and injected
    /// environments.
    pub(crate) clipboard_image_client_override: Option<Arc<dyn ClipboardImageClient>>,
    /// Shared filesystem client for async filesystem operations.
    pub(crate) fs_client: Arc<dyn FsClient>,
    /// Shared git client for async git operations.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// Optional isolated-prompt client override used by tests and injected
    /// environments.
    pub(crate) one_shot_client_override: Option<Arc<dyn OneShotClient>>,
    /// Optional workspace personality catalog override used by tests.
    pub(crate) personality_catalog_client_override: Option<Arc<dyn PersonalityCatalogClient>>,
    /// Shared repository bundle used by app workflows.
    pub(crate) repositories: AppRepositories,
    /// Shared forge review-request client.
    pub(crate) review_request_client: Arc<dyn ReviewRequestClient>,
}

/// Shared app dependencies used by managers and background workflows.
#[derive(Clone)]
pub struct AppServices {
    available_agent_clis: Arc<Mutex<Vec<AgentCliInfo>>>,
    available_agent_kinds: Arc<[AgentKind]>,
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
    base_path: PathBuf,
    cleanup_task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    creation_task_handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    clipboard_image_client: Arc<dyn ClipboardImageClient>,
    clock: Arc<dyn Clock>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    one_shot_client: Arc<dyn OneShotClient>,
    personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    repositories: AppRepositories,
    review_request_client: Arc<dyn ReviewRequestClient>,
    session_update_versions: SessionUpdateVersionMap,
}

impl AppServices {
    /// Creates a shared service container with versioned agent CLI
    /// availability captured at startup.
    pub(crate) fn new_with_agent_clis(
        base_path: PathBuf,
        clock: Arc<dyn Clock>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        deps: AppServiceDeps,
        available_agent_clis: Vec<AgentCliInfo>,
    ) -> Self {
        let AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override,
            fs_client,
            git_client,
            one_shot_client_override,
            personality_catalog_client_override,
            repositories,
            review_request_client,
        } = deps;
        let clipboard_image_client = clipboard_image_client_override.unwrap_or_else(|| {
            Arc::new(RealClipboardImageClient::new(
                Arc::clone(&clock),
                Arc::clone(&fs_client),
            ))
        });
        let one_shot_client = one_shot_client_override.unwrap_or_else(|| {
            Arc::new(RealOneShotClient::new(
                app_server_client_override.as_ref().map(Arc::clone),
            ))
        });
        let personality_catalog_client = personality_catalog_client_override
            .unwrap_or_else(|| Arc::new(RealPersonalityCatalogClient));

        Self {
            available_agent_clis: Arc::new(Mutex::new(available_agent_clis)),
            available_agent_kinds: Arc::<[AgentKind]>::from(available_agent_kinds),
            app_server_client_override,
            base_path,
            cleanup_task_handles: Arc::default(),
            creation_task_handles: Arc::default(),
            clipboard_image_client,
            clock,
            event_tx,
            fs_client,
            git_client,
            one_shot_client,
            personality_catalog_client,
            repositories,
            review_request_client,
            session_update_versions: Arc::default(),
        }
    }

    /// Returns the session base path.
    pub(crate) fn base_path(&self) -> &Path {
        self.base_path.as_path()
    }

    /// Returns the cached locally runnable agent kinds.
    pub(crate) fn available_agent_kinds(&self) -> Vec<AgentKind> {
        self.available_agent_kinds.as_ref().to_vec()
    }

    /// Returns the cached locally runnable agent CLIs and detected versions.
    pub(crate) fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        self.available_agent_clis
            .lock()
            .map(|agent_clis| agent_clis.clone())
            .unwrap_or_default()
    }

    /// Replaces the cached CLI rows after background version detection
    /// completes.
    pub(crate) fn replace_available_agent_clis(&self, available_agent_clis: Vec<AgentCliInfo>) {
        if let Ok(mut agent_clis) = self.available_agent_clis.lock() {
            *agent_clis = available_agent_clis;
        }
    }

    /// Returns the application repository bundle.
    pub(crate) fn db(&self) -> &AppRepositories {
        &self.repositories
    }

    /// Returns the shared wall-clock used by session workflows.
    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// Returns the shared clipboard-image client for pasted image capture.
    pub(crate) fn clipboard_image_client(&self) -> Arc<dyn ClipboardImageClient> {
        Arc::clone(&self.clipboard_image_client)
    }

    /// Returns the shared client for isolated structured agent prompts.
    pub(crate) fn one_shot_client(&self) -> Arc<dyn OneShotClient> {
        Arc::clone(&self.one_shot_client)
    }

    /// Returns the workspace personality discovery client.
    pub(crate) fn personality_catalog_client(&self) -> Arc<dyn PersonalityCatalogClient> {
        Arc::clone(&self.personality_catalog_client)
    }

    /// Enqueues an app event onto the internal event bus with debug
    /// instrumentation for producer-side event volume.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        let event_label = app_event_label(&event);
        debug!(
            event = event_label,
            "enqueueing app event through app services"
        );

        // Fire-and-forget: receiver may be dropped during shutdown.
        if self.event_tx.send(event).is_err() {
            warn!(
                event = event_label,
                "failed to send app event because the receiver is closed"
            );
        }
    }

    /// Enqueues refresh events for workflows that changed both session
    /// snapshots and project-level session aggregates.
    pub(crate) fn emit_session_and_project_refresh_events(&self) {
        self.emit_app_event(AppEvent::RefreshSessions);
        self.emit_app_event(AppEvent::RefreshProjects);
    }

    /// Tracks worktree creation, which must settle before cleanup because its
    /// blocking Git operation cannot safely be canceled midway through setup.
    pub(crate) fn track_session_creation_task(
        &self,
        request_id: String,
        join_handle: JoinHandle<()>,
    ) {
        self.creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request_id, join_handle);
    }

    /// Releases a completed request's handle before acknowledging its result.
    /// Completion is emitted after external work, so joining only settles the
    /// task's return. Other requests remain tracked for shutdown.
    pub(crate) async fn finish_session_creation_task(&self, request_id: &str) {
        let task = self
            .creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(request_id);
        if let Some(task) = task
            && let Err(error) = task.await
        {
            warn!(error = %error, "session creation task failed during completion");
        }
    }

    /// Tracks one best-effort cleanup task that should complete before the app
    /// finishes graceful shutdown.
    pub(crate) fn track_cleanup_task(&self, join_handle: JoinHandle<()>) {
        if let Ok(mut cleanup_task_handles) = self.cleanup_task_handles.lock() {
            cleanup_task_handles.push(join_handle);
        }
    }

    /// Settles worktree creation, then waits for tracked cleanup tasks.
    ///
    /// The task list is drained before awaiting so the synchronous mutex guard
    /// is never held across an `.await`. The loop repeats in case a cleanup
    /// task registers additional cleanup work before it exits. Cleanup tasks
    /// share one shutdown deadline; unfinished tasks are canceled after it
    /// expires.
    pub(crate) async fn wait_for_cleanup_tasks(&self) {
        let creation_tasks = self
            .creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in creation_tasks {
            if let Err(error) = task.await {
                warn!(error = %error, "session creation task failed during shutdown");
            }
        }

        wait_for_cleanup_task_handles(
            self.cleanup_task_handles.as_ref(),
            CLEANUP_TASK_SHUTDOWN_TIMEOUT,
        )
        .await;
    }

    /// Returns a clone of the app event sender.
    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.event_tx.clone()
    }

    /// Returns the shared filesystem client for async filesystem operations.
    pub(crate) fn fs_client(&self) -> Arc<dyn FsClient> {
        Arc::clone(&self.fs_client)
    }

    /// Returns the shared git client for async git operations.
    pub(crate) fn git_client(&self) -> Arc<dyn GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns the shared forge review-request client.
    pub(crate) fn review_request_client(&self) -> Arc<dyn ReviewRequestClient> {
        Arc::clone(&self.review_request_client)
    }

    /// Returns the shared per-app session update version counters.
    pub(crate) fn session_update_versions(&self) -> SessionUpdateVersionMap {
        Arc::clone(&self.session_update_versions)
    }

    /// Returns the optional app-server client override used by tests and
    /// injected environments.
    pub(crate) fn app_server_client_override(&self) -> Option<Arc<dyn AppServerClient>> {
        self.app_server_client_override.as_ref().map(Arc::clone)
    }
}

/// Waits for tracked cleanup tasks until one shared deadline, then cancels
/// every unfinished task so terminal shutdown can continue.
async fn wait_for_cleanup_task_handles(
    cleanup_task_handles: &Mutex<Vec<JoinHandle<()>>>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;

    loop {
        let task_handles = cleanup_task_handles
            .lock()
            .map(|mut task_handles| task_handles.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();

        if task_handles.is_empty() {
            break;
        }

        for mut task_handle in task_handles {
            match time::timeout_at(deadline, &mut task_handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    warn!(
                        error = %error,
                        "background cleanup task failed during shutdown"
                    );
                }
                Err(_) => {
                    task_handle.abort();
                    warn!(
                        timeout_seconds = timeout.as_secs(),
                        "background cleanup task exceeded the shutdown deadline and was canceled"
                    );

                    if let Err(error) = task_handle.await
                        && !error.is_cancelled()
                    {
                        warn!(
                            error = %error,
                            "background cleanup task failed while being canceled"
                        );
                    }
                }
            }
        }
    }
}

/// Returns a stable instrumentation label for one app event variant.
fn app_event_label(event: &AppEvent) -> &'static str {
    match event {
        AppEvent::SessionCreationCompleted { .. } => "SessionCreationCompleted",
        AppEvent::AtMentionEntriesLoaded { .. } => "AtMentionEntriesLoaded",
        AppEvent::DiffPreviewLoaded { .. } => "DiffPreviewLoaded",
        AppEvent::SessionDiffLoaded { .. } => "SessionDiffLoaded",
        AppEvent::GitStatusUpdated { .. } => "GitStatusUpdated",
        AppEvent::VersionAvailabilityUpdated { .. } => "VersionAvailabilityUpdated",
        AppEvent::AgentCliVersionsUpdated { .. } => "AgentCliVersionsUpdated",
        AppEvent::UpdateStatusChanged { .. } => "UpdateStatusChanged",
        AppEvent::SessionModelUpdated { .. } => "SessionModelUpdated",
        AppEvent::SessionPersonalityUpdated { .. } => "SessionPersonalityUpdated",
        AppEvent::SessionPermissionModeUpdated { .. } => "SessionPermissionModeUpdated",
        AppEvent::SessionReasoningLevelUpdated { .. } => "SessionReasoningLevelUpdated",
        AppEvent::SessionResponseStyleUpdated { .. } => "SessionResponseStyleUpdated",
        AppEvent::SessionSpeedModeUpdated { .. } => "SessionSpeedModeUpdated",
        AppEvent::RefreshSessions => "RefreshSessions",
        AppEvent::RefreshProjects => "RefreshProjects",
        AppEvent::RefreshGitStatus => "RefreshGitStatus",
        AppEvent::SessionReviewCommentSnapshotLoaded { .. } => "SessionReviewCommentSnapshotLoaded",
        AppEvent::SessionProgressUpdated { .. } => "SessionProgressUpdated",
        AppEvent::SyncMainCompleted { .. } => "SyncMainCompleted",
        AppEvent::SyncMainConflictResolutionStarted { .. } => "SyncMainConflictResolutionStarted",
        AppEvent::SessionDiffStatsUpdated { .. } => "SessionDiffStatsUpdated",
        AppEvent::SessionTitleGenerationFinished { .. } => "SessionTitleGenerationFinished",
        AppEvent::BranchPublishActionCompleted { .. } => "BranchPublishActionCompleted",
        AppEvent::BranchPublishActionResolved { .. } => "BranchPublishActionResolved",
        AppEvent::BranchPublishActionStarted { .. } => "BranchPublishActionStarted",
        AppEvent::SessionQueuedSyncResolved { .. } => "SessionQueuedSyncResolved",
        AppEvent::SessionTurnStarted { .. } => "SessionTurnStarted",
        AppEvent::ReviewPrepared { .. } => "ReviewPrepared",
        AppEvent::ReviewPreparationFailed { .. } => "ReviewPreparationFailed",
        AppEvent::DeferredAutoReviewPersistenceRetry { .. } => "DeferredAutoReviewPersistenceRetry",
        AppEvent::FocusedReviewPersistenceRetry { .. } => "FocusedReviewPersistenceRetry",
        AppEvent::SessionUpdated { .. } => "SessionUpdated",
        AppEvent::AgentResponseReceived { .. } => "AgentResponseReceived",
        AppEvent::StackedParentTurnCompleted { .. } => "StackedParentTurnCompleted",
        AppEvent::StackedParentSyncCompleted { .. } => "StackedParentSyncCompleted",
        AppEvent::StackedParentMergeCompleted { .. } => "StackedParentMergeCompleted",
        AppEvent::SessionWorkflowNoticeUpdated { .. } => "SessionWorkflowNoticeUpdated",
        AppEvent::SessionOrchestrationProgressUpdated { .. } => {
            "SessionOrchestrationProgressUpdated"
        }
        AppEvent::PublishedBranchSyncUpdated { .. } => "PublishedBranchSyncUpdated",
        AppEvent::ReviewRequestStatusUpdated { .. } => "ReviewRequestStatusUpdated",
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use super::*;

    #[test]
    fn app_event_label_names_session_review_comment_snapshot_loads() {
        // Arrange
        let event = AppEvent::SessionReviewCommentSnapshotLoaded {
            request_id: 1,
            result: Err("forge unavailable".to_string()),
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "SessionReviewCommentSnapshotLoaded");
    }

    #[test]
    fn app_event_label_names_diff_preview_loads() {
        // Arrange
        let event = AppEvent::DiffPreviewLoaded {
            path: "README.md".to_string(),
            request_id: 1,
            result: Ok(ag_git::WorktreeFileContent::Missing),
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "DiffPreviewLoaded");
    }

    #[test]
    fn app_event_label_names_session_diff_loads() {
        // Arrange
        let event = AppEvent::SessionDiffLoaded {
            request_id: 1,
            result: Ok("diff".to_string()),
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "SessionDiffLoaded");
    }

    #[test]
    fn app_event_label_names_focused_review_persistence_retries() {
        // Arrange
        let event = AppEvent::FocusedReviewPersistenceRetry {
            retry: crate::app::review::FocusedReviewPersistenceRetry {
                attempt: 1,
                persistence_update: crate::app::review::FocusedReviewPersistence {
                    diff_hash: Some(42),
                    session_id: "session-id".into(),
                    status: crate::domain::review::FocusedReviewStatus::Ready,
                    text: Some("review".to_string()),
                },
            },
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "FocusedReviewPersistenceRetry");
    }

    #[test]
    fn app_event_label_names_deferred_auto_review_persistence_retries() {
        // Arrange
        let event = AppEvent::DeferredAutoReviewPersistenceRetry {
            retry: crate::app::session_diff::DeferredAutoReviewPersistenceRetry {
                attempt: 1,
                session_id: "session-id".into(),
            },
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "DeferredAutoReviewPersistenceRetry");
    }

    #[test]
    fn app_event_label_names_session_diff_stats_updates() {
        // Arrange
        let event = AppEvent::SessionDiffStatsUpdated {
            diff_stats: crate::domain::session::SessionDiffStats::Unknown,
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "SessionDiffStatsUpdated");
    }

    #[test]
    fn app_event_label_names_orchestration_progress_updates() {
        // Arrange
        let event = AppEvent::SessionOrchestrationProgressUpdated {
            progress: Some("Working... protocol: running".to_string()),
            session_id: "controller".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "SessionOrchestrationProgressUpdated");
    }

    #[test]
    fn app_event_label_names_branch_publish_starts() {
        // Arrange
        let event = AppEvent::BranchPublishActionStarted {
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "BranchPublishActionStarted");
    }

    #[test]
    fn app_event_label_names_branch_publish_resolutions() {
        // Arrange
        let event = AppEvent::BranchPublishActionResolved {
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "BranchPublishActionResolved");
    }

    #[test]
    fn app_event_label_names_queued_sync_resolutions() {
        // Arrange
        let event = AppEvent::SessionQueuedSyncResolved {
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "SessionQueuedSyncResolved");
    }

    #[test]
    fn app_event_label_names_turn_starts() {
        // Arrange
        let event = AppEvent::SessionTurnStarted {
            session_id: "session-id".into(),
        };

        // Act
        let label = app_event_label(&event);

        // Assert
        assert_eq!(label, "SessionTurnStarted");
    }

    #[tokio::test]
    async fn creation_completion_releases_handles_and_preserves_pending_work() {
        // Arrange
        let (mut app, _directory) = crate::test_support::new_test_app().await;
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        app.services.track_session_creation_task(
            "pending".to_string(),
            tokio::spawn(async move {
                release_rx.await.expect("release pending creation");
            }),
        );

        for index in 0..64 {
            let request_id = format!("completed-{index}");
            let services = app.services.clone();
            let (settled_tx, mut settled_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                // Joining must not retain the tracker lock.
                let unlocked = services.creation_task_handles.try_lock().is_ok();
                let _ = settled_tx.send(unlocked);
            });
            app.services
                .track_session_creation_task(request_id.clone(), task);

            // Act
            app.complete_session_creations(vec![(request_id, Err("setup failed".to_string()))])
                .await;

            // Assert
            assert!(
                settled_rx
                    .try_recv()
                    .expect("task joined before completion")
            );
            let tasks = app.services.creation_task_handles.lock().expect("tracker");
            assert_eq!(tasks.len(), 1);
            assert!(
                !tasks
                    .get("pending")
                    .expect("pending creation")
                    .is_finished()
            );
        }

        release_tx.send(()).expect("release creation");
        app.services.wait_for_cleanup_tasks().await;
        assert!(
            app.services
                .creation_task_handles
                .lock()
                .expect("tracker")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn creation_completion_releases_canceled_tasks_and_tolerates_duplicates() {
        // Arrange
        let (app, _directory) = crate::test_support::new_test_app().await;
        let task = tokio::spawn(future::pending::<()>());
        task.abort();
        app.services
            .track_session_creation_task("canceled".to_string(), task);

        // Act
        app.services.finish_session_creation_task("canceled").await;
        app.services.finish_session_creation_task("canceled").await;

        // Assert
        assert!(
            app.services
                .creation_task_handles
                .lock()
                .expect("tracker")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn shutdown_settles_creation_before_cleanup() {
        // Arrange
        let (app, _directory) = crate::test_support::new_test_app().await;
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let creation_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.services.track_session_creation_task(
            "pending".to_string(),
            tokio::spawn({
                let creation_finished = Arc::clone(&creation_finished);
                async move {
                    release_rx.await.expect("creation release");
                    creation_finished.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }),
        );

        // Act
        let shutdown = app.services.wait_for_cleanup_tasks();
        tokio::pin!(shutdown);
        let completed_before_release = tokio::select! {
            () = &mut shutdown => true,
            () = time::sleep(Duration::from_millis(25)) => false,
        };
        assert!(!completed_before_release, "shutdown abandoned creation");
        release_tx.send(()).expect("release creation");
        shutdown.await;

        // Assert
        assert!(creation_finished.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            app.services
                .creation_task_handles
                .lock()
                .expect("creation tasks")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn shutdown_observes_canceled_creation_task() {
        // Arrange
        let (app, _directory) = crate::test_support::new_test_app().await;
        let task = tokio::spawn(future::pending::<()>());
        task.abort();
        app.services
            .track_session_creation_task("canceled".to_string(), task);

        // Act
        app.services.wait_for_cleanup_tasks().await;

        // Assert
        assert!(
            app.services
                .creation_task_handles
                .lock()
                .expect("creation tasks")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cleanup_task_wait_cancels_work_after_shared_deadline() {
        // Arrange
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task_handle = tokio::spawn(async move {
            let _ = started_tx.send(());
            future::pending::<()>().await;
        });
        let cleanup_task_handles = Mutex::new(vec![task_handle]);
        started_rx.await.expect("cleanup task should start");

        // Act
        time::timeout(
            Duration::from_secs(1),
            wait_for_cleanup_task_handles(&cleanup_task_handles, Duration::from_millis(25)),
        )
        .await
        .expect("cleanup wait should honor its shared deadline");

        // Assert
        assert!(
            cleanup_task_handles
                .lock()
                .expect("cleanup task mutex should remain available")
                .is_empty()
        );
    }
}
