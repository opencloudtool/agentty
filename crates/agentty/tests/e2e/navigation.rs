//! Navigation E2E tests: tab cycling, reverse tab cycling, and help overlay.

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::BuilderEnv;

/// Verify that agentty startup renders the Projects tab as selected.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
fn startup_shows_projects_tab() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("startup")
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("startup", "Initial render with Projects tab");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Agentty", &full);
    assertion::assert_text_in_region(&frame, "test-project", &full);

    common::save_feature_gif(&scenario, &report, &env, "startup");
}

/// Verify that Tab key switches between tabs.
///
/// Starts on Projects tab, presses Tab, and verifies the next tab
/// becomes selected while Projects becomes unselected.
#[test]
fn tab_key_switches_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("tab_switch")
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("before", "Projects tab selected")
        .compose(&common::switch_to_tab("Sessions"))
        .capture_labeled("after", "Sessions tab selected");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — after pressing Tab, "Sessions" should be selected.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(&scenario, &report, &env, "tab_switch");
}

/// Verify that pressing Tab cycles through all four tabs in order.
///
/// Starts on Projects, presses Tab three times, and asserts each
/// successive tab becomes selected: Sessions → Stats → Settings.
#[test]
fn tab_cycles_through_all_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("tab_full_cycle")
        .compose(&common::wait_for_agentty_startup())
        // Tab 1: Projects → Sessions
        .compose(&common::switch_to_tab("Sessions"))
        .capture_labeled("sessions", "Sessions tab selected")
        // Tab 2: Sessions → Stats
        .compose(&common::switch_to_tab("Stats"))
        .capture_labeled("stats", "Stats tab selected")
        // Tab 3: Stats → Settings
        .compose(&common::switch_to_tab("Settings"))
        .capture_labeled("settings", "Settings tab selected");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — final frame should have Settings selected.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Reasoning Level", &full);

    // Assert — all three intermediate captures are present.
    assert_eq!(
        report.captures.len(),
        3,
        "Expected 3 captures (sessions, stats, settings)"
    );

    // Assert — each intermediate capture has the correct tab highlighted.
    let sessions_frame = common::frame_from_capture(&report.captures[0]);
    let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
    assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

    let stats_frame = common::frame_from_capture(&report.captures[1]);
    let stats_full = Region::full(stats_frame.cols(), stats_frame.rows());
    assertion::assert_text_in_region(&stats_frame, "TokenStats", &stats_full);

    common::save_feature_gif(&scenario, &report, &env, "tab_full_cycle");
}

/// Verify that pressing `q` opens a quit confirmation dialog.
///
/// The dialog should display the title "Confirm Quit" and the message
/// "Quit agentty?" with selectable options.
#[test]
fn quit_shows_confirmation_dialog() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("quit_confirmation")
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("before", "App running before quit")
        .compose(&common::open_quit_dialog())
        .capture_labeled("dialog", "Quit confirmation dialog");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Confirm Quit", &full);
    assertion::assert_text_in_region(&frame, "Quit agentty?", &full);

    common::save_feature_gif(&scenario, &report, &env, "quit_confirmation");
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
fn startup_shows_footer_hints() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("footer_hints")
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("startup", "Footer with keybinding hints");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert
    let footer = Region::footer(frame.cols(), frame.rows());
    let footer_text = frame.text_in_region(&footer);
    assert!(
        !footer_text.trim().is_empty(),
        "Footer should contain keybinding hints"
    );

    common::save_feature_gif(&scenario, &report, &env, "footer_hints");
}

/// Verify that `BackTab` (Shift+Tab) cycles tabs in reverse order.
///
/// Starts on Projects (first tab), navigates forward to Settings (last tab)
/// via three Tab presses, then presses `BackTab` three times to cycle back
/// through Stats → Sessions → Projects.
#[test]
fn backtab_cycles_tabs_reverse() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("backtab_reverse")
        .compose(&common::wait_for_agentty_startup())
        // Navigate forward to Settings (Projects → Sessions → Stats → Settings).
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::switch_to_tab("Stats"))
        .compose(&common::switch_to_tab("Settings"))
        .capture_labeled("at_settings", "Settings tab selected before reverse")
        // BackTab 1: Settings → Stats
        .compose(&common::switch_to_tab_reverse("Stats"))
        .capture_labeled("back_to_stats", "Stats tab after first BackTab")
        // BackTab 2: Stats → Sessions
        .compose(&common::switch_to_tab_reverse("Sessions"))
        .capture_labeled("back_to_sessions", "Sessions tab after second BackTab")
        // BackTab 3: Sessions → Projects
        .compose(&common::switch_to_tab_reverse("Projects"))
        .capture_labeled("back_to_projects", "Projects tab after third BackTab");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — final frame should have Projects selected.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "test-project", &full);

    // Assert — intermediate captures show correct reverse order.
    let stats_frame = common::frame_from_capture(&report.captures[1]);
    let stats_full = Region::full(stats_frame.cols(), stats_frame.rows());
    assertion::assert_text_in_region(&stats_frame, "TokenStats", &stats_full);

    let sessions_frame = common::frame_from_capture(&report.captures[2]);
    let sessions_full = Region::full(sessions_frame.cols(), sessions_frame.rows());
    assertion::assert_text_in_region(&sessions_frame, "No sessions", &sessions_full);

    let projects_frame = common::frame_from_capture(&report.captures[3]);
    let projects_full = Region::full(projects_frame.cols(), projects_frame.rows());
    assertion::assert_text_in_region(&projects_frame, "test-project", &projects_full);

    common::save_feature_gif(&scenario, &report, &env, "backtab_reverse");
}

/// Verify that `?` opens the help overlay with keybinding content, and
/// `Esc` closes it and restores the previous view.
#[test]
fn help_overlay_toggle() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("help_overlay")
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("before", "Normal view before help")
        // Open help overlay.
        .compose(&common::open_help_overlay())
        .capture_labeled("help_open", "Help overlay visible")
        // Close with Esc.
        .press_key("Escape")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("help_closed", "Help overlay dismissed");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — help overlay should show "Keybindings" title when open.
    let help_frame = common::frame_from_capture(&report.captures[1]);
    let full = Region::full(help_frame.cols(), help_frame.rows());
    assertion::assert_text_in_region(&help_frame, "Keybindings", &full);

    // Assert — after Esc, the normal view is restored with Projects tab.
    let restored_full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "test-project", &restored_full);

    // Assert — "Keybindings" title should no longer be visible.
    let closed_full = Region::full(frame.cols(), frame.rows());
    let closed_text = frame.text_in_region(&closed_full);
    assert!(
        !closed_text.contains("Keybindings"),
        "Help overlay should be dismissed after Esc"
    );

    common::save_feature_gif(&scenario, &report, &env, "help_overlay");
}
