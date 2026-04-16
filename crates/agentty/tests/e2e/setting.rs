//! Settings page E2E tests: content rendering, navigation, and editing.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

/// Verify that the Settings tab renders all setting rows with labels.
///
/// Navigates to the Settings tab and asserts that the settings table
/// appears with expected row labels including "Reasoning Level" and
/// "Open Commands".
#[test]
fn settings_tab_shows_content() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_content")
        .zola(
            "Settings tab",
            "View and configure agent settings like reasoning level and models.",
            150,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(3000)
                    .capture_labeled("settings_tab", "Settings tab with all rows")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Settings", &full);
                assertion::assert_text_in_region(frame, "Default Reasoning Level", &full);
                assertion::assert_text_in_region(frame, "Default Smart Model", &full);
                assertion::assert_text_in_region(frame, "Open Commands", &full);
            },
        );
}

/// Verify that `j` and `k` navigate through settings rows.
///
/// Opens the Settings tab and presses `j` multiple times to move the
/// selection highlight down, then `k` to move back up. Captures
/// intermediate states to show the navigation in the GIF.
#[test]
fn settings_jk_navigation() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_navigation")
        .zola(
            "Settings navigation",
            "Navigate settings rows with j/k keys.",
            152,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("initial", "Settings tab at first row")
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("moved_down", "Selection moved down two rows")
                    .press_key("k")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("moved_up", "Selection moved back up one row")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Settings", &full);

                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (initial, moved_down, moved_up)"
                );

                let initial_frame = common::frame_from_capture(&report.captures[0]);
                assertion::assert_span_is_highlighted(&initial_frame, "Default Reasoning Level");

                let moved_down_frame = common::frame_from_capture(&report.captures[1]);
                assertion::assert_span_is_highlighted(&moved_down_frame, "Default Fast Model");
                assertion::assert_span_is_not_highlighted(
                    &moved_down_frame,
                    "Default Reasoning Level",
                );

                let moved_up_frame = common::frame_from_capture(&report.captures[2]);
                assertion::assert_span_is_highlighted(&moved_up_frame, "Default Smart Model");
                assertion::assert_span_is_not_highlighted(&moved_up_frame, "Default Fast Model");
            },
        );
}

/// Verify that `Enter` cycles a selector setting value.
///
/// Opens the Settings tab and presses `Enter` on the first row
/// ("Reasoning Level") to cycle its value. Captures before and after
/// to show the change in the GIF.
#[test]
fn settings_enter_cycles_value() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_edit")
        .zola(
            "Settings editing",
            "Press Enter to cycle setting values or edit text fields.",
            154,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("before_edit", "Reasoning Level before cycling")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("after_edit", "Reasoning Level after cycling")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Default Reasoning Level", &full);

                assert_eq!(
                    report.captures.len(),
                    2,
                    "Expected 2 captures (before and after cycling)"
                );

                let before_frame = common::frame_from_capture(&report.captures[0]);
                let before_full = Region::full(before_frame.cols(), before_frame.rows());
                assertion::assert_text_in_region(&before_frame, "high", &before_full);

                let after_frame = common::frame_from_capture(&report.captures[1]);
                let after_full = Region::full(after_frame.cols(), after_frame.rows());
                assertion::assert_text_in_region(&after_frame, "xhigh", &after_full);
            },
        );
}

/// Verify that the help overlay on the Settings tab shows settings-specific
/// keybinding hints.
#[test]
fn settings_help_shows_edit_hint() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_help")
        .zola(
            "Settings help",
            "Press ? on the Settings tab to see settings-specific keybindings.",
            156,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .compose(&common::open_help_overlay())
                    .viewing_pause_ms(3000)
                    .capture_labeled("settings_help", "Help overlay on Settings tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Keybindings", &full);
                assertion::assert_text_in_region(frame, "edit", &full);
            },
        );
}
