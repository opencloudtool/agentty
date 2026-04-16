//! Projects page E2E tests: project listing with current working directory.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

/// Verify that the Projects tab lists the registered project name from
/// the temp workdir.
///
/// Agentty auto-registers the current working directory as a project on
/// startup. The test creates a `test-project` working directory and asserts
/// that the project name appears in the project list.
#[test]
fn projects_page_shows_cwd() {
    // Arrange, Act, Assert
    FeatureTest::new("projects_cwd")
        .zola(
            "Project directory",
            "See and switch the active project directory.",
            90,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(3000)
                    .capture_labeled("projects", "Projects page with registered project")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agentty", &full);
                assertion::assert_text_in_region(frame, "test-project", &full);
            },
        );
}
