//! Confirmation dialog E2E tests: quit-confirm-yes exits, quit-confirm
//! dismiss via `n` and `Esc` returns to the app.

use std::time::Duration;

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Verify that confirming quit with `y` causes the process to exit with
/// code 0.
///
/// Opens the quit dialog, presses `y`, and asserts that the PTY child
/// process terminates successfully.
#[test]
fn quit_confirm_yes_exits() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    let mut session = env.builder().spawn().expect("failed to spawn session");

    let scenario = Scenario::new("quit_yes")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::open_quit_dialog())
        .press_key("y")
        .sleep(Duration::from_millis(500));

    // Act
    scenario
        .execute_in_pty(&mut session)
        .expect("scenario execution failed");

    // Assert — process should exit with code 0.
    let exited_successfully = session
        .wait_for_exit(Duration::from_secs(5))
        .expect("process did not exit within timeout");

    assert!(exited_successfully, "Process should exit with code 0");
}

/// Verify that dismissing the quit dialog with `n` and `Esc` returns to
/// the normal app view.
///
/// Opens the quit dialog twice: first dismisses with `n`, then with `Esc`.
/// After each dismissal, asserts that the app is back on the Projects tab.
#[test]
fn quit_confirm_dismiss_returns() {
    // Arrange, Act, Assert
    FeatureTest::new("quit_dismiss")
        .zola(
            "Quit dismiss",
            "Dismiss the quit dialog to stay in the session.",
            140,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::open_quit_dialog())
                    .viewing_pause_ms(2500)
                    .capture_labeled("dialog_n", "Quit dialog before n")
                    .press_key("n")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("after_n", "App restored after n")
                    .compose(&common::open_quit_dialog())
                    .viewing_pause_ms(2500)
                    .capture_labeled("dialog_esc", "Quit dialog before Esc")
                    .press_key("Escape")
                    .wait_for_stable_frame(300, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("after_esc", "App restored after Esc")
            },
            |frame, report| {
                let dialog_n_frame = common::frame_from_capture(&report.captures[0]);
                let full = Region::full(dialog_n_frame.cols(), dialog_n_frame.rows());
                assertion::assert_text_in_region(&dialog_n_frame, "Confirm Quit", &full);

                let dialog_esc_frame = common::frame_from_capture(&report.captures[2]);
                let full_esc = Region::full(dialog_esc_frame.cols(), dialog_esc_frame.rows());
                assertion::assert_text_in_region(&dialog_esc_frame, "Confirm Quit", &full_esc);

                let after_n_frame = common::frame_from_capture(&report.captures[1]);
                let restored_full = Region::full(after_n_frame.cols(), after_n_frame.rows());
                assertion::assert_text_in_region(&after_n_frame, "test-project", &restored_full);

                let after_n_text = after_n_frame.text_in_region(&restored_full);
                assert!(
                    !after_n_text.contains("Confirm Quit"),
                    "Quit dialog should be dismissed after 'n'"
                );

                let final_full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test-project", &final_full);

                let final_text = frame.text_in_region(&final_full);
                assert!(
                    !final_text.contains("Confirm Quit"),
                    "Quit dialog should be dismissed after Esc"
                );
            },
        );
}
