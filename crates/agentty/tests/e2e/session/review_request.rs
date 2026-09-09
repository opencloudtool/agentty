//! Review request publication, status, and cleanup.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use agentty::domain::session_message::SessionMessageKind;
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{
    E2eResult, RESOLVED_DECISION_REVIEW_TEXT, run_git, seed_review_ready_session,
    seed_review_ready_session_with_review_request, seed_review_with_resolved_decision,
    seed_review_worktree_with_diff,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Review-request notice body used by the timeline-order regression.
const REVIEW_REQUEST_TIMELINE_NOTICE_TEXT: &str =
    "Created PR https://github.com/agentty-xyz/agentty/pull/42";

/// Seeds a review-ready worktree whose delayed successful publish keeps the
/// manual task active beyond the upstream-ref refresh observed by the feature
/// scenario.
fn seed_slow_successful_review_request_publish(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    seed_successful_review_request_publish(env, 3, 15, "wt/review-s", false)
}

/// Seeds a review-ready session whose chosen review branch was deleted from
/// the remote before its first publish.
fn seed_review_request_publish_with_deleted_remote_branch(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    seed_successful_review_request_publish(env, 0, 0, "review/deleted", true)
}

/// Seeds a live focused review that completes before a delayed review-request
/// publish, reproducing the cross-source transcript ordering boundary.
fn seed_review_request_timeline(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_with_resolved_decision(env)?;

    seed_successful_review_request_publish(env, 0, 0, "wt/review-s", false)
}

/// Configures the review-ready worktree and forge stubs for one successful
/// review-request publish with deterministic command delays.
fn seed_successful_review_request_publish(
    env: &BuilderEnv,
    push_delay_seconds: u64,
    create_delay_seconds: u64,
    review_branch_name: &str,
    remote_branch_was_deleted: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    run_git(&session_worktree, &["branch", "-m", "wt/review-s"])?;
    run_git(&session_worktree, &["branch", "main", "HEAD"])?;
    if remote_branch_was_deleted {
        let stale_tracking_ref = format!("refs/remotes/origin/{review_branch_name}");
        run_git(
            &session_worktree,
            &["update-ref", stale_tracking_ref.as_str(), "HEAD"],
        )?;
    }

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let remote_lookup = if remote_branch_was_deleted {
        r#"if [ "$1" = "ls-remote" ]; then
  exit 0
fi
"#
        .to_string()
    } else {
        String::new()
    };
    let push_lease_guard = if remote_branch_was_deleted {
        format!(
            r#"  case "$*" in
    *"--force-with-lease=refs/heads/{review_branch_name}: --set-upstream"*) ;;
    *) printf '%s\n' 'missing empty lease for deleted remote branch' >&2; exit 1 ;;
  esac
"#
        )
    } else {
        String::new()
    };
    let script = format!(
        r#"#!/bin/sh
{remote_lookup}if [ "$1" = "push" ]; then
{push_lease_guard}  sleep {push_delay_seconds}
  exit 0
fi
exec '{}' "$@"
"#,
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    let gh_path = env.stub_bin.join("gh");
    let gh_script = r#"#!/bin/sh
marker_path="${0}.created"
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"api"*"/pulls"*)
    if [ -f "$marker_path" ]; then
      printf '%s\n' '[{"number":42}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  *"pr create"*)
    sleep __CREATE_DELAY_SECONDS__
    touch "$marker_path"
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Review-ready session shortcuts","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"__REVIEW_BRANCH_NAME__","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#
    .replace(
        "__CREATE_DELAY_SECONDS__",
        &create_delay_seconds.to_string(),
    )
    .replace("__REVIEW_BRANCH_NAME__", review_branch_name);
    std::fs::write(&gh_path, gh_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Seeds one published review-ready session whose latest auto-push completion
/// is persisted as transcript output.
fn seed_session_with_published_branch_push_notice(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = "published-push-0001";

    common::seed_session(
        env,
        SessionSeed::regular(session_id, "gpt-5.6-sol", "main", "Review")
            .with_title("Published push notice"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_published_upstream_ref(
                session_id,
                Some("origin/wt/published-push".to_string()),
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::WorkflowNotice,
                "\n[Branch Push] Auto-pushed published branch after completed turn.\n",
            )
            .await
    })?;

    // Match `session_folder()` so startup loads the seeded review session.
    let worktree_name = &session_id[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    Ok(())
}

/// Seeds a merged GitHub review response and delays runtime worktree removal
/// so the feature scenario can prove terminal rendering stays responsive.
fn seed_slow_merged_review_request_status(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;

    let sync_origin = env.agentty_root.join("sync-origin.git");
    std::fs::create_dir_all(&sync_origin)?;
    run_git(&sync_origin, &["init", "--bare", "."])?;
    let sync_origin_path = sync_origin.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &["remote", "add", "origin", sync_origin_path.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Review-ready session shortcuts","state":"MERGED","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/review-s","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"APPROVED","mergedAt":"2026-01-01T00:00:00Z"}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o750))?;

    install_delayed_worktree_remove_stub(env, 4)
}

/// Seeds a merged parent and merged stacked child whose review target still
/// names the parent branch, plus a remote for the manual main sync.
fn seed_merged_stacked_review_requests(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("stack-parent-0001", "gpt-5.6-sol", "main", "Merged")
            .with_title("Merged stack parent"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            "stack-child-0001",
            "gpt-5.6-sol",
            "wt/stack-pa",
            "Merged",
            "stack-parent-0001",
        )
        .with_title("Merged stack child"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let parent_review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/stack-pa".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Merged stack parent".to_string(),
                web_url: "https://github.com/example/project/pull/42".to_string(),
            },
        };
        let child_review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#43".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/stack-ch".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: None,
                target_branch: "wt/stack-pa".to_string(),
                title: "Merged stack child".to_string(),
                web_url: "https://github.com/example/project/pull/43".to_string(),
            },
        };

        database
            .reviews()
            .update_session_review_request("stack-parent-0001", Some(parent_review_request))
            .await?;
        database
            .reviews()
            .update_session_review_request("stack-child-0001", Some(child_review_request))
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-pa"))?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-ch"))?;
    let sync_origin = env.agentty_root.join("sync-origin.git");
    std::fs::create_dir_all(&sync_origin)?;
    run_git(&sync_origin, &["init", "--bare", "."])?;
    let sync_origin_path = sync_origin.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &["remote", "add", "origin", sync_origin_path.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    install_delayed_worktree_remove_stub(env, 1)
}

/// Installs a git wrapper that delays worktree removal while forwarding all
/// other commands to the real executable.
fn install_delayed_worktree_remove_stub(
    env: &BuilderEnv,
    delay_seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let real_git = real_git.to_string_lossy().replace('\'', "'\"'\"'");
    let git_path = env.stub_bin.join("git");
    std::fs::write(
        &git_path,
        format!(
            r#"#!/bin/sh
if [ "$1" = "worktree" ] && [ "$2" = "remove" ]; then
  exec sleep {delay_seconds}
fi
exec '{real_git}' "$@"
"#
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Verify that completed published-branch auto-push feedback is rendered as a
/// transcript message rather than as a transient status line.
#[test]
fn published_branch_push_notice_renders_as_transcript_message() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("published_branch_push_notice")
        .with_git()
        .setup(seed_session_with_published_branch_push_notice)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Branch Push]", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "published_branch_push_notice",
                        "Published branch auto-push completion rendered as a transcript message",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "[Branch Push]", &full);
                assert_eq!(
                    view_text
                        .matches("Auto-pushed published branch after completed turn.")
                        .count(),
                    1
                );
            },
        )?;

    Ok(())
}

/// Verify that pressing `p` in a review-ready session opens the review-request
/// publish popup.
#[test]
fn review_request_publish_shortcut_opens_publish_popup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_publish_shortcut")
        .with_git()
        .setup(seed_review_ready_session)
        .zola(
            "Review request publish shortcut",
            "Open the review-request publish popup directly from session view with `p`.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_publish_popup",
                        "Review-request publish popup after pressing p",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Publish Review Request", &full);
                assertion::assert_text_in_region(frame, "Enter: publish review request", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Leave blank to push as `wt/review-s`",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that a first review-request publish can recreate a custom branch
/// that was deleted remotely despite a stale local remote-tracking ref.
#[test]
fn review_request_publish_recreates_deleted_remote_branch() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_deleted_remote_branch")
        .with_git()
        .setup(seed_review_request_publish_with_deleted_remote_branch)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .write_text("review/deleted")
                    .press_key("Enter")
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "deleted_remote_branch_recreated",
                        "Review request published after its former remote branch was deleted",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                assertion::assert_not_visible(frame, "already exists");
            },
        )?;

    Ok(())
}

/// Verify that confirming review-request publish returns to an interactive
/// session chat while the push runs in the background.
#[test]
fn review_request_publish_runs_in_background() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_background_publish")
        .with_git()
        .zola(
            "Background review-request publish",
            "Publish a review request in the background and receive its link in session chat.",
            41,
        )
        .setup(seed_slow_successful_review_request_publish)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("Enter")
                    .wait_for_text("Publishing review request...", 3000)
                    .capture_labeled(
                        "background_publish_started",
                        "Review-request publish progress shown inline in session chat",
                    )
                    .press_key("?")
                    .wait_for_text("Keybindings", 3000)
                    .capture_labeled(
                        "background_publish_help",
                        "Session help remains available while publishing",
                    )
                    .press_key("q")
                    .viewing_pause_ms(5500)
                    .capture_labeled(
                        "background_publish_waiting_for_url",
                        "Review-request publish progress remains until its URL is ready",
                    )
                    .wait_for_text("[Review Request] Created PR", 20000)
                    .capture_labeled(
                        "background_publish_finished",
                        "Review-request link recorded in session transcript history",
                    )
            },
            |frame, report| {
                let loading_frame = common::frame_from_capture(&report.captures[0]);
                let loading_full = Region::full(loading_frame.cols(), loading_frame.rows());
                assertion::assert_text_in_region(
                    &loading_frame,
                    "Publishing review request...",
                    &loading_full,
                );
                assertion::assert_text_in_region(&loading_frame, "q: back", &loading_full);

                let help_frame = common::frame_from_capture(&report.captures[1]);
                let help_full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &help_full);

                let waiting_frame = common::frame_from_capture(&report.captures[2]);
                let waiting_full = Region::full(waiting_frame.cols(), waiting_frame.rows());
                assertion::assert_text_in_region(
                    &waiting_frame,
                    "Publishing review request...",
                    &waiting_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                assertion::assert_text_in_region(frame, "q: back", &full);
            },
        )?;

    Ok(())
}

/// Verify a review-request notice created after focused review completion is
/// rendered below that review instead of being regrouped above it.
#[test]
fn review_request_notice_follows_completed_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_timeline_order")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(seed_review_request_timeline)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("f")
                    .wait_for_text(RESOLVED_DECISION_REVIEW_TEXT, 30000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 3000)
                    .press_key("Enter")
                    .wait_for_text(REVIEW_REQUEST_TIMELINE_NOTICE_TEXT, 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "review_request_timeline_order",
                        "Review-request result follows the earlier focused review",
                    )
            },
            |frame, _report| {
                let review_finding = frame
                    .find_text(RESOLVED_DECISION_REVIEW_TEXT)
                    .into_iter()
                    .next()
                    .expect("completed focused review should render");
                let review_request_notice = frame
                    .find_text(REVIEW_REQUEST_TIMELINE_NOTICE_TEXT)
                    .into_iter()
                    .next()
                    .expect("review-request notice should render");

                assert!(
                    review_finding.rect.row < review_request_notice.rect.row,
                    "review-request notice should follow the earlier focused review"
                );
            },
        )?;

    Ok(())
}

/// Verify that linked review requests refresh in the background without a
/// manual review-request sync shortcut and disable local merge queueing.
#[test]
fn review_request_sync_runs_in_background() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_background_sync")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .zola(
            "Background review-request sync",
            "Review sessions track linked pull requests in the background instead of exposing a \
             manual sync shortcut.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_background_sync",
                        "Review session help overlay without a manual sync shortcut",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "p: Create or refresh", &full);
                assert!(
                    !view_text.contains("s: Sync"),
                    "manual sync help action should be absent"
                );
                assertion::assert_not_visible(frame, "m: add to merge queue");
            },
        )?;

    Ok(())
}

/// Verify that a remote merge stays read-only `Merged` until the user syncs
/// main, then moves to `Done` without waiting for slow worktree cleanup.
#[test]
fn test_merged_review_request_waits_for_manual_sync() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("merged_review_request_manual_sync")
        .with_git()
        .setup(seed_slow_merged_review_request_status)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Merged", 10_000)
                    .capture_labeled(
                        "merged_status",
                        "Merged session waits in Active for manual main sync",
                    )
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "merged_read_only_actions",
                        "Merged session exposes only read-only review actions",
                    )
                    .press_key("?")
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_text("Enter/l: open", 5000)
                    .press_key("Enter")
                    .wait_for_text("Esc/Left: files", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "merged_diff_read_only",
                        "Merged diff hides inline comment actions",
                    )
                    .press_key("q")
                    .press_key("q")
                    .press_key("s")
                    .wait_for_text("Synced test-project/main", 10_000)
                    .wait_for_text("Done", 10_000)
                    .capture_labeled(
                        "merged_done_after_sync",
                        "Manual main sync archives the merged session",
                    )
            },
            |frame, report| {
                assert_eq!(report.captures.len(), 4);
                let merged_frame = common::frame_from_capture(&report.captures[0]);
                let merged_full = Region::full(merged_frame.cols(), merged_frame.rows());
                assertion::assert_text_in_region(&merged_frame, "ACTIVE —— 1", &merged_full);
                assertion::assert_text_in_region(&merged_frame, "Merged", &merged_full);

                let read_only_frame = common::frame_from_capture(&report.captures[1]);
                let read_only_full = Region::full(read_only_frame.cols(), read_only_frame.rows());
                assertion::assert_text_in_region(&read_only_frame, "Show diff", &read_only_full);
                let read_only_text = read_only_frame.text_in_region(&read_only_full);
                for mutating_action in ["Reply", "Open commands menu", "Add to merge queue", "Sync"]
                {
                    assert!(
                        !read_only_text.contains(mutating_action),
                        "Merged help must hide `{mutating_action}`"
                    );
                }

                let diff_frame = common::frame_from_capture(&report.captures[2]);
                let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                let diff_text = diff_frame.text_in_region(&diff_full);
                assertion::assert_text_in_region(&diff_frame, "Esc/Left: files", &diff_full);
                assert!(!diff_text.contains("Enter: comment"));
                assert!(!diff_text.contains("s: submit comments"));

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Done", &full);
            },
        )?;

    Ok(())
}

/// Verify one successful manual main sync archives both reviews in a fully
/// merged stack, including the child that still targets the parent branch.
#[test]
fn merged_stacked_reviews_complete_together_after_manual_sync() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("merged_stacked_reviews_complete_together_after_manual_sync")
        .with_git()
        .setup(seed_merged_stacked_review_requests)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Merged stack child", 5000)
                    .press_key("s")
                    .wait_for_text("Synced test-project/main", 10_000)
                    .wait_for_text("Done", 10_000)
                    .capture_labeled(
                        "merged_stack_done",
                        "Manual main sync archives both merged stack sessions",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let session_list_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "Merged stack parent", &full);
                assertion::assert_text_in_region(frame, "Merged stack child", &full);
                assert!(
                    session_list_text.matches("Done").count() >= 2,
                    "expected both merged stack rows to be Done:\n{session_list_text}"
                );
            },
        )?;

    Ok(())
}

/// Verify that confirming quit does not wait indefinitely for externally
/// merged worktree cleanup.
#[test]
fn merged_review_request_cleanup_does_not_block_quit() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock();
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    seed_slow_merged_review_request_status(&env)?;
    install_delayed_worktree_remove_stub(&env, 30)?;
    let mut session = env.builder().spawn()?;
    let scenario = Scenario::new("merged_cleanup_quit")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .wait_for_text("Merged", 10_000)
        .press_key("s")
        .wait_for_text("Synced test-project/main", 10_000)
        .wait_for_text("Done", 10_000)
        .compose(&common::open_quit_dialog())
        .press_key("y");

    // Act
    scenario.execute_in_pty(&mut session)?;
    let exited_successfully = session.wait_for_exit(Duration::from_secs(8))?;

    // Assert
    assert!(
        exited_successfully,
        "confirmed quit should cancel cleanup after the shutdown deadline"
    );

    Ok(())
}

/// Verify that linked review requests expose their browser URL in the session
/// header.
#[test]
fn review_request_url_appears_in_session_header() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_url_header")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("https://github.com/agentty-xyz/agentty/pull/42", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_url_header",
                        "Linked review-request URL visible in the session header",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
            },
        )?;

    Ok(())
}
