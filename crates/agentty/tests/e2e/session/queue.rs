//! Queued session messages and actions.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentty::domain::session_message::SessionMessageKind;
use testty::assertion;
use testty::region::Region;

use super::fixture::{
    E2eResult, run_git, seed_project_settings, seed_rebase_transcript_session_with_delay,
    seed_running_stop_session,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Stable id for the seeded rebasing session used by message-queue tests.
const REBASING_QUEUE_SESSION_ID: &str = "rebasing-queue-0001";

/// Clarification question emitted after session sync has already been queued.
const QUEUED_SYNC_QUESTION_TEXT: &str = "Should I continue before syncing?";

/// Seeds one rebasing session so message queueing can be exercised without a
/// live git operation or agent backend.
fn seed_rebasing_queue_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(REBASING_QUEUE_SESSION_ID, "gpt-5.6-sol", "main", "Rebasing")
            .with_title("Rebasing message queue"),
    )?;

    let worktree_name = &REBASING_QUEUE_SESSION_ID[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                REBASING_QUEUE_SESSION_ID,
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                REBASING_QUEUE_SESSION_ID,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Assist] Resolving existing conflicts.\n",
            )
            .await
    })?;

    Ok(())
}

/// Answer emitted before the queued session sync is allowed to start.
const QUEUED_SYNC_TURN_ANSWER: &str = "Running turn completed before sync";

/// Answer emitted before queued review-request creation begins.
const QUEUED_REVIEW_REQUEST_TURN_ANSWER: &str =
    "Running turn completed before review request creation";

/// Answer emitted for the chat message queued before review-request creation.
const QUEUED_REVIEW_FOLLOW_UP_ANSWER: &str = "Queued review follow-up completed before publish";

/// Answer emitted for one-shot utility prompts in the mixed FIFO scenario.
///
/// Title generation, commit-message generation, and review-request metadata
/// run as detached one-shot commands alongside session turns, so they need a
/// response that no scenario assertion looks for.
const QUEUED_FIFO_UTILITY_ANSWER: &str = "Queued FIFO helper response";

/// Focused-review result emitted after a turn completes in an inactive project.
const DEFERRED_PROJECT_REVIEW_TEXT: &str = "Deferred project completion received focused review.";

/// Prompt persisted before the owning project becomes inactive.
const DEFERRED_PROJECT_REVIEW_PROMPT: &str = "Finish while I view another project";

/// Turn result that the inactive project's focused review must receive as
/// history.
const DEFERRED_PROJECT_TURN_ANSWER: &str = "Completed work while another project was active.";

/// Diagnostic emitted when inactive focused review loses persisted chat
/// history.
const MISSING_DEFERRED_PROJECT_HISTORY_TEXT: &str =
    "Inactive focused review omitted saved session history.";

/// Installs a delayed session turn plus a distinct focused-review response so
/// project-switching scenarios can prove the automatic review ran.
fn install_deferred_project_review_claude_stub(
    env: &BuilderEnv,
    review_started_marker: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
case "$prompt" in
  *"Review the Git diff for display in a terminal UI."*)
    case "$prompt" in
      *"{DEFERRED_PROJECT_REVIEW_PROMPT}"*"{DEFERRED_PROJECT_TURN_ANSWER}"*)
        printf 'started\n' > '{}'
        result='{{\"project_impact\":[\"{DEFERRED_PROJECT_REVIEW_TEXT}\"],\"suggestions\":[]}}'
        ;;
      *)
        result='{{\"project_impact\":[\"{MISSING_DEFERRED_PROJECT_HISTORY_TEXT}\"],\"suggestions\":[{{\"details\":\"Restore saved session history.\",\"severity\":\"medium\"}}]}}'
        ;;
    esac
    ;;
  *"Generate a concise, commit-style title"*)
    result='{{\"answer\":\"Cross-project focused review\",\"questions\":[]}}'
    ;;
  *"Generate the canonical session commit message"*)
    result='{{\"answer\":\"test: exercise cross-project review\",\"questions\":[]}}'
    ;;
  *)
    sleep 3
    printf 'review me\n' > deferred-project-review.txt
    result='{{\"answer\":\"{DEFERRED_PROJECT_TURN_ANSWER}\",\"questions\":[]}}'
    ;;
esac
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$result\",\"usage\":{{\"input_tokens\":5,\"output_tokens\":9}}}}"
"#,
        review_started_marker.display(),
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
            ("DefaultFastAgent", "claude"),
            ("DefaultFastModel", "claude-haiku-4-5-20251001"),
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", "claude-haiku-4-5-20251001"),
        ],
    )
}

/// Installs a delayed Claude turn so the scenario can queue sync while the
/// worker is still active, optionally forcing its later validation to fail.
fn install_delayed_sync_claude_stub(
    env: &BuilderEnv,
    fail_sync_validation: bool,
    delay_seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let validation_failure_marker = env.stub_bin.join("queued-sync-validation-failure");
    let mark_validation_failure = if fail_sync_validation {
        format!("touch '{}'; ", validation_failure_marker.display())
    } else {
        String::new()
    };
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep {delay_seconds}
{mark_validation_failure}printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{QUEUED_SYNC_TURN_ANSWER}"}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{QUEUED_SYNC_TURN_ANSWER}\",\"questions\":[]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    if fail_sync_validation {
        install_sync_validation_failure_git_stub(env, &validation_failure_marker)?;
    }

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Installs a delayed Claude turn that ends with a clarification question so
/// the scenario can cancel it while sync remains queued on the worker.
fn install_queued_sync_question_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 8
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"Need one clarification before sync.\",\"questions\":[{{\"text\":\"{QUEUED_SYNC_QUESTION_TEXT}\",\"options\":[\"Continue\"]}}]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Installs a Git wrapper that fails only the marked queued-sync validation.
fn install_sync_validation_failure_git_stub(
    env: &BuilderEnv,
    validation_failure_marker: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let script = format!(
        r#"#!/bin/sh
if [ -f '{}' ] && [ "$1" = "rev-parse" ] && [ "$2" = "--is-bare-repository" ]; then
  printf '%s\n' 'forced queued-sync validation failure' >&2
  exit 1
fi
exec '{}' "$@"
"#,
        validation_failure_marker.display(),
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Installs delayed agent, Git, and GitHub stubs so review-request creation
/// can be queued during a live turn and observed after that turn completes.
fn install_queued_review_request_stubs(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    run_git(
        &env.workdir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ],
    )?;

    let claude_path = env.stub_bin.join("claude");
    let claude_script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 8
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{QUEUED_REVIEW_REQUEST_TURN_ANSWER}"}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{QUEUED_REVIEW_REQUEST_TURN_ANSWER}\",\"questions\":[]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );
    std::fs::write(&claude_path, claude_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let git_script = format!(
        r#"#!/bin/sh
if [ "$1" = "push" ]; then
  sleep 2
  exit 0
fi
exec '{}' "$@"
"#,
        real_git.display()
    );
    std::fs::write(&git_path, git_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
marker_path="${0}.created"
case "$*" in
  *"auth status"*)
    exit 0
    ;;
esac
case "$*" in
  *"api"*"/pulls"*)
    if [ -f "$marker_path" ]; then
      printf '%s\n' '[{"number":42}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  *"pr create"*)
    touch "$marker_path"
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Queued review request","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/queued-review","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}'
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

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Extends the queued-review stubs with enough turn latency for a deliberate
/// cancellation after the publish action has been queued.
fn install_cancelled_queued_review_request_stubs(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    install_queued_review_request_stubs(env)?;

    let claude_path = env.stub_bin.join("claude");
    let claude_script = std::fs::read_to_string(&claude_path)?;
    let delayed_script = claude_script.replacen("sleep 8", "sleep 30", 1);
    if delayed_script == claude_script {
        return Err("queued review-request stub is missing its turn delay".into());
    }
    std::fs::write(&claude_path, delayed_script)?;

    Ok(())
}

/// Extends the queued-review stubs with a distinct second agent turn so the
/// mixed FIFO feature can prove the chat message executes before publishing.
///
/// Each response is selected from the prompt on stdin rather than from
/// invocation order: Agentty runs detached one-shot utility commands (title
/// generation first of all) concurrently with the opening turn, so an
/// order-based stub can hand the delayed first-turn response to a helper
/// command and answer the real turn immediately.
fn install_fifo_queued_review_request_stubs(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    install_queued_review_request_stubs(env)?;

    let claude_path = env.stub_bin.join("claude");
    let claude_script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
emit_answer() {{
  printf '%s\n' '{{"type":"system","subtype":"init"}}'
  printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"'"$1"'"}}]}}}}'
  printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"'"$1"'\",\"questions\":[]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
}}
input=$(cat)
case "$input" in
  *"For this one-shot utility prompt"*)
    emit_answer '{QUEUED_FIFO_UTILITY_ANSWER}'
    ;;
  *"Review once more"*)
    emit_answer '{QUEUED_REVIEW_FOLLOW_UP_ANSWER}'
    ;;
  *"Queue mixed work"*)
    # Keep the first turn active while the PTY driver queues both items, even
    # when the E2E test group is running four scenarios concurrently.
    sleep 20
    emit_answer '{QUEUED_REVIEW_REQUEST_TURN_ANSWER}'
    ;;
  *)
    emit_answer '{QUEUED_FIFO_UTILITY_ANSWER}'
    ;;
esac
"#
    );
    std::fs::write(&claude_path, claude_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Verify the per-press LIFO `Ctrl+c` flow for queued chat messages: pressing
/// `Enter` while a session is `InProgress` opens the chat composer and queues
/// the typed message inline beneath the running turn; with two messages
/// queued, the first `Ctrl+c` pops only the most recently queued entry while
/// the older entry and the running turn remain (`Ctrl+c: stop` still
/// rendered); a follow-up `Ctrl+c` pops the remaining queued entry; and a
/// final `Ctrl+c` with an empty queue cancels the running turn and returns
/// the session to review-ready controls.
#[test]
fn session_queue_chat_messages_during_in_progress_turn() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queue_chat_messages")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("first queued")
                    .wait_for_text("first queued", 3000)
                    .press_key("Enter")
                    .wait_for_text("≡ queued ›", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("second queued")
                    .wait_for_text("second queued", 3000)
                    .press_key("Enter")
                    .wait_for_text("second queued", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "two_queued_messages_visible",
                        "Two queued chat messages rendered inline beneath the running turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "first_ctrl_c_pops_last_queued",
                        "First Ctrl+c pops the most recently queued chat message and leaves the \
                         running turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "second_ctrl_c_pops_remaining_queued",
                        "Second Ctrl+c pops the last remaining queued chat message and leaves the \
                         running turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_text("Enter: reply", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "third_ctrl_c_cancels_turn",
                        "Third Ctrl+c cancels the running turn and returns the session to review",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(&queued_frame, "≡ queued ›", &queued_full);
                assertion::assert_text_in_region(&queued_frame, "first queued", &queued_full);
                assertion::assert_text_in_region(&queued_frame, "second queued", &queued_full);

                let after_first_frame = common::frame_from_capture(&report.captures[1]);
                let after_first_full =
                    Region::full(after_first_frame.cols(), after_first_frame.rows());
                assertion::assert_text_in_region(
                    &after_first_frame,
                    "Ctrl+c: stop",
                    &after_first_full,
                );
                assertion::assert_text_in_region(
                    &after_first_frame,
                    "≡ queued ›",
                    &after_first_full,
                );
                assertion::assert_text_in_region(
                    &after_first_frame,
                    "first queued",
                    &after_first_full,
                );
                assertion::assert_not_visible(&after_first_frame, "second queued");

                let after_second_frame = common::frame_from_capture(&report.captures[2]);
                let after_second_full =
                    Region::full(after_second_frame.cols(), after_second_frame.rows());
                assertion::assert_text_in_region(
                    &after_second_frame,
                    "Ctrl+c: stop",
                    &after_second_full,
                );
                assertion::assert_not_visible(&after_second_frame, "queued ›");
                assertion::assert_not_visible(&after_second_frame, "first queued");
                assertion::assert_not_visible(&after_second_frame, "second queued");

                let cleared_full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &cleared_full);
                assertion::assert_not_visible(frame, "Ctrl+c: stop");
                assertion::assert_not_visible(frame, "queued ›");
            },
        )?;

    Ok(())
}

/// Verify a `Rebasing` session exposes review-request publishing and still
/// queues submitted follow-up messages behind the active sync.
#[test]
fn session_queue_chat_message_during_rebase() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queue_chat_message_during_rebase")
        .with_git()
        .setup(seed_rebasing_queue_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Rebasing...", 5000)
                    .wait_for_text("Enter: queue message", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("follow up after sync")
                    .wait_for_text("follow up after sync", 3000)
                    .press_key("Enter")
                    .wait_for_text("≡ queued ›", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "message_queued_during_rebase",
                        "Follow-up message queued inline while session sync remains active",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Rebasing...", &full);
                assertion::assert_text_in_region(frame, "p: PR", &full);
                assertion::assert_text_in_region(frame, "[Commit] No changes to commit.", &full);
                assertion::assert_text_in_region(
                    frame,
                    "[Sync Assist] Resolving existing conflicts.",
                    &full,
                );
                assertion::assert_text_in_region(frame, "≡ queued ›", &full);
                assertion::assert_text_in_region(frame, "follow up after sync", &full);
                let commit_row = frame
                    .find_text("[Commit] No changes to commit.")
                    .first()
                    .expect("missing commit notice")
                    .rect
                    .row;
                let sync_assist_row = frame
                    .find_text("[Sync Assist] Resolving existing conflicts.")
                    .first()
                    .expect("missing sync-assist notice")
                    .rect
                    .row;
                let queued_message_row = frame
                    .find_text("queued › follow up after sync")
                    .first()
                    .expect("missing queued follow-up")
                    .rect
                    .row;

                assert_eq!(sync_assist_row, commit_row + 2);
                assert_eq!(queued_message_row, sync_assist_row + 2);
            },
        )?;

    Ok(())
}

/// Verify review-request creation submitted during a live rebase stays queued
/// until sync completes, then publishes on the same session worker.
#[test]
fn review_request_creation_queues_during_rebase() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_queued_during_rebase")
        .with_git()
        .setup(|env| {
            seed_rebase_transcript_session_with_delay(env, 10)?;
            install_queued_review_request_stubs(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Completed answer before rebase.", 5000)
                    .wait_for_text("r: sync", 5000)
                    .press_key("r")
                    .wait_for_text("Rebasing...", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .capture_labeled(
                        "review_request_queued_during_rebase",
                        "Review-request creation queued behind the active session sync",
                    )
                    .wait_for_text("[Sync] Successfully synced", 20000)
                    .wait_for_text("Publishing review request...", 5000)
                    .capture_labeled(
                        "review_request_started_after_rebase",
                        "Review-request creation starts after session sync completes",
                    )
                    .wait_for_text("[Review Request] Created PR", 15000)
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(&queued_frame, "Rebasing...", &queued_full);
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ review request — publish after this turn",
                    &queued_full,
                );
                assertion::assert_not_visible(&queued_frame, "Publishing review request...");

                let started_frame = common::frame_from_capture(&report.captures[1]);
                let started_full = Region::full(started_frame.cols(), started_frame.rows());
                assertion::assert_text_in_region(
                    &started_frame,
                    "[Sync] Successfully synced",
                    &started_full,
                );
                assertion::assert_text_in_region(
                    &started_frame,
                    "Publishing review request...",
                    &started_full,
                );
                assertion::assert_not_visible(&started_frame, "Rebasing...");
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify running sessions queue `r` sync without interrupting the active
/// turn, then rebase before returning to review.
#[test]
fn session_running_turn_shows_sync_shortcut() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_running_sync_shortcut")
        .with_git()
        .setup(|env| install_delayed_sync_claude_stub(env, false, 10))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Keep the active turn running")
                    .wait_for_text("Keep the active turn running", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("r: sync", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "running_sync_queued",
                        "Running session view after queueing sync",
                    )
                    .wait_for_text(QUEUED_SYNC_TURN_ANSWER, 30000)
                    .wait_for_text("[Sync] Successfully synced", 10000)
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled(
                        "running_sync_completed",
                        "Running turn completes before queued sync",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &queued_full,
                );
                assertion::assert_text_in_region(&queued_frame, "Ctrl+c: stop", &queued_full);
                let active_turn_row = queued_frame
                    .find_text("Keep the active turn running")
                    .first()
                    .expect("missing active turn prompt")
                    .rect
                    .row;
                let queued_sync_row = queued_frame
                    .find_text("rebase onto the base branch after this turn")
                    .first()
                    .expect("missing queued sync notice")
                    .rect
                    .row;
                assert!(active_turn_row < queued_sync_row);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, QUEUED_SYNC_TURN_ANSWER, &full);
                assertion::assert_text_in_region(frame, "[Sync] Successfully synced", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "[Stopped]");
                assertion::assert_not_visible(frame, "≡ sync —");

                let answer_row = frame
                    .find_text(QUEUED_SYNC_TURN_ANSWER)
                    .first()
                    .expect("missing completed turn answer")
                    .rect
                    .row;
                let sync_row = frame
                    .find_text("[Sync] Successfully synced")
                    .first()
                    .expect("missing queued sync result")
                    .rect
                    .row;
                assert!(answer_row < sync_row);
            },
        )?;

    Ok(())
}

/// Verify canceling a clarification question wakes the session worker and
/// resumes sync that was queued during the preceding active turn.
#[test]
fn session_queued_sync_resumes_after_question_cancel() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_sync_resumes_after_question_cancel")
        .with_git()
        .setup(install_queued_sync_question_claude_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Ask before completing this turn")
                    .wait_for_text("Ask before completing this turn", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("r: sync", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .wait_for_text(QUEUED_SYNC_QUESTION_TEXT, 30000)
                    .capture_labeled(
                        "queued_sync_waiting_on_question",
                        "Queued sync pauses while the active turn asks a question",
                    )
                    .press_key("ctrl+c")
                    .wait_for_text("[Sync] Successfully synced", 10000)
                    .wait_for_text("Enter: reply", 5000)
            },
            |frame, report| {
                let question_frame = common::frame_from_capture(&report.captures[0]);
                let question_full = Region::full(question_frame.cols(), question_frame.rows());
                assertion::assert_text_in_region(
                    &question_frame,
                    QUEUED_SYNC_QUESTION_TEXT,
                    &question_full,
                );
                assertion::assert_text_in_region(
                    &question_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &question_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Sync] Successfully synced", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, QUEUED_SYNC_QUESTION_TEXT);
                assertion::assert_not_visible(frame, "≡ sync —");
            },
        )?;

    Ok(())
}

/// Verify a running session remains controllable and completes work queued
/// after switching away from its owning project and back.
#[test]
fn session_queued_action_survives_project_switching() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_action_survives_project_switching")
        .with_git()
        .setup(|env| {
            install_delayed_sync_claude_stub(env, false, 10)?;
            common::seed_second_project(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Keep queued sync visible")
                    .wait_for_text("Keep queued sync visible", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Keep queued sync visible", 5000)
                    .wait_for_text("r: sync", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .capture_labeled(
                        "restored_queued_action",
                        "Running session accepts work after project switching",
                    )
                    .wait_for_text(QUEUED_SYNC_TURN_ANSWER, 30000)
                    .wait_for_text("[Sync] Successfully synced", 10000)
                    .wait_for_text("Enter: reply", 5000)
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &queued_full,
                );
                assertion::assert_text_in_region(&queued_frame, "Ctrl+c: stop", &queued_full);
                assertion::assert_text_in_region(
                    &queued_frame,
                    "Keep queued sync visible",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, QUEUED_SYNC_TURN_ANSWER, &full);
                assertion::assert_text_in_region(frame, "[Sync] Successfully synced", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "≡ sync —");
            },
        )?;

    Ok(())
}

/// Verify a turn that finishes while another project is active starts focused
/// review immediately and displays it after its owning project is restored.
#[test]
fn completed_session_review_survives_project_switching() -> E2eResult {
    // Arrange
    let review_started_marker = Arc::new(Mutex::new(None::<PathBuf>));
    let setup_review_started_marker = Arc::clone(&review_started_marker);

    FeatureTest::new("completed_session_review_survives_project_switching")
        .with_git()
        .setup(move |env| {
            let marker_path = env.stub_bin.join("deferred-project-review-started");
            setup_review_started_marker
                .lock()
                .expect("review marker capture should remain available")
                .replace(marker_path.clone());
            install_deferred_project_review_claude_stub(env, &marker_path)?;
            common::seed_second_project(env)
        })
        .run(
            move |scenario| {
                // Act
                let review_started_marker = review_started_marker
                    .lock()
                    .expect("review marker capture should remain available")
                    .clone()
                    .expect("setup should capture the review marker path");

                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        DEFERRED_PROJECT_REVIEW_PROMPT,
                    ))
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .eventually(
                        Duration::from_secs(30),
                        Duration::from_millis(100),
                        move |frame| {
                            if review_started_marker.is_file() {
                                return Ok(());
                            }

                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(
                                frame,
                                "focused review request started",
                                &full,
                            )
                        },
                    )
                    .capture_labeled(
                        "inactive_project_review_started",
                        "Focused review starts while zeta-project remains active",
                    )
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text(DEFERRED_PROJECT_REVIEW_TEXT, 30000)
                    .capture_labeled(
                        "restored_review",
                        "Focused review after inactive-project completion",
                    )
            },
            |frame, report| {
                // Assert
                let inactive_project_frame = common::frame_from_capture(&report.captures[0]);
                let inactive_project_full =
                    Region::full(inactive_project_frame.cols(), inactive_project_frame.rows());
                assertion::assert_text_in_region(
                    &inactive_project_frame,
                    "Project: zeta-project",
                    &inactive_project_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, DEFERRED_PROJECT_REVIEW_TEXT, &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_not_visible(frame, "Reviewing changes with");
                assertion::assert_not_visible(frame, MISSING_DEFERRED_PROJECT_HISTORY_TEXT);
            },
        )?;

    Ok(())
}

/// Verify stopping an active turn also removes its canceled queued-sync row
/// without promoting the skipped command to active rebase work.
#[test]
fn session_queued_sync_clears_when_turn_is_cancelled() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_sync_cancelled_with_turn")
        .with_git()
        .setup(|env| install_delayed_sync_claude_stub(env, false, 10))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Cancel the turn and queued sync")
                    .wait_for_text("Cancel the turn and queued sync", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .capture_labeled(
                        "queued_sync_before_turn_cancel",
                        "Sync waits behind the active turn before cancellation",
                    )
                    .press_key("ctrl+c")
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(frame, "Enter: reply", &full)?;

                            assertion::match_not_visible(frame, "≡ sync —")
                        },
                    )
                    .capture_labeled(
                        "queued_sync_cleared_after_turn_cancel",
                        "Canceled queued sync disappears without starting rebase",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "≡ sync —");
                assertion::assert_not_visible(frame, "Rebasing...");
                assertion::assert_not_visible(frame, "[Sync] Successfully synced");
                assertion::assert_not_visible(frame, QUEUED_SYNC_TURN_ANSWER);
            },
        )?;

    Ok(())
}

/// Verify queued sync validation failures replace waiting state with a
/// durable error without briefly presenting the sync as active work.
#[test]
fn session_queued_sync_validation_failure_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_sync_validation_failure")
        .with_git()
        .setup(|env| install_delayed_sync_claude_stub(env, true, 10))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Fail queued sync validation")
                    .wait_for_text("Fail queued sync validation", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .press_key("r")
                    .wait_for_text("rebase onto the base branch after this turn", 5000)
                    .capture_labeled(
                        "queued_sync_waiting",
                        "Sync remains queued behind the active turn",
                    )
                    .wait_for_text(QUEUED_SYNC_TURN_ANSWER, 30000)
                    .wait_for_text("[Sync Error] Session isolation violation", 10000)
                    .wait_for_text("Enter: reply", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "queued_sync_validation_failed",
                        "Validation failure replaces queued sync with a durable error",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ sync — rebase onto the base branch after this turn",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, QUEUED_SYNC_TURN_ANSWER, &full);
                assertion::assert_text_in_region(
                    frame,
                    "[Sync Error] Session isolation violation",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "≡ sync —");
                assertion::assert_not_visible(frame, "Rebasing...");
                let answer_row = frame
                    .find_text(QUEUED_SYNC_TURN_ANSWER)
                    .first()
                    .expect("missing completed turn answer")
                    .rect
                    .row;
                let error_row = frame
                    .find_text("[Sync Error] Session isolation violation")
                    .first()
                    .expect("missing queued sync validation error")
                    .rect
                    .row;
                assert!(answer_row < error_row);
            },
        )?;

    Ok(())
}

/// Verify review-request creation queues behind a running turn, begins only
/// after the answer is complete, and records the created forge link.
#[test]
fn review_request_creation_queues_during_running_turn() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_queued_creation")
        .with_git()
        .setup(install_queued_review_request_stubs)
        .zola(
            "Queued review-request creation",
            "Queue review-request creation behind a running session turn and publish it next.",
            41,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Queue the review request")
                    .wait_for_text("Queue the review request", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "review_request_queued",
                        "Review-request creation queued behind the active turn",
                    )
                    .wait_for_text(QUEUED_REVIEW_REQUEST_TURN_ANSWER, 30000)
                    .wait_for_text("Publishing review request...", 5000)
                    .capture_labeled(
                        "review_request_started",
                        "Queued review-request creation starts after the turn completes",
                    )
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "review_request_created",
                        "Created review-request link recorded after the completed turn",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ review request — publish after this turn",
                    &queued_full,
                );
                assertion::assert_text_in_region(
                    &queued_frame,
                    "Queue the review request",
                    &queued_full,
                );
                assertion::assert_text_in_region(&queued_frame, "Ctrl+c: stop", &queued_full);

                let started_frame = common::frame_from_capture(&report.captures[1]);
                let started_full = Region::full(started_frame.cols(), started_frame.rows());
                assertion::assert_text_in_region(
                    &started_frame,
                    QUEUED_REVIEW_REQUEST_TURN_ANSWER,
                    &started_full,
                );
                assertion::assert_text_in_region(
                    &started_frame,
                    "Publishing review request...",
                    &started_full,
                );
                assertion::assert_not_visible(&started_frame, "≡ review request —");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Review Request] Created PR", &full);
                assertion::assert_text_in_region(
                    frame,
                    "https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                assertion::assert_not_visible(frame, "≡ review request —");
            },
        )?;

    Ok(())
}

/// Verify stopping an active turn also removes its canceled queued
/// review-request row without promoting the skipped command to publish work.
#[test]
fn review_request_queued_creation_clears_when_turn_is_cancelled() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_queued_creation_cancelled_with_turn")
        .with_git()
        .setup(install_cancelled_queued_review_request_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Cancel the turn and queued review request")
                    .wait_for_text("Cancel the turn and queued review request", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .capture_labeled(
                        "queued_review_request_before_turn_cancel",
                        "Review-request creation waits behind the active turn",
                    )
                    .press_key("ctrl+c")
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(frame, "Enter: reply", &full)?;

                            assertion::match_not_visible(frame, "≡ review request —")
                        },
                    )
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .capture_labeled(
                        "queued_review_request_cleared_after_turn_cancel",
                        "Canceled review-request row disappears and publish opens again",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "≡ review request — publish after this turn",
                    &queued_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Publish Review Request", &full);
                assertion::assert_not_visible(frame, "≡ review request —");
                assertion::assert_not_visible(frame, "Publishing review request...");
                assertion::assert_not_visible(frame, "[Review Request] Created PR");
                assertion::assert_not_visible(frame, QUEUED_REVIEW_REQUEST_TURN_ANSWER);
            },
        )?;

    Ok(())
}

/// Verify mixed chat and workflow work renders and executes from top to
/// bottom in the order each item was submitted.
#[test]
fn session_queued_work_uses_fifo_display_and_execution_order() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_queued_work_fifo")
        .with_git()
        // Tall enough to hold both completed turns plus the review-request
        // notice, so execution order never depends on transcript scrolling.
        .with_terminal_size(80, 44)
        .setup(install_fifo_queued_review_request_stubs)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Queue mixed work")
                    .wait_for_text("Queue mixed work", 3000)
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .wait_for_text("p: PR", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Review once more")
                    .wait_for_text("Review once more", 3000)
                    .press_key("Enter")
                    .wait_for_text("≡ queued › Review once more", 5000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("≡ review request — publish after this turn", 5000)
                    .capture_labeled(
                        "mixed_queue_submission_order",
                        "Queued chat appears above the later review-request action",
                    )
                    .wait_for_text(QUEUED_REVIEW_REQUEST_TURN_ANSWER, 30000)
                    .wait_for_text(QUEUED_REVIEW_FOLLOW_UP_ANSWER, 10000)
                    .capture_labeled(
                        "mixed_queue_chat_execution_order",
                        "Active chat completes before the queued chat message",
                    )
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "mixed_queue_execution_order",
                        "Queued chat completes before the later review-request action",
                    )
            },
            |frame, report| {
                let queued_frame = common::frame_from_capture(&report.captures[0]);
                let queued_chat_row = queued_frame
                    .find_text("queued › Review once more")
                    .first()
                    .expect("missing queued chat message")
                    .rect
                    .row;
                let queued_publish_row = queued_frame
                    .find_text("review request — publish after this turn")
                    .first()
                    .expect("missing queued review-request action")
                    .rect
                    .row;
                assert!(queued_chat_row < queued_publish_row);

                let chat_execution_frame = common::frame_from_capture(&report.captures[1]);
                let active_answer_row = chat_execution_frame
                    .find_text(QUEUED_REVIEW_REQUEST_TURN_ANSWER)
                    .first()
                    .expect("missing active turn answer")
                    .rect
                    .row;
                let queued_answer_row = chat_execution_frame
                    .find_text(QUEUED_REVIEW_FOLLOW_UP_ANSWER)
                    .first()
                    .expect("missing queued chat answer")
                    .rect
                    .row;
                assert!(active_answer_row < queued_answer_row);

                let queued_answer_row = frame
                    .find_text(QUEUED_REVIEW_FOLLOW_UP_ANSWER)
                    .first()
                    .expect("missing queued chat answer")
                    .rect
                    .row;
                let review_request_row = frame
                    .find_text("[Review Request] Created PR")
                    .first()
                    .expect("missing created review request")
                    .rect
                    .row;
                assert!(queued_answer_row < review_request_row);
            },
        )?;

    Ok(())
}
