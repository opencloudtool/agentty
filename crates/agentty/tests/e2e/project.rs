//! Projects page E2E tests: project listing with current working directory.

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::BuilderEnv;

/// Verify that the Projects tab lists the registered project name from
/// the temp workdir.
///
/// Agentty auto-registers the current working directory as a project on
/// startup. The test creates a `test-project` working directory and asserts
/// that the project name appears in the project list.
#[test]
fn projects_page_shows_cwd() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("projects_cwd")
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("projects", "Projects page with registered project");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — Projects tab is selected (default startup tab).
    let header = common::header_region(frame.cols());
    assertion::assert_span_is_highlighted(&frame, "Projects");
    assertion::assert_text_in_region(&frame, "Projects", &header);

    // Assert — the test-project directory name appears in the project list.
    // `BuilderEnv` creates a `test-project` workdir, which agentty
    // auto-registers on startup.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "test-project", &full);

    common::save_feature_gif(&scenario, &report, &env, "projects_page_shows_cwd");
}
