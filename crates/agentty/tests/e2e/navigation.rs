//! Navigation E2E tests: tab cycling, reverse tab cycling, and help overlay.

use agentty::infra::db::{
    DB_DIR, DB_FILE, Database, SessionPreparationState, acquire_instance_lock,
};
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// A contending CLI must exit before startup recovery mutates live work.
#[tokio::test]
async fn second_instance_preserves_live_operations() -> E2eResult {
    // Arrange
    let root = tempfile::tempdir()?;
    let _owner = acquire_instance_lock(root.path()).await?;
    let database = Database::open(&root.path().join(DB_DIR).join(DB_FILE)).await?;
    let project_id = database
        .projects()
        .upsert_project("live-project", None)
        .await?;
    database
        .sessions()
        .insert_session("live", "gpt-5.6-sol", "main", "InProgress", project_id)
        .await?;
    database
        .sessions()
        .insert_session_preparation("live", "main")
        .await?;
    database
        .operations()
        .insert_session_operation("live-turn", "live", "rebase")
        .await?;
    database
        .operations()
        .mark_session_operation_running("live-turn")
        .await?;

    // Act
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(assert_cmd::cargo::cargo_bin!("agentty"))
            .arg("--no-update")
            .env("AGENTTY_ROOT", root.path())
            .current_dir(root.path())
            .kill_on_drop(true)
            .output(),
    )
    .await??;

    // Assert
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Another Agentty instance is already using")
    );
    assert_eq!(
        database
            .sessions()
            .load_session("live")
            .await?
            .expect("live session")
            .status,
        "InProgress"
    );
    assert_eq!(
        database
            .sessions()
            .load_session_preparation("live")
            .await?
            .expect("live preparation")
            .state,
        SessionPreparationState::Preparing
    );
    let operations = database
        .operations()
        .load_unfinished_session_operations()
        .await?;
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status, "running");
    assert!(!operations[0].cancel_requested);

    Ok(())
}

/// Verify that agentty startup renders the Sessions tab when an active
/// project already exists.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
fn startup_shows_sessions_tab_for_active_project() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("startup")
        .with_git()
        .setup(common::seed_active_project_setting)
        .zola(
            "Startup",
            "Launch agentty and land on the session list in seconds.",
            10,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("startup", "Initial render with Sessions tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agentty", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that Tab key switches between tabs.
///
/// Starts on Projects tab, presses Tab, and verifies the next tab
/// becomes selected while Projects becomes unselected.
#[test]
fn tab_key_switches_tabs() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("tab_switch")
        .with_git()
        .zola(
            "Tab switching",
            "Jump between workspace tabs with a single keypress.",
            60,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .capture_labeled("before", "Projects tab selected")
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2500)
                    .capture_labeled("after", "Sessions tab selected")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing Tab cycles through all primary tabs in order.
///
/// Starts on Projects and asserts each successive tab becomes selected:
/// Sessions and Settings.
#[test]
fn tab_cycles_through_all_tabs() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("tab_full_cycle")
        .with_git()
        .zola(
            "Full tab cycle",
            "Cycle through every workspace tab in order.",
            70,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("sessions", "Sessions tab selected")
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2500)
                    .capture_labeled("settings", "Settings tab selected")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Default Smart Model", &full);

                assert_eq!(
                    report.captures.len(),
                    2,
                    "Expected 2 captures (sessions, settings)"
                );

                let sessions_frame = common::frame_from_capture(&report.captures[0]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let settings_frame = common::frame_from_capture(&report.captures[1]);
                let settings_full = Region::full(settings_frame.cols(), settings_frame.rows());
                assertion::assert_text_in_region(
                    &settings_frame,
                    "Default Smart Model",
                    &settings_full,
                );
            },
        )?;

    Ok(())
}

/// Verify that pressing `q` opens a quit confirmation dialog.
///
/// The dialog should display the title "Confirm Quit" and the message
/// "Quit agentty?" with selectable options.
#[test]
fn quit_shows_confirmation_dialog() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("quit_confirmation")
        .with_git()
        .zola(
            "Quit confirmation",
            "Confirm before quitting to prevent accidental exits.",
            130,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .capture_labeled("before", "App running before quit")
                    .compose(&common::open_quit_dialog())
                    .viewing_pause_ms(2500)
                    .capture_labeled("dialog", "Quit confirmation dialog")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Confirm Quit", &full);
                assertion::assert_text_in_region(frame, "Quit agentty?", &full);
            },
        )?;

    Ok(())
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
fn startup_shows_footer_hints() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("footer_hints")
        .with_git()
        .zola(
            "Footer hints",
            "Context-sensitive hints in the footer guide available actions.",
            110,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("startup", "Footer with keybinding hints")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "q: quit", &full);
                assertion::assert_text_in_region(frame, "?: help", &full);
            },
        )?;

    Ok(())
}

/// Verify that `BackTab` (Shift+Tab) cycles tabs in reverse order.
///
/// Starts on Projects (first tab), then presses `BackTab` to cycle back
/// through Settings, Sessions, and Projects.
#[test]
fn backtab_cycles_tabs_reverse() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("backtab_reverse")
        .with_git()
        .zola(
            "Reverse tab navigation",
            "Navigate tabs in reverse with Shift+Tab.",
            80,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_settings", "Settings tab after first BackTab")
                    .compose(&common::switch_to_tab_reverse("Sessions"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_sessions", "Sessions tab after second BackTab")
                    .compose(&common::switch_to_tab_reverse("Projects"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_projects", "Projects tab after third BackTab")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test-project", &full);

                let settings_frame = common::frame_from_capture(&report.captures[0]);
                let settings_full = Region::full(settings_frame.cols(), settings_frame.rows());
                assertion::assert_text_in_region(
                    &settings_frame,
                    "Default Smart Model",
                    &settings_full,
                );

                let sessions_frame = common::frame_from_capture(&report.captures[1]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let projects_frame = common::frame_from_capture(&report.captures[2]);
                let projects_full = Region::full(projects_frame.cols(), projects_frame.rows());
                assertion::assert_text_in_region(&projects_frame, "test-project", &projects_full);
            },
        )?;

    Ok(())
}

/// Verify that `?` opens the help overlay with keybinding content, and
/// `Esc` closes it and restores the previous view.
#[test]
fn help_overlay_toggle() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("help_overlay")
        .with_git()
        .zola(
            "Help overlay",
            "Press ? to see available keybindings for the current view.",
            100,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .capture_labeled("before", "Normal view before help")
                    .compose(&common::open_help_overlay())
                    .viewing_pause_ms(2500)
                    .capture_labeled("help_open", "Help overlay visible")
                    .press_key("Escape")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("help_closed", "Help overlay dismissed")
            },
            |frame, report| {
                let help_frame = common::frame_from_capture(&report.captures[1]);
                let full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &full);

                let restored_full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test-project", &restored_full);

                let closed_text = frame.text_in_region(&restored_full);
                assert!(
                    !closed_text.contains("Keybindings"),
                    "Help overlay should be dismissed after Esc"
                );
            },
        )?;

    Ok(())
}
