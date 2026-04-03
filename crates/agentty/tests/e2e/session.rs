//! Session list E2E tests: empty-state rendering.

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::BuilderEnv;

/// Verify that the Sessions tab shows an empty-state message when no
/// sessions exist.
///
/// Starts agentty in a fresh temp directory (no database, no sessions),
/// switches to the Sessions tab, and asserts that the placeholder text
/// is visible.
#[test]
fn session_list_empty_state() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("session_empty")
        .compose(&common::wait_for_agentty_startup())
        // Navigate to Sessions tab.
        .compose(&common::switch_to_tab("Sessions"))
        .capture_labeled("sessions_tab", "Sessions tab with no sessions");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — Sessions tab is selected.
    let header = common::header_region(frame.cols());
    assertion::assert_span_is_highlighted(&frame, "Sessions");
    assertion::assert_text_in_region(&frame, "Sessions", &header);

    // Assert — empty-state placeholder is visible.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(&scenario, &report, &env, "session_list_empty_state");
}
