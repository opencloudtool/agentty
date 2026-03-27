//! TUI end-to-end tests for the agentty binary.
//!
//! Uses the `testty` framework to drive the real binary in a PTY and
//! capture terminal frames for semantic assertions.
//!
//! # Running
//!
//! ```sh
//! cargo test -p agentty --test e2e
//! ```

use assert_cmd::cargo::cargo_bin;
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;

/// Header region covering the title bar and tab bar (rows 0-3).
///
/// Agentty renders a title bar at row 0 and a tab bar at row 2. This region
/// is wider than a single row so tab-related assertions match the actual
/// layout.
fn header_region(cols: u16) -> Region {
    Region::new(0, 0, cols, 4)
}

/// Create a `PtySessionBuilder` with a clean isolated environment.
///
/// Sets `AGENTTY_ROOT` to a temporary directory, creates a deterministic
/// `test-project` working directory so frame snapshots are stable across
/// machines, and uses 80x24 terminal.
///
/// # Errors
///
/// Returns an error if directory creation fails.
fn test_builder(temp_root: &std::path::Path) -> std::io::Result<PtySessionBuilder> {
    let agentty_root = temp_root.join("agentty_root");
    let workdir = temp_root.join("test-project");

    std::fs::create_dir_all(&agentty_root)?;
    std::fs::create_dir_all(&workdir)?;

    Ok(PtySessionBuilder::new(cargo_bin("agentty"))
        .size(80, 24)
        .env("AGENTTY_ROOT", agentty_root.to_string_lossy())
        .workdir(workdir))
}

/// Verify that agentty startup renders the Projects tab as selected.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
fn startup_shows_projects_tab() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("startup")
        .wait_for_stable_frame(500, 10000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario execution failed");

    // Assert
    let header = header_region(frame.cols());
    assertion::assert_text_in_region(&frame, "Projects", &header);
    assertion::assert_span_is_highlighted(&frame, "Projects");
}

/// Verify that Tab key switches between tabs.
///
/// Starts on Projects tab, presses Tab, and verifies the next tab
/// becomes selected while Projects becomes unselected.
#[test]
fn tab_key_switches_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("tab_switch")
        .wait_for_stable_frame(500, 10000)
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario execution failed");

    // Assert — after pressing Tab, "Sessions" should be selected.
    let header = header_region(frame.cols());
    assertion::assert_text_in_region(&frame, "Sessions", &header);
    assertion::assert_span_is_highlighted(&frame, "Sessions");
    assertion::assert_text_in_region(&frame, "Projects", &header);
    assertion::assert_span_is_not_highlighted(&frame, "Projects");
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
fn startup_shows_footer_hints() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("footer_hints")
        .wait_for_stable_frame(500, 10000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario execution failed");

    // Assert
    let footer = Region::footer(frame.cols(), frame.rows());
    let footer_text = frame.text_in_region(&footer);
    assert!(
        !footer_text.trim().is_empty(),
        "Footer should contain keybinding hints"
    );
}
