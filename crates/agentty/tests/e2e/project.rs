//! Projects page E2E tests: project dashboard activity and summary data.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use testty::assertion;
use testty::proof::report::ProofReport;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Configures two local upstreams and delays `git pull` long enough to prove
/// navigation and cross-project sync queueing remain non-modal.
fn seed_delayed_project_sync(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_second_project(env)?;

    let origin = env.agentty_root.join("project-sync-origin.git");
    configure_local_upstream(&env.workdir, &origin)?;
    let second_project_dir = env
        .workdir
        .parent()
        .ok_or("missing temp root for second project")?
        .join("zeta-project");
    let second_origin = env.agentty_root.join("second-project-sync-origin.git");
    configure_local_upstream(&second_project_dir, &second_origin)?;

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
if [ "$1" = "pull" ]; then
  sleep 4
fi
exec '{real_git}' "$@"
"#
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Creates a bare local remote and publishes one test project's `main` branch.
fn configure_local_upstream(
    project_dir: &Path,
    origin: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(origin)?;
    run_git(origin, &["init", "--bare", "."])?;
    let origin_path = origin.to_string_lossy().into_owned();
    run_git(
        project_dir,
        &["remote", "add", "origin", origin_path.as_str()],
    )?;
    run_git(project_dir, &["push", "--set-upstream", "origin", "main"])?;

    Ok(())
}

/// Runs one Git command with deterministic test identity.
fn run_git(working_directory: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .env("GIT_AUTHOR_NAME", "Agentty Test")
        .env("GIT_AUTHOR_EMAIL", "agentty@example.com")
        .env("GIT_COMMITTER_NAME", "Agentty Test")
        .env("GIT_COMMITTER_EMAIL", "agentty@example.com")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("git {} failed with {status}", args.join(" ")).into());
    }

    Ok(())
}

/// Verifies that session creation is rejected in-app during project sync.
fn verify_session_creation_blocked(report: &ProofReport) -> Result<(), &'static str> {
    let capture = report
        .captures
        .iter()
        .find(|capture| capture.label == "session_creation_blocked")
        .ok_or("missing blocked session creation capture")?;
    let frame = common::frame_from_capture(capture);
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Session creation unavailable", &full);
    assertion::assert_text_in_region(&frame, "is synchronizing main", &full);

    Ok(())
}

/// Verify that the Projects tab lists the registered git project name and
/// branch from the temp workdir and shows dashboard activity plus work stats.
///
/// Agentty auto-registers the current git working directory as a project on
/// startup. The test creates a `test-project` repository and asserts that
/// the project name appears in the project list.
#[test]
fn projects_page_shows_cwd() {
    // Arrange, Act, Assert
    FeatureTest::new("projects_cwd")
        .with_git()
        .zola(
            "Project dashboard",
            "See project activity and switch between registered projects.",
            90,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("gemini 0.0.1-updated", 10000)
                    .wait_for_text("claude 0.0.1-updated", 10000)
                    .wait_for_text("codex 0.0.1-updated", 10000)
                    .viewing_pause_ms(3000)
                    .capture_labeled("projects", "Projects page with registered project")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Activity", &full);
                assertion::assert_text_in_region(frame, "Branch", &full);
                assertion::assert_text_in_region(frame, "Sessions", &full);
                assertion::assert_text_in_region(frame, "Work Pace", &full);
                assertion::assert_text_in_region(frame, "Agent CLIs", &full);
                assertion::assert_text_in_region(frame, "claude", &full);
                assertion::assert_text_in_region(frame, "0.0.1-updated", &full);
                assertion::assert_text_in_region(frame, "gemini 0.0.1-updated", &full);
                assertion::assert_text_in_region(frame, "Tokens In", &full);
                assertion::assert_text_in_region(frame, "Out", &full);
                assertion::assert_not_visible(frame, "Version");
                assertion::assert_not_visible(frame, "Agentty is an ADE");
                assertion::assert_not_visible(frame, "Last Opened");
                assertion::assert_text_in_region(frame, "Active", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
                assertion::assert_text_in_region(frame, "main", &full);
            },
        )
        .expect("feature test failed");
}

/// Verify that `p` on the Sessions tab opens the MRU-ordered project switcher
/// popup and that selecting another project switches the active project
/// without leaving the Sessions view.
///
/// The test seeds a second registered project (`zeta-project`) that was never
/// opened, so the active `test-project` stays first in MRU order and the
/// seeded project sorts below it even when both projects receive the same
/// pinned last-opened timestamp. The scenario starts on a pre-persisted
/// Sessions tab and toggles the active project there and back, so the follow-up
/// VHS recording replays against the same MRU order as the assertion run.
#[test]
fn test_project_switcher() {
    // Arrange, Act, Assert
    FeatureTest::new("project_switcher")
        .with_git()
        .setup(common::seed_second_project)
        .zola(
            "Project switcher",
            "Switch the active project from the Sessions view with a quick MRU popup.",
            91,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("switcher", "MRU project switcher popup")
                    .press_key("j")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(1000)
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("switched", "Sessions view scoped to the switched project")
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled(
                        "switched_back",
                        "Sessions view restored to the first project",
                    )
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "p: projects", &full);
                assertion::assert_text_in_region(frame, "Project: test-project", &full);
                assertion::assert_not_visible(frame, "Switch project");

                let popup_capture = report
                    .captures
                    .iter()
                    .find(|capture| capture.label == "switcher")
                    .expect("missing switcher capture");
                let popup_frame = common::frame_from_capture(popup_capture);
                let popup_region = Region::full(popup_frame.cols(), popup_frame.rows());
                assertion::assert_text_in_region(&popup_frame, "Switch project", &popup_region);
                assertion::assert_text_in_region(&popup_frame, "* test-project", &popup_region);
                assertion::assert_text_in_region(&popup_frame, "zeta-project", &popup_region);

                let switched_capture = report
                    .captures
                    .iter()
                    .find(|capture| capture.label == "switched")
                    .expect("missing switched capture");
                let switched_frame = common::frame_from_capture(switched_capture);
                let switched_region = Region::full(switched_frame.cols(), switched_frame.rows());
                assertion::assert_text_in_region(
                    &switched_frame,
                    "Project: zeta-project",
                    &switched_region,
                );
            },
        )
        .expect("feature test failed");
}

/// Verify explicit project sync reports progress in the status bar while tab
/// navigation remains available, then runs a queued sync for another project.
#[test]
fn test_project_sync_non_modal() {
    // Arrange, Act, Assert
    FeatureTest::new("project_sync_non_modal")
        .with_git()
        .setup(seed_delayed_project_sync)
        .zola(
            "Non-modal project sync",
            "Keep navigating while Agentty safely synchronizes the project branch.",
            92,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("s")
                    .wait_for_text("Syncing test-project/main...", 5000)
                    .capture_labeled(
                        "syncing_while_navigating",
                        "Sessions tab remains interactive during project sync",
                    )
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Session creation unavailable", 5000)
                    .capture_labeled(
                        "session_creation_blocked",
                        "Session creation is safely blocked without exiting Agentty",
                    )
                    .press_key("Enter")
                    .wait_for_text("Sessions", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: zeta-project", 5000)
                    .press_key("s")
                    .wait_for_text("Syncing zeta-project/main...", 15_000)
                    .capture_labeled(
                        "queued_project_sync",
                        "Second project sync starts after the first finishes",
                    )
                    .wait_for_text("Synced zeta-project/main", 15_000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "sync_complete",
                        "Project sync completes in the status bar without a popup",
                    )
                    .sleep_ms(10_500)
                    .capture_labeled(
                        "sync_status_expired",
                        "The status bar returns to its normal page hint",
                    )
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_not_visible(frame, "Synced zeta-project/main");
                assertion::assert_text_in_region(frame, "FYI:", &full);
                assertion::assert_not_visible(frame, "Sync complete");

                let complete_capture = report
                    .captures
                    .iter()
                    .find(|capture| capture.label == "sync_complete")
                    .expect("missing completed sync capture");
                let complete_frame = common::frame_from_capture(complete_capture);
                let complete_full = Region::full(complete_frame.cols(), complete_frame.rows());
                assertion::assert_text_in_region(
                    &complete_frame,
                    "Synced zeta-project/main",
                    &complete_full,
                );

                let syncing_capture = report
                    .captures
                    .iter()
                    .find(|capture| capture.label == "syncing_while_navigating")
                    .expect("missing in-progress sync capture");
                let syncing_frame = common::frame_from_capture(syncing_capture);
                let syncing_full = Region::full(syncing_frame.cols(), syncing_frame.rows());
                assertion::assert_text_in_region(
                    &syncing_frame,
                    "Syncing test-project/main...",
                    &syncing_full,
                );
                assertion::assert_text_in_region(&syncing_frame, "Sessions", &syncing_full);
                assertion::assert_not_visible(&syncing_frame, "Sync in progress");

                verify_session_creation_blocked(report)
                    .expect("missing blocked session creation capture");

                let queued_capture = report
                    .captures
                    .iter()
                    .find(|capture| capture.label == "queued_project_sync")
                    .expect("missing queued project sync capture");
                let queued_frame = common::frame_from_capture(queued_capture);
                let queued_full = Region::full(queued_frame.cols(), queued_frame.rows());
                assertion::assert_text_in_region(
                    &queued_frame,
                    "Syncing zeta-project/main...",
                    &queued_full,
                );
            },
        )
        .expect("feature test failed");
}

/// Verify a staged draft cannot materialize its worktree while project sync
/// owns the base checkout.
#[test]
fn project_sync_blocks_staged_draft_start() {
    // Arrange, Act, Assert
    FeatureTest::new("project_sync_blocks_staged_draft_start")
        .with_git()
        .setup(seed_delayed_project_sync)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .write_text("Start after project sync")
                    .press_key("Enter")
                    .wait_for_text("s: start", 5000)
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .press_key("s")
                    .wait_for_text("Syncing test-project/main...", 5000)
                    .press_key("Enter")
                    .wait_for_text("Draft Session", 5000)
                    .press_key("s")
                    .wait_for_text("[Start Error]", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Start Error]", &full);
                assertion::assert_text_in_region(frame, "is synchronizing", &full);
                assertion::assert_text_in_region(frame, "s: start", &full);
            },
        )
        .expect("feature test failed");
}
