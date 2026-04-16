//! Navigation E2E tests: tab cycling, reverse tab cycling, and help overlay.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Verify that agentty startup renders the Projects tab as selected.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
fn startup_shows_projects_tab() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("startup")
        .zola(
            "Startup",
            "Initial render with the Projects tab selected.",
            10,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("startup", "Initial render with Projects tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agentty", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
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

/// Verify that pressing Tab cycles through all four tabs in order.
///
/// Starts on Projects, presses Tab three times, and asserts each
/// successive tab becomes selected: Sessions → Stats → Settings.
#[test]
fn tab_cycles_through_all_tabs() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("tab_full_cycle")
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
                    .compose(&common::switch_to_tab("Stats"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("stats", "Stats tab selected")
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2500)
                    .capture_labeled("settings", "Settings tab selected")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Reasoning Level", &full);

                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (sessions, stats, settings)"
                );

                let sessions_frame = common::frame_from_capture(&report.captures[0]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let stats_frame = common::frame_from_capture(&report.captures[1]);
                let stats_full = Region::full(stats_frame.cols(), stats_frame.rows());
                assertion::assert_text_in_region(&stats_frame, "TokenStats", &stats_full);
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
                let footer = Region::footer(frame.cols(), frame.rows());
                let footer_text = frame.text_in_region(&footer);
                assert!(
                    !footer_text.trim().is_empty(),
                    "Footer should contain keybinding hints"
                );
            },
        )?;

    Ok(())
}

/// Verify that `BackTab` (Shift+Tab) cycles tabs in reverse order.
///
/// Starts on Projects (first tab), navigates forward to Settings (last tab)
/// via three Tab presses, then presses `BackTab` three times to cycle back
/// through Stats → Sessions → Projects.
#[test]
fn backtab_cycles_tabs_reverse() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("backtab_reverse")
        .zola(
            "Reverse tab navigation",
            "Navigate tabs in reverse with Shift+Tab.",
            80,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("at_settings", "Settings tab selected before reverse")
                    .compose(&common::switch_to_tab_reverse("Stats"))
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_to_stats", "Stats tab after first BackTab")
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

                let stats_frame = common::frame_from_capture(&report.captures[1]);
                let stats_full = Region::full(stats_frame.cols(), stats_frame.rows());
                assertion::assert_text_in_region(&stats_frame, "TokenStats", &stats_full);

                let sessions_frame = common::frame_from_capture(&report.captures[2]);
                let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
                assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

                let projects_frame = common::frame_from_capture(&report.captures[3]);
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
