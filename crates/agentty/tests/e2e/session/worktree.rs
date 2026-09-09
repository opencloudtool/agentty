//! Bare repository worktree layout.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use testty::assertion;
use testty::region::Region;

use super::fixture::{E2eResult, run_git, run_git_stdout, seed_project_settings};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Answer text emitted by the bare-layout success stub once its turn completes.
const BARE_LAYOUT_ANSWER_TEXT: &str = "Bare worktree turn completed";

/// Rewrites the test project into a bare-repository worktree layout and
/// installs a Claude stub that completes one turn successfully.
///
/// Creates a bare shared repository as a sibling of the project directory,
/// seeds an initial `main` commit without a main working checkout, adds a
/// sibling `main` worktree, and adds `env.workdir` as the `feature` linked
/// worktree that Agentty opens as the project. This reproduces the
/// container-of-worktrees layout where the shared repository is bare and there
/// is no main working checkout for the dirty-status snapshot, which previously
/// failed the first turn with `this operation must be run in a work tree`.
fn seed_bare_repo_worktree_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let container = env
        .workdir
        .parent()
        .ok_or_else(|| std::io::Error::other("workdir has no parent container"))?
        .to_path_buf();
    let bare_dir = container.join("project.bare");
    std::fs::create_dir_all(&bare_dir)?;

    run_git(&bare_dir, &["init", "--bare", "."])?;
    run_git(&bare_dir, &["config", "user.email", "test@test.com"])?;
    run_git(&bare_dir, &["config", "user.name", "Test"])?;

    let empty_tree = run_git_stdout(&bare_dir, &["hash-object", "-w", "-t", "tree", "/dev/null"])?;
    let init_commit = run_git_stdout(&bare_dir, &["commit-tree", &empty_tree, "-m", "init"])?;
    run_git(&bare_dir, &["update-ref", "refs/heads/main", &init_commit])?;
    run_git(&bare_dir, &["symbolic-ref", "HEAD", "refs/heads/main"])?;

    // Sibling main worktree so the shared repo genuinely holds per-branch
    // worktrees as siblings, then the project worktree Agentty opens.
    let main_worktree = container.join("main").to_string_lossy().into_owned();
    run_git(
        &bare_dir,
        &["worktree", "add", main_worktree.as_str(), "main"],
    )?;
    let project_path = env.workdir.to_string_lossy().into_owned();
    run_git(
        &bare_dir,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            project_path.as_str(),
            "main",
        ],
    )?;

    install_bare_layout_success_claude_stub(env)
}

/// Installs a Claude stub that completes one turn with a fixed successful
/// answer so the bare-layout scenario can drive a turn without a live backend.
fn install_bare_layout_success_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{{\\"answer\\":\\"{BARE_LAYOUT_ANSWER_TEXT}\\",\\"questions\\":[]}}"}}]}}}}\n'
printf '{{"type":"result","subtype":"success","result":"{{\"answer\":\"{BARE_LAYOUT_ANSWER_TEXT}\",\"questions\":[]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}\n'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Verify that Agentty can create a session and drive one turn to review when
/// the project is a linked worktree of a bare shared repository.
///
/// The bare layout has no main working checkout, so the pre-fix dirty-status
/// snapshot failed the first turn with `this operation must be run in a work
/// tree`. This test drives a full successful turn and asserts the session
/// reaches the review-ready state instead of surfacing that error.
#[test]
fn bare_repo_worktree_layout_supports_session_turn() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("bare_repo_worktree_layout")
        .setup(seed_bare_repo_worktree_project)
        .zola(
            "Bare repository worktree layout",
            "Run sessions from a project that is a linked worktree of a bare shared repository.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Summarize the bare worktree layout")
                    .wait_for_text("Summarize the bare worktree layout", 3000)
                    .press_key("Enter")
                    .wait_for_text(BARE_LAYOUT_ANSWER_TEXT, 30000)
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled(
                        "bare_layout_review",
                        "Session reaches review after one turn in a bare worktree layout",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, BARE_LAYOUT_ANSWER_TEXT, &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "this operation must be run in a work tree");
            },
        )?;

    Ok(())
}
