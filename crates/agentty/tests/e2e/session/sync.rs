//! Session merge, rebase, and push.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use testty::assertion;
use testty::region::Region;

use super::fixture::{
    E2eResult, run_git, seed_rebase_transcript_session_with_delay,
    seed_session_title_candidate_project,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Stable id for the session whose branch conflicts with `main`.
const MERGE_CONFLICT_SESSION_ID: &str = "merge-conflict-0001";

/// Seeds a review-ready worktree whose committed change conflicts with a
/// newer commit on the stored base branch.
fn seed_merge_conflict_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_merge_conflict_session_with_model(env, "gpt-5.6-sol")
}

/// Seeds the merge-conflict fixture with a specific persisted agent model.
fn seed_merge_conflict_session_with_model(
    env: &BuilderEnv,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(MERGE_CONFLICT_SESSION_ID, model, "main", "Review")
            .with_title("Update shared configuration"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        test_support::persist_active_tab_for_test(&database, agentty::app::Tab::Sessions).await
    })?;

    std::fs::write(env.workdir.join("shared.txt"), "initial\n")?;
    run_git(&env.workdir, &["add", "shared.txt"])?;
    run_git(&env.workdir, &["commit", "-m", "Add shared configuration"])?;

    let session_worktree =
        test_support::session_folder(&env.agentty_root.join("wt"), MERGE_CONFLICT_SESSION_ID);
    let session_worktree_text = session_worktree
        .to_str()
        .ok_or_else(|| std::io::Error::other("session worktree path is not UTF-8"))?;
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/merge-co",
            session_worktree_text,
            "main",
        ],
    )?;
    std::fs::write(session_worktree.join("shared.txt"), "session change\n")?;
    run_git(&session_worktree, &["add", "shared.txt"])?;
    run_git(
        &session_worktree,
        &["commit", "-m", "Change session configuration"],
    )?;

    std::fs::write(env.workdir.join("shared.txt"), "main change\n")?;
    run_git(&env.workdir, &["add", "shared.txt"])?;
    run_git(&env.workdir, &["commit", "-m", "Change main configuration"])?;

    Ok(())
}

/// Seeds an assisted rebase conflict whose staged resolution is rejected by
/// the effective pre-commit hook.
fn seed_rebase_pre_commit_hook_failure_session(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_merge_conflict_session_with_model(env, "gemini-3.1-pro-preview")?;

    let antigravity_path = env.stub_bin.join("agy");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'agy 1.2.0\n'; exit 0; fi

printf '%s\n' '{"event":"init","conversation_id":"rebase-hook-test","init":{"cwd":"stub"}}'
turn=0
while IFS= read -r prompt_event; do
  turn=$((turn + 1))
  printf 'resolved by rebase assistance\n' > shared.txt
  answer='Resolved the staged rebase conflict.'
  printf '{"event":"step_update","step_update":{"conversation_id":"rebase-hook-test","step_index":%s,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":4,"output_tokens":4}}}\n' "$turn"
  printf '{"event":"result","result":{"conversation_id":"rebase-hook-test","status":"SUCCESS","response":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"review_comment_outcomes\\":[]}","structured_output":{"answer":"%s","questions":[],"review_comment_outcomes":[]},"error":"","duration_seconds":0.1,"num_turns":%s,"usage":{"input_tokens":4,"output_tokens":4,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":8}}}\n' "$answer" "$answer" "$turn"
done
"#;
    std::fs::write(&antigravity_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&antigravity_path, std::fs::Permissions::from_mode(0o750))?;

    let pre_commit_hook = env.workdir.join(".git").join("hooks").join("pre-commit");
    std::fs::write(
        &pre_commit_hook,
        "#!/bin/sh\nprintf 'resolved conflict rejected by pre-commit hook\\n' >&2\nexit 1\n",
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&pre_commit_hook, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Seeds a review-ready transcript and delays its Git rebase long enough to
/// inspect the in-progress session output ordering.
fn seed_rebase_transcript_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_rebase_transcript_session_with_delay(env, 5)
}

/// Seeds one synced published session whose next completed turn appends a
/// durable commit notice and then starts a delayed auto-push, leaving the
/// earlier sync result, the turn commit notice, and the auto-push progress row
/// visible in one frame.
fn seed_published_session_output_chronology(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_session_title_candidate_project(env)?;

    let session_id = "review-shortcut-0001";
    common::seed_session(
        env,
        SessionSeed::regular(session_id, "claude-haiku-4-5-20251001", "main", "Review")
            .with_title("Chronological session output"),
    )?;

    // The session worktree must stay a linked worktree of the project
    // checkout; a standalone repository trips the session isolation guard and
    // the turn never starts.
    let remote_path = env.agentty_root.join("chronology-remote.git");
    let remote_path_text = remote_path.to_string_lossy().into_owned();
    run_git(&env.workdir, &["init", "--bare", remote_path_text.as_str()])?;
    run_git(
        &env.workdir,
        &["remote", "add", "origin", remote_path_text.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    let session_worktree = test_support::session_folder(&env.agentty_root.join("wt"), session_id);
    let session_worktree_text = session_worktree.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/review-s",
            session_worktree_text.as_str(),
        ],
    )?;
    run_git(
        &session_worktree,
        &["push", "--set-upstream", "origin", "wt/review-s"],
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_published_upstream_ref(
                session_id,
                Some("origin/wt/review-s".to_string()),
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync] Successfully synced wt/review-s onto origin/main\n",
            )
            .await
    })?;

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  push)
    sleep 8
    ;;
esac
exec '{}' "$@"
"#,
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Verify that a session branch conflicting with its stored base is marked in
/// both the Sessions list and the open session header.
#[test]
fn test_session_merge_conflict_alert() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_merge_conflict_alert")
        .with_git()
        .setup(seed_merge_conflict_session)
        .zola(
            "See merge conflicts before syncing",
            "Agentty marks sessions whose branch conflicts with its base branch in the list and \
             session view.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("[merge conflict]", 10000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_list_alert",
                        "The conflicting session is marked beside its title",
                    )
                    .press_key("Enter")
                    .wait_for_text("Merge conflict with main", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_view_alert",
                        "The session header names the conflicting base branch",
                    )
            },
            |frame, report| {
                let list_frame = common::frame_from_capture(&report.captures[0]);
                let list_region = Region::full(list_frame.cols(), list_frame.rows());
                assertion::assert_text_in_region(&list_frame, "[merge conflict]", &list_region);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Merge conflict with main", &full);
            },
        )?;

    Ok(())
}

/// Verify assisted rebase conflict resolutions must pass the effective
/// pre-commit hook before Agentty continues the rebase.
#[test]
fn test_session_rebase_pre_commit_hook_failure() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_rebase_pre_commit_hook_failure")
        .with_git()
        .setup(seed_rebase_pre_commit_hook_failure_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("[merge conflict]", 10000)
                    .press_key("Enter")
                    .wait_for_text("Merge conflict with main", 5000)
                    .press_key("r")
                    .wait_for_text("[Sync Error]", 15000)
                    .capture_labeled(
                        "rebase_pre_commit_hook_failure",
                        "Pre-commit hook blocks the assisted rebase resolution",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Sync Error]", &full);
                assertion::assert_text_in_region(frame, "Pre-commit hook rejected", &full);
                assertion::assert_text_in_region(frame, "resolved conflict rejected", &full);
            },
        )?;

    Ok(())
}

/// Verify a manual rebase keeps the completed answer stable while only the
/// workflow status tail animates.
#[test]
fn session_rebase_keeps_completed_transcript_stable() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_rebase_transcript_stability")
        .with_git()
        .setup(seed_rebase_transcript_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Completed answer before rebase.", 5000)
                    .press_key("r")
                    .wait_for_text("Rebasing...", 5000)
                    .capture_labeled(
                        "rebase_transcript_stable",
                        "Completed transcript remains stable during rebase",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Completed answer before rebase.", &full);
                assertion::assert_not_visible(frame, "Change Summary");
                assertion::assert_text_in_region(frame, "Rebasing...", &full);
            },
        )?;

    Ok(())
}

/// Verify post-turn auto-push progress renders below every durable notice that
/// preceded it — the earlier sync result and the completed turn's commit
/// notice — preserving workflow chronology in session output.
#[test]
fn test_session_output_chronology() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_output_chronology")
        .with_git()
        .with_terminal_size(120, 30)
        .setup(seed_published_session_output_chronology)
        .zola(
            "Chronological session output",
            "Follow sync, commit, and auto-push progress in execution order.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Continue after the sync")
                    .press_key("Enter")
                    .wait_for_text("Got it. What would you like me to do?", 30000)
                    .wait_for_text("Auto-pushing", 10000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "session_output_chronology",
                        "Earlier sync and commit results remain above later auto-push progress",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);
                // Row lookup goes through the rendered lines instead of
                // `find_text` because unpainted terminal cells drop the spaces
                // inside multi-word notices.
                let notice_row =
                    |needle: &str| view_text.lines().position(|line| line.contains(needle));
                for needle in [
                    "[Sync] Successfully synced",
                    "[Commit] No changes to commit.",
                    "Auto-pushing published branch",
                ] {
                    assert!(
                        notice_row(needle).is_some(),
                        "missing `{needle}` in frame:\n{view_text}"
                    );
                }

                let sync_row = notice_row("[Sync] Successfully synced").expect("sync result row");
                let commit_row =
                    notice_row("[Commit] No changes to commit.").expect("turn commit notice row");
                let auto_push_row =
                    notice_row("Auto-pushing published branch").expect("auto-push status row");

                assert!(sync_row < commit_row);
                assert!(commit_row < auto_push_row);
            },
        )?;

    Ok(())
}

/// Verify session sync queues behind an active published-branch auto-push
/// without blocking session-view input or redraws.
#[test]
fn session_sync_remains_responsive_during_auto_push() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_sync_responsive_during_auto_push")
        .with_git()
        .with_terminal_size(120, 30)
        .setup(seed_published_session_output_chronology)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Continue after the sync")
                    .press_key("Enter")
                    .wait_for_text("Got it. What would you like me to do?", 30000)
                    .wait_for_text("Auto-pushing", 10000)
                    .press_key("r")
                    .press_key("?")
                    .wait_for_text("Keybindings", 3000)
                    .capture_labeled(
                        "responsive_during_auto_push",
                        "Session help opens while sync waits behind auto-push",
                    )
                    .press_key("q")
                    .wait_for_text("[Sync] Successfully synced", 15000)
            },
            |frame, report| {
                let help_frame = common::frame_from_capture(&report.captures[0]);
                let help_full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &help_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Sync] Successfully synced", &full);
                assertion::assert_not_visible(frame, "active session worker is unavailable");
            },
        )?;

    Ok(())
}

/// Verify a completed automatic branch push remains visible after its owning
/// project is switched out while the push is running and then restored.
#[test]
fn published_branch_push_survives_project_switching() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("published_branch_push_survives_project_switching")
        .with_git()
        .setup(|env| {
            seed_published_session_output_chronology(env)?;
            common::seed_second_project(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Continue after the sync")
                    .press_key("Enter")
                    .wait_for_text("Got it. What would you like me to do?", 30000)
                    .wait_for_text("Auto-pushing", 10000)
                    .wait_for_text("Enter: reply", 5000)
                    .compose(&common::return_to_session_list())
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .sleep_ms(9000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("[Branch Push]", 5000)
                    .wait_for_text("Auto-pushed published branch after completed turn.", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Branch Push]", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Auto-pushed published branch after completed turn.",
                    &full,
                );
            },
        )?;

    Ok(())
}
