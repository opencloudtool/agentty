//! Session creation, navigation, continuation, and cancellation.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::db::{DB_DIR, DB_FILE};
use agentty::domain::agent::ReasoningLevel;
use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Executor};
use testty::assertion;
use testty::region::Region;

use super::fixture::{
    CLAUDE_STRUCTURED_RESPONSE_TEXT, E2eResult, seed_claude_structured_output_project,
    seed_review_ready_session, seed_running_stop_session, seed_session_title_candidate_project,
    seed_sessions_tab,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Seeds one review-ready session with a persisted reasoning level and timer.
fn seed_session_with_reasoning_level(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_reasoning_level("review-shortcut-0001", ReasoningLevel::Medium)
            .await?;
        database
            .sessions()
            .update_session_status_with_timing_at("review-shortcut-0001", "InProgress", 0)
            .await?;
        database
            .sessions()
            .update_session_status_with_timing_at("review-shortcut-0001", "Review", 125)
            .await
    })?;

    Ok(())
}

/// Seeds sessions whose matching update times require creation-time ordering.
fn seed_sessions_with_matching_update_times(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("a-older", "gpt-5.6-sol", "main", "Review")
            .with_title("Older created session"),
    )?;
    common::seed_session(
        env,
        SessionSeed::regular("z-newer", "gpt-5.6-sol", "main", "Review")
            .with_title("Newer created session"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        let query = sqlx::query!(
            r"
UPDATE session
SET created_at = CASE id WHEN 'a-older' THEN 100 ELSE 200 END,
    updated_at = 300
WHERE id IN ('a-older', 'z-newer')
"
        );
        connection.execute(query).await?;
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    for session_id in ["a-older", "z-newer"] {
        std::fs::create_dir_all(test_support::session_folder(
            &env.agentty_root.join("wt"),
            session_id,
        ))?;
    }

    Ok(())
}

/// Seeds one done session whose merged commit hash can drive the continuation
/// draft flow.
fn seed_done_session_for_continuation(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    common::seed_session(
        env,
        SessionSeed::regular("done-continue-0001", "gpt-5.6-sol", "main", "Done")
            .with_title("Continue terminal session"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/done-continue".to_string(),
                state: ReviewRequestState::Merged,
                status_summary: Some("Merged".to_string()),
                target_branch: "main".to_string(),
                title: "Completed review request".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        };
        database
            .sessions()
            .update_session_merged_commit_hash(
                "done-continue-0001",
                Some(merged_commit_hash.to_string()),
            )
            .await?;
        database
            .reviews()
            .update_session_review_request("done-continue-0001", Some(review_request))
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds one canceled session whose transcript can drive the continuation
/// draft flow.
fn seed_canceled_session_for_continuation(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("canceled-continue-0001", "gpt-5.6-sol", "main", "Canceled")
            .with_title("Continue canceled session"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "canceled-continue-0001",
                SessionMessageKind::UserPrompt,
                "Resume the remaining work.",
            )
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Verify that the Sessions tab hides empty groups and guides session creation.
///
/// Starts Agentty with no sessions, then creates an active session and verifies
/// that only the populated group is visible.
#[test]
fn session_list_empty_state() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_empty")
        .with_git()
        .zola(
            "Empty session state",
            "See how Agentty guides session creation and hides empty groups.",
            40,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("sessions_tab", "Sessions tab with no sessions")
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "Keep only populated groups",
                    ))
                    .viewing_pause_ms(2000)
                    .capture_labeled("active_group", "Only the populated group is visible")
            },
            |frame, report| {
                let empty_frame = common::frame_from_capture(&report.captures[0]);
                let empty_full = Region::full(empty_frame.cols(), empty_frame.rows());
                assertion::assert_text_in_region(
                    &empty_frame,
                    "No sessions. Press 'a' to start one.",
                    &empty_full,
                );
                assertion::assert_not_visible(&empty_frame, "MERGE QUEUE");
                assertion::assert_not_visible(&empty_frame, "ACTIVE");
                assertion::assert_not_visible(&empty_frame, "ARCHIVE");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "ACTIVE —— 1", &full);
                assertion::assert_not_visible(frame, "MERGE QUEUE");
                assertion::assert_not_visible(frame, "ARCHIVE");
                assertion::assert_not_visible(frame, "No sessions");
            },
        )?;

    Ok(())
}

/// Verify that a later narrow follow-up is titled from stable session context
/// instead of becoming the whole visible session title.
#[test]
fn test_session_title_uses_stable_context() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_title_uses_stable_context")
        .with_git()
        .setup(seed_session_title_candidate_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Improve session title generation.")
                    .wait_for_text("Improve session title generation.", 3000)
                    .press_key("Enter")
                    .wait_for_text(
                        "The session title workflow is ready for a focused follow-up.",
                        30000,
                    )
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Also reject punctuation-only copies.")
                    .wait_for_text("Also reject punctuation-only copies.", 3000)
                    .press_key("Enter")
                    .wait_for_text("Follow-up complete. No files were changed.", 30000)
                    .wait_for_text("Stabilize session title generation", 30000)
                    .capture_labeled(
                        "stable_context_title",
                        "The session title preserves the durable overall goal",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Stabilize session title generation",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Follow-up complete. No files were changed.",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that the Sessions tab separates timer units and shows reasoning.
#[test]
fn session_list_model_reasoning_level() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_list_model_reasoning_level")
        .with_git()
        .setup(seed_session_with_reasoning_level)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("gpt-5.6-sol [medium]", 5000)
                    .wait_for_text("2m 5s", 5000)
                    .capture_labeled(
                        "model_reasoning",
                        "Session row with readable model and timer details",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.6-sol [medium]", &full);
                assertion::assert_text_in_region(frame, "2m 5s", &full);
            },
        )?;

    Ok(())
}

/// Verify matching update times fall back to newest creation time first.
#[test]
fn session_list_matching_update_times_use_creation_order() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_list_creation_order")
        .with_git()
        .setup(seed_sessions_with_matching_update_times)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Newer created session", 5000)
                    .capture_labeled(
                        "creation_order",
                        "Matching update times ordered by creation time",
                    )
            },
            |frame, _report| {
                let newer_row = frame
                    .find_text("Newer created session")
                    .first()
                    .expect("missing newer session row")
                    .rect
                    .row;
                let older_row = frame
                    .find_text("Older created session")
                    .first()
                    .expect("missing older session row")
                    .rect
                    .row;

                assert!(newer_row < older_row);
            },
        )?;

    Ok(())
}

/// Verify that changing the project reasoning default does not relabel a turn
/// that is already in progress.
#[test]
fn existing_session_keeps_persisted_reasoning_label() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_persisted_reasoning_label")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Running session stop", 5000)
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("j")
                    .press_key("j")
                    .press_key("j")
                    .press_key("Enter")
                    .press_key("Enter")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("[xhigh]", 5000)
                    .press_key("BackTab")
                    .wait_for_text("gpt-5.6-sol [high]", 5000)
                    .capture_labeled(
                        "active_reasoning",
                        "Existing session retains persisted reasoning",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "gpt-5.6-sol [high]", &full);
            },
        )?;

    Ok(())
}

/// Verify that the session chat header shows the selected agent immediately
/// before the model name.
#[test]
fn session_chat_header_agent_model() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_chat_header_agent_model")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Agent: codex  Model: gpt-5.6-sol", 5000)
                    .capture_labeled(
                        "agent_model_header",
                        "Session chat header showing agent before model",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agent: codex  Model: gpt-5.6-sol", &full);
            },
        )?;

    Ok(())
}

/// Verify that a selected session row remains readable under Dark Horizon.
///
/// The source renderer test covers the exact selected-cell background color;
/// this PTY-level test keeps the user-visible selection path covered without
/// depending on terminal background-color capture fidelity. The scenario
/// finishes by wrapping the theme back to `Agentty Default` so the persisted
/// theme is identical before the PTY proof run and the VHS replay.
#[test]
fn session_list_selected_row_remains_readable_under_dark_horizon() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_selection_highlight")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Agentty Green", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Dark Horizon", 5000)
                    .compose(&common::switch_to_tab("Projects"))
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .wait_for_text("Enter: open", 3000)
                    .capture_labeled(
                        "selected_row_highlight",
                        "Selected session row on the dedicated selection surface",
                    )
                    .compose(&common::switch_to_tab("Settings"))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Agentty Default", 5000)
            },
            |_frame, report| {
                assert_eq!(
                    report.captures.len(),
                    1,
                    "Expected 1 capture (selected session row under Dark Horizon)"
                );

                // Dark Horizon `surface_selection` and `surface` RGB values
                // from the `ThemePalette` definitions in `ui/style.rs`.
                let sessions_frame = common::frame_from_capture(&report.captures[0]);
                let selected_title = sessions_frame
                    .find_text("Review-ready session shortcuts")
                    .into_iter()
                    .next()
                    .expect("expected selected session title to be visible");
                let selected_row_region =
                    Region::new(0, selected_title.rect.row, sessions_frame.cols(), 1);
                sessions_frame
                    .find_text_in_region("gpt-5.6-sol", &selected_row_region)
                    .into_iter()
                    .next()
                    .expect("expected selected session model to be visible");
            },
        )?;

    Ok(())
}

/// Verify that `Ctrl+c` in a running session stops only the current turn and
/// returns the session to review-ready controls.
#[test]
fn session_stop_turn_returns_to_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_stop_turn_returns_to_review")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "running_session",
                        "Running session before stopping the turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_text("Enter: reply", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "review_after_stop",
                        "Session view after Ctrl+c stops only the active turn",
                    )
            },
            |frame, report| {
                let running_frame = common::frame_from_capture(&report.captures[0]);
                let running_full = Region::full(running_frame.cols(), running_frame.rows());
                assertion::assert_text_in_region(&running_frame, "Ctrl+c: stop", &running_full);
                assertion::assert_not_visible(&running_frame, "o: open");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "Ctrl+c: stop");
            },
        )?;

    Ok(())
}

/// Delays the checkout hook so navigation must work before creation finishes.
fn seed_delayed_session_creation(env: &BuilderEnv) -> E2eResult {
    seed_sessions_tab(env)?;
    let hook = env.workdir.join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nsleep 8\n")?;
    #[cfg(unix)]
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Navigation and painting remain available during slow worktree setup.
#[test]
fn session_creation_keeps_navigation_responsive() -> E2eResult {
    // Arrange
    FeatureTest::new("session_creation_responsive")
        .with_git()
        .setup(seed_delayed_session_creation)
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 1000)
                    .write_text("Typing while workspace prepares")
                    .capture_labeled("creating", "Composer accepts input during setup")
                    .press_key("Tab")
                    .wait_for_text("q: sessions", 2000)
                    .press_key("q")
                    .press_key("?")
                    .wait_for_text("Keybindings", 2000)
                    .capture_labeled("navigation", "Help opens before worktree setup finishes")
                    .sleep_ms(9000)
                    .capture_labeled("completed", "Creation completion preserves help")
            },
            |frame, report| {
                // Assert
                let pending = common::frame_from_capture(&report.captures[0]);
                let pending_full = Region::full(pending.cols(), pending.rows());
                assertion::assert_text_in_region(
                    &pending,
                    "Typing while workspace prepares",
                    &pending_full,
                );
                assertion::assert_not_visible(&pending, "Creating session");
                assertion::assert_not_visible(&pending, "Preparing workspace");
                assertion::assert_not_visible(&pending, "Regular");
                let navigation = common::frame_from_capture(&report.captures[1]);
                let navigation_full = Region::full(navigation.cols(), navigation.rows());
                assertion::assert_text_in_region(&navigation, "Keybindings", &navigation_full);
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Keybindings", &full);
            },
        )?;

    Ok(())
}

/// Accepts a first prompt before checkout finishes, then dispatches it once.
#[test]
fn session_creation_runs_early_prompt_once() -> E2eResult {
    // Arrange
    FeatureTest::new("session_creation_early_prompt")
        .with_git()
        .setup(|env| {
            seed_delayed_session_creation(env)?;
            seed_claude_structured_output_project(env)
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: send", 2000)
                    .write_text("Run this early prompt exactly once")
                    .press_key("Enter")
                    .wait_for_text("Saved prompt:", 2000)
                    .capture_labeled("queued", "Prompt waits for workspace setup")
                    .press_key("Enter")
                    .press_key("Enter")
                    .wait_for_text(CLAUDE_STRUCTURED_RESPONSE_TEXT, 30000)
                    .wait_for_stable_frame(300, 3000)
                    .capture_labeled("started", "Saved prompt runs after workspace setup")
            },
            |frame, report| {
                // Assert
                let queued = common::frame_from_capture(&report.captures[0]);
                assertion::assert_not_visible(&queued, CLAUDE_STRUCTURED_RESPONSE_TEXT);
                assertion::assert_match_count(frame, "Run this early prompt exactly once", 1);
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, CLAUDE_STRUCTURED_RESPONSE_TEXT, &full);
                assertion::assert_not_visible(frame, "Saved prompt:");
            },
        )?;

    Ok(())
}

/// Restart before gate release preserves a first prompt for explicit retry.
#[test]
fn session_creation_recovers_unreleased_first_prompt() -> E2eResult {
    // Arrange
    FeatureTest::new("session_creation_restart_before_start")
        .with_git()
        .setup(|env| {
            seed_sessions_tab(env)?;
            seed_claude_structured_output_project(env)?;
            common::seed_session(
                env,
                SessionSeed::regular(
                    "unreleased-first",
                    "claude-haiku-4-5-20251001",
                    "main",
                    "InProgress",
                )
                .with_title("Recover the pending first prompt"),
            )?;
            common::seed_runtime()?.block_on(async {
                let db = common::open_database(env).await?;
                let prompt = agentty::domain::turn_prompt::TurnPrompt::from_text(
                    "Recover the pending first prompt".to_string(),
                );
                db.sessions()
                    .insert_session_preparation("unreleased-first", "main")
                    .await?;
                db.sessions()
                    .save_preparation_prompt("unreleased-first", &serde_json::to_string(&prompt)?)
                    .await?;
                db.sessions()
                    .update_session_preparation(
                        "unreleased-first",
                        ag_store::SessionPreparationState::Ready,
                        None,
                    )
                    .await?;
                db.sessions()
                    .update_session_prompt("unreleased-first", &prompt.text)
                    .await?;
                // Older handoffs could publish the first prompt before
                // releasing the gate.
                db.sessions()
                    .append_session_message(
                        "unreleased-first",
                        SessionMessageKind::UserPrompt,
                        &prompt.text,
                    )
                    .await?;
                db.operations()
                    .insert_session_operation(
                        "workspace:unreleased-first",
                        "unreleased-first",
                        "start_prompt",
                    )
                    .await?;
                Ok::<(), Box<dyn std::error::Error>>(())
            })
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("Enter")
                    .wait_for_text("Press s to retry.", 5000)
                    .capture_labeled("recovered", "Unstarted prompt survives restart")
                    .press_key("s")
                    .wait_for_text(CLAUDE_STRUCTURED_RESPONSE_TEXT, 30000)
                    .wait_for_stable_frame(300, 3000)
            },
            |frame, report| {
                // Assert
                let recovered = common::frame_from_capture(&report.captures[0]);
                assertion::assert_not_visible(&recovered, CLAUDE_STRUCTURED_RESPONSE_TEXT);
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, CLAUDE_STRUCTURED_RESPONSE_TEXT, &full);
                assertion::assert_not_visible(frame, "Saved prompt:");
            },
        )?;

    Ok(())
}

/// A rejected first-turn handoff keeps its saved prompt retryable with `s`.
#[test]
fn session_creation_retries_rejected_handoff() -> E2eResult {
    // Arrange
    FeatureTest::new("session_creation_retry_handoff")
        .with_git()
        .setup(|env| {
            seed_delayed_session_creation(env)?;
            seed_claude_structured_output_project(env)?;
            common::seed_runtime()?.block_on(async {
                let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
                let mut connection = SqliteConnectOptions::new()
                    .filename(db_path)
                    .connect()
                    .await?;
                connection
                    .execute(
                        "CREATE TABLE reject_first_handoff (pending INTEGER); INSERT INTO \
                         reject_first_handoff VALUES (1); CREATE TRIGGER reject_start BEFORE \
                         INSERT ON session_operation WHEN EXISTS (SELECT 1 FROM \
                         reject_first_handoff) BEGIN SELECT RAISE(ABORT, 'handoff rejected'); \
                         END; CREATE TRIGGER allow_retry AFTER UPDATE ON session_preparation WHEN \
                         NEW.state = 'failed' BEGIN DELETE FROM reject_first_handoff; END;",
                    )
                    .await?;
                connection.close().await
            })?;

            Ok(())
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: send", 2000)
                    .write_text("Retry this saved first prompt")
                    .press_key("Enter")
                    .wait_for_text("Saved prompt:", 2000)
                    .wait_for_text("Press s to retry.", 30000)
                    .capture_labeled(
                        "rejected",
                        "Saved prompt remains retryable after handoff failure",
                    )
                    .press_key("s")
                    .wait_for_text(CLAUDE_STRUCTURED_RESPONSE_TEXT, 30000)
                    .wait_for_stable_frame(300, 3000)
                    .capture_labeled("retried", "Retry submits the saved prompt once")
            },
            |frame, report| {
                // Assert
                let rejected = common::frame_from_capture(&report.captures[0]);
                let rejected_full = Region::full(rejected.cols(), rejected.rows());
                assertion::assert_text_in_region(&rejected, "Saved prompt:", &rejected_full);
                assertion::assert_not_visible(&rejected, CLAUDE_STRUCTURED_RESPONSE_TEXT);
                assertion::assert_match_count(frame, "Retry this saved first prompt", 1);
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, CLAUDE_STRUCTURED_RESPONSE_TEXT, &full);
                assertion::assert_not_visible(frame, "Saved prompt:");
            },
        )?;

    Ok(())
}

/// Verify that pressing `a` on the Sessions tab opens the creation selector,
/// and choosing the regular option opens prompt mode with the submit footer.
#[test]
fn session_creation_opens_prompt_mode() -> E2eResult {
    // Arrange
    FeatureTest::new("session_creation")
        .with_git()
        .setup(seed_sessions_tab)
        .zola(
            "Session creation",
            "Create a session or append a review-ready session to an existing stack.",
            30,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("creation_selector", "Session creation selector")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("prompt_mode", "Prompt mode after choosing Regular")
            },
            |frame, report| {
                // Assert
                let selector_frame = common::frame_from_capture(&report.captures[0]);
                let selector_full = Region::full(selector_frame.cols(), selector_frame.rows());
                assertion::assert_text_in_region(&selector_frame, "Regular", &selector_full);
                assertion::assert_text_in_region(&selector_frame, "Draft", &selector_full);
                assertion::assert_text_in_region(&selector_frame, "Orchestrator", &selector_full);
                assertion::assert_text_in_region(
                    &selector_frame,
                    "[Preview] Plan workers",
                    &selector_full,
                );
                assertion::assert_text_in_region(&selector_frame, "Stacked", &selector_full);
                assertion::assert_text_in_region(
                    &selector_frame,
                    "Select parent first",
                    &selector_full,
                );
                assertion::assert_text_in_region(
                    &selector_frame,
                    "Append to stack",
                    &selector_full,
                );
                assertion::assert_text_in_region(
                    &selector_frame,
                    "[Preview] Review only",
                    &selector_full,
                );
                assertion::assert_text_in_region(&selector_frame, "Enter: select", &selector_full);
                assertion::assert_text_in_region(&selector_frame, "q: close", &selector_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Tab: focus | Enter: send", &full);
            },
        )?;

    Ok(())
}

/// Verify that choosing Draft in the creation selector opens draft-session
/// staging with explicit draft guidance before any message is staged.
#[test]
fn draft_session_creation_opens_staging_mode() -> E2eResult {
    // Arrange
    FeatureTest::new("draft_session_creation")
        .with_git()
        .zola(
            "Draft session creation",
            "Create a draft session that clearly starts in local staging mode before the bundle \
             runs.",
            31,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_session_prompt_mode",
                        "Draft-session prompt mode immediately after creation",
                    )
            },
            |frame, _report| {
                // Assert
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Draft Session", &full);
                assertion::assert_text_in_region(frame, "No draft messages staged yet.", &full);
                assertion::assert_text_in_region(frame, "Tab: focus | Enter: stage draft", &full);
                assertion::assert_text_in_region(frame, "Ctrl+V/Alt+V", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `Enter` on a session opens the session view and
/// pressing `q` returns to the session list.
#[test]
fn session_open_and_return_to_list() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_open")
        .with_git()
        .zola(
            "Session open and return",
            "Open a session with Enter and return to the list with q.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_and_return_to_list())
                    .viewing_pause_ms(1500)
                    .compose(&common::open_selected_session_view())
                    .viewing_pause_ms(2000)
                    .capture_labeled("session_view", "Session view after Enter")
                    .compose(&common::return_to_session_list())
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_list", "Sessions list after q")
            },
            |frame, report| {
                let session_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
                assertion::assert_text_in_region(&session_view_frame, "q: back", &view_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `c` in a terminal session opens a confirmation and,
/// after acceptance, stages the continuation message before focusing an empty
/// draft composer.
#[test]
fn terminal_session_continue_opens_seeded_prompt() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("terminal_session_continue")
        .with_git()
        .setup(seed_done_session_for_continuation)
        .zola(
            "Continue terminal session",
            "Confirm continuation from a done session and stage a merged-commit context message \
             before focusing an empty draft composer.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("c: continue", 5000)
                    .wait_for_text("Press 'c' to continue in a new session.", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "done_session_actions",
                        "Linked done session offers continuation without review comments",
                    )
                    .press_key("c")
                    .wait_for_text("Confirm Continue", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "continue_confirmation",
                        "Continuation confirmation for the selected done session",
                    )
                    .press_key("y")
                    .wait_for_stable_frame(500, 15000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "terminal_session_continue",
                        "Continuation draft composer with the staged merged-commit context",
                    )
            },
            |frame, report| {
                let done_session_frame = common::frame_from_capture(&report.captures[0]);
                let done_session_full =
                    Region::full(done_session_frame.cols(), done_session_frame.rows());
                assertion::assert_text_in_region(
                    &done_session_frame,
                    "c: continue",
                    &done_session_full,
                );
                assertion::assert_not_visible(&done_session_frame, "comments");

                let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                let confirmation_full =
                    Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Confirm Continue",
                    &confirmation_full,
                );
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Create a new draft sess...",
                    &confirmation_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
                assertion::assert_text_in_region(frame, "Use 704de31d0f4b5a12", &full);
                assertion::assert_text_in_region(frame, "704de31d0f4b5a12", &full);
                assertion::assert_text_in_region(frame, "commit as an initial context", &full);
                assertion::assert_text_in_region(frame, "Type your message", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `c` in a canceled session opens a confirmation and,
/// after acceptance, stages its saved context in a new draft composer.
#[test]
fn canceled_session_continue_opens_seeded_prompt() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("canceled_session_continue")
        .with_git()
        .setup(seed_canceled_session_for_continuation)
        .zola(
            "Continue canceled session",
            "Continue a canceled session in a new draft with its saved transcript staged as \
             context.",
            46,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("c: continue", 5000)
                    .press_key("c")
                    .wait_for_text("Confirm Continue", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "continue_confirmation",
                        "Continuation confirmation for the canceled session",
                    )
                    .press_key("y")
                    .wait_for_text("Resume the remaining work.", 15000)
                    .wait_for_stable_frame(500, 15000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "canceled_session_continue",
                        "New draft composer with canceled-session context staged",
                    )
            },
            |frame, report| {
                let confirmation_frame = common::frame_from_capture(&report.captures[0]);
                let confirmation_full =
                    Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Confirm Continue",
                    &confirmation_full,
                );
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Create a new draft sess...",
                    &confirmation_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
                assertion::assert_text_in_region(frame, "Status: Canceled", &full);
                assertion::assert_text_in_region(frame, "Previous session transcript:", &full);
                assertion::assert_text_in_region(frame, "Resume the remaining work.", &full);
                assertion::assert_text_in_region(frame, "Type your message", &full);
            },
        )?;

    Ok(())
}

/// Verify that `j` and `k` navigate the session list and that `Enter`
/// opens the currently selected session.
///
/// Creates two sessions ("alpha" and "beta"), navigates down with `j`,
/// opens the selection with `Enter`, returns with `q`, navigates back
/// up with `k`, and opens again. Asserts that the list shows display-only
/// size prefixes and that both navigations still land on openable session
/// views after moving the cursor in the list.
#[test]
fn session_list_jk_navigation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_navigation")
        .with_git()
        .zola(
            "Session list navigation",
            "Navigate sessions with j/k keys to select and open different entries.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "alpha",
                    ))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "beta",
                    ))
                    .viewing_pause_ms(2000)
                    .capture_labeled("two_sessions", "Two sessions in list")
                    // Navigate down with j, open the selection, and capture.
                    .press_key("j")
                    .wait_for_stable_frame(300, 3000)
                    .compose(&common::open_selected_session_view())
                    .viewing_pause_ms(2000)
                    .capture_labeled("opened_after_j", "Session opened after pressing j")
                    .compose(&common::return_to_session_list())
                    // Navigate back up with k, open the selection, and capture.
                    .press_key("k")
                    .wait_for_stable_frame(300, 3000)
                    .compose(&common::open_selected_session_view())
                    .viewing_pause_ms(2000)
                    .capture_labeled("opened_after_k", "Session opened after pressing k")
            },
            |_frame, report| {
                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (list, opened_after_j, opened_after_k)"
                );

                // Both sessions visible in the initial list.
                let initial_frame = common::frame_from_capture(&report.captures[0]);
                let initial_full = Region::full(initial_frame.cols(), initial_frame.rows());
                let initial_text = initial_frame.text_in_region(&initial_full);
                assert!(
                    initial_text.contains("alpha") && initial_text.contains("beta"),
                    "Expected both session prompts visible in list"
                );
                assert!(
                    initial_text.contains("[XS]"),
                    "Expected session-size prefix visible in list"
                );

                // Extract text from the two opened-session captures.
                let session_after_down_navigation_frame =
                    common::frame_from_capture(&report.captures[1]);
                let session_after_up_navigation_frame =
                    common::frame_from_capture(&report.captures[2]);

                let down_navigation_full = Region::full(
                    session_after_down_navigation_frame.cols(),
                    session_after_down_navigation_frame.rows(),
                );
                let up_navigation_full = Region::full(
                    session_after_up_navigation_frame.cols(),
                    session_after_up_navigation_frame.rows(),
                );

                let down_navigation_text =
                    session_after_down_navigation_frame.text_in_region(&down_navigation_full);
                let up_navigation_text =
                    session_after_up_navigation_frame.text_in_region(&up_navigation_full);

                // Each opened view must contain one of the session prompts.
                assert!(
                    down_navigation_text.contains("alpha") || down_navigation_text.contains("beta"),
                    "Session opened after j must contain alpha or beta"
                );
                assert!(
                    up_navigation_text.contains("alpha") || up_navigation_text.contains("beta"),
                    "Session opened after k must contain alpha or beta"
                );
            },
        )?;

    Ok(())
}
