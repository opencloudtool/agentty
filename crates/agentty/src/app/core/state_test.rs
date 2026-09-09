use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mockall::predicate::eq;
use tempfile::tempdir;

use super::{
    AGENTTY_WT_DIR, AgentCliInfo, AgentKind, AgentSelection, App, AppError, AppEvent,
    AppEventBatch, AppMode, AppServices, BranchPublishTaskContext, BranchPublishTaskFailure,
    BranchPublishTaskSession, ConfirmationViewMode, DiffReviewComments, HashMap, HashSet,
    InputState, PromptModeSnapshot, PublishBranchAction, QuestionProgress, RealFsClient,
    ReviewCacheEntry, ReviewRequestClient, ReviewRequestStatusUpdate, SessionId, SyncMainOutcome,
    SyncReviewRequestTaskResult, SyncSessionStartError, TurnAppliedState, UpdateStatus,
    branch_push_failure, db, detected_forge_kind_from_git_push_error, forge, push_session_branch,
    run_branch_publish_action, session, sync,
};
use crate::app::branch_publish::{BranchPublishActionUpdate, BranchPublishTaskSuccess};
use crate::app::review::ReviewUpdate;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppServiceDeps, Tab, diff_content_hash};
use crate::domain::agent::{AgentModel, ReasoningLevel, SpeedMode};
use crate::domain::composer::PromptAttachment;
use crate::domain::file_entry::FileEntry;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    ForgeKind, PublishedBranchSyncStatus, QueuedMessage, ReviewRequest, ReviewRequestState,
    ReviewRequestSummary, SESSION_DATA_DIR, SessionDiffState, SessionDiffStats,
    SessionFollowUpTask, SessionHandles, SessionRole, SessionSize, SessionStats, Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::setting::SettingName;
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::AppRepositories;
use crate::infra::project_discovery::{HOME_PROJECT_SCAN_MAX_RESULTS, RealProjectDiscoveryClient};
use crate::infra::tmux::{MockTmuxClient, TmuxClient};
use crate::presentation::app_mode::{
    DiffCommentTarget, DiffFocus, DiffLineComments, DiffPreview, DiffSidebarFocus,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::presentation::settings::SettingsAction;
use crate::runtime::mode::diff;

/// Builds one reducer-ready turn projection for tests.
fn test_turn_applied_state(
    questions: Vec<QuestionItem>,
    follow_up_tasks: Vec<&str>,
    token_usage_delta: SessionStats,
) -> TurnAppliedState {
    TurnAppliedState {
        follow_up_tasks: follow_up_tasks
            .into_iter()
            .enumerate()
            .map(|(position, text)| SessionFollowUpTask {
                id: i64::try_from(position).unwrap_or(i64::MAX),
                launched_session_id: None,
                position,
                text: text.to_string(),
            })
            .collect(),
        questions,
        token_usage_delta,
    }
}

fn test_view_app_mode(session_id: &str) -> AppMode {
    AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    }
}

/// Builds one restorable prompt snapshot without attachments.
fn test_prompt_mode_snapshot(session_id: SessionId) -> PromptModeSnapshot {
    PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::with_text("saved reply".to_string()),
        scroll_offset: None,
        session_id,
        slash_state: PromptSlashState::default(),
    }
}

async fn test_app_viewing_reconcile_session(
    status: Status,
    questions: Vec<QuestionItem>,
    folder_name: &str,
) -> App {
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session-1")
            .folder(PathBuf::from(format!("/tmp/{folder_name}")))
            .status(status)
            .questions(questions)
            .build(),
    );
    app.mode = test_view_app_mode("session-1");

    app
}

/// Seeds one materialized session row for project-switching tests.
async fn seed_materialized_session(
    database: &AppRepositories,
    base_path: &Path,
    project_id: i64,
    session_id: &str,
    status: Status,
) {
    database
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gpt56Sol.as_str(),
            "main",
            &status.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert materialized session");
    fs::create_dir_all(session::session_folder(base_path, session_id).join(SESSION_DATA_DIR))
        .expect("failed to create materialized session data dir");
}

/// Seeds one review-ready session and its persisted focused review.
async fn seed_persisted_review_session(
    database: &AppRepositories,
    base_path: &Path,
    project_id: i64,
    session_id: &str,
    diff_hash: &str,
    review_text: &str,
) {
    seed_materialized_session(database, base_path, project_id, session_id, Status::Review).await;
    database
        .sessions()
        .update_session_focused_review(
            session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some(diff_hash.to_string()),
            Some(review_text.to_string()),
        )
        .await
        .expect("failed to persist focused review");
}

/// Inserts one completed focused review into an app cache for eviction tests.
fn insert_test_ready_review(app: &mut App, session_id: &str) {
    app.review_cache.insert(
        session_id.into(),
        ReviewCacheEntry::Ready {
            diff_hash: 1,
            text: "inactive review".to_string(),
        },
    );
}

/// Builds one loading focused-review entry with a stable test profile.
fn test_loading_review(diff_hash: u64) -> ReviewCacheEntry {
    ReviewCacheEntry::Loading {
        diff_hash,
        review_agent: (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            ReasoningLevel::High,
            SpeedMode::Normal,
        ),
    }
}

/// Builds a successful branch-publish batch payload for one session.
fn test_pushed_branch_result(branch_name: &str) -> BranchPublishTaskSuccess {
    BranchPublishTaskSuccess::Pushed {
        branch_name: branch_name.to_string(),
        review_request_creation: None,
        upstream_reference: format!("origin/{branch_name}"),
    }
}

#[tokio::test]
async fn test_new_with_clients_fails_when_no_backend_cli_is_available() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients_with_available_agent_kinds(Vec::new())
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));

    // Act
    let result = App::new_with_clients(base_path.clone(), base_path, None, database, clients).await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message
                == "No supported backend CLI found on `PATH`. Install `codex`, `claude`, `gemini`, or Antigravity CLI 1.1.7 or newer. For an older `agy`, run `agy update`, then restart `agentty`."
    ));
}

#[tokio::test]
async fn merge_session_rejects_linked_review_request_before_queueing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let review_request = ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Linked review request".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    };
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .review_request(Some(review_request))
            .build(),
    );

    // Act
    let result = app.merge_session("session-id").await;

    // Assert
    let error = result.expect_err("linked review request should block merge queueing");
    assert_eq!(
        error.to_string(),
        "Merge cannot run for linked review requests or while another stack session is active"
    );
}

#[tokio::test]
async fn diff_comments_have_no_tick_driven_ui() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(DiffReviewComments::loading(1)),
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };

    // Act
    let has_tick_driven_ui = app.has_visible_tick_driven_ui();

    // Assert
    assert!(!has_tick_driven_ui);
}

#[tokio::test]
async fn queued_session_work_has_tick_driven_ui() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut session = crate::test_support::SessionFixtureBuilder::new()
        .id("session-id")
        .status(Status::Review)
        .build();
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(
            0,
            "sync after this turn".to_string(),
        )),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::SyncQueue,
        turn_position: None,
    });
    app.sessions.push_session(session);
    app.mode = test_view_app_mode("session-id");

    // Act
    let queued_action_has_tick_driven_ui = app.has_visible_tick_driven_ui();
    let session = app
        .sessions
        .sessions_mut()
        .first_mut()
        .expect("queued session should remain available");
    session
        .transient_messages
        .retract(TransientMessageSlot::SyncQueue);
    session.queued_messages.push(QueuedMessage::new(
        1,
        TurnPrompt::from_text("follow up".to_string()),
    ));
    let queued_message_has_tick_driven_ui = app.has_visible_tick_driven_ui();

    // Assert
    assert!(queued_action_has_tick_driven_ui);
    assert!(queued_message_has_tick_driven_ui);
}

#[tokio::test]
async fn session_git_status_targets_include_active_unpublished_sessions() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    let mut done_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-done"));
    done_session.id = "session-2".into();
    done_session.status = Status::Done;
    app.sessions.push_session(review_session);
    app.sessions.push_session(done_session);

    // Act
    let targets = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/session-".to_string(),
            session_id: "session-1".into(),
        }]
    );
}

#[tokio::test]
async fn session_git_status_targets_use_detected_session_branch_name_when_available() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    app.sessions.push_session(review_session);
    app.sessions.replace_session_branch_names(HashMap::from([(
        SessionId::from("session-1"),
        "agentty/session-".to_string(),
    )]));

    // Act
    let targets = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "agentty/session-".to_string(),
            session_id: "session-1".into(),
        }]
    );
}

#[tokio::test]
async fn session_git_status_targets_skip_unmaterialized_drafts() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut draft_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-draft"));
    draft_session.id = "draft-1".into();
    draft_session.is_draft = true;
    draft_session.status = Status::Draft;
    app.sessions.push_session(draft_session);

    // Act
    let targets_before_materialization = App::session_git_status_targets(&app.sessions);
    app.sessions.set_session_worktree_available("draft-1", true);
    let targets_after_materialization = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets_before_materialization,
        [] as [crate::app::sync::SessionGitStatusTarget; 0]
    );
    assert_eq!(
        targets_after_materialization,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/draft-1".to_string(),
            session_id: "draft-1".into(),
        }]
    );
}

#[tokio::test]
async fn test_switch_project_reloads_project_scoped_settings() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .upsert_project_setting(
            first_project_id,
            SettingName::DefaultSmartModel,
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to persist first project smart model");
    database
        .settings()
        .upsert_project_setting(
            first_project_id,
            SettingName::LaunchConfiguration,
            "npm run dev",
        )
        .await
        .expect("failed to persist first project launch configuration");
    database
        .settings()
        .upsert_project_setting(
            second_project_id,
            SettingName::DefaultSmartModel,
            AgentModel::Gpt56Sol.as_str(),
        )
        .await
        .expect("failed to persist second project smart model");
    database
        .settings()
        .upsert_project_setting(
            second_project_id,
            SettingName::LaunchConfiguration,
            "cargo test",
        )
        .await
        .expect("failed to persist second project launch configuration");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let settings_view = app.settings.view();
    let _ = app
        .settings_presentation
        .apply(&settings_view, SettingsAction::Activate);
    assert!(app.settings_presentation.is_selector_dropdown_open());

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert_eq!(
        app.settings.default_smart_selection.model(),
        AgentModel::Gpt56Sol
    );
    assert_eq!(app.settings.launch_configuration, "cargo test");
    assert!(!app.settings_presentation.is_selector_dropdown_open());
    assert_eq!(
        app.settings_presentation
            .snapshot(&app.settings.view())
            .selected_row_index,
        Some(0)
    );
}

#[tokio::test]
async fn test_switch_project_restores_project_scoped_focused_reviews() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let second_session_id = "second-review";
    let loading_session_id = "loading-review";
    let review_text = "## Review\nSecond project finding.";
    seed_persisted_review_session(
        &database,
        &base_path,
        second_project_id,
        second_session_id,
        "42",
        review_text,
    )
    .await;
    seed_persisted_review_session(
        &database,
        &base_path,
        second_project_id,
        loading_session_id,
        "7",
        "outdated persisted review",
    )
    .await;
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(3)
        .returning(|_| Box::pin(async { None }));
    install_mock_git_client(&mut app, mock_git_client);
    app.review_cache
        .insert(loading_session_id.into(), test_loading_review(21));
    app.deferred_auto_review_session_ids
        .insert(loading_session_id.into());
    insert_test_ready_review(&mut app, "inactive-review");

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert!(matches!(
        app.review_cache.get(second_session_id),
        Some(ReviewCacheEntry::Ready { diff_hash: 42, text }) if text == review_text
    ));
    assert!(matches!(
        app.review_cache.get(loading_session_id),
        Some(ReviewCacheEntry::Loading { diff_hash: 21, .. })
    ));
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert!(!app.review_cache.contains_key("inactive-review"));
    assert_eq!(
        app.sessions
            .session_or_err(second_session_id)
            .expect("second-project session should be loaded")
            .transient_messages
            .get(TransientMessageSlot::Review)
            .map(|message| message.body.text()),
        Some(review_text)
    );
    assert!(matches!(
        app.sessions
            .session_or_err(loading_session_id)
            .expect("loading review session should be loaded")
            .transient_messages
            .get(TransientMessageSlot::Review)
            .map(|message| &message.body),
        Some(TransientMessageBody::Loading(_))
    ));
}

#[tokio::test]
async fn test_switch_project_recovers_persisted_deferred_review_after_restart() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let session_id = "pending-review";
    database
        .sessions()
        .insert_session(
            session_id,
            "gpt-5.6-sol",
            "main",
            "Review",
            second_project_id,
        )
        .await
        .expect("failed to insert pending review session");
    fs::create_dir_all(session::session_folder(&base_path, session_id).join(SESSION_DATA_DIR))
        .expect("failed to create pending review session data dir");
    assert!(
        database
            .sessions()
            .defer_session_focused_review(session_id)
            .await
            .expect("failed to persist deferred review")
    );
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(3)
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(std::future::pending()));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert_eq!(app.pending_session_diff_requests.len(), 1);
}

#[tokio::test]
async fn session_chat_history_loads_persisted_transcript_for_unloaded_session() {
    // Arrange
    let (app, _base_dir) = crate::test_support::new_test_app().await;
    let session_id = "unloaded-review-history";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gpt56Sol.as_str(),
            "main",
            "Review",
            app.active_project_id(),
        )
        .await
        .expect("failed to insert unloaded review session");
    app.services
        .db()
        .sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::UserPrompt,
            "Keep the accepted tradeoff",
        )
        .await
        .expect("failed to persist review prompt");
    app.services
        .db()
        .sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::AssistantAnswer,
            "The accepted tradeoff remains in place.",
        )
        .await
        .expect("failed to persist review answer");
    assert!(app.sessions.session_for_id(session_id).is_none());

    // Act
    let session_chat_history = app.session_chat_history(session_id).await;

    // Assert
    assert_eq!(
        session_chat_history.as_deref(),
        Some(" › Keep the accepted tradeoff\n\nThe accepted tradeoff remains in place.\n\n")
    );
}

#[tokio::test]
async fn test_switch_immediately_after_response_recovers_in_progress_review() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    let session_id = "in-progress-completed-review";
    seed_materialized_session(
        &database,
        &base_path,
        first_project_id,
        session_id,
        Status::Review,
    )
    .await;
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let repositories = database.clone();
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    crate::test_support::set_session_status_for_test(&mut app, session_id, Status::InProgress);
    repositories
        .sessions()
        .update_session_status_with_timing_at(session_id, "InProgress", 0)
        .await
        .expect("failed to persist in-progress status");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(6)
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(std::future::pending()));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;
    let deferred_before_switch = app.deferred_auto_review_session_ids.clone();
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch away from completed session");
    repositories
        .sessions()
        .update_session_status_with_timing_at(session_id, "Review", 1)
        .await
        .expect("failed to persist final review status");
    crate::test_support::set_session_status_for_test(&mut app, session_id, Status::Review);
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: 1,
    })
    .await;
    let pending_before_return = app.pending_session_diff_requests.len();
    let pending_after_status_transition = repositories
        .sessions()
        .load_pending_focused_review_session_ids(first_project_id)
        .await
        .expect("failed to load deferred review after status transition");
    app.switch_project(first_project_id)
        .await
        .expect("failed to restore completed session project");

    // Assert
    assert_eq!(
        deferred_before_switch,
        HashSet::from([SessionId::from(session_id)])
    );
    assert_eq!(pending_after_status_transition, [session_id]);
    assert_eq!(pending_before_return, 1);
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert_eq!(app.pending_session_diff_requests.len(), 1);
}

#[tokio::test]
async fn test_switch_project_updates_active_git_upstream_reference() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let second_project_dir = tempdir().expect("failed to create second temp dir");
    let base_path = base_dir.path().to_path_buf();
    let second_project_path = second_project_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("feature/footer-bar".to_string()) }));
    mock_git_client
        .expect_current_upstream_reference()
        .once()
        .returning(|_| Box::pin(async { Ok("origin/feature/footer-bar".to_string()) }));
    mock_git_client
        .expect_find_git_repo_root()
        .times(0..)
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch project");

    // Assert
    assert_eq!(app.git_branch(), Some("feature/footer-bar"));
    assert_eq!(app.git_upstream_ref(), Some("origin/feature/footer-bar"));
}

#[tokio::test]
async fn open_session_review_comments_requires_link_and_applies_background_snapshot() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = SessionId::from("session-review-comments");
    let session = crate::test_support::SessionFixtureBuilder::new()
        .id(session_id.clone())
        .folder(PathBuf::from("/tmp/session-review-comments"))
        .build();
    app.sessions.push_session(session);

    let missing_session_comments =
        app.start_session_review_comment_load(&SessionId::from("missing-session"));
    let unlinked_session_comments = app.start_session_review_comment_load(&session_id);

    let session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-review-comments".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Review comments".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    });
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    install_mock_git_client(&mut app, mock_git_client);
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(forge_remote()));
    mock_review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot()) }));
    install_mock_review_request_client(&mut app, mock_review_request_client);

    // Act
    diff::enter_diff_mode(
        &mut app,
        &session_id,
        "review diff".to_string(),
        None,
        DiffSidebarFocus::Comments,
    );
    wait_for_app_condition(&mut app, |app| {
        matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    is_loading_comments: false,
                    ..
                }),
                ..
            }
        )
    })
    .await;

    // Assert
    assert!(missing_session_comments.is_none());
    assert!(unlinked_session_comments.is_none());
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            ref diff,
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                comment_error: None,
                comment_snapshot: Some(ref snapshot),
                is_loading_comments: false,
                request_id,
                selected_comment_index: 0,
                sidebar_focus: DiffSidebarFocus::Comments,
            }),
            ref session_id,
            scroll_offset: 0,
            ..
        } if selected_comments.is_empty()
            && snapshot == &review_comment_snapshot()
            && diff == "review diff"
            && request_id > 0
            && session_id == "session-review-comments"
    ));
}

#[tokio::test]
/// Ensures startup selection prefers active sessions over archive rows.
async fn test_new_prefers_active_session_for_initial_selection() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    let active_session_id = "z-active-session";
    let archive_session_id = "a-archive-session";
    database
        .sessions()
        .insert_session(
            active_session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert active session");
    database
        .sessions()
        .insert_session(
            archive_session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Done.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert archived session");

    let active_folder_name = active_session_id.chars().take(8).collect::<String>();
    let active_session_data_dir = base_path.join(active_folder_name).join(SESSION_DATA_DIR);
    fs::create_dir_all(active_session_data_dir).expect("failed to create active session dir");

    // Act
    let app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some(active_session_id)
    );
}

#[tokio::test]
async fn test_new_returns_error_when_startup_project_upsert_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query!("DROP TABLE project")
        .execute(&pool)
        .await
        .expect("failed to drop project table");

    // Act
    let error = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .err()
    .expect("expected startup project upsert failure");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Failed to persist startup project")
    );
}

#[tokio::test]
async fn test_new_returns_error_when_startup_active_project_persistence_fails() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query!("DROP TABLE setting")
        .execute(&pool)
        .await
        .expect("failed to drop setting table");

    // Act
    let error = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .err()
    .expect("expected startup active project persistence failure");

    // Assert
    assert!(
        error
            .to_string()
            .contains("Failed to store active startup project")
    );
}

#[tokio::test]
async fn test_new_with_clients_falls_back_from_stale_active_project_and_loads_current_sessions() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let current_project_path = temp_dir.path().join("current-project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(&current_project_path).expect("failed to create current project");
    fs::create_dir_all(current_project_path.join(".git"))
        .expect("failed to create current project git marker");
    let missing_project_path = temp_dir.path().join("missing-project");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let current_project_id = database
        .projects()
        .upsert_project(
            &current_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to insert current project");
    let missing_project_id = database
        .projects()
        .upsert_project(
            &missing_project_path.to_string_lossy(),
            Some("missing".to_string()),
        )
        .await
        .expect("failed to insert missing project");
    database
        .settings()
        .set_active_project_id(missing_project_id)
        .await
        .expect("failed to persist stale active project");
    let current_session_id = "session-current";
    let missing_session_id = "session-missing";
    database
        .sessions()
        .insert_session(
            current_session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            current_project_id,
        )
        .await
        .expect("failed to insert current project session");
    database
        .sessions()
        .insert_session(
            missing_session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            missing_project_id,
        )
        .await
        .expect("failed to insert stale project session");
    let current_session_folder =
        agentty_home.join(current_session_id.chars().take(8).collect::<String>());
    fs::create_dir_all(current_session_folder.join(SESSION_DATA_DIR))
        .expect("failed to create current session folder");

    // Act
    let app = App::new_with_clients(
        agentty_home.clone(),
        current_project_path.clone(),
        Some("main".to_string()),
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(app.active_project_id(), current_project_id);
    assert_eq!(app.working_dir(), current_project_path.as_path());
    assert_eq!(app.git_branch(), Some("main"));
    assert_eq!(
        app.selected_session().map(|session| session.id.as_str()),
        Some(current_session_id)
    );
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, current_session_id);
    let project_items = app.projects.render_parts().project_items;
    assert!(
        project_items
            .iter()
            .any(|item| item.project.id == current_project_id)
    );
    assert!(
        !project_items
            .iter()
            .any(|item| item.project.id == missing_project_id)
    );
}

#[tokio::test]
async fn test_new_with_clients_restores_persisted_active_tab() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let project_path = temp_dir.path().join("project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(project_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await.expect("db should open");
    database
        .settings()
        .upsert_setting(SettingName::ActiveTab, Tab::Settings.as_str())
        .await
        .expect("failed to persist active tab");

    // Act
    let app = App::new_with_clients(
        agentty_home,
        project_path,
        Some("main".to_string()),
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(app.tabs.current(), Tab::Settings);
}

#[tokio::test]
async fn test_new_with_clients_defaults_to_sessions_when_active_project_exists() {
    // Arrange
    let temp_dir = tempdir().expect("failed to create temp dir");
    let agentty_home = temp_dir.path().join("agentty-home");
    let project_path = temp_dir.path().join("project");
    fs::create_dir_all(&agentty_home).expect("failed to create agentty home");
    fs::create_dir_all(project_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&project_path.to_string_lossy(), Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .settings()
        .set_active_project_id(project_id)
        .await
        .expect("failed to persist active project");

    // Act
    let app = App::new_with_clients(
        agentty_home,
        project_path,
        Some("main".to_string()),
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    // Assert
    assert_eq!(app.tabs.current(), Tab::Sessions);
}

#[tokio::test]
async fn test_persist_current_tab_stores_active_tab() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.tabs.set(Tab::Settings);

    // Act
    app.persist_current_tab().await;

    // Assert
    let persisted_tab = app
        .services
        .db()
        .settings()
        .get_setting(SettingName::ActiveTab)
        .await
        .expect("failed to load active tab");
    assert_eq!(persisted_tab.as_deref(), Some(Tab::Settings.as_str()));
}

/// Builds a test app with one selected session, configurable launch
/// configuration, and injected tmux boundary.
async fn new_test_app_with_selected_session(
    session_folder: PathBuf,
    launch_configuration: &str,
    tmux_client: Arc<dyn TmuxClient>,
) -> App {
    // Arrange
    let mut app =
        crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(tmux_client)
            .await;
    if !session_folder.as_os_str().is_empty() {
        std::fs::create_dir_all(&session_folder).expect("failed to create session folder");
    }

    // Act
    app.settings.launch_configuration = launch_configuration.to_string();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            session_folder,
        ));
    app.sessions.select_session_index(Some(0));

    // Assert
    app
}

/// Inserts the selected session into the test database for durable transcript
/// assertions.
async fn persist_selected_session(app: &App) {
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    app.services
        .db()
        .sessions()
        .insert_session("session-1", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
}

/// Persists the selected session with a known-empty diff and aligns its
/// loaded snapshot with that durable state.
async fn seed_selected_session_empty_diff_state(app: &mut App) {
    persist_selected_session(app).await;
    app.services
        .db()
        .sessions()
        .update_session_diff_stats(0, 0, false, "session-1", "XS")
        .await
        .expect("failed to seed empty diff state");
    app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Empty;
}

#[test]
fn branch_publish_inline_helpers_format_copy() {
    // Act
    let loading_label = App::branch_publish_loading_label(PublishBranchAction::Push);
    let success_title = App::branch_publish_success_title(PublishBranchAction::Push);
    let success_message = App::branch_publish_success_message(
        "wt/session-1",
        Some(&crate::app::branch_publish::ReviewRequestCreationInfo {
            forge_kind: forge::ForgeKind::GitHub,
            web_url: Some(
                "https://github.com/org/repo/compare/main...wt%2Fsession-1?expand=1".to_string(),
            ),
        }),
    );
    let fallback_success_message = App::branch_publish_success_message("wt/session-1", None);
    let pull_request_loading_label =
        App::branch_publish_loading_label(PublishBranchAction::PublishPullRequest);
    let pull_request_success_title =
        App::branch_publish_success_title(PublishBranchAction::PublishPullRequest);

    // Assert
    assert_eq!(loading_label, "Pushing branch...");
    assert_eq!(success_title, "Branch pushed");
    assert!(success_message.contains("Pushed session branch `wt/session-1`."));
    assert!(success_message.contains("Open this link to create the pull request"));
    assert!(
        success_message
            .contains("https://github.com/org/repo/compare/main...wt%2Fsession-1?expand=1")
    );
    assert!(fallback_success_message.contains("Create the review request manually"));
    assert_eq!(pull_request_loading_label, "Publishing review request...");
    assert_eq!(pull_request_success_title, "Review request published");
}

/// Verifies generic and authentication-related branch-push failures map
/// to the correct popup severity and current recovery guidance.
#[test]
fn branch_push_failure_maps_blocked_and_failed_errors() {
    // Arrange
    let auth_error = "Git push failed: fatal: could not read Username for 'https://github.com': \
                      terminal prompts disabled";
    let failed_error = "remote rejected";

    // Act
    let blocked = branch_push_failure(PublishBranchAction::Push, auth_error);
    let failed = branch_push_failure(PublishBranchAction::Push, failed_error);

    // Assert
    assert_eq!(blocked.title, "Branch push blocked");
    assert!(blocked.message.contains("Git push requires authentication"));
    assert!(blocked.message.contains("gh auth login"));
    assert_eq!(failed.title, "Branch push failed");
    assert!(
        failed
            .message
            .contains("Failed to publish session branch: remote rejected")
    );
}

/// Verifies pushing a review session surfaces forge-specific git
/// authentication guidance when the remote rejects credentials.
#[tokio::test]
async fn push_session_branch_auth_failure_shows_git_guidance() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq(session::session_branch("session-1")),
        )
        .returning(|_, _| {
            Box::pin(async {
                Err(ag_git::GitError::OutputParse(
                    "Git push failed: fatal: could not read Username for 'https://github.com': \
                     terminal prompts disabled"
                        .to_string(),
                ))
            })
        });
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        None,
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(BranchPublishTaskFailure {
            ref title,
            ref message,
            ..
        }) if title == "Branch push blocked"
            && message.contains("Git push requires authentication")
            && message.contains("gh auth login")
    ));
}

#[tokio::test]
async fn push_session_branch_preserves_blocked_when_remote_branch_exists() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| Box::pin(async { Ok(true) }));
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        Some("feature/existing"),
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("already exists"));
}

#[tokio::test]
async fn push_session_branch_shows_auth_guidance_when_ls_remote_fails_with_auth_error() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(ag_git::GitError::CommandFailed {
                    command: "git ls-remote".to_string(),
                    stderr: "fatal: could not read Username for 'https://github.com/org/repo': \
                             terminal prompts disabled"
                        .to_string(),
                })
            })
        });
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        Some("feature/new"),
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("Git push requires authentication"));
    assert!(failure.message.contains("gh auth login"));
}

#[tokio::test]
async fn branch_publish_task_helpers_reject_unsupported_session_states() {
    // Arrange
    let app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session"));
    review_session.status = Status::Done;
    let done_snapshot = BranchPublishTaskSession::from_session(&review_session);

    // Act
    let push_result = run_branch_publish_action(
        PublishBranchAction::Push,
        BranchPublishTaskContext {
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            session: done_snapshot.clone(),
        },
        app.services.db().clone(),
        app.services.clock(),
        app.services.git_client(),
        app.services.review_request_client(),
        None,
    )
    .await;
    let helper_result = push_session_branch(
        PublishBranchAction::Push,
        &done_snapshot,
        app.services.db().clone(),
        app.services.git_client(),
        None,
    )
    .await;

    // Assert
    assert_eq!(
        push_result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
    assert_eq!(
        helper_result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
}

#[tokio::test]
async fn manual_branch_publish_waits_for_existing_branch_operation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut review_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session"));
    review_session.status = Status::Done;
    app.sessions.push_session(review_session);
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Done));
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err("session-1")
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let branch_publish_context = app
        .branch_publish_task_context("session-1")
        .expect("expected branch-publish context");

    // Act
    let publish_task = tokio::spawn(run_branch_publish_action(
        PublishBranchAction::Push,
        branch_publish_context,
        app.services.db().clone(),
        app.services.clock(),
        app.services.git_client(),
        app.services.review_request_client(),
        None,
    ));
    tokio::task::yield_now().await;
    let waited_for_existing_operation = !publish_task.is_finished();
    drop(existing_operation_guard);
    let result = tokio::time::timeout(Duration::from_secs(1), publish_task)
        .await
        .expect("manual publish should resume after the existing branch operation")
        .expect("manual publish task should not panic");

    // Assert
    assert!(waited_for_existing_operation);
    assert_eq!(
        result,
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::Push,
            "Session must be in review to push the branch.".to_string(),
        ))
    );
}

#[tokio::test]
async fn review_request_enqueue_does_not_wait_for_existing_branch_operation() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let restore_view = ConfirmationViewMode {
        scroll_offset: None,
        session_id: session_id.clone().into(),
    };

    // Act
    let enqueue_result = tokio::time::timeout(
        Duration::from_secs(1),
        app.start_publish_branch_action(
            restore_view,
            &session_id,
            PublishBranchAction::PublishPullRequest,
            None,
        ),
    )
    .await;
    let publish_label = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .map(|message| message.body.text().to_string());
    drop(existing_operation_guard);

    // Assert
    assert!(
        enqueue_result.is_ok(),
        "queueing should not wait for the existing branch operation"
    );
    assert_eq!(
        publish_label.as_deref(),
        Some("Publishing review request...")
    );
}

#[tokio::test]
async fn rebasing_review_request_action_queues_on_existing_worker() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Done);
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("expected session handles")
            .branch_operation_lock,
    );
    let existing_operation_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    app.start_publish_branch_action(
        ConfirmationViewMode {
            scroll_offset: None,
            session_id: session_id.clone().into(),
        },
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Rebasing);

    // Act
    app.start_publish_branch_action(
        ConfirmationViewMode {
            scroll_offset: Some(4),
            session_id: session_id.clone().into(),
        },
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    let publish_body = app.sessions.state().sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::BranchPublish)
        .map(|message| &message.body);

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(4),
        } if viewed_session_id == &session_id
    ));
    assert!(matches!(
        publish_body,
        Some(TransientMessageBody::Queued(action))
            if action.order == 0 && action.text == "review request — publish after this turn"
    ));
    drop(existing_operation_guard);
}

#[tokio::test]
async fn review_request_enqueue_failure_replaces_queued_status_with_error() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: session_id.clone().into(),
    };

    // Act
    app.start_publish_branch_action(
        restore_view,
        &session_id,
        PublishBranchAction::PublishPullRequest,
        None,
    )
    .await;
    let publish_body = app
        .sessions
        .state()
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .and_then(|session| {
            session
                .transient_messages
                .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        })
        .map(|message| message.body.text().to_string());

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(3),
        } if viewed_session_id == &session_id
    ));
    assert!(
        publish_body
            .as_deref()
            .is_some_and(|body| body.contains("**Review request publish failed**"))
    );
    assert!(
        publish_body
            .as_deref()
            .is_some_and(|body| body.contains("active session worker is unavailable"))
    );
}

#[tokio::test]
async fn push_action_still_dispatches_through_background_publish_path() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created");
    crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::InProgress);
    let restore_view = ConfirmationViewMode {
        scroll_offset: Some(5),
        session_id: session_id.clone().into(),
    };
    let pending_git_status_event =
        tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
            .await
            .expect("pending git status event should arrive")
            .expect("app event channel should remain open");
    assert!(matches!(
        pending_git_status_event,
        AppEvent::GitStatusUpdated { .. }
    ));

    // Act
    app.start_publish_branch_action(restore_view, &session_id, PublishBranchAction::Push, None)
        .await;
    let completion_event = tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
        .await
        .expect("background branch publish should complete")
        .expect("app event channel should remain open");
    let publish_label = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .map(|message| message.body.text());

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref viewed_session_id,
            scroll_offset: Some(5),
        } if viewed_session_id == &session_id
    ));
    assert_eq!(publish_label, Some("Pushing branch..."));
    assert!(matches!(
        completion_event,
        AppEvent::BranchPublishActionCompleted {
            result,
            session_id: completed_session_id,
        } if completed_session_id == session_id
            && matches!(
                *result,
                Err(BranchPublishTaskFailure { ref message, .. })
                    if message == "Session must be in review to push the branch."
            )
    ));
}

#[tokio::test]
async fn branch_publish_task_context_targets_stacked_parent_review_source_branch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut parent_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/parent-session"));
    parent_session.id = "parent-session".into();
    parent_session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#12".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "review/parent-session".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Parent review".to_string(),
            web_url: "https://github.com/org/repo/pull/12".to_string(),
        },
    });
    let mut child_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/child-session"));
    child_session.id = "child-session".into();
    child_session.base_branch = session::session_branch("parent-session");
    child_session.parent_session_id = Some("parent-session".into());
    app.sessions.push_session(parent_session);
    app.sessions.push_session(child_session);
    app.sessions
        .session_handles_mut()
        .insert("child-session".into(), SessionHandles::new(Status::Review));

    // Act
    let branch_publish_context = app
        .branch_publish_task_context("child-session")
        .expect("expected branch-publish context");

    // Assert
    assert_eq!(
        branch_publish_context.session.base_branch,
        "review/parent-session"
    );
    let session_lock = &app
        .sessions
        .session_handles_or_err("child-session")
        .expect("expected child session handles")
        .branch_operation_lock;
    assert!(Arc::ptr_eq(
        &branch_publish_context.branch_operation_lock,
        session_lock
    ));
}

#[tokio::test]
async fn branch_publish_task_context_targets_stacked_parent_upstream_branch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut parent_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/parent-session"));
    parent_session.id = "parent-session".into();
    parent_session.published_upstream_ref = Some("origin/review/parent-custom".to_string());
    let mut child_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/child-session"));
    child_session.id = "child-session".into();
    child_session.base_branch = session::session_branch("parent-session");
    child_session.parent_session_id = Some("parent-session".into());
    app.sessions.push_session(parent_session);
    app.sessions.push_session(child_session);
    app.sessions
        .session_handles_mut()
        .insert("child-session".into(), SessionHandles::new(Status::Review));

    // Act
    let branch_publish_context = app
        .branch_publish_task_context("child-session")
        .expect("expected branch-publish context");

    // Assert
    assert_eq!(
        branch_publish_context.session.base_branch,
        "review/parent-custom"
    );
}

#[tokio::test]
async fn push_session_branch_uses_custom_remote_branch_name_when_provided() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_push_current_branch_to_new_remote_branch()
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq("review/custom-branch".to_string()),
        )
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/review/custom-branch".to_string()) }));
    mock_git_client
        .expect_repo_url()
        .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
        .once()
        .returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database.clone(),
        git_client,
        Some("review/custom-branch"),
    )
    .await;

    // Assert
    assert_eq!(
            result,
            Ok(BranchPublishTaskSuccess::Pushed {
                branch_name: "review/custom-branch".to_string(),
                review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                    forge_kind: forge::ForgeKind::GitHub,
                    web_url: Some(
                        "https://github.com/agentty-xyz/agentty/compare/main...review%2Fcustom-branch?expand=1"
                            .to_string()
                    ),
                }),
                upstream_reference: "origin/review/custom-branch".to_string(),
            })
        );
}

#[tokio::test]
async fn push_session_branch_succeeds_without_review_request_link_for_unsupported_remote() {
    // Arrange
    let branch_session = BranchPublishTaskSession::from_session(
        &crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/review-session")),
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some(session::session_branch("session-1")) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .with(
            mockall::predicate::eq(PathBuf::from("/tmp/review-session")),
            mockall::predicate::eq(session::session_branch("session-1")),
        )
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-1".to_string()) }));
    mock_git_client
        .expect_repo_url()
        .with(mockall::predicate::eq(PathBuf::from("/tmp/review-session")))
        .once()
        .returning(|_| Box::pin(async { Ok("https://example.com/team/project.git".to_string()) }));
    let git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let database = crate::infra::db::AppRepositories::in_memory()
        .await
        .expect("db should open");

    // Act
    let result = push_session_branch(
        PublishBranchAction::Push,
        &branch_session,
        database,
        git_client,
        None,
    )
    .await;

    // Assert
    assert_eq!(
        result,
        Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: session::session_branch("session-1"),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        })
    );
}

#[tokio::test]
async fn apply_app_events_branch_publish_action_sets_inline_success() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: Some(crate::app::branch_publish::ReviewRequestCreationInfo {
                forge_kind: forge::ForgeKind::GitHub,
                web_url: Some(
                    "https://github.com/agentty-xyz/agentty/compare/main...wt%2Fsession-1?expand=1"
                        .to_string(),
                ),
            }),
            upstream_reference: "origin/wt/session-1".to_string(),
        })),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    let publish_message = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .expect("branch publish result should be visible inline")
        .body
        .text();
    assert!(publish_message.contains("Branch pushed"));
    assert!(publish_message.contains("Pushed session branch `wt/session-1`."));
    assert!(
        publish_message.contains(
            "https://github.com/agentty-xyz/agentty/compare/main...wt%2Fsession-1?expand=1"
        )
    );
    assert_eq!(
        app.sessions
            .state()
            .sessions()
            .first()
            .and_then(|session| session.published_upstream_ref.as_deref()),
        Some("origin/wt/session-1")
    );
}

#[tokio::test]
async fn apply_branch_publish_action_persists_result_for_unloaded_project() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Review));
    app.sessions.state_mut().replace_sessions(Vec::new());

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert!(persisted_messages[0].content.contains("Branch pushed"));
    assert!(
        persisted_messages[0]
            .content
            .contains("Pushed session branch `wt/session-1`.")
    );
    {
        let live_transcript = app
            .sessions
            .session_handles()
            .get("session-1")
            .expect("session handles should remain loaded")
            .transcript
            .lock()
            .expect("session transcript lock should succeed");
        assert_eq!(
            live_transcript
                .messages()
                .last()
                .map(|message| message.content.as_str()),
            Some(persisted_messages[0].content.as_str())
        );
    }

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "remote rejected".to_string(),
        )),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 2);
    assert_eq!(
        persisted_messages[1].content,
        "**Review request publish failed**\n\nremote rejected"
    );
}

#[tokio::test]
async fn apply_branch_publish_action_update_persists_pull_request_notice() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Review));
    seed_completed_review_transient_message(&mut app);
    app.mode = AppMode::List;
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 55,
        summary: crate::domain::session::ReviewRequestSummary {
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request: review_request.clone(),
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert_review_message_reanchored_after_publish(&app);
    let transcript = app.sessions.state().sessions()[0]
        .transcript
        .as_ref()
        .expect("review request notice should be appended to transcript");
    let transcript_notice = transcript
        .messages()
        .last()
        .expect("review request notice should be present");
    assert_eq!(transcript_notice.kind, SessionMessageKind::WorkflowNotice);
    assert_eq!(
        transcript_notice.content,
        "\n[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42\n"
    );
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(persisted_messages[0].content, transcript_notice.content);
    {
        let handles = app
            .sessions
            .session_handles()
            .get("session-1")
            .expect("session handles should exist");
        let mut live_transcript = handles
            .transcript
            .lock()
            .expect("session transcript lock should succeed");
        live_transcript.append_message(SessionMessageKind::UserPrompt, "continue the session");
    }
    app.sessions
        .state_mut()
        .sync_session_from_handle("session-1");
    let messages_after_new_turn = app.sessions.state().sessions()[0]
        .transcript
        .as_ref()
        .expect("session transcript should remain available")
        .messages();
    assert_eq!(messages_after_new_turn.len(), 2);
    assert_eq!(
        messages_after_new_turn[0].kind,
        SessionMessageKind::WorkflowNotice
    );
    assert_eq!(
        messages_after_new_turn[1].kind,
        SessionMessageKind::UserPrompt
    );
    assert_eq!(
        app.sessions
            .state()
            .sessions()
            .first()
            .and_then(|session| session.review_request.clone()),
        Some(review_request)
    );
}

#[tokio::test]
async fn apply_branch_publish_action_persists_review_request_for_unloaded_project() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Review));
    app.sessions.state_mut().replace_sessions(Vec::new());
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 55,
        summary: crate::domain::session::ReviewRequestSummary {
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request,
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].content,
        "\n[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42\n"
    );
}

fn seed_completed_review_transient_message(app: &mut App) {
    app.sessions.state_mut().sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Markdown(
                "## Review\n\nReview completed before publishing.".to_string(),
            ),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
}

fn assert_review_message_reanchored_after_publish(app: &App) {
    let transient_messages = &app.sessions.state().sessions()[0].transient_messages;

    assert!(
        transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
    assert_eq!(
        transient_messages
            .get(TransientMessageSlot::Review)
            .expect("completed review should remain visible")
            .anchor,
        TransientMessageAnchor::AfterCompletedTurn
    );
}

#[tokio::test]
async fn apply_branch_publish_started_replaces_queued_label() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.queue_branch_publish(
        "session-1",
        0,
        "review request — publish after this turn".to_string(),
    );

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionStarted {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
            .map(|message| message.body.text()),
        Some("Publishing review request...")
    );
}

#[tokio::test]
async fn apply_branch_publish_resolved_retracts_waiting_row() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.queue_branch_publish(
        "session-1",
        0,
        "review request — publish after this turn".to_string(),
    );

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionResolved {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
}

#[tokio::test]
async fn apply_queued_sync_resolved_retracts_waiting_row() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.queue_session_sync("session-1", 0);

    // Act
    app.apply_app_events(AppEvent::SessionQueuedSyncResolved {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .is_none()
    );
}

#[tokio::test]
async fn apply_turn_started_clears_saved_diff_comments() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
    app.save_diff_comment_progress("session-1".into(), line_comments);

    // Act
    app.apply_app_events(AppEvent::SessionTurnStarted {
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(!app.diff_comment_progress.contains_key("session-1"));
}

#[tokio::test]
async fn apply_branch_publish_action_update_persists_gitlab_merge_request_notice() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .start_branch_publish("session-1", "Publishing review request...".to_string());
    app.mode = AppMode::List;
    let review_request = crate::domain::session::ReviewRequest {
        last_refreshed_at: 77,
        summary: crate::domain::session::ReviewRequestSummary {
            display_id: "!24".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "wt/session-1".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Add GitLab support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
        },
    };

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request,
            upstream_reference: "origin/wt/session-1".to_string(),
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(
        app.sessions.state().sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
            .is_none()
    );
    let transcript_notice = app.sessions.state().sessions()[0]
        .transcript
        .as_ref()
        .and_then(|transcript| transcript.messages().last())
        .expect("merge request notice should be appended to transcript");
    assert_eq!(transcript_notice.kind, SessionMessageKind::WorkflowNotice);
    assert_eq!(
        transcript_notice.content,
        "\n[Review Request] Created MR \
         https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24\n"
    );
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 1);
    assert_eq!(
        persisted_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(persisted_messages[0].content, transcript_notice.content);
}

#[tokio::test]
async fn apply_branch_publish_action_update_keeps_active_mode_on_failure() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_branch_publish_action_update(BranchPublishActionUpdate {
        result: Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "remote rejected".to_string(),
        )),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    let publish_message = app.sessions.state().sessions()[0]
        .transient_messages
        .get(crate::domain::transient_message::TransientMessageSlot::BranchPublish)
        .expect("review request publish failure should be visible inline");
    assert_eq!(
        publish_message.body,
        TransientMessageBody::Markdown(
            "**Review request publish failed**\n\nremote rejected".to_string()
        )
    );
}

#[tokio::test]
async fn configured_launch_configurations_returns_trimmed_non_empty_entries() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.settings.launch_configuration = "  cargo test \n npm run dev \n".to_string();

    // Act
    let launch_configurations = app.configured_launch_configurations();

    // Assert
    assert_eq!(
        launch_configurations,
        vec!["cargo test".to_string(), "npm run dev".to_string()]
    );
}

#[tokio::test]
async fn open_session_worktree_in_tmux_runs_configured_launch_configuration_when_window_opens() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-launch-configuration");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client
        .expect_run_command_in_window()
        .with(eq("@42".to_string()), eq("npm run dev".to_string()))
        .times(1)
        .returning(|_, _| Box::pin(async {}));
    let mut app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-launch-configuration"),
        "  npm run dev  ",
        Arc::new(mock_tmux_client),
    )
    .await;
    seed_selected_session_empty_diff_state(&mut app).await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    let persisted_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load persisted session");
    let persisted_session = persisted_sessions
        .iter()
        .find(|session| session.id == "session-1")
        .expect("missing persisted session");
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Unknown
    );
    assert_eq!(persisted_session.has_diff, None);
}

#[tokio::test]
async fn open_session_worktree_in_tmux_keeps_invalidation_when_window_open_fails() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-window-open-failure");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder.clone()))
        .times(1)
        .returning(|_| Box::pin(async { None }));
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = new_test_app_with_selected_session(
        session_folder,
        "npm run dev",
        Arc::new(mock_tmux_client),
    )
    .await;
    seed_selected_session_empty_diff_state(&mut app).await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    let persisted_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load persisted session");
    let persisted_session = persisted_sessions
        .iter()
        .find(|session| session.id == "session-1")
        .expect("missing persisted session");
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Unknown
    );
    assert_eq!(persisted_session.has_diff, None);
}

#[tokio::test]
async fn open_session_worktree_in_tmux_is_disabled_outside_tmux() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let session_folder = temp_dir.path().join("session-worktree");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client.expect_open_window_for_folder().times(0);
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = new_test_app_with_selected_session(
        session_folder,
        "cargo test",
        Arc::new(mock_tmux_client),
    )
    .await;
    app.is_tmux_session = false;
    app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Empty;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Empty
    );
}

#[tokio::test]
async fn open_session_worktree_in_tmux_skips_launch_configuration_when_setting_is_blank() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-empty-launch-configuration");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-empty-launch-configuration"),
        "   ",
        Arc::new(mock_tmux_client),
    )
    .await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
}

#[tokio::test]
async fn open_session_worktree_in_tmux_skips_missing_worktree_folder() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let missing_session_folder = temp_dir.path().join("missing-session-worktree");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client.expect_open_window_for_folder().times(0);
    mock_tmux_client.expect_run_command_in_window().times(0);
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(mock_tmux_client),
    )
    .await;
    app.settings.launch_configuration = "npm run dev".to_string();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            missing_session_folder,
        ));
    app.sessions.select_session_index(Some(0));

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
}

#[tokio::test]
async fn open_session_worktree_in_tmux_uses_first_configured_command() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-multiple-launch-configurations");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client
        .expect_open_window_for_folder()
        .with(eq(session_folder))
        .times(1)
        .returning(|_| Box::pin(async { Some("@42".to_string()) }));
    mock_tmux_client
        .expect_run_command_in_window()
        .with(eq("@42".to_string()), eq("cargo test".to_string()))
        .times(1)
        .returning(|_, _| Box::pin(async {}));
    let mut app = new_test_app_with_selected_session(
        PathBuf::from("/tmp/session-multiple-launch-configurations"),
        " cargo test \n npm run dev ",
        Arc::new(mock_tmux_client),
    )
    .await;

    // Act
    app.open_session_worktree_in_tmux().await;

    // Assert
    // Expectations are validated by `mockall`.
}

#[tokio::test]
async fn open_session_worktree_in_tmux_stays_closed_when_persistence_fails() {
    // Arrange
    let (mut app, base_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let session_folder = base_dir.path().join("session-persistence-failure");
    fs::create_dir_all(&session_folder).expect("failed to create session folder");
    let mut mock_tmux_client = MockTmuxClient::new();
    mock_tmux_client.expect_open_window_for_folder().times(0);
    mock_tmux_client.expect_run_command_in_window().times(0);
    app.tmux_client = Arc::new(mock_tmux_client);
    app.is_tmux_session = true;
    let mut session = crate::test_support::session_fixture_with_folder(session_folder);
    session.stats.diff_state = SessionDiffState::Empty;
    app.sessions.push_session(session);
    app.sessions.select_session_index(Some(0));
    pool.close().await;

    // Act
    app.open_session_worktree_in_tmux_with_command(None).await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].stats.diff_state,
        SessionDiffState::Empty
    );
}

#[tokio::test]
async fn apply_app_events_sync_conflicts_updates_non_modal_status() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let operation = sync::ProjectSyncContext {
        default_branch: "develop".to_string(),
        operation_id: 7,
        project_id: app.active_project_id(),
        project_name: "agentty".to_string(),
    };
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: operation.clone(),
        phase: sync::ProjectSyncPhase::Running,
    });
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string(), "README.md".to_string()],
        operation,
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status,
        Some(sync::ProjectSyncStatus {
            phase: sync::ProjectSyncPhase::ResolvingConflicts {
                conflicted_file_count: 2
            },
            ..
        })
    ));
}

#[tokio::test]
async fn apply_app_events_stale_sync_conflict_does_not_replace_live_status() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let live_operation = sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 8,
        project_id: app.active_project_id(),
        project_name: "agentty".to_string(),
    };
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: live_operation.clone(),
        phase: sync::ProjectSyncPhase::Running,
    });
    let mut stale_operation = live_operation;
    stale_operation.operation_id = 7;

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string()],
        operation: stale_operation,
    })
    .await;

    // Assert
    assert!(matches!(
        app.project_sync_status,
        Some(sync::ProjectSyncStatus {
            phase: sync::ProjectSyncPhase::Running,
            ..
        })
    ));
}

#[tokio::test]
async fn apply_app_events_sync_conflict_without_running_status_is_ignored() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let operation = sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 7,
        project_id: app.active_project_id(),
        project_name: "agentty".to_string(),
    };

    // Act
    app.apply_app_events(AppEvent::SyncMainConflictResolutionStarted {
        conflicted_files: vec!["src/lib.rs".to_string()],
        operation,
    })
    .await;

    // Assert
    assert!(app.project_sync_status.is_none());
}

#[tokio::test]
async fn project_sync_completion_preserves_the_active_navigation_mode() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.active_project_id();
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 0,
    };

    // Act
    app.apply_app_events(successful_manual_sync(project_id, "main", 2))
        .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectSwitcher {
            selected_option_index: 0
        }
    ));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            pulled_commits: Some(2),
            ..
        })
    ));
    assert!(app.project_sync_status_expires_at.is_some());
}

#[tokio::test]
async fn project_sync_terminal_status_expires_at_its_deadline() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.active_project_id();
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.apply_app_events(successful_manual_sync(project_id, "main", 2))
        .await;
    let expires_at = app
        .project_sync_status_expires_at
        .expect("terminal sync status should have an expiry");

    // Act
    app.expire_project_sync_status(
        expires_at
            .checked_sub(Duration::from_millis(1))
            .expect("sync status expiry should be after the monotonic clock origin"),
    );

    // Assert
    assert!(app.project_sync_status.is_some());

    // Act
    app.clear_redraw();
    app.expire_project_sync_status(expires_at);

    // Assert
    assert!(app.project_sync_status.is_none());
    assert!(app.project_sync_status_expires_at.is_none());
    assert!(app.needs_redraw());
}

#[tokio::test]
async fn project_sync_running_status_has_no_expiry() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(1)
        .returning(|_, _, _, _| {});
    app.sync_main_runner = Arc::new(sync_main_runner);

    // Act
    app.start_sync_main();
    let future = app.services.clock().now_instant() + Duration::from_secs(60);
    app.expire_project_sync_status(future);

    // Assert
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Running)
    ));
    assert!(app.project_sync_status_expires_at.is_none());
}

#[tokio::test]
async fn project_sync_completion_applies_captured_review_updates() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.active_project_id();
    let operation = sync::ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 1,
        project_id,
        project_name: "agentty".to_string(),
    };
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: operation.clone(),
        phase: sync::ProjectSyncPhase::Running,
    });
    app.latest_project_sync_operation_ids.insert(project_id, 1);
    let mut completion = successful_manual_sync_completion(project_id, "main", 2);
    completion.review_request_updates = vec![sync::SyncMainReviewUpdate {
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
            summary: None,
        }),
        session_id: "missing-session".into(),
    }];

    // Act
    app.apply_app_events(AppEvent::SyncMainCompleted { completion })
        .await;

    // Assert
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete { .. })
    ));
}

#[tokio::test]
async fn superseded_project_sync_completions_do_not_reconcile_session_state() {
    // Arrange
    let (mut app, _pool, _base_dir) = new_test_app_with_database_pool().await;
    let project_id = app.active_project_id();
    let session_id = "superseded-sync-session";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gemini38Flash.as_str(),
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert review session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    fs::create_dir_all(
        app.services
            .base_path()
            .join(session_folder_name)
            .join(SESSION_DATA_DIR),
    )
    .expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    app.latest_project_sync_operation_ids.insert(project_id, 2);
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 2,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    let mut stale_completion = successful_manual_sync_completion(project_id, "main", 1);
    stale_completion.review_request_updates = vec![sync::SyncMainReviewUpdate {
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    }];

    // Act
    app.apply_app_events(AppEvent::SyncMainCompleted {
        completion: stale_completion.clone(),
    })
    .await;
    app.pending_project_sync_completions
        .insert(project_id, stale_completion);
    app.apply_pending_project_sync_completion().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Review);
    assert!(matches!(
        app.project_sync_status.as_ref(),
        Some(sync::ProjectSyncStatus {
            context,
            phase: sync::ProjectSyncPhase::Running,
        }) if context.operation_id == 2
    ));
}

#[tokio::test]
async fn stale_project_sync_completion_does_not_replace_newer_deferred_completion() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let inactive_project_id = app.active_project_id() + 1;
    app.latest_project_sync_operation_ids
        .insert(inactive_project_id, 2);
    let mut newer_completion = successful_manual_sync_completion(inactive_project_id, "main", 2);
    newer_completion.operation.operation_id = 2;
    app.pending_project_sync_completions
        .insert(inactive_project_id, newer_completion);

    // Act
    app.apply_app_events(successful_manual_sync(inactive_project_id, "main", 1))
        .await;

    // Assert
    assert!(matches!(
        app.pending_project_sync_completions
            .get(&inactive_project_id),
        Some(sync::SyncMainCompletion { operation, .. })
            if operation.operation_id == 2
    ));
}

#[tokio::test]
async fn project_sync_blocks_only_base_checkout_operations_for_its_project() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let session_id = app
        .create_session()
        .await
        .expect("session should be created before sync");
    let project_id = app.active_project_id();
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    let session = app
        .sessions
        .state_mut()
        .session_mut_for_id(&session_id)
        .expect("session should remain loaded");
    session.is_draft = true;
    session.status = Status::Draft;
    insert_test_ready_review(&mut app, &session_id);

    // Act
    let create_result = app.create_session().await;
    let draft_result = app.create_draft_session().await;
    let start_result = app.start_session(&session_id, "continue").await;
    let staged_start_result = app.start_staged_session(&session_id).await;
    let merge_result = app.merge_session(&session_id).await;
    let rebase_result = app.rebase_session(&session_id).await;
    let unrelated_project_result = app.ensure_project_checkout_available(project_id + 1);

    // Assert
    for result in [
        create_result.map(|_| ()),
        draft_result.map(|_| ()),
        start_result,
        staged_start_result,
        merge_result,
        rebase_result,
    ] {
        assert!(matches!(
            result,
            Err(AppError::Workflow(message))
                if message.contains("is synchronizing `main`")
        ));
    }
    assert!(unrelated_project_result.is_ok());
    assert!(app.review_cache.contains_key(session_id.as_str()));
}

#[tokio::test]
async fn project_sync_completion_reconciles_after_switching_back_to_its_project() {
    // Arrange
    let first_project_dir = tempdir().expect("failed to create first project dir");
    let second_project_dir = tempdir().expect("failed to create second project dir");
    let first_project_path = first_project_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let first_project_id = database
        .projects()
        .upsert_project(&first_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert first project");
    let second_project_id = database
        .projects()
        .upsert_project(&second_project_dir.path().to_string_lossy(), None)
        .await
        .expect("failed to insert second project");
    database
        .settings()
        .set_active_project_id(first_project_id)
        .await
        .expect("failed to persist initial active project");
    let mut app = App::new_with_clients(
        first_project_path.clone(),
        first_project_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id: first_project_id,
            project_name: "first".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.switch_project(second_project_id)
        .await
        .expect("failed to switch away from syncing project");

    // Act
    app.apply_app_events(successful_manual_sync(first_project_id, "main", 3))
        .await;
    let was_deferred = app
        .pending_project_sync_completions
        .contains_key(&first_project_id);
    app.switch_project(first_project_id)
        .await
        .expect("failed to switch back to synced project");

    // Assert
    assert!(was_deferred);
    assert!(
        !app.pending_project_sync_completions
            .contains_key(&first_project_id)
    );
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            pulled_commits: Some(3),
            ..
        })
    ));
}

#[tokio::test]
async fn project_sync_queues_other_project_and_coalesces_duplicate_request() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let first_project_id = app.active_project_id();
    let second_project_id = first_project_id + 1;
    let started_project_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_project_ids = Arc::clone(&started_project_ids);
    let mut sync_main_runner = crate::app::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(2)
        .returning(move |_, operation, _, _| {
            captured_project_ids
                .lock()
                .expect("started project ids lock should remain available")
                .push(operation.project_id);
        });
    app.sync_main_runner = Arc::new(sync_main_runner);

    app.start_sync_main();
    let mut second_project_context = app.sync_handle.context_snapshot();
    second_project_context.project_id = second_project_id;
    second_project_context.project_name = "second-project".to_string();
    app.sync_handle.publish_context(second_project_context);

    // Act
    app.start_sync_main();
    app.start_sync_main();
    let queued_request_count = app.pending_project_sync_requests.len();
    app.apply_app_events(successful_manual_sync(first_project_id, "main", 1))
        .await;

    // Assert
    assert_eq!(queued_request_count, 1);
    assert!(app.pending_project_sync_requests.is_empty());
    assert_eq!(
        *started_project_ids
            .lock()
            .expect("started project ids lock should remain available"),
        vec![first_project_id, second_project_id]
    );
    assert!(matches!(
        app.project_sync_status.as_ref(),
        Some(sync::ProjectSyncStatus {
            context,
            phase: sync::ProjectSyncPhase::Running,
        }) if context.project_id == second_project_id
    ));
}

#[tokio::test]
async fn project_sync_is_blocked_while_merge_work_is_pending() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::MockSyncMainRunner::new();
    sync_main_runner.expect_start_sync_main().times(0);
    app.sync_main_runner = Arc::new(sync_main_runner);
    app.merge_queue.enqueue("pending-merge".into());

    // Act
    app.start_sync_main();

    // Assert
    assert!(app.merge_queue.is_queued_or_active("pending-merge"));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Blocked { message })
            if message.contains("merge is active or queued")
    ));
    assert!(app.project_sync_status_expires_at.is_some());
}

#[tokio::test]
async fn merge_queue_drain_waits_for_active_project_sync() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    app.project_sync_status = Some(sync::ProjectSyncStatus {
        context: sync::ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id: app.active_project_id(),
            project_name: "test-project".to_string(),
        },
        phase: sync::ProjectSyncPhase::Running,
    });
    app.merge_queue.enqueue("pending-merge".into());

    // Act
    let result = app.start_next_merge_from_queue(false).await;

    // Assert
    assert!(result.is_ok());
    assert!(app.merge_queue.is_queued_or_active("pending-merge"));
    assert!(!app.merge_queue.has_active());
}

#[tokio::test]
async fn project_sync_scheduler_keeps_requests_queued_while_slots_are_occupied() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(1)
        .returning(|_, _, _, _| {
            // Keep the first operation running so the queued request can be
            // inspected.
        });
    app.sync_main_runner = Arc::new(sync_main_runner);
    app.start_sync_main();
    let mut second_project_context = app.sync_handle.context_snapshot();
    second_project_context.project_id = app.active_project_id() + 1;
    second_project_context.project_name = "second-project".to_string();
    app.sync_handle.publish_context(second_project_context);
    app.start_sync_main();

    // Act
    app.start_next_project_sync_from_queue();
    app.resume_base_checkout_work().await;
    let queued_during_sync = app.pending_project_sync_requests.len();
    app.project_sync_status = None;
    app.merge_queue.set_active("active-merge".into());
    app.resume_base_checkout_work().await;

    // Assert
    assert_eq!(queued_during_sync, 1);
    assert_eq!(app.pending_project_sync_requests.len(), 1);
    assert!(app.merge_queue.has_active());
}

#[test]
fn sync_push_auth_error_detects_github_from_prompt_url() {
    // Arrange
    let detail =
        "Git push failed: fatal: could not read Password for 'https://github.com/team/project': \
         terminal prompts disabled\nConfigured remotes:\n  github.com";

    // Act
    let forge_kind = detected_forge_kind_from_git_push_error(detail);

    // Assert
    assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
}

#[test]
fn sync_push_auth_error_prefers_github_when_fallback_markers_are_ambiguous() {
    // Arrange
    let detail = "Git push failed: authentication failed. Configure remotes:\n  github.com";

    // Act
    let forge_kind = detected_forge_kind_from_git_push_error(detail);

    // Assert
    assert_eq!(forge_kind, Some(forge::ForgeKind::GitHub));
}

#[test]
fn app_event_batch_collect_event_keeps_latest_at_mention_entries_update() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    let first_entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];
    let second_entries = vec![FileEntry {
        is_dir: true,
        path: "crates".to_string(),
    }];

    // Act
    event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
        entries: first_entries,
        session_id: "session-1".into(),
    });
    event_batch.collect_event(AppEvent::AtMentionEntriesLoaded {
        entries: second_entries.clone(),
        session_id: "session-1".into(),
    });

    // Assert
    assert_eq!(
        event_batch
            .at_mention_entries_updates
            .get("session-1")
            .cloned(),
        Some(second_entries)
    );
}

#[tokio::test]
async fn app_event_applies_at_mention_entries_to_session_runtime() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("missing-session");
    let lookup_root = app.at_mention_lookup_root(&session_id);
    let entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];

    // Act
    app.apply_app_events(AppEvent::AtMentionEntriesLoaded {
        entries: entries.clone(),
        session_id,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.at_mention_index_for_root(&lookup_root),
        Some(entries)
    );
}

#[tokio::test]
async fn at_mention_lookup_root_uses_nearest_materialized_stacked_ancestor() {
    // Arrange
    let (mut app, temp_dir) = crate::test_support::new_test_app().await;
    let ancestor_folder = temp_dir.path().join("materialized-ancestor");
    fs::create_dir(&ancestor_folder).expect("failed to create ancestor folder");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("ancestor-session")
            .folder(ancestor_folder.clone())
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("unmaterialized-parent")
            .folder(temp_dir.path().join("missing-parent"))
            .parent_session_id(Some(SessionId::from("ancestor-session")))
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("unmaterialized-child")
            .folder(temp_dir.path().join("missing-child"))
            .parent_session_id(Some(SessionId::from("unmaterialized-parent")))
            .build(),
    );

    // Act
    let lookup_root = app.at_mention_lookup_root("unmaterialized-child");

    // Assert
    assert_eq!(lookup_root, ancestor_folder);
}

#[tokio::test]
async fn at_mention_lookup_root_falls_back_for_cyclic_parent_chain() {
    // Arrange
    let (mut app, temp_dir) = crate::test_support::new_test_app().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("first-session")
            .folder(temp_dir.path().join("missing-first"))
            .parent_session_id(Some(SessionId::from("second-session")))
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("second-session")
            .folder(temp_dir.path().join("missing-second"))
            .parent_session_id(Some(SessionId::from("first-session")))
            .build(),
    );

    // Act
    let lookup_root = app.at_mention_lookup_root("first-session");

    // Assert
    assert_eq!(lookup_root, app.working_dir());
}

#[test]
/// Verifies repeated `AgentResponseReceived` events keep the newest
/// reducer projection while accumulating token usage for the session.
fn app_event_batch_collect_event_merges_agent_response_token_usage() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    let latest_turn = test_turn_applied_state(
        vec![
            QuestionItem::new("Need branch?"),
            QuestionItem::new("Need tests?"),
        ],
        vec!["Document the batched reducer path."],
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 7,
            output_tokens: 11,
        },
    );

    // Act
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("Old question")],
            vec!["Old follow-up task"],
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 3,
                output_tokens: 5,
            },
        ),
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: latest_turn.clone(),
    });

    // Assert
    let merged_turn = event_batch.applied_turns.get("session-1");
    assert_eq!(
        merged_turn.map(|turn| turn.questions.clone()),
        Some(latest_turn.questions)
    );
    assert_eq!(
        merged_turn.map(|turn| {
            turn.follow_up_tasks
                .iter()
                .map(|task| task.text.clone())
                .collect::<Vec<_>>()
        }),
        Some(vec!["Document the batched reducer path.".to_string()])
    );
    assert_eq!(
        merged_turn.map(|turn| turn.token_usage_delta.input_tokens),
        Some(10)
    );
    assert_eq!(
        merged_turn.map(|turn| turn.token_usage_delta.output_tokens),
        Some(16)
    );
}

#[test]
/// Verifies that `UpdateStatusChanged` events update the event batch so
/// the reducer can apply the latest update progress state.
fn app_event_batch_collect_event_stores_update_status() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::InProgress {
            version: "v1.0.0".to_string(),
        },
    });
    event_batch.collect_event(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::Complete {
            version: "v1.0.0".to_string(),
        },
    });

    // Assert
    assert_eq!(
        event_batch.update_status,
        Some(UpdateStatus::Complete {
            version: "v1.0.0".to_string()
        })
    );
}

#[test]
/// Verifies that `AgentCliVersionsUpdated` events keep the latest
/// completed version snapshot in one reducer batch.
fn app_event_batch_collect_event_stores_agent_cli_versions() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string()),
        )],
    });
    event_batch.collect_event(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("0.139.0".to_string()),
        )],
    });

    // Assert
    assert_eq!(
        event_batch.agent_cli_updates,
        Some(vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("0.139.0".to_string())
        )])
    );
}

#[test]
fn app_event_batch_collect_event_keeps_latest_same_session_updates() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SessionModelUpdated {
        session_id: "session-a".into(),
        session_agent: AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini38Flash),
    });
    event_batch.collect_event(AppEvent::SessionModelUpdated {
        session_id: "session-a".into(),
        session_agent: AgentSelection::new(AgentKind::Gemini, AgentModel::Gemini31Pro),
    });
    event_batch.collect_event(AppEvent::SessionProgressUpdated {
        progress_message: Some("first".to_string()),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionProgressUpdated {
        progress_message: Some("second".to_string()),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionDiffStatsUpdated {
        diff_stats: known_session_diff_stats(1, 2, SessionSize::S),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionDiffStatsUpdated {
        diff_stats: known_session_diff_stats(8, 13, SessionSize::L),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
        generation: 1,
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionTitleGenerationFinished {
        generation: 2,
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::SessionUpdated {
        session_id: "session-a".into(),
        version: 1,
    });
    event_batch.collect_event(AppEvent::SessionUpdated {
        session_id: "session-a".into(),
        version: 2,
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-a".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("first question")],
            Vec::new(),
            SessionStats::default(),
        ),
    });
    event_batch.collect_event(AppEvent::AgentResponseReceived {
        session_id: "session-a".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("second question")],
            Vec::new(),
            SessionStats::default(),
        ),
    });

    // Assert
    assert_eq!(
        event_batch.session_model_updates.get("session-a"),
        Some(&AgentSelection::new(
            AgentKind::Gemini,
            AgentModel::Gemini31Pro
        ))
    );
    assert_eq!(
        event_batch.session_progress_updates.get("session-a"),
        Some(&Some("second".to_string()))
    );
    assert_eq!(
        event_batch.session_diff_stats_updates.get("session-a"),
        Some(&known_session_diff_stats(8, 13, SessionSize::L))
    );
    assert_eq!(
        event_batch.session_update_versions.get("session-a"),
        Some(&2)
    );
    assert_eq!(
        event_batch
            .session_title_generation_finished
            .get("session-a"),
        Some(&2)
    );
    assert_eq!(event_batch.session_ids.len(), 1);
    assert_eq!(
        event_batch
            .applied_turns
            .get("session-a")
            .map(|turn_applied_state| turn_applied_state.questions.clone()),
        Some(vec![QuestionItem::new("second question")])
    );
}

fn known_session_diff_stats(
    added_lines: u64,
    deleted_lines: u64,
    session_size: SessionSize,
) -> SessionDiffStats {
    SessionDiffStats::Known {
        added_lines,
        deleted_lines,
        has_diff: true,
        session_size,
    }
}

#[test]
fn app_event_batch_collect_event_keeps_publish_results_and_latest_reviews() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 11,
        review_text: "first review".to_string(),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::ReviewPreparationFailed {
        diff_hash: 12,
        error: "latest failure".to_string(),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 21,
        review_text: "stable review".to_string(),
        session_id: "session-b".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(test_pushed_branch_result("feature/first"))),
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(test_pushed_branch_result("feature/final"))),
        session_id: "session-b".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionStarted {
        session_id: "session-a".into(),
    });
    event_batch.collect_event(AppEvent::BranchPublishActionResolved {
        session_id: "session-b".into(),
    });
    event_batch.collect_event(AppEvent::SessionQueuedSyncResolved {
        session_id: "session-b".into(),
    });

    // Assert
    assert_eq!(
        event_batch.review_updates.get("session-a"),
        Some(&ReviewUpdate {
            diff_hash: 12,
            result: Err("latest failure".to_string()),
        })
    );
    assert_eq!(
        event_batch.review_updates.get("session-b"),
        Some(&ReviewUpdate {
            diff_hash: 21,
            result: Ok("stable review".to_string()),
        })
    );
    assert_eq!(
        event_batch.branch_publish_action_updates,
        vec![
            BranchPublishActionUpdate {
                result: Ok(test_pushed_branch_result("feature/first")),
                session_id: "session-a".into(),
            },
            BranchPublishActionUpdate {
                result: Ok(test_pushed_branch_result("feature/final")),
                session_id: "session-b".into(),
            },
        ]
    );
    assert!(
        event_batch
            .branch_publish_resolved_session_ids
            .contains("session-b")
    );
    assert!(
        event_batch
            .branch_publish_started_session_ids
            .contains("session-a")
    );
    assert!(
        event_batch
            .session_queued_sync_resolved_ids
            .contains("session-b")
    );
    assert!(event_batch.should_refresh_git_status);
}

#[test]
/// Verifies successful sync completion requests an immediate git-status
/// refresh in the reducer batch.
fn app_event_batch_collect_event_marks_successful_sync_for_git_status_refresh() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::SyncMainCompleted {
        completion: sync::SyncMainCompletion {
            operation: sync::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id: 1,
                project_name: "agentty".to_string(),
            },
            result: Ok(SyncMainOutcome {
                default_branch: "main".to_string(),
                deferred_merged_session_ids: Vec::new(),
                pulled_commit_titles: vec!["Upstream fix".to_string()],
                pulled_commits: Some(1),
                pushed_commit_titles: vec!["Local tweak".to_string()],
                pushed_commits: Some(2),
                resolved_conflict_files: Vec::new(),
            }),
            review_request_updates: Vec::new(),
        },
    });

    // Assert
    assert!(event_batch.should_refresh_git_status);
    assert!(matches!(
        event_batch.sync_main_completion,
        Some(sync::SyncMainCompletion {
            result: Ok(SyncMainOutcome {
                default_branch,
                pulled_commits: Some(1),
                pushed_commits: Some(2),
                ..
            }),
            ..
        }) if default_branch == "main"
    ));
}

#[tokio::test]
/// Verifies that the reducer applies `UpdateStatusChanged` events to
/// `App.update_status`.
async fn apply_app_events_update_status_changed_updates_app_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    assert!(app.update_status().is_none());
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::UpdateStatusChanged {
        update_status: UpdateStatus::InProgress {
            version: "v2.0.0".to_string(),
        },
    })
    .await;

    // Assert
    assert_eq!(
        app.update_status().cloned(),
        Some(UpdateStatus::InProgress {
            version: "v2.0.0".to_string()
        })
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies that completed CLI version events replace startup loading
/// rows and request a redraw.
async fn apply_app_events_agent_cli_versions_updates_app_services() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.services
        .replace_available_agent_clis(vec![AgentCliInfo::loading(AgentKind::Claude)]);
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::AgentCliVersionsUpdated {
        agent_clis: vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string()),
        )],
    })
    .await;

    // Assert
    assert_eq!(
        app.services.available_agent_clis(),
        vec![AgentCliInfo::new(
            AgentKind::Claude,
            Some("2.1.39".to_string())
        )]
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies workflow notices append to in-memory session state without
/// changing persisted transcript messages.
async fn apply_app_events_session_workflow_notice_updates_session_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review"));
    session.id = "session-1".into();
    session.status = Status::Review;
    session.transcript = Some(crate::test_support::assistant_transcript(
        "assistant output",
    ));
    app.sessions.push_session(session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);
    app.services
        .event_sender()
        .send(AppEvent::SessionWorkflowNoticeUpdated {
            notice: "[Merge] Successfully merged wt/session-1 into main".to_string(),
            session_id: "session-1".into(),
        })
        .expect("queued workflow notice should send");
    app.clear_redraw();

    // Act
    app.apply_app_events(AppEvent::SessionWorkflowNoticeUpdated {
        notice: "[Commit] No changes to commit.".to_string(),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "session-1")
        .expect("session should exist");
    assert_eq!(
        session
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::WorkflowNotice)
            .map(|message| message.body.text()),
        Some(
            "[Commit] No changes to commit.\n\n[Merge] Successfully merged wt/session-1 into main"
        )
    );
    assert!(app.pending_session_diff_requests.is_empty());
    assert_eq!(
        session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .as_deref(),
        Some("assistant output\n\n")
    );
    assert!(app.needs_redraw());
}

#[tokio::test]
/// Verifies orchestration progress updates the board without transcript noise.
async fn apply_app_events_orchestration_progress_updates_board_snapshot() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/controller"));
    session.id = "controller".into();
    app.sessions.push_session(session);

    // Act
    app.apply_app_events(AppEvent::SessionOrchestrationProgressUpdated {
        progress: Some("Working... Protocol: running".to_string()),
        session_id: "controller".into(),
    })
    .await;

    // Assert
    let controller = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "controller")
        .expect("controller should remain loaded");
    assert_eq!(
        controller.orchestration_progress.as_deref(),
        Some("Working... Protocol: running")
    );
    assert!(
        controller
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::Orchestration)
            .is_none()
    );

    // Act
    app.apply_app_events(AppEvent::SessionOrchestrationProgressUpdated {
        progress: None,
        session_id: "controller".into(),
    })
    .await;

    // Assert
    assert!(
        app.sessions
            .sessions()
            .iter()
            .find(|session| session.id == "controller")
            .is_some_and(|session| session.orchestration_progress.is_none())
    );
}

#[tokio::test]
/// Verifies stale `SessionUpdated` versions do not re-arm redraw when the
/// reducer has already applied that handle snapshot.
async fn apply_app_events_session_updated_same_version_keeps_redraw_clean() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 7,
    })
    .await;
    app.clear_redraw();
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 7,
    })
    .await;

    // Assert
    assert!(!app.needs_redraw());
}

#[tokio::test]
/// Verifies that one combined git-status event updates the in-memory
/// session snapshot cache.
async fn apply_app_events_git_status_updated_updates_project_and_session_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-git-status"),
        ));

    // Act
    app.apply_app_events(AppEvent::GitStatusUpdated {
        generation: app.sync_handle.current_generation(),
        session_statuses: HashMap::from([(
            SessionId::from("session-1"),
            SessionGitStatus {
                base_status: Some((4, 2)),
                has_merge_conflict: Some(true),
                remote_status: Some((1, 0)),
            },
        )]),
        status: Some((1, 3)),
    })
    .await;

    // Assert
    assert_eq!(app.git_status_info(), Some((1, 3)));
    assert_eq!(
        app.sessions
            .render_parts()
            .session_git_statuses
            .get("session-1"),
        Some(&SessionGitStatus {
            base_status: Some((4, 2)),
            has_merge_conflict: Some(true),
            remote_status: Some((1, 0)),
        })
    );
}

#[tokio::test]
/// Verifies stale git-status snapshots do not overwrite the current sync
/// generation.
async fn apply_app_events_git_status_updated_ignores_stale_generation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.publish_sync_context_for_refresh();
    let stale_generation = app.sync_handle.current_generation().saturating_sub(1);

    // Act
    app.apply_app_events(AppEvent::GitStatusUpdated {
        generation: stale_generation,
        session_statuses: HashMap::new(),
        status: Some((9, 9)),
    })
    .await;

    // Assert
    assert_eq!(app.git_status_info(), None);
    assert!(app.sessions.render_parts().session_git_statuses.is_empty());
}

#[tokio::test]
/// Verifies stale review-request status results cannot transition a
/// session after the sync context moved to a newer generation.
async fn apply_app_events_review_request_status_updated_ignores_stale_generation() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-review-stale"),
        ));
    app.publish_sync_context_for_refresh();
    let stale_generation = app.sync_handle.current_generation().saturating_sub(1);

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation: stale_generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: "session-1".into(),
    })
    .await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == "session-1")
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Review);
}

/// Verifies reducer-applied review-request status transitions update the
/// session state.
#[tokio::test]
async fn apply_app_events_review_request_status_transition_updates_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-status-transition";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gemini38Flash.as_str(),
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    let generation = app.sync_handle.current_generation();
    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
/// Verifies review-request status updates emitted before a sync
/// completion in the same reducer batch are applied before the
/// post-sync refresh bumps the status generation.
async fn apply_app_events_review_request_status_survives_same_batch_sync_refresh() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-sync-batch";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;
    let generation = app.sync_handle.current_generation();
    app.services
        .event_sender()
        .send(AppEvent::SyncMainCompleted {
            completion: sync::SyncMainCompletion {
                operation: sync::ProjectSyncContext {
                    default_branch: "main".to_string(),
                    operation_id: 1,
                    project_id,
                    project_name: "agentty".to_string(),
                },
                result: Ok(SyncMainOutcome {
                    default_branch: "main".to_string(),
                    deferred_merged_session_ids: Vec::new(),
                    pulled_commit_titles: Vec::new(),
                    pulled_commits: Some(0),
                    pushed_commit_titles: Vec::new(),
                    pushed_commits: Some(0),
                    resolved_conflict_files: Vec::new(),
                }),
                review_request_updates: Vec::new(),
            },
        })
        .expect("sync completion should queue");

    // Act
    app.apply_app_events(AppEvent::ReviewRequestStatusUpdated {
        generation,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Closed {
                display_id: "#42".to_string(),
            },
            summary: None,
        }),
        session_id: session_id.into(),
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
/// Verifies explicit git-status refresh events request an immediate
/// orchestrator pass instead of waiting for the periodic cadence.
async fn apply_app_events_refresh_git_status_requests_orchestrator_refresh() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(|dir| Box::pin(async move { Some(dir) }));
    mock_git_client
        .expect_fetch_remote()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(1)
        .returning(|_| {
            Box::pin(async { Ok(HashMap::from([("main".to_string(), Some((2_u32, 1_u32)))])) })
        });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::RefreshGitStatus).await;
    let mut observed_events = vec![
        tokio::time::timeout(Duration::from_secs(1), app.next_app_event())
            .await
            .expect("first app event should arrive")
            .expect("app event channel should remain open"),
    ];
    if !observed_events
        .iter()
        .any(|event| matches!(event, AppEvent::GitStatusUpdated { .. }))
    {
        let next_event = tokio::time::timeout(Duration::from_secs(1), app.next_app_event()).await;
        assert!(
            next_event.is_ok(),
            "git status refresh event should arrive after observed events: {observed_events:?}"
        );
        let next_event = next_event
            .expect("git status refresh timeout should be checked")
            .expect("app event channel should remain open");
        observed_events.push(next_event);
    }

    // Assert
    assert!(
        observed_events.contains(&AppEvent::GitStatusUpdated {
            generation: app.sync_handle.current_generation(),
            session_statuses: HashMap::new(),
            status: Some((2, 1)),
        }),
        "expected git status update among observed events: {observed_events:?}"
    );
}

#[tokio::test]
async fn apply_app_events_agent_response_switches_view_mode_to_question_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-question-view"),
        ));
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: None,
    };
    let expected_questions = vec![
        QuestionItem::with_options(
            "Need a target branch?",
            vec!["main".to_string(), "develop".to_string()],
        ),
        QuestionItem::with_options(
            "Need integration tests?",
            vec!["Yes".to_string(), "No".to_string()],
        ),
    ];
    let turn_applied_state = test_turn_applied_state(
        vec![
            QuestionItem::with_options(
                "Need a target branch?",
                vec!["main".to_string(), "develop".to_string()],
            ),
            QuestionItem::with_options(
                "Need integration tests?",
                vec!["Yes".to_string(), "No".to_string()],
            ),
        ],
        Vec::new(),
        SessionStats::default(),
    );

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            ref questions,
            ref responses,
            current_index: 0,
            ref input,
            selected_option_index: Some(0),
            ..
        } if session_id == "session-1"
            && questions == &expected_questions
            && responses.is_empty()
            && input.text().is_empty()
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_enters_question_mode_from_view() {
    // Arrange — a viewed session reached `Question` status with pending
    // questions, but the view was never flipped into the clarification panel
    // (for example the live projection was missed while an overlay was open).
    let pending_questions = vec![
        QuestionItem::with_options("Need a target branch?", vec!["main".to_string()]),
        QuestionItem::new("Need integration tests?"),
    ];
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        pending_questions.clone(),
        "session-question-reconcile",
    )
    .await;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Question {
            ref session_id,
            questions: ref mode_questions,
            current_index: 0,
            ..
        } if session_id == "session-1" && mode_questions == &pending_questions
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_ignores_non_question_status() {
    // Arrange — the viewed session is in `Review`, not awaiting a question.
    let mut app =
        test_app_viewing_reconcile_session(Status::Review, Vec::new(), "session-review-reconcile")
            .await;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert — the view is preserved.
    assert!(matches!(
        app.mode,
        AppMode::View { ref session_id, .. } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_ignores_non_view_modes() {
    // Arrange — a `Question` session exists, but the user is on the list, not
    // viewing that session, so the panel must not steal focus.
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        vec![QuestionItem::new("Need integration tests?")],
        "session-list-reconcile",
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.reconcile_open_session_question_mode().await;

    // Assert — the list stays active.
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_reloads_detail_at_most_once_when_still_empty() {
    // Arrange — a viewed session reports `Question` status but carries no
    // questions in the snapshot, and no persisted detail exists to reload, so
    // the reconciliation cannot open the panel.
    let mut app =
        test_app_viewing_reconcile_session(Status::Question, Vec::new(), "session-question-empty")
            .await;

    // Act — run two reconciliations to emulate two consecutive render cycles
    // while the session stays stuck without questions.
    app.reconcile_open_session_question_mode().await;
    let attempted_after_first = app.question_reconcile_reload_attempted.clone();
    app.reconcile_open_session_question_mode().await;

    // Assert — the first pass records the stuck session so the second cycle
    // short-circuits before reloading detail again, and the view is preserved
    // because no questions became available.
    assert_eq!(attempted_after_first.as_deref(), Some("session-1"));
    assert_eq!(
        app.question_reconcile_reload_attempted.as_deref(),
        Some("session-1")
    );
    assert!(matches!(
        app.mode,
        AppMode::View { ref session_id, .. } if session_id == "session-1"
    ));
}

#[tokio::test]
async fn reconcile_open_session_question_mode_clears_reload_guard_when_leaving_view() {
    // Arrange — a stuck `Question` view records the reload guard, then the user
    // navigates back to the list.
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        Vec::new(),
        "session-question-guard-reset",
    )
    .await;
    app.reconcile_open_session_question_mode().await;
    assert_eq!(
        app.question_reconcile_reload_attempted.as_deref(),
        Some("session-1")
    );

    // Act — leave the session view and reconcile again.
    app.mode = AppMode::List;
    app.reconcile_open_session_question_mode().await;

    // Assert — the guard is cleared so a later legitimate transition reloads.
    assert!(app.question_reconcile_reload_attempted.is_none());
}

#[tokio::test]
async fn apply_app_events_agent_response_clears_saved_question_progress() {
    // Arrange — stale partial answers saved from the previous question set
    // must not survive a new turn result for the session.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-progress-clear"),
        ));
    app.question_progress.insert(
        "session-1".into(),
        QuestionProgress {
            current_index: 1,
            input: InputState::default(),
            responses: vec!["Old answer".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("New question?")],
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
async fn enter_question_mode_restores_saved_progress() {
    // Arrange — progress saved by a previous `q` exit from question mode.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let questions = vec![
        QuestionItem::with_options("First?", vec!["Yes".to_string(), "No".to_string()]),
        QuestionItem::new("Second?"),
    ];
    app.question_progress.insert(
        "session-restore".into(),
        QuestionProgress {
            current_index: 1,
            input: InputState::with_text("draft answer".to_string()),
            responses: vec!["Yes".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.enter_question_mode("session-restore", questions);

    // Assert — resumes at the second question with the saved answer, and
    // the stored entry is consumed.
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            current_index: 1,
            responses,
            input,
            selected_option_index: None,
            session_id,
            ..
        } if responses == &vec!["Yes".to_string()]
            && input.text() == "draft answer"
            && session_id == "session-restore"
    ));
    assert!(app.question_progress.is_empty());
}

#[tokio::test]
async fn enter_question_mode_discards_progress_for_changed_question_list() {
    // Arrange — saved progress no longer matches the question list.
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let questions = vec![QuestionItem::with_options(
        "Only question?",
        vec!["Yes".to_string()],
    )];
    app.question_progress.insert(
        "session-stale".into(),
        QuestionProgress {
            current_index: 2,
            input: InputState::default(),
            responses: vec!["One".to_string(), "Two".to_string()],
            selected_option_index: None,
        },
    );

    // Act
    app.enter_question_mode("session-stale", questions);

    // Assert — starts fresh at the first question with its first option
    // highlighted.
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            current_index: 0,
            responses,
            selected_option_index: Some(0),
            ..
        } if responses.is_empty()
    ));
}

#[tokio::test]
async fn apply_app_events_agent_response_keeps_list_mode_when_not_viewing_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            vec![QuestionItem::new("Need context?")],
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
/// Verifies agent responses update cached follow-up tasks immediately for
/// the active session.
async fn apply_app_events_agent_response_updates_session_follow_up_tasks() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-follow-up-view"),
        ));

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            vec![
                "Document the new shortcut.",
                "Add a focused regression test.",
            ],
            SessionStats::default(),
        ),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0]
            .follow_up_tasks
            .iter()
            .map(|task| task.text.clone())
            .collect::<Vec<_>>(),
        vec![
            "Document the new shortcut.".to_string(),
            "Add a focused regression test.".to_string()
        ]
    );
}
#[tokio::test]
/// Verifies stale published-branch sync completions do not overwrite the
/// latest in-progress auto-push state.
async fn apply_app_events_ignores_stale_published_branch_sync_updates() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-branch-sync-view"),
        ));

    // Act
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-1".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-2".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: Some("[Branch Push Error] stale failure".to_string()),
        session_id: "session-1".into(),
        sync_operation_id: "sync-1".to_string(),
        sync_status: PublishedBranchSyncStatus::Failed,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::PublishedBranchSync)
            .map(|message| message.body.text()),
        Some("Auto-pushing published branch after completed turn...")
    );
    assert!(
        app.sessions.sessions()[0]
            .transcript
            .as_ref()
            .is_none_or(|transcript| transcript.messages().is_empty())
    );
}

#[tokio::test]
/// Verifies one reducer tick preserves a completed auto-push message even
/// when start and success updates are drained together.
async fn apply_app_events_preserves_completed_published_branch_sync_updates() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let event_sender = app.services.event_sender();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-branch-sync-success"),
        ));

    event_sender
        .send(AppEvent::PublishedBranchSyncUpdated {
            persistent_notice: Some(
                "[Branch Push] Auto-pushed published branch after completed turn.".to_string(),
            ),
            session_id: "session-1".into(),
            sync_operation_id: "sync-1".to_string(),
            sync_status: PublishedBranchSyncStatus::Succeeded,
        })
        .expect("queued event should send");

    // Act
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-1".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;

    // Assert
    let session = &app.sessions.sessions()[0];
    assert!(
        session
            .transient_messages
            .get(crate::domain::transient_message::TransientMessageSlot::PublishedBranchSync)
            .is_none()
    );
    assert_eq!(
        session
            .transcript
            .as_ref()
            .expect("promoted notice should update transcript")
            .messages()
            .iter()
            .filter(|message| {
                message.kind == crate::domain::session_message::SessionMessageKind::WorkflowNotice
            })
            .count(),
        1
    );
}

#[tokio::test]
/// Verifies terminal auto-push notices are persisted while their project
/// snapshot is unloaded, so both outcomes survive a later reload.
async fn apply_published_branch_sync_persists_notices_for_unloaded_project() {
    // Arrange
    let session_folder = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_selected_session(
        session_folder.path().to_path_buf(),
        "",
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    persist_selected_session(&app).await;
    app.sessions
        .session_handles_mut()
        .insert("session-1".into(), SessionHandles::new(Status::Review));
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-success".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.sessions.state_mut().replace_sessions(Vec::new());

    // Act
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: Some(
            "[Branch Push] Auto-pushed published branch after completed turn.".to_string(),
        ),
        session_id: "session-1".into(),
        sync_operation_id: "sync-success".to_string(),
        sync_status: PublishedBranchSyncStatus::Succeeded,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: None,
        session_id: "session-1".into(),
        sync_operation_id: "sync-failure".to_string(),
        sync_status: PublishedBranchSyncStatus::InProgress,
    })
    .await;
    app.apply_app_events(AppEvent::PublishedBranchSyncUpdated {
        persistent_notice: Some("[Branch Push Error] Remote rejected the push.".to_string()),
        session_id: "session-1".into(),
        sync_operation_id: "sync-failure".to_string(),
        sync_status: PublishedBranchSyncStatus::Failed,
    })
    .await;

    // Assert
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages("session-1")
        .await
        .expect("failed to load persisted session messages");
    assert_eq!(persisted_messages.len(), 2);
    assert_eq!(
        persisted_messages
            .iter()
            .map(|message| (message.kind.as_str(), message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                SessionMessageKind::WorkflowNotice.as_str(),
                "[Branch Push] Auto-pushed published branch after completed turn.",
            ),
            (
                SessionMessageKind::WorkflowNotice.as_str(),
                "[Branch Push Error] Remote rejected the push.",
            ),
        ]
    );
}

#[tokio::test]
/// Verifies reducer-applied turn projections clear stale questions and add
/// token deltas to cached session stats.
async fn apply_app_events_agent_response_updates_questions_and_token_usage() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-stats-view"));
    session.questions = vec![QuestionItem::new("Old question?")];
    session.stats.input_tokens = 5;
    session.stats.output_tokens = 8;
    app.sessions.push_session(session);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: SessionDiffState::Unknown,
                input_tokens: 13,
                output_tokens: 21,
            },
        ),
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].questions,
        [] as [ag_protocol::QuestionItem; 0]
    );
    assert_eq!(app.sessions.sessions()[0].stats.input_tokens, 18);
    assert_eq!(app.sessions.sessions()[0].stats.output_tokens, 29);
}

#[tokio::test]
/// Verifies an unchanged review-ready snapshot does not trigger another full
/// diff when an unrelated handle field emits `SessionUpdated`.
async fn apply_app_events_session_updated_skips_auto_review_without_status_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-update",
    ));
    session.status = Status::Review;
    app.sessions.push_session(session);
    app.sessions
        .session_handles_mut()
        .insert(session_id.into(), SessionHandles::new(Status::Review));
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: 1,
    })
    .await;

    // Assert
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
/// Verifies `SessionUpdated` still triggers automatic review when the synced
/// handle actually transitions into `Review`.
async fn apply_app_events_session_updated_starts_auto_review_on_status_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-transition",
    ));
    session.status = Status::InProgress;
    app.sessions.push_session(session);
    app.sessions
        .session_handles_mut()
        .insert(session_id.into(), SessionHandles::new(Status::Review));
    let diff_call_count = Arc::new(AtomicUsize::new(0));
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().once().returning({
        let diff_call_count = Arc::clone(&diff_call_count);

        move |_, _| {
            diff_call_count.fetch_add(1, Ordering::Relaxed);

            Box::pin(std::future::pending())
        }
    });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: 1,
    })
    .await;
    tokio::task::yield_now().await;

    // Assert
    assert_eq!(diff_call_count.load(Ordering::Relaxed), 1);
    assert_eq!(app.pending_session_diff_requests.len(), 1);
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
/// Verifies agent-response events still trigger auto review when the
/// handle has already advanced to `Review` but the paired
/// `SessionUpdated` event has not been reduced yet.
async fn apply_app_events_agent_response_starts_auto_review_from_synced_handle_status() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);

    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-auto-review-sync"),
        ));
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::InProgress),
    );
    *app.sessions
        .session_handles()
        .get(session_id)
        .expect("expected session handles")
        .status
        .lock()
        .expect("expected handle status lock") = Status::Review;

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::AgentReview
    );
}

#[tokio::test]
/// Verifies a completed turn supersedes an older pending review diff before
/// starting review preparation for the latest generation.
async fn apply_app_events_agent_response_supersedes_pending_auto_review_diff() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);

    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-already-review"),
        ));
    // Simulate sync_from_handles() having already updated the snapshot
    // to `Review` in a prior render tick.
    app.sessions.sessions_mut()[0].status = Status::AgentReview;
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Review),
    );
    app.mode = AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    };

    let diff_call_count = Arc::new(AtomicUsize::new(0));
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(2).returning({
        let diff_call_count = Arc::clone(&diff_call_count);

        move |_, _| {
            let is_obsolete_request = diff_call_count.fetch_add(1, Ordering::Relaxed) == 0;
            Box::pin(async move {
                if is_obsolete_request {
                    std::future::pending::<()>().await;
                }

                Ok(diff_text.to_string())
            })
        }
    });
    install_mock_git_client(&mut app, mock_git_client);
    let session_ids = HashSet::from([SessionId::from(session_id)]);
    app.auto_start_reviews(&session_ids);
    tokio::task::yield_now().await;
    assert_eq!(diff_call_count.load(Ordering::Relaxed), 1);
    let obsolete_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("obsolete review diff should be pending");

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.into(),
        turn_applied_state: test_turn_applied_state(
            Vec::new(),
            Vec::new(),
            SessionStats::default(),
        ),
    })
    .await;
    let replacement_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("replacement review diff should be pending");
    apply_next_session_diff(&mut app).await;

    // Assert
    assert_ne!(replacement_request_id, obsolete_request_id);
    assert!(
        !app.pending_session_diff_requests
            .contains_key(&obsolete_request_id)
    );
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
    assert!(matches!(
        app.mode,
        AppMode::View {
            session_id: ref mode_session_id,
            ..
        } if mode_session_id == session_id
    ));
}

#[tokio::test]
/// Verifies one reducer tick preserves the latest turn projection while
/// accumulating token usage from multiple queued completions.
async fn apply_app_events_agent_response_batches_same_session_turns() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let event_sender = app.services.event_sender();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-batched-turns"),
        ));

    let first_turn = test_turn_applied_state(
        vec![QuestionItem::new("First question?")],
        Vec::new(),
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 2,
            output_tokens: 3,
        },
    );
    let second_turn = test_turn_applied_state(
        vec![QuestionItem::new("Latest question?")],
        vec!["Capture reducer batching coverage."],
        SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens: 5,
            output_tokens: 8,
        },
    );

    event_sender
        .send(AppEvent::AgentResponseReceived {
            session_id: "session-1".into(),
            turn_applied_state: second_turn,
        })
        .expect("queued event should send");

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: "session-1".into(),
        turn_applied_state: first_turn,
    })
    .await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].questions,
        vec![QuestionItem::new("Latest question?")]
    );
    assert_eq!(app.sessions.sessions()[0].stats.input_tokens, 7);
    assert_eq!(app.sessions.sessions()[0].stats.output_tokens, 11);
    assert_eq!(
        app.sessions.sessions()[0]
            .follow_up_tasks
            .iter()
            .map(|task| task.text.clone())
            .collect::<Vec<_>>(),
        vec!["Capture reducer batching coverage.".to_string()]
    );
}

#[tokio::test]
async fn failed_follow_up_preparation_rolls_back_reserved_sibling() {
    // Arrange
    let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let source_id = app.create_session().await.expect("source");
    crate::test_support::set_session_status_for_test(&mut app, &source_id, Status::Review);
    app.sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == source_id)
        .expect("source")
        .follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: None,
        position: 0,
        text: "Follow up on the source".to_string(),
    }];
    let original_resources = session_creation_resources(directory.path()).await;
    sqlx::query(
        "CREATE TRIGGER reject_ready BEFORE UPDATE OF state ON session_preparation WHEN NEW.state \
         = 'ready' BEGIN SELECT RAISE(ABORT, 'preparation rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject preparation after checkout");

    for _attempt in 0..2 {
        // Act
        let result = app.launch_or_open_selected_follow_up_task(&source_id).await;

        // Assert
        assert!(
            result
                .expect_err("preparation failure")
                .to_string()
                .contains("preparation rejected")
        );
        let sessions = app
            .services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, source_id);
        let unlinked_preparations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_preparation WHERE session_id != ?")
                .bind(&source_id)
                .fetch_one(&pool)
                .await
                .expect("preparation count");
        assert_eq!(unlinked_preparations, 0);
        assert!(
            app.sessions
                .session_for_id(&source_id)
                .expect("source")
                .follow_up_tasks[0]
                .launched_session_id
                .is_none()
        );
        assert_eq!(
            session_creation_resources(directory.path()).await,
            original_resources
        );
    }

    // Act: readiness remains the success contract after the fault is removed.
    sqlx::query("DROP TRIGGER reject_ready")
        .execute(&pool)
        .await
        .expect("restore preparation");
    let retry_id = app.create_session().await.expect("retry creation");
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&retry_id)
        .await
        .expect("load")
        .expect("preparation");

    // Assert
    assert_eq!(preparation.state, db::SessionPreparationState::Ready);
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions")
            .len(),
        2
    );
}

#[tokio::test]
async fn failed_synchronous_fork_rolls_back_snapshot_and_workspace() {
    // Arrange
    let (mut app, directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let source_id = app.create_session().await.expect("source");
    crate::test_support::set_session_status_for_test(&mut app, &source_id, Status::Review);
    for (kind, text) in [
        (SessionMessageKind::UserPrompt, "Source question"),
        (SessionMessageKind::AssistantAnswer, "Source answer"),
    ] {
        app.services
            .db()
            .sessions()
            .append_session_message(&source_id, kind, text)
            .await
            .expect("source history");
    }
    let original_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&source_id)
        .await
        .expect("source messages");
    let original_resources = session_creation_resources(directory.path()).await;
    sqlx::query(
        "CREATE TRIGGER reject_ready BEFORE UPDATE OF state ON session_preparation WHEN NEW.state \
         = 'ready' BEGIN SELECT RAISE(ABORT, 'preparation rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("reject after checkout");

    for _attempt in 0..2 {
        // Act
        let result = app.sessions.fork_session(&app.services, &source_id).await;

        // Assert
        assert!(
            result
                .expect_err("fork preparation failure")
                .to_string()
                .contains("preparation rejected")
        );
        let sessions = app
            .services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, source_id);
        let (preparations, messages): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM session_preparation), (SELECT COUNT(*) FROM \
             session_message)",
        )
        .fetch_one(&pool)
        .await
        .expect("remaining metadata");
        assert_eq!(preparations, 1);
        assert_eq!(messages, 2);
        assert_eq!(
            app.services
                .db()
                .sessions()
                .load_session_messages(&source_id)
                .await
                .expect("source retained"),
            original_messages
        );
        assert_eq!(
            session_creation_resources(directory.path()).await,
            original_resources
        );
    }

    // Act: a subsequent successful synchronous fork returns a ready snapshot.
    sqlx::query("DROP TRIGGER reject_ready")
        .execute(&pool)
        .await
        .expect("restore setup");
    assert_synchronous_fork_is_ready_with_history(&mut app, &source_id).await;
}

/// Verifies successful synchronous creation still returns a ready fork with
/// the source conversation after an earlier preparation fault is removed.
async fn assert_synchronous_fork_is_ready_with_history(app: &mut App, source_id: &str) {
    // Act
    let fork_id = app
        .sessions
        .fork_session(&app.services, source_id)
        .await
        .expect("fork");
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&fork_id)
        .await
        .expect("load")
        .expect("fork preparation");
    let fork_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&fork_id)
        .await
        .expect("copied history");

    // Assert
    assert_eq!(preparation.state, db::SessionPreparationState::Ready);
    assert_eq!(
        fork_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Source question", "Source answer"]
    );
}

/// Snapshots checkout directories, session branches, and worktree registrations
/// so failed creation cannot leave resources that are invisible in the
/// database.
async fn session_creation_resources(root: &Path) -> Vec<String> {
    let mut snapshot: Vec<String> = fs::read_dir(root)
        .expect("repository entries")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    snapshot.sort();
    for arguments in [
        ["branch", "--list", "wt/*"],
        ["worktree", "list", "--porcelain"],
    ] {
        let output = tokio::process::Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()
            .await
            .expect("read git state");
        assert!(output.status.success());
        snapshot.push(String::from_utf8(output.stdout).expect("git output"));
    }

    snapshot
}

#[tokio::test]
/// Verifies launching an already-linked follow-up task opens its sibling
/// session instead of creating another session.
async fn launch_or_open_selected_follow_up_task_opens_existing_sibling_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: Some("session-2".into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    let mut sibling_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/sibling-session"));
    sibling_session.id = "session-2".into();
    sibling_session.title = Some("Sibling session".to_string());
    app.sessions.push_session(source_session);
    app.sessions.push_session(sibling_session);

    // Act
    app.launch_or_open_selected_follow_up_task("session-1")
        .await
        .expect("follow-up task should open the linked sibling session");

    // Assert
    assert_eq!(app.sessions.selected_session_index(), Some(1));
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id == "session-2"
    ));
}

#[tokio::test]
async fn merged_session_rejects_unlaunched_follow_up_task() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.status = Status::Merged;
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: None,
        position: 0,
        text: "Must wait for local merge integration.".to_string(),
    }];
    app.sessions.push_session(source_session);

    // Act
    let action = app.selected_follow_up_task_action("session-1");
    let reply_enqueued = app.reply("session-1", "must stay read-only").await;
    let result = app
        .launch_or_open_selected_follow_up_task("session-1")
        .await;

    // Assert
    assert_eq!(action, None);
    assert!(!reply_enqueued);
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Merged sessions cannot launch new follow-up tasks"
    ));
    assert_eq!(app.sessions.sessions().len(), 1);
}

#[tokio::test]
/// Verifies a stale launched-session link is cleared before replacement
/// session creation starts, so a failed launch does not keep retrying the
/// same orphaned sibling id.
async fn launch_or_open_selected_follow_up_task_clears_stale_sibling_link_before_launch() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut source_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/source-session"));
    source_session.follow_up_tasks = vec![SessionFollowUpTask {
        id: 1,
        launched_session_id: Some("missing-session".into()),
        position: 0,
        text: "Open the sibling session.".to_string(),
    }];
    app.sessions.push_session(source_session);

    // Act
    let result = app
        .launch_or_open_selected_follow_up_task("session-1")
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Session(crate::app::SessionError::Workflow(message)))
            if message == "Git branch is required to create a session"
    ));
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(
        app.sessions.sessions()[0].follow_up_tasks[0].launched_session_id,
        None
    );
}

#[tokio::test]
/// Verifies a viewed session keeps its review state when its live status
/// transition reaches `Done`.
async fn apply_app_events_session_updated_keeps_done_view_review_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-done-view"),
        ));
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(
            Status::Done,
            crate::test_support::assistant_transcript("Merge finished"),
        ),
    );
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: Some(9),
    };

    // Act
    app.apply_app_events(AppEvent::SessionUpdated {
        session_id: "session-1".into(),
        version: 1,
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(9),
            ..
        }
    ));
}

#[tokio::test]
/// Verifies refresh keeps the active session view when merge cleanup has
/// removed the worktree just before `Done` persists.
async fn apply_app_events_refresh_keeps_viewed_merging_session_without_worktree() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-1",
            AgentModel::Gemini38Flash.as_str(),
            "main",
            &Status::Merging.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert merging session");

    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");
    let session_folder = base_path.join("session-1");
    let mut viewed_session = crate::test_support::session_fixture_with_folder(session_folder);
    viewed_session.status = Status::Merging;
    app.sessions.push_session(viewed_session);
    app.sessions.session_handles_mut().insert(
        "session-1".into(),
        SessionHandles::new_with_transcript(
            Status::Merging,
            crate::test_support::assistant_transcript("Merging"),
        ),
    );
    app.mode = AppMode::View {
        session_id: "session-1".into(),
        scroll_offset: None,
    };

    // Act
    app.apply_app_events(AppEvent::RefreshSessions).await;

    // Assert
    assert!(
        app.sessions
            .sessions()
            .iter()
            .any(|session| session.id == "session-1" && session.status == Status::Merging)
    );
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id, ..
        } if session_id == "session-1"
    ));
}

#[test]
fn discover_home_project_paths_includes_git_repos_and_excludes_session_worktrees() {
    // Arrange
    let home_directory = tempdir().expect("failed to create temp dir");
    let top_level_repo = home_directory.path().join("agentty");
    create_git_repo_marker(top_level_repo.as_path());
    let nested_repo = home_directory.path().join("code").join("service");
    create_git_repo_marker(nested_repo.as_path());
    let session_worktree_root = home_directory.path().join("agentty-worktrees");
    let session_worktree_repo = session_worktree_root.join("a1b2c3d4");
    create_git_repo_marker(session_worktree_repo.as_path());

    // Act
    let discovered_project_paths =
        App::discover_home_project_paths(home_directory.path(), session_worktree_root.as_path());

    // Assert
    assert!(
        discovered_project_paths.contains(&top_level_repo),
        "top-level git repository should be discovered"
    );
    assert!(
        discovered_project_paths.contains(&nested_repo),
        "nested git repository should be discovered"
    );
    assert!(
        !discovered_project_paths.contains(&session_worktree_repo),
        "session worktree repositories must be excluded"
    );
}

#[test]
fn discover_home_project_paths_respects_repository_limit() {
    // Arrange
    let home_directory = tempdir().expect("failed to create temp dir");
    for index in 0..=HOME_PROJECT_SCAN_MAX_RESULTS {
        let repository = home_directory.path().join(format!("repo-{index}"));
        create_git_repo_marker(repository.as_path());
    }

    // Act
    let discovered_project_paths = App::discover_home_project_paths(
        home_directory.path(),
        Path::new("/tmp/non-session-worktree"),
    );

    // Assert
    assert_eq!(
        discovered_project_paths.len(),
        HOME_PROJECT_SCAN_MAX_RESULTS
    );
}

#[test]
fn is_session_worktree_project_path_returns_true_for_agentty_worktree_path() {
    // Arrange
    let session_worktree_root = Path::new("/home/test/.agentty/wt");
    let project_path = "/home/test/.agentty/wt/a1b2c3d4";

    // Act
    let is_session_worktree =
        App::is_session_worktree_project_path(project_path, session_worktree_root);

    // Assert
    assert!(is_session_worktree);
}

#[test]
fn is_session_worktree_project_path_returns_false_for_main_repository_path() {
    // Arrange
    let session_worktree_root = Path::new("/home/test/.agentty/wt");
    let project_path = "/home/test/src/agentty";

    // Act
    let is_session_worktree =
        App::is_session_worktree_project_path(project_path, session_worktree_root);

    // Assert
    assert!(!is_session_worktree);
}

#[test]
fn is_existing_project_path_returns_true_when_fs_client_reports_directory() {
    // Arrange
    let project_path = "/home/test/src/agentty";
    let expected_path = PathBuf::from(project_path);
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &expected_path)
        .return_const(true);

    // Act
    let project_exists = App::is_existing_project_path(&fs_client, project_path);

    // Assert
    assert!(project_exists);
}

#[test]
fn visible_project_rows_excludes_missing_nongit_and_session_worktree_projects() {
    // Arrange
    let existing_project_path = "/home/test/src/agentty".to_string();
    let nongit_project_path = "/home/test/src/notes".to_string();
    let session_worktree_project_path = "/home/test/.agentty/wt/a1b2c3d4".to_string();
    let missing_project_path = "/home/test/src/removed".to_string();
    let session_worktree_root = Path::new("/home/test/.agentty/wt");
    let project_rows = vec![
        project_list_row_fixture(1, existing_project_path.clone()),
        project_list_row_fixture(2, nongit_project_path.clone()),
        project_list_row_fixture(3, session_worktree_project_path),
        project_list_row_fixture(4, missing_project_path.clone()),
    ];
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    let existing_project_path_for_match = PathBuf::from(existing_project_path.clone());
    let existing_git_marker_for_match = existing_project_path_for_match.join(".git");
    let nongit_project_path_for_match = PathBuf::from(nongit_project_path);
    let missing_project_path_for_match = PathBuf::from(missing_project_path);
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &existing_project_path_for_match)
        .return_const(true);
    fs_client
        .expect_exists()
        .once()
        .withf(move |path| path == &existing_git_marker_for_match)
        .return_const(true);
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &nongit_project_path_for_match)
        .return_const(true);
    fs_client.expect_exists().once().return_const(false);
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &missing_project_path_for_match)
        .return_const(false);

    // Act
    let visible_rows = App::visible_project_rows(project_rows, &fs_client, session_worktree_root);

    // Assert
    assert_eq!(visible_rows.len(), 1);
    assert_eq!(visible_rows[0].path, existing_project_path);
}

#[tokio::test]
async fn resolve_startup_active_project_id_falls_back_when_stored_project_path_is_missing() {
    // Arrange
    let current_project_dir = tempdir().expect("failed to create current project dir");
    let current_project_path = current_project_dir.path().to_path_buf();
    let missing_project_path = current_project_path.join("removed-project");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let current_project_id = database
        .projects()
        .upsert_project(
            &current_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to insert current project");
    let missing_project_id = database
        .projects()
        .upsert_project(
            &missing_project_path.to_string_lossy(),
            Some("main".to_string()),
        )
        .await
        .expect("failed to insert missing project");
    database
        .settings()
        .set_active_project_id(missing_project_id)
        .await
        .expect("failed to persist active project");
    let missing_project_path = missing_project_path.clone();
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_is_dir()
        .once()
        .withf(move |path| path == &missing_project_path)
        .return_const(false);

    // Act
    let resolved_project_id =
        App::resolve_startup_active_project_id(&database, &fs_client, current_project_id).await;

    // Assert
    assert_eq!(resolved_project_id, current_project_id);
}

#[tokio::test]
async fn apply_app_events_refresh_projects_reloads_project_active_session_count() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    fs::create_dir_all(base_path.join(".git")).expect("failed to create project git marker");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            "session-active",
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert active session");

    let session_folder_name = "session-".chars().take(8).collect::<String>();
    let session_data_dir = base_path.join(session_folder_name).join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session dir");

    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        None,
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let initial_active_count = app
        .projects
        .render_parts()
        .project_items
        .iter()
        .find(|item| item.project.id == project_id)
        .map_or(0, |item| item.active_session_count);
    assert_eq!(initial_active_count, 1);

    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("session-active", &Status::Done.to_string(), 0)
        .await
        .expect("failed to update session status");

    // Act
    app.apply_app_events(AppEvent::RefreshProjects).await;

    // Assert
    let updated_active_count = app
        .projects
        .render_parts()
        .project_items
        .iter()
        .find(|item| item.project.id == project_id)
        .map_or(0, |item| item.active_session_count);
    assert_eq!(updated_active_count, 0);
}

#[tokio::test]
/// Verifies project list loads reuse only persisted rows and do not
/// discover repositories implicitly.
async fn load_project_items_uses_persisted_rows_without_home_scan() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let home_directory = tempdir().expect("failed to create temp dir");
    let discovered_repo = home_directory.path().join("agentty");
    create_git_repo_marker(discovered_repo.as_path());
    let fs_client = RealFsClient;
    let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);

    // Act
    let project_items = App::load_project_items_with_session_worktree_root(
        &database,
        &fs_client,
        session_worktree_root.as_path(),
    )
    .await;

    // Assert
    assert_eq!(
        project_items,
        [] as [crate::domain::project::ProjectListItem; 0]
    );
    assert!(
        database
            .projects()
            .load_projects_with_stats()
            .await
            .expect("failed to load projects")
            .is_empty()
    );
}

#[tokio::test]
/// Verifies the startup-only catalog refresh discovers repositories before
/// the first project list load.
async fn refresh_project_catalog_on_startup_discovers_home_directory_repositories() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let home_directory = tempdir().expect("failed to create temp dir");
    let discovered_repo = home_directory.path().join("agentty");
    create_git_repo_marker(discovered_repo.as_path());
    let fs_client = RealFsClient;
    let mut mock_git_client = ag_git::MockGitClient::new();
    let session_worktree_root = home_directory.path().join(".agentty").join(AGENTTY_WT_DIR);
    mock_git_client
        .expect_detect_git_info()
        .times(1)
        .returning(|_| Box::pin(async { Some("main".to_string()) }));

    // Act
    App::load_projects_from_home_directory(
        &database,
        &mock_git_client,
        &RealProjectDiscoveryClient,
        session_worktree_root.as_path(),
        Some(home_directory.path()),
    )
    .await;

    let project_items = App::load_project_items_with_session_worktree_root(
        &database,
        &fs_client,
        session_worktree_root.as_path(),
    )
    .await;

    // Assert
    assert_eq!(project_items.len(), 1);
    assert_eq!(project_items[0].project.path, discovered_repo);
    assert_eq!(project_items[0].project.git_branch.as_deref(), Some("main"));
}

/// Creates one directory with a `.git` marker for repository discovery
/// tests.
fn create_git_repo_marker(repository_path: &Path) {
    fs::create_dir_all(repository_path.join(".git"))
        .expect("failed to create repository .git marker");
}

/// Builds one lightweight project row fixture for project list tests.
fn project_list_row_fixture(project_id: i64, project_path: String) -> db::ProjectListRow {
    db::ProjectListRow {
        active_session_count: 0,
        created_at: 0,
        display_name: None,
        git_branch: Some("main".to_string()),
        id: project_id,
        input_tokens: 0,
        is_favorite: false,
        last_opened_at: None,
        last_session_updated_at: None,
        output_tokens: 0,
        path: project_path,
        session_count: 0,
        updated_at: 0,
    }
}

/// Applies queued app events until `condition` observes the expected app
/// state, or fails the test after a short timeout.
async fn wait_for_app_condition(app: &mut App, condition: impl Fn(&App) -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if condition(app) {
                break;
            }

            let app_event = app
                .next_app_event()
                .await
                .expect("background task should emit an app event");
            app.apply_app_events(app_event).await;
        }
    })
    .await
    .expect("timed out waiting for app condition");
}

/// Replaces the app-level git dependencies with one caller-provided mock.
fn install_mock_git_client(app: &mut App, mock_git_client: ag_git::MockGitClient) {
    let mock_git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let fs_client = app.services.fs_client();
    let review_request_client = app.services.review_request_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client: Arc::clone(&mock_git_client),
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Replaces the app-level review-request dependency with one
/// caller-provided mock.
fn install_mock_review_request_client(
    app: &mut App,
    mock_review_request_client: forge::MockReviewRequestClient,
) {
    let review_request_client: Arc<dyn ReviewRequestClient> = Arc::new(mock_review_request_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let app_server_client_override = app.services.app_server_client_override();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let fs_client = app.services.fs_client();
    let git_client = app.services.git_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Builds one GitHub remote fixture for review-comment state tests.
fn forge_remote() -> forge::ForgeRemote {
    forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: forge::ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Builds one review-comment snapshot fixture for app-state detail tests.
fn review_comment_snapshot() -> forge::ReviewCommentSnapshot {
    forge::ReviewCommentSnapshot {
        pr_level_comments: vec![forge::ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "Looks good.".to_string(),
        }],
        threads: Vec::new(),
    }
}

#[tokio::test]
async fn test_continue_terminal_session_opens_draft_prompt_for_done_session_with_hash() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert project");
    app.services
        .db()
        .sessions()
        .insert_session("done-source", "gpt-5.6-sol", "release", "Done", project_id)
        .await
        .expect("failed to insert source session row");
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    app.services
        .db()
        .sessions()
        .update_session_merged_commit_hash("done-source", Some(merged_commit_hash.to_string()))
        .await
        .expect("failed to persist merged commit hash");
    let mut source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("done-source")
        .status(Status::Done)
        .project_name("project-alpha")
        .title(Some("Done source".to_string()))
        .build();
    source_session.base_branch = "release".to_string();
    app.sessions.push_session(source_session);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .never()
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let continued_session_id = app
        .continue_terminal_session("done-source")
        .await
        .expect("expected terminal continuation to succeed");

    // Assert
    assert_ne!(continued_session_id, "done-source");
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id
            && input.text().is_empty()
    ));
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert!(continued_session.is_draft_session());
    assert_eq!(continued_session.base_branch, "release");
    assert_eq!(continued_session.status, Status::Draft);
    assert_eq!(
        continued_session.prompt,
        format!("Use {merged_commit_hash} commit as an initial context for this session")
    );
    assert!(matches!(
        app.selected_session(),
        Some(session) if session.id == continued_session_id
    ));
}

#[tokio::test]
async fn test_continue_terminal_session_falls_back_to_persisted_context_without_hash() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session("done-source", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert source session row");
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("done-source")
        .status(Status::Done)
        .title(Some("Done source".to_string()))
        .transcript("Use the saved context.")
        .build();
    app.sessions.push_session(source_session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .never()
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let continued_session_id = app
        .continue_terminal_session("done-source")
        .await
        .expect("expected done continuation to succeed");

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id && input.text().is_empty()
    ));
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert_eq!(
        continued_session.prompt,
        "Continue the work from this previous Agentty session.\n\nPrevious session: Done \
         source\nProject: project\nStatus: Done\n\nPrevious session transcript:\nUse the saved \
         context.\n"
    );
}

#[tokio::test]
async fn test_continue_terminal_session_rejects_non_terminal_source_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("review-source")
        .status(Status::Review)
        .build();
    app.sessions.push_session(source_session);

    // Act
    let result = app.continue_terminal_session("review-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Only `Done` or `Canceled` sessions can be continued"
    ));
}

#[tokio::test]
async fn test_continue_terminal_session_uses_persisted_context_for_canceled_source_session() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session(
            "canceled-source",
            "gpt-5.6-sol",
            "main",
            "Canceled",
            project_id,
        )
        .await
        .expect("failed to insert source session row");
    app.services
        .db()
        .sessions()
        .update_session_merged_commit_hash("canceled-source", Some("stale-merged-hash".to_string()))
        .await
        .expect("failed to persist stale merged commit hash");
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("canceled-source")
        .status(Status::Canceled)
        .title(Some("Canceled source".to_string()))
        .transcript("Resume the remaining work.")
        .build();
    app.sessions.push_session(source_session);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .never()
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let continued_session_id = app
        .continue_terminal_session("canceled-source")
        .await
        .expect("expected canceled continuation to succeed");

    // Assert
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert_eq!(continued_session.status, Status::Draft);
    assert_eq!(
        continued_session.prompt,
        "Continue the work from this previous Agentty session.\n\nPrevious session: Canceled \
         source\nProject: project\nStatus: Canceled\n\nPrevious session transcript:\nResume the \
         remaining work.\n"
    );
    assert!(!continued_session.prompt.contains("stale-merged-hash"));
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id && input.text().is_empty()
    ));
}

#[tokio::test]
async fn test_continue_terminal_session_reports_legacy_session_without_project() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("legacy-source")
        .status(Status::Done)
        .build();
    app.sessions.push_session(source_session);

    // Act
    let result = app.continue_terminal_session("legacy-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Workflow(message))
            if message == "Source session has no project association. Restart Agentty from \
                this project to backfill legacy sessions, then continue the session again."
    ));
}

#[tokio::test]
async fn apply_review_update_stores_success_in_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-cache";
    let review_text = "## Review\nLooks good.";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-cache",
    ));
    session.id = session_id.to_string().into();
    session.status = Status::AgentReview;
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::AgentReview),
    );
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(123));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 123,
            result: Ok(review_text.to_string()),
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Ready { text, diff_hash }) if text == review_text && *diff_hash == 123
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::Review
    );
}

#[tokio::test]
async fn apply_review_update_stores_failure_in_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-fail";
    let error_message = "Review assist failed with exit code 1";
    let mut session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-review-fail"));
    session.id = session_id.to_string().into();
    session.status = Status::AgentReview;
    app.sessions.push_session(session);
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(456));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 456,
            result: Err(error_message.to_string()),
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Failed { error, diff_hash }) if error == error_message && *diff_hash == 456
    ));
}

#[tokio::test]
async fn apply_review_update_ignores_stale_diff_hash() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-stale";
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(999));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 111,
            result: Ok("stale review".to_string()),
        },
    );

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == 999
    ));
}

#[tokio::test]
async fn apply_review_update_keeps_non_agent_review_status_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-review-progress";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/session-review-progress",
    ));
    session.id = session_id.to_string().into();
    session.status = Status::InProgress;
    app.sessions.push_session(session);
    app.sessions.session_handles_mut().insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::InProgress),
    );
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(222));

    // Act
    app.apply_review_update(
        session_id,
        ReviewUpdate {
            diff_hash: 222,
            result: Ok("## Review\nBackground review".to_string()),
        },
    );

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    assert_eq!(
        *app.sessions
            .session_handles()
            .get(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("expected handle status lock"),
        Status::InProgress
    );
}

#[tokio::test]
async fn auto_start_reviews_clears_cache_on_in_progress_transition() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-cache-clear"),
        ));
    app.sessions.sessions_mut()[0].status = Status::InProgress;
    app.set_review_ready_output(session_id, 789, "old review".to_string());
    let session_ids = HashSet::from([session_id.into()]);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].transient_messages.messages(), []);
}

/// Applies queued app events until the pending session-diff request settles.
async fn apply_next_session_diff(app: &mut App) {
    let expected_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("session diff request should be pending");
    tokio::time::timeout(Duration::from_secs(5), async {
        while app
            .pending_session_diff_requests
            .contains_key(&expected_request_id)
        {
            let event = app
                .next_app_event()
                .await
                .expect("app event channel should remain open");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("session diff request should settle");
}

#[tokio::test]
async fn auto_start_reviews_skips_when_diff_hash_unchanged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-hash-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let hash = diff_content_hash(diff_text);
    app.review_cache.insert(
        session_id.to_string().into(),
        ReviewCacheEntry::Ready {
            diff_hash: hash,
            text: "existing review".to_string(),
        },
    );
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Ready { text, .. }) if text == "existing review"
    ));
}

#[tokio::test]
/// Verifies that a review already in `Loading` state with matching diff
/// hash is not re-triggered by a subsequent reducer tick.
async fn auto_start_reviews_skips_when_already_loading_with_same_hash() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-loading-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let hash = diff_content_hash(diff_text);
    app.review_cache
        .insert(session_id.to_string().into(), test_loading_review(hash));
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert — still Loading, not re-triggered
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == hash
    ));
    // Status remains Review because mark_session_agent_review was not called.
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_skips_when_auto_review_is_suppressed() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-suppressed-skip"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    app.review_cache
        .insert(session_id.to_string().into(), ReviewCacheEntry::Suppressed);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Suppressed)
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_keeps_orchestrator_controller_in_review() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    let mut session = crate::test_support::session_fixture_with_folder(PathBuf::from(
        "/tmp/orchestrator-auto-review-skip",
    ));
    session.role = SessionRole::Orchestrator;
    session.status = Status::Review;
    app.sessions.push_session(session);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().times(0);
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);

    // Assert
    assert!(!app.review_cache.contains_key(session_id));
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn auto_start_reviews_starts_loading_for_review_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-hash-start"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;

    let diff_text = "diff --git a/file.rs b/file.rs\n+new line";
    let expected_hash = diff_content_hash(diff_text);
    let session_ids = HashSet::from([session_id.into()]);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.auto_start_reviews(&session_ids);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
}

#[tokio::test]
async fn startup_recovery_restarts_incomplete_managed_focused_review() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-1";
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-recovered-review"),
        ));
    app.sessions.sessions_mut()[0].status = Status::Review;
    let diff_text = "diff --git a/file.rs b/file.rs\n+recovered line";
    let expected_hash = diff_content_hash(diff_text);
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    app.recover_startup_focused_reviews(vec![session_id.to_string()]);
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(
        app.review_cache.get(session_id),
        Some(ReviewCacheEntry::Loading { diff_hash, .. }) if *diff_hash == expected_hash
    ));
    assert_eq!(app.sessions.sessions()[0].status, Status::AgentReview);
}

#[tokio::test]
async fn delete_selected_session_clears_review_cache() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            PathBuf::from("/tmp/session-delete-cache"),
        ));
    app.sessions.select_session_index(Some(0));
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "cached review".to_string(),
        },
    );
    app.save_prompt_progress(test_prompt_mode_snapshot(session_id.clone()));

    // Act
    app.delete_selected_session().await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id.as_str()));
    assert!(!app.prompt_progress.contains_key(session_id.as_str()));
}

#[tokio::test]
async fn restore_prompt_progress_returns_false_without_saved_snapshot() {
    // Arrange
    let mut app = test_app_viewing_reconcile_session(
        Status::Review,
        Vec::new(),
        "session-without-prompt-progress",
    )
    .await;

    // Act
    let restored = app.restore_prompt_progress("session-1").await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.is_empty());
}

#[tokio::test]
async fn restore_prompt_progress_retains_snapshot_when_session_is_missing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = SessionId::from("missing-session");
    app.save_prompt_progress(test_prompt_mode_snapshot(session_id.clone()));

    // Act
    let restored = app.restore_prompt_progress(&session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.contains_key(&session_id));
}

#[tokio::test]
async fn restore_prompt_progress_retains_snapshot_for_question_session() {
    // Arrange
    let mut app = test_app_viewing_reconcile_session(
        Status::Question,
        vec![QuestionItem::new("Continue?")],
        "question-prompt-progress",
    )
    .await;
    let session_id = SessionId::from("session-1");
    app.save_prompt_progress(test_prompt_mode_snapshot(session_id.clone()));

    // Act
    let restored = app.restore_prompt_progress(&session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.contains_key(&session_id));
}

#[tokio::test]
async fn restore_prompt_progress_retains_snapshot_while_stack_reply_is_blocked() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let parent_session_id = SessionId::from("parent-session");
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(parent_session_id.clone())
            .status(Status::Review)
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("active-child")
            .parent_session_id(Some(parent_session_id.clone()))
            .status(Status::InProgress)
            .build(),
    );
    app.save_prompt_progress(test_prompt_mode_snapshot(parent_session_id.clone()));

    // Act
    let restored = app.restore_prompt_progress(&parent_session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.contains_key(&parent_session_id));
}

#[tokio::test]
async fn restore_prompt_progress_cleans_attachments_for_merged_session() {
    // Arrange
    let session_id = SessionId::from("merged-session");
    let attachment_path = crate::app::agentty_home()
        .join("tmp")
        .join(session_id.as_str())
        .join("images")
        .join("image-1.png");
    let attachment_directory = attachment_path
        .parent()
        .expect("attachment path should have a parent")
        .to_path_buf();
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client
        .expect_cleanup_agent_artifacts()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    fs_client.expect_is_dir().times(0..).return_const(true);
    fs_client.expect_exists().times(0..).return_const(true);
    fs_client
        .expect_remove_file()
        .once()
        .with(eq(attachment_path.clone()))
        .returning(|_| Box::pin(async { Ok(()) }));
    fs_client
        .expect_remove_dir()
        .once()
        .with(eq(attachment_directory))
        .returning(|_| Box::pin(async { Ok(()) }));
    let mut clients = crate::test_support::test_app_clients();
    clients.fs_client = Arc::new(fs_client);
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(session_id.clone())
            .status(Status::Merged)
            .build(),
    );
    let mut snapshot = test_prompt_mode_snapshot(session_id.clone());
    snapshot.attachment_state.attachments = vec![PromptAttachment::new(1, attachment_path)];
    app.save_prompt_progress(snapshot);

    // Act
    let restored = app.restore_prompt_progress(&session_id).await;

    // Assert
    assert!(!restored);
    assert!(app.prompt_progress.is_empty());
}

/// Builds one test review request summary for background sync tests.
fn test_review_request_summary(
    display_id: &str,
    state: ReviewRequestState,
) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: display_id.to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session-id".to_string(),
        state,
        status_summary: None,
        target_branch: "main".to_string(),
        title: "feat".to_string(),
        web_url: String::new(),
    }
}

async fn insert_review_session_with_data_dir(app: &App, session_id: &str) {
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            app.active_project_id(),
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    fs::create_dir_all(
        app.services
            .base_path()
            .join(session_folder_name)
            .join(SESSION_DATA_DIR),
    )
    .expect("failed to create session data dir");
}

async fn new_test_app_with_database_pool() -> (App, sqlx::SqlitePool, tempfile::TempDir) {
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let clients = crate::test_support::test_app_clients_with_mock_app_server()
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let app = App::new_with_clients(base_path.clone(), base_path, None, database, clients)
        .await
        .expect("failed to build app");

    (app, pool, base_dir)
}

fn merged_review_request_status_update(
    session_id: &str,
    display_id: &str,
    session_head_hash: &str,
    target_branch: &str,
) -> ReviewRequestStatusUpdate {
    let mut summary = test_review_request_summary(display_id, ReviewRequestState::Merged);
    summary.target_branch = target_branch.to_string();

    ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Merged {
                display_id: display_id.to_string(),
                session_head_hash: Some(session_head_hash.to_string()),
            },
            summary: Some(summary),
        }),
        session_id: session_id.into(),
    }
}

fn successful_manual_sync(project_id: i64, default_branch: &str, pulled_commits: u32) -> AppEvent {
    AppEvent::SyncMainCompleted {
        completion: successful_manual_sync_completion(project_id, default_branch, pulled_commits),
    }
}

fn successful_manual_sync_completion(
    project_id: i64,
    default_branch: &str,
    pulled_commits: u32,
) -> sync::SyncMainCompletion {
    sync::SyncMainCompletion {
        operation: sync::ProjectSyncContext {
            default_branch: default_branch.to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        result: Ok(SyncMainOutcome {
            default_branch: default_branch.to_string(),
            deferred_merged_session_ids: Vec::new(),
            pulled_commit_titles: Vec::new(),
            pulled_commits: Some(pulled_commits),
            pushed_commit_titles: Vec::new(),
            pushed_commits: Some(0),
            resolved_conflict_files: Vec::new(),
        }),
        review_request_updates: Vec::new(),
    }
}

#[tokio::test]
async fn externally_merged_helpers_ignore_missing_session_runtime_state() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("snapshot-only")
            .status(Status::Merged)
            .build(),
    );

    // Act
    let record_result = app
        .record_externally_merged_session("snapshot-only", None)
        .await;
    let missing_session_result = app
        .complete_externally_merged_session("missing", None)
        .await;
    let missing_handles_result = app
        .complete_externally_merged_session("snapshot-only", None)
        .await;

    // Assert
    assert_eq!(record_result, None);
    assert_eq!(missing_session_result, None);
    assert_eq!(missing_handles_result, None);
}

#[tokio::test]
async fn record_externally_merged_session_reports_persistence_failures() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "session-persist-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    sqlx::query!(
        "CREATE TRIGGER fail_merged_hash BEFORE UPDATE OF merged_commit_hash ON session BEGIN \
         SELECT RAISE(FAIL, 'merged hash failed'); END"
    )
    .execute(&pool)
    .await
    .expect("failed to install merged hash trigger");
    let handles = app
        .sessions
        .session_handles_or_err(session_id)
        .expect("expected session handles");
    *handles.status.lock().expect("status lock poisoned") = Status::Done;

    // Act
    let warning = app
        .record_externally_merged_session(session_id, Some("abc1234".to_string()))
        .await
        .expect("persistence failures should produce a warning");

    // Assert
    assert!(warning.contains("Merged commit hash persistence failed"));
    assert!(warning.contains("Could not mark the merged session read-only"));
    assert_eq!(
        app.sessions
            .session_or_err(session_id)
            .expect("expected session")
            .status,
        Status::Review
    );
}

#[tokio::test]
async fn manual_sync_defers_merged_session_when_commit_hash_cannot_load() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "session-load-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let update = merged_review_request_status_update(session_id, "#12", "abc1234", "main");
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    pool.close().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;

    // Assert
    assert_eq!(
        app.sessions
            .session_or_err(session_id)
            .expect("expected session")
            .status,
        Status::Merged
    );
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            deferred_session_count: 1,
            ..
        })
    ));
}

#[tokio::test]
async fn manual_sync_surfaces_restack_failure_and_keeps_parent_merged() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let project_id = app.active_project_id();
    let session_id = "session-restack-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gemini-3.8-flash",
            "wt/parent",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    app.refresh_sessions_now().await;
    let update = merged_review_request_status_update(session_id, "#13", "abc1234", "main");
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    sqlx::query!(
        "CREATE TRIGGER fail_restack BEFORE UPDATE OF parent_session_id ON session BEGIN SELECT \
         RAISE(FAIL, 'restack failed'); END"
    )
    .execute(&pool)
    .await
    .expect("failed to install restack trigger");

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;

    // Assert
    assert_eq!(
        app.sessions
            .session_or_err(session_id)
            .expect("expected parent session")
            .status,
        Status::Merged
    );
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            deferred_session_count: 1,
            ..
        })
    ));
}

#[tokio::test]
async fn complete_externally_merged_session_reports_invalid_done_transition() {
    // Arrange
    let (mut app, _pool, _base_dir) = new_test_app_with_database_pool().await;
    let session_id = "session-done-failure";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let handles = app
        .sessions
        .session_handles_or_err(session_id)
        .expect("expected session handles");
    *handles.status.lock().expect("status lock poisoned") = Status::Draft;

    // Act
    let warning = app
        .complete_externally_merged_session(session_id, Some("abc1234".to_string()))
        .await
        .expect("invalid transition should produce a warning");

    // Assert
    assert_eq!(warning, "Could not archive the merged session");
    assert_eq!(
        *app.sessions
            .session_handles_or_err(session_id)
            .expect("expected session handles")
            .status
            .lock()
            .expect("status lock poisoned"),
        Status::Draft
    );
}

#[tokio::test]
async fn test_apply_review_request_status_update_ignores_background_errors() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.mode = AppMode::List;

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Err("network timeout".to_string()),
        session_id: "session-1".into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_apply_review_request_status_update_persists_summary() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-1";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let summary = test_review_request_summary("#5", ReviewRequestState::Open);
    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Open {
            display_id: "#5".to_string(),
            status_summary: None,
        },
        summary: Some(summary),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    let session = &app.sessions.sessions()[0];
    let review_request = session
        .review_request
        .as_ref()
        .expect("expected linked review request after sync");
    assert_eq!(review_request.summary.display_id, "#5");
}

#[tokio::test]
async fn test_apply_review_request_status_update_closed_cancels_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-closed";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        },
        summary: Some(test_review_request_summary(
            "#7",
            ReviewRequestState::Closed,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded");
    assert_eq!(session.status, Status::Canceled);
}

#[tokio::test]
async fn test_apply_review_request_status_update_closed_cancels_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-closed";
    let child_session_id = "session-child";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert parent session");
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3.8-flash",
            "wt/session",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    let session_data_dir = app
        .services
        .base_path()
        .join(session_folder_name)
        .join(SESSION_DATA_DIR);
    fs::create_dir_all(session_data_dir).expect("failed to create session data dir");
    app.refresh_sessions_now().await;

    let task_result = SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        },
        summary: Some(test_review_request_summary(
            "#7",
            ReviewRequestState::Closed,
        )),
    };

    let update = ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(task_result),
        session_id: session_id.into(),
    };

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Assert
    let parent_session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected parent session to remain loaded");
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child session to remain loaded");
    assert_eq!(parent_session.status, Status::Canceled);
    assert_eq!(child_session.status, Status::Canceled);
}

#[tokio::test]
async fn merged_review_waits_for_successful_manual_sync_before_cleanup() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-merged";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let (cleanup_started_tx, mut cleanup_started_rx) = tokio::sync::mpsc::unbounded_channel();
    let cleanup_release = Arc::new(tokio::sync::Notify::new());
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_main_repo_root().times(1).returning({
        let cleanup_release = Arc::clone(&cleanup_release);

        move |_| {
            let cleanup_release = Arc::clone(&cleanup_release);
            let cleanup_started_tx = cleanup_started_tx.clone();

            Box::pin(async move {
                let _ = cleanup_started_tx.send(());
                cleanup_release.notified().await;

                Ok(PathBuf::from("/tmp/repo"))
            })
        }
    });
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);

    let merged_update = merged_review_request_status_update(session_id, "#9", "abc1234", "main");

    // Act
    tokio::time::timeout(
        Duration::from_millis(250),
        app.apply_review_request_status_update(merged_update),
    )
    .await
    .expect("foreground status update should not start worktree cleanup");
    app.process_pending_app_events().await;
    let status_before_sync = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded")
        .status;
    let cleanup_started_before_sync =
        tokio::time::timeout(Duration::from_millis(50), cleanup_started_rx.recv()).await;
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;
    tokio::time::timeout(Duration::from_secs(1), cleanup_started_rx.recv())
        .await
        .expect("cleanup task should start after manual sync")
        .expect("cleanup task should report startup");

    // Assert
    assert_eq!(status_before_sync, Status::Merged);
    assert!(
        cleanup_started_before_sync.is_err(),
        "remote merge detection must not start cleanup"
    );
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected session to remain loaded");
    assert_eq!(session.status, Status::Done);
    let merged_commit_hash = app
        .services
        .db()
        .sessions()
        .load_session_merged_commit_hash(session_id)
        .await
        .expect("failed to load merged commit hash")
        .expect("expected persisted merged commit hash");
    assert_eq!(merged_commit_hash, "abc1234");

    // Cleanup
    cleanup_release.notify_one();
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn failed_or_unrelated_manual_sync_keeps_session_merged() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let session_id = "session-waiting";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.refresh_sessions_now().await;
    let update = merged_review_request_status_update(session_id, "#11", "def5678", "main");
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;

    // Act
    app.apply_app_events(successful_manual_sync(
        app.active_project_id(),
        "develop",
        0,
    ))
    .await;
    app.apply_app_events(AppEvent::SyncMainCompleted {
        completion: sync::SyncMainCompletion {
            operation: sync::ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 2,
                project_id: app.active_project_id(),
                project_name: "agentty".to_string(),
            },
            result: Err(SyncSessionStartError::Other("sync failed".to_string())),
            review_request_updates: Vec::new(),
        },
    })
    .await;
    app.process_pending_app_events().await;

    // Assert
    let session = app
        .sessions
        .session_or_err(session_id)
        .expect("expected merged session to remain loaded");
    assert_eq!(session.status, Status::Merged);
}

#[tokio::test]
async fn test_apply_review_request_status_update_merged_restacks_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let session_id = "session-merged";
    let child_session_id = "session-child";
    insert_review_session_with_data_dir(&app, session_id).await;
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3.8-flash",
            "wt/session",
            &Status::Draft.to_string(),
            session_id,
            project_id,
        )
        .await
        .expect("failed to insert child session");
    app.services
        .db()
        .sessions()
        .update_session_prompt(child_session_id, "Ready to start")
        .await
        .expect("failed to stage child prompt");
    app.refresh_sessions_now().await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_main_repo_root()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::OutputParse(
                    "test repository root unavailable".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_remove_worktree()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);

    let update = merged_review_request_status_update(session_id, "#9", "abc1234", "main");

    // Act
    app.apply_review_request_status_update(update).await;
    app.process_pending_app_events().await;
    let child_before_sync = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child before manual sync");
    let parent_status_before_sync = app
        .sessions
        .session_or_err(session_id)
        .expect("expected parent before manual sync")
        .status;
    let child_parent_before_sync = child_before_sync.parent_session_id.clone();
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;
    app.refresh_sessions_now().await;
    app.sessions
        .load_session_detail_into_state(app.services.db(), child_session_id)
        .await;

    // Assert
    assert_eq!(parent_status_before_sync, Status::Merged);
    assert_eq!(child_parent_before_sync.as_deref(), Some(session_id));
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected child session to remain loaded");
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert!(child_session.can_start_staged_session());

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    assert_eq!(db_child_session.parent_session_id, None);
    assert_eq!(db_child_session.base_branch, "main");
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn manual_sync_archives_merged_parent_and_merged_stacked_child() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let project_id = app.active_project_id();
    let parent_session_id = "merged-parent";
    let child_session_id = "merged-child";
    insert_review_session_with_data_dir(&app, parent_session_id).await;
    app.services
        .db()
        .sessions()
        .insert_stacked_draft_session(
            child_session_id,
            "gemini-3.8-flash",
            "wt/session-id",
            &Status::Review.to_string(),
            parent_session_id,
            project_id,
        )
        .await
        .expect("failed to insert stacked child session");
    fs::create_dir_all(
        app.services
            .base_path()
            .join(child_session_id.chars().take(8).collect::<String>())
            .join(SESSION_DATA_DIR),
    )
    .expect("failed to create child session data dir");
    app.refresh_sessions_now().await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_main_repo_root()
        .times(2)
        .returning(|_| Box::pin(async { Ok(PathBuf::from("/tmp/repo")) }));
    mock_git_client
        .expect_remove_worktree()
        .times(2)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .times(2)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);
    let parent_update =
        merged_review_request_status_update(parent_session_id, "#20", "parent-tip", "main");
    let child_update =
        merged_review_request_status_update(child_session_id, "#21", "child-tip", "wt/session-id");
    app.apply_review_request_status_update(parent_update).await;
    app.apply_review_request_status_update(child_update).await;
    app.process_pending_app_events().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;

    // Assert
    let parent_session = app
        .sessions
        .session_or_err(parent_session_id)
        .expect("expected merged parent session");
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected merged child session");
    assert_eq!(parent_session.status, Status::Done);
    assert_eq!(child_session.status, Status::Done);
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn manual_sync_recovers_merged_child_after_parent_was_already_archived() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let child_session_id = "stuck-child";
    insert_review_session_with_data_dir(&app, child_session_id).await;
    app.services
        .db()
        .sessions()
        .update_session_stack_base_commit_hash(child_session_id, Some("parent-tip".to_string()))
        .await
        .expect("failed to persist completed parent restack marker");
    app.refresh_sessions_now().await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_main_repo_root()
        .once()
        .returning(|_| Box::pin(async { Ok(PathBuf::from("/tmp/repo")) }));
    mock_git_client
        .expect_remove_worktree()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_delete_branch()
        .once()
        .returning(|_, _| Box::pin(async { Ok(()) }));
    install_mock_git_client(&mut app, mock_git_client);
    let child_update = merged_review_request_status_update(
        child_session_id,
        "#21",
        "child-tip",
        "wt/archived-parent",
    );
    app.apply_review_request_status_update(child_update).await;
    app.process_pending_app_events().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;
    app.process_pending_app_events().await;

    // Assert
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected recovered child session");
    assert_eq!(child_session.status, Status::Done);
    app.services.wait_for_cleanup_tasks().await;
}

#[tokio::test]
async fn manual_sync_defers_stranded_child_when_restack_marker_cannot_load() {
    // Arrange
    let (mut app, pool, _base_dir) = new_test_app_with_database_pool().await;
    let child_session_id = "stranded-child";
    insert_review_session_with_data_dir(&app, child_session_id).await;
    app.services
        .db()
        .sessions()
        .update_session_stack_base_commit_hash(child_session_id, Some("parent-tip".to_string()))
        .await
        .expect("failed to persist completed parent restack marker");
    app.refresh_sessions_now().await;
    let child_update = merged_review_request_status_update(
        child_session_id,
        "#22",
        "child-tip",
        "wt/archived-parent",
    );
    app.apply_review_request_status_update(child_update).await;
    app.process_pending_app_events().await;
    pool.close().await;

    // Act
    app.apply_app_events(successful_manual_sync(app.active_project_id(), "main", 1))
        .await;

    // Assert
    let child_session = app
        .sessions
        .session_or_err(child_session_id)
        .expect("expected stranded child session");
    assert_eq!(child_session.status, Status::Merged);
    assert!(matches!(app.mode, AppMode::List));
    assert!(matches!(
        app.project_sync_status.as_ref().map(|status| &status.phase),
        Some(sync::ProjectSyncPhase::Complete {
            deferred_session_count: 1,
            ..
        })
    ));
    let workflow_output = app
        .sessions
        .session_handles_or_err(child_session_id)
        .expect("expected stranded child handles")
        .transcript
        .lock()
        .expect("transcript lock poisoned")
        .replay_text()
        .expect("expected durable marker warning");
    assert!(workflow_output.contains("Durable restack marker load failed"));
}
