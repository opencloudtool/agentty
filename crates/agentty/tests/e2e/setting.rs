//! Settings page E2E tests: content rendering, navigation, and editing.

use agentty::db::{DB_DIR, DB_FILE, Database};
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Seeds deterministic selector values for the settings navigation test.
///
/// Persists the three model selectors to `claude-opus-4-6` so the test can
/// verify row navigation by observing stable, provider-agnostic value
/// transitions even when only the stub Claude executable is available.
fn seed_settings_navigation_models(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .upsert_project(&canonical_workdir.to_string_lossy(), None)
            .await?;

        for setting_name in [
            "DefaultSmartModel",
            "DefaultFastModel",
            "DefaultReviewModel",
        ] {
            sqlx::query(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
            )
            .bind(project_id)
            .bind(setting_name)
            .bind("claude-opus-4-6")
            .execute(database.pool())
            .await?;
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

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
        )
        .expect("feature test failed");
}

/// Verify that `j` and `k` navigate through settings rows.
///
/// Opens the Settings tab and presses `j` multiple times to move the
/// selection down, then `k` to move back up. The test confirms the
/// selected row by seeding deterministic model values and observing which
/// selector value changes after each `Enter` press.
#[test]
fn settings_jk_navigation() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_navigation")
        .setup(seed_settings_navigation_models)
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
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("moved_down", "Selection moved down two rows")
                    .press_key("k")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
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
                assertion::assert_match_count(&initial_frame, "claude-opus-4-6", 3);
                assertion::assert_match_count(&initial_frame, "claude-sonnet-4-6", 0);

                let moved_down_frame = common::frame_from_capture(&report.captures[1]);
                assertion::assert_match_count(&moved_down_frame, "claude-opus-4-6", 2);
                assertion::assert_match_count(&moved_down_frame, "claude-sonnet-4-6", 1);

                let moved_up_frame = common::frame_from_capture(&report.captures[2]);
                assertion::assert_match_count(&moved_up_frame, "claude-opus-4-6", 1);
                assertion::assert_match_count(&moved_up_frame, "claude-sonnet-4-6", 2);
            },
        )
        .expect("feature test failed");
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
        )
        .expect("feature test failed");
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
        )
        .expect("feature test failed");
}
