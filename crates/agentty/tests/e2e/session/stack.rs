//! Session stacking, restacking, and forks.

use std::path::Path;

use agentty::test_support;
use testty::assertion;
use testty::region::Region;

use super::fixture::{
    E2eResult, run_git, run_git_stdout, seed_review_ready_session, seed_review_worktree_with_diff,
    seed_running_stop_session,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Parent count that exceeds the append selector's terminal viewport.
const APPEND_OVERFLOW_PARENT_COUNT: u8 = 36;

/// Seeds a stack where both parent and child are review-ready.
fn seed_review_ready_parent_with_review_child(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("stack-parent-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Parent stack review"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            "stack-child-0001",
            "gpt-5.6-sol",
            "wt/stack-pa",
            "Review",
            "stack-parent-0001",
        )
        .with_title("Child stack review"),
    )?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-pa"))?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("stack-ch"))?;

    Ok(())
}

/// Seeds four review-ready stack levels for nested creation coverage.
fn seed_four_level_review_stack(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("stackl00-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Stack root"),
    )?;
    for level in 1..=4 {
        let session_id = format!("stackl0{level}-0001");
        let parent_level = level - 1;
        let parent_session_id = format!("stackl0{parent_level}-0001");
        let parent_branch = format!("wt/stackl0{parent_level}");
        let title = format!("Stack level {level}");
        common::seed_session(
            env,
            SessionSeed::stacked_draft(
                &session_id,
                "gpt-5.6-sol",
                &parent_branch,
                "Review",
                &parent_session_id,
            )
            .with_title(&title),
        )?;
    }

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        for level in 0..=4 {
            let session_id = format!("stackl0{level}-0001");
            let updated_at = i64::from(5 - level);
            database
                .sessions()
                .update_session_updated_at(&session_id, updated_at)
                .await?;
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    for level in 0..=4 {
        std::fs::create_dir_all(env.agentty_root.join("wt").join(format!("stackl0{level}")))?;
    }

    Ok(())
}

/// Seeds two independent review branches that can be combined into one stack.
fn seed_appendable_review_sessions(env: &BuilderEnv) -> E2eResult {
    let parent_session_id = "append-p-0001";
    let child_session_id = "append-c-0001";
    let parent_worktree = env.agentty_root.join("wt").join("append-p");
    let child_worktree = env.agentty_root.join("wt").join("append-c");
    std::fs::create_dir_all(env.agentty_root.join("wt"))?;
    let parent_worktree_path = parent_worktree.to_string_lossy().into_owned();
    let child_worktree_path = child_worktree.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/append-p",
            parent_worktree_path.as_str(),
            "main",
        ],
    )?;
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/append-c",
            child_worktree_path.as_str(),
            "main",
        ],
    )?;
    std::fs::write(parent_worktree.join("parent.txt"), "parent change\n")?;
    run_git(&parent_worktree, &["add", "."])?;
    run_git(&parent_worktree, &["commit", "-m", "parent change"])?;
    std::fs::write(child_worktree.join("child.txt"), "child change\n")?;
    run_git(&child_worktree, &["add", "."])?;
    run_git(&child_worktree, &["commit", "-m", "child change"])?;

    common::seed_session(
        env,
        SessionSeed::regular(parent_session_id, "gpt-5.6-sol", "main", "Review")
            .with_title("Append parent session"),
    )?;
    common::seed_session(
        env,
        SessionSeed::regular(child_session_id, "gpt-5.6-sol", "main", "Review")
            .with_title("Append child session"),
    )?;
    for parent_index in 0..APPEND_OVERFLOW_PARENT_COUNT {
        let overflow_parent_id = format!("append-overflow-{parent_index:02}");
        let overflow_parent_title = format!("Overflow parent {parent_index:02}");
        std::fs::create_dir_all(test_support::session_folder(
            &env.agentty_root.join("wt"),
            overflow_parent_id.as_str(),
        ))?;
        common::seed_session(
            env,
            SessionSeed::regular(overflow_parent_id.as_str(), "gpt-5.6-sol", "main", "Review")
                .with_title(overflow_parent_title.as_str()),
        )?;
    }
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_updated_at(parent_session_id, 10)
            .await?;
        database
            .sessions()
            .update_session_updated_at(child_session_id, 1_000)
            .await?;
        for parent_index in 0..APPEND_OVERFLOW_PARENT_COUNT {
            database
                .sessions()
                .update_session_updated_at(
                    format!("append-overflow-{parent_index:02}").as_str(),
                    100 + i64::from(parent_index),
                )
                .await?;
        }
        test_support::persist_active_tab_for_test(&database, agentty::app::Tab::Sessions).await?;

        Ok::<(), agentty::db::DbError>(())
    })?;

    Ok(())
}

/// Seeds a parentless child session that still has pending post-merge stack
/// restack metadata and a real git branch requiring `git rebase --onto`.
fn seed_pending_post_merge_restack_child(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let child_worktree = env.agentty_root.join("wt").join("stack-re");
    let parent_tip = seed_child_worktree_for_onto_rebase(&env.workdir, &child_worktree)?;
    common::seed_session(
        env,
        SessionSeed::regular("stack-restack-child-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Pending post-merge child sync"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_stack_base_commit_hash("stack-restack-child-0001", Some(parent_tip))
            .await
    })?;

    Ok(())
}

/// Seeds a pending post-merge restack with an invalid old parent tip so the
/// automatic startup sync reports its failure in the child session view.
fn seed_failing_pending_post_merge_restack_child(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let child_worktree = env.agentty_root.join("wt").join("stack-re");
    let _parent_tip = seed_child_worktree_for_onto_rebase(&env.workdir, &child_worktree)?;
    common::seed_session(
        env,
        SessionSeed::regular(
            "stack-restack-failure-0001",
            "gpt-5.6-sol",
            "main",
            "Review",
        )
        .with_title("Blocked post-merge child sync"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_stack_base_commit_hash(
                "stack-restack-failure-0001",
                Some("missing-parent-tip".to_string()),
            )
            .await
    })?;

    Ok(())
}

/// Creates a child branch with one parent commit and one child commit so the
/// app can recover it using `git rebase --onto main <parent-tip>`.
fn seed_child_worktree_for_onto_rebase(
    main_worktree: &Path,
    child_worktree: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(parent) = child_worktree.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(main_worktree.join("base.txt"), "base\n")?;
    run_git(main_worktree, &["add", "."])?;
    run_git(main_worktree, &["commit", "-m", "base"])?;
    run_git(main_worktree, &["checkout", "-b", "parent"])?;
    std::fs::write(main_worktree.join("parent.txt"), "parent\n")?;
    run_git(main_worktree, &["add", "."])?;
    run_git(main_worktree, &["commit", "-m", "parent change"])?;
    let parent_tip = run_git_stdout(main_worktree, &["rev-parse", "HEAD"])?;
    run_git(main_worktree, &["checkout", "main"])?;
    std::fs::write(main_worktree.join("merged-parent.txt"), "merged parent\n")?;
    run_git(main_worktree, &["add", "."])?;
    run_git(main_worktree, &["commit", "-m", "merged parent"])?;
    let child_worktree_path = child_worktree.to_string_lossy().into_owned();
    run_git(
        main_worktree,
        &[
            "worktree",
            "add",
            "-b",
            "wt/stack-re",
            child_worktree_path.as_str(),
            "parent",
        ],
    )?;
    std::fs::write(child_worktree.join("child.txt"), "child\n")?;
    run_git(child_worktree, &["add", "."])?;
    run_git(child_worktree, &["commit", "-m", "child change"])?;

    Ok(parent_tip)
}

/// Seeds a review-ready fork source whose persisted and actual worktree diff
/// must not be inherited by the forked branch-tip worktree.
fn seed_dirty_fork_source_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)
}

/// Verify that choosing Stacked creates a fifth-level draft under the selected
/// parent without a preview marker and renders the nested tree.
#[test]
fn stacked_session_creation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_session_creation")
        .with_git()
        .setup(seed_four_level_review_stack)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Stack level 4", 5000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .viewing_pause_ms(1200)
                    .press_key("a")
                    .wait_for_text("Stacked", 5000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .wait_for_text("Stack on selected", 5000)
                    .capture_labeled("stacked_selector", "Stacked creation selector")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .capture_labeled("stacked_draft_view", "Stacked draft action footer")
                    .write_text("Stack level 5")
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .capture_labeled(
                        "stacked_draft_ready",
                        "Stacked draft staged with start action available",
                    )
                    .viewing_pause_ms(1200)
                    .press_key("q")
                    .wait_for_text("ACTIVE", 5000)
                    .capture_labeled("stacked_list", "Stacked draft connected in session list")
            },
            |frame, report| {
                let selector_frame = common::frame_from_capture(&report.captures[0]);
                let selector_full = Region::full(selector_frame.cols(), selector_frame.rows());
                assertion::assert_text_in_region(&selector_frame, "Stacked", &selector_full);
                assertion::assert_text_in_region(
                    &selector_frame,
                    "Stack on selected",
                    &selector_full,
                );
                assertion::assert_not_visible(&selector_frame, "[Preview] Stack on selected");

                let draft_view_frame = common::frame_from_capture(&report.captures[1]);
                assertion::assert_not_visible(&draft_view_frame, "s: start");
                assertion::assert_not_visible(&draft_view_frame, "m: add to merge queue");
                assertion::assert_not_visible(&draft_view_frame, "r: sync");

                let ready_frame = common::frame_from_capture(&report.captures[2]);
                let ready_full = Region::full(ready_frame.cols(), ready_frame.rows());
                assertion::assert_text_in_region(&ready_frame, "s: start", &ready_full);
                assertion::assert_not_visible(&ready_frame, "m: add to merge queue");
                assertion::assert_not_visible(&ready_frame, "r: sync");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Stack root", &full);
                assertion::assert_text_in_region(frame, "        └ [XS]", &full);
            },
        )?;

    Ok(())
}

/// Verify that an independent review-ready session can be moved beneath a
/// selected parent from the session creation overlay.
#[test]
fn append_session_to_stack() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("append_session_to_stack")
        .with_git()
        .setup(seed_appendable_review_sessions)
        .zola(
            "Append a session to a stack",
            "Move a review-ready session beneath another session and sync its branch.",
            41,
        )
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Append child session", 5000)
                    .press_key("a")
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .wait_for_text("[Preview] Move under parent", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "append_action",
                        "Append to stack action for a review-ready session",
                    )
                    .press_key("Enter")
                    .wait_for_text("Choose parent session", 5000)
                    .capture_labeled("parent_selector", "Eligible destination parent sessions")
                    .viewing_pause_ms(1500);
                let scenario = (1..APPEND_OVERFLOW_PARENT_COUNT).fold(scenario, |scenario, _| {
                    scenario.press_key("Down").sleep_ms(30)
                });

                scenario
                    .wait_for_text("Choose parent session", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "scrolled_parent_selector",
                        "Selected destination stays visible after scrolling",
                    )
            },
            |frame, report| {
                let action_frame = common::frame_from_capture(&report.captures[0]);
                let action_full = Region::full(action_frame.cols(), action_frame.rows());
                assertion::assert_text_in_region(&action_frame, "Append to stack", &action_full);
                assertion::assert_text_in_region(
                    &action_frame,
                    "[Preview] Move under parent",
                    &action_full,
                );

                let selector_frame = common::frame_from_capture(&report.captures[1]);
                let selector_full = Region::full(selector_frame.cols(), selector_frame.rows());
                assertion::assert_text_in_region(
                    &selector_frame,
                    "Choose parent session",
                    &selector_full,
                );
                assertion::assert_text_in_region(
                    &selector_frame,
                    "Overflow parent 35",
                    &selector_full,
                );

                let scrolled_selector_frame = common::frame_from_capture(&report.captures[2]);
                let scrolled_selector_full = Region::full(
                    scrolled_selector_frame.cols(),
                    scrolled_selector_frame.rows(),
                );
                assertion::assert_text_in_region(
                    &scrolled_selector_frame,
                    "Overflow parent 00",
                    &scrolled_selector_full,
                );
                assertion::assert_text_in_region(
                    &scrolled_selector_frame,
                    "Enter: append",
                    &scrolled_selector_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Overflow parent 00", &full);
                assertion::assert_text_in_region(frame, "Choose parent session", &full);
            },
        )?;

    Ok(())
}

/// Verify that a review-ready parent can still open the reply composer, sync
/// the stack, and queue merge after its stacked child has also reached review,
/// while direct slash entry opens the same command menu available after
/// entering the reply composer.
#[test]
fn stacked_parent_merge_remains_available_with_review_child() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_parent_merge_remains_available_with_review_child")
        .with_git()
        .setup(seed_review_ready_parent_with_review_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Parent stack review", 5000)
                    .press_key("Up")
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled(
                        "parent_review",
                        "Parent review session with reply, commands, and sync available",
                    )
                    .press_key("/")
                    .wait_for_text("Slash Command", 3000)
                    .capture_labeled(
                        "parent_slash_commands",
                        "Direct slash entry opens commands for a stacked parent",
                    )
            },
            |frame, report| {
                let parent_frame = common::frame_from_capture(&report.captures[0]);
                let parent_full = Region::full(parent_frame.cols(), parent_frame.rows());
                assertion::assert_text_in_region(
                    &parent_frame,
                    "Parent stack review",
                    &parent_full,
                );
                assertion::assert_text_in_region(&parent_frame, "Enter: reply", &parent_full);
                assertion::assert_text_in_region(&parent_frame, "/: commands menu", &parent_full);
                assertion::assert_text_in_region(
                    &parent_frame,
                    "m: add to merge queue",
                    &parent_full,
                );
                assertion::assert_text_in_region(&parent_frame, "r: sync", &parent_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Parent stack review", &full);
                assertion::assert_text_in_region(frame, "Slash Command", &full);
                assertion::assert_text_in_region(frame, "/model", &full);
            },
        )?;

    Ok(())
}

/// Verify that startup recovery requeues a pending post-merge stacked child
/// restack and completes the deterministic sync in the child session view.
#[test]
fn stacked_pending_post_merge_restack_recovers_on_startup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_pending_post_merge_restack_recovers_on_startup")
        .with_git()
        .setup(seed_pending_post_merge_restack_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Pending post-merge child sync", 5000)
                    .press_key("Enter")
                    .wait_for_text("Successfully synced", 10000)
                    .capture_labeled(
                        "pending_restack_recovered",
                        "Pending post-merge stacked child sync recovered after startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Pending post-merge child sync", &full);
                assertion::assert_text_in_region(frame, "Successfully synced", &full);
                assertion::assert_not_visible(frame, "[Sync Error]");
            },
        )?;

    Ok(())
}

/// Verify that an automatic post-merge child sync failure remains visible in
/// the affected child session after startup.
#[test]
fn stacked_pending_post_merge_restack_failure_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_pending_post_merge_restack_failure_is_visible")
        .with_git()
        .setup(seed_failing_pending_post_merge_restack_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Blocked post-merge child sync", 5000)
                    .press_key("Enter")
                    .wait_for_text("[Sync Error]", 10000)
                    .capture_labeled(
                        "pending_restack_failure",
                        "Pending post-merge stacked child sync failure after startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Blocked post-merge child sync", &full);
                assertion::assert_text_in_region(frame, "[Sync Error]", &full);
                assertion::assert_text_in_region(frame, "Failed to sync", &full);
            },
        )?;

    Ok(())
}

/// Verify that a stacked draft can keep collecting staged prompts and parent
/// another stacked draft while its own parent is still running, but the start
/// shortcut stays hidden until the parent returns to review.
#[test]
fn stacked_session_start_waits_for_parent_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_session_start_waits_for_parent_review")
        .with_git()
        .setup(seed_running_stop_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Running session stop", 5000)
                    .press_key("a")
                    .wait_for_text("Stacked", 5000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .wait_for_text("Stack on selected", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .write_text("Waiting child draft")
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .capture_labeled(
                        "stacked_draft_waiting_parent",
                        "Stacked draft staged while parent is still running",
                    )
                    .press_key("q")
                    .wait_for_text("ACTIVE", 5000)
                    .press_key("a")
                    .wait_for_text("Stacked", 5000)
                    .press_key("Down")
                    .press_key("Down")
                    .press_key("Down")
                    .wait_for_text("Stack on selected", 5000)
                    .capture_labeled(
                        "stacked_draft_parent_selector",
                        "Stacked draft can parent another staged draft",
                    )
            },
            |frame, report| {
                let draft_frame = common::frame_from_capture(&report.captures[0]);
                let draft_full = Region::full(draft_frame.cols(), draft_frame.rows());
                assertion::assert_text_in_region(&draft_frame, "Enter: add draft", &draft_full);
                assertion::assert_not_visible(&draft_frame, "s: start");
                assertion::assert_not_visible(&draft_frame, "m: add to merge queue");
                assertion::assert_not_visible(&draft_frame, "r: sync");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Stack on selected", &full);
                assertion::assert_not_visible(frame, "Select parent first");
            },
        )?;

    Ok(())
}

/// Verify that root review-ready sessions can confirm session forking from
/// session view.
#[test]
fn session_fork_confirmation_creates_session_from_review_session() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_fork_confirmation")
        .with_git()
        .with_terminal_size(120, 24)
        .setup(seed_dirty_fork_source_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("session_view", "Fork shortcut visible in session view")
                    .write_text("F")
                    .wait_for_text("Confirm Fork", 3000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("fork_confirmation", "Fork confirmation popup")
                    .write_text("y")
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("forked_session_view", "Forked session opened")
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .capture_labeled(
                        "forked_session_chat_focus",
                        "Fork keeps chat navigation available",
                    )
                    .press_key("q")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("session_list_after_fork", "Source and fork listed")
            },
            |frame, report| {
                let session_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
                assertion::assert_text_in_region(&session_view_frame, "F: fork", &view_full);

                let confirmation_frame = common::frame_from_capture(&report.captures[1]);
                let confirmation_full =
                    Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Confirm Fork",
                    &confirmation_full,
                );
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Fork this session",
                    &confirmation_full,
                );

                let forked_view_frame = common::frame_from_capture(&report.captures[2]);
                let forked_view_full =
                    Region::full(forked_view_frame.cols(), forked_view_frame.rows());
                assertion::assert_text_in_region(
                    &forked_view_frame,
                    "Review-ready session shortcuts",
                    &forked_view_full,
                );
                assertion::assert_not_visible(&forked_view_frame, "Confirm Fork");

                let forked_chat_frame = common::frame_from_capture(&report.captures[3]);
                let forked_chat_full =
                    Region::full(forked_chat_frame.cols(), forked_chat_frame.rows());
                assertion::assert_text_in_region(
                    &forked_chat_frame,
                    "Tab: focus | j/k: scroll",
                    &forked_chat_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                let session_list_text = frame.text_in_region(&full);
                let session_title_count = session_list_text
                    .matches("Review-ready session shortcuts")
                    .count();
                assert!(
                    session_title_count >= 2,
                    "expected source and fork rows in session list, got \
                     {session_title_count}:\n{session_list_text}"
                );
            },
        )?;

    Ok(())
}

/// Verify that review-ready stacked children do not expose session forking.
#[test]
fn stacked_child_hides_session_fork_shortcut() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_child_hides_session_fork_shortcut")
        .with_git()
        .with_terminal_size(120, 24)
        .setup(seed_review_ready_parent_with_review_child)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Child stack review", 5000)
                    .press_key("Enter")
                    .wait_for_text("Child stack review", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("child_view", "Fork shortcut hidden for stacked child")
                    .write_text("F")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1000)
                    .capture_labeled("after_f", "No fork confirmation opens for stacked child")
            },
            |frame, report| {
                let child_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(child_view_frame.cols(), child_view_frame.rows());
                assertion::assert_text_in_region(
                    &child_view_frame,
                    "Child stack review",
                    &view_full,
                );
                assertion::assert_not_visible(&child_view_frame, "F: fork");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Child stack review", &full);
                assertion::assert_not_visible(frame, "F: fork");
                assertion::assert_not_visible(frame, "Confirm Fork");
                assertion::assert_not_visible(frame, "Reviewing changes with");
                assertion::assert_not_visible(frame, "No diff changes found for review.");
            },
        )?;

    Ok(())
}
