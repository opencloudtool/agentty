//! TUI end-to-end tests for the agentty binary.
//!
//! Uses the `testty` framework to drive the real binary in a PTY,
//! capture terminal frames for semantic assertions, and optionally generate
//! VHS tapes for visual screenshot capture.
//!
//! # Running
//!
//! ```sh
//! cargo test -p agentty --test e2e -- --ignored
//! ```
//!
//! # Updating baselines
//!
//! ```sh
//! TUI_TEST_UPDATE=1 cargo test -p agentty --test e2e -- --ignored
//! ```

use std::path::PathBuf;

use testty::recipe;
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;
use testty::snapshot::{self, SnapshotConfig};

/// Return the path to the compiled agentty binary.
fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agentty"))
}

/// Build a [`SnapshotConfig`] with baselines committed alongside the tests
/// and failure artifacts written under the workspace `target/` directory.
fn snapshot_config() -> SnapshotConfig {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let baselines_dir = manifest_dir.join("tests").join("e2e_baselines");
    let artifacts_dir = manifest_dir.join("tests").join("e2e_artifacts");

    SnapshotConfig::new(baselines_dir, artifacts_dir)
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

    Ok(PtySessionBuilder::new(binary_path())
        .size(80, 24)
        .env("AGENTTY_ROOT", agentty_root.to_string_lossy())
        .workdir(workdir))
}

/// Verify that agentty startup renders the Projects tab as selected.
///
/// Launches agentty in a clean environment and asserts that the expected
/// tabs and labels appear in the correct regions with appropriate styling.
#[test]
#[ignore = "requires agentty binary — run with: cargo test --test e2e -- --ignored"]
fn startup_shows_projects_tab() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("startup")
        .wait_for_stable_frame(500, 5000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario execution failed");

    // Assert
    recipe::expect_selected_tab(&frame, "Projects");
    snapshot::assert_frame_snapshot_matches(
        &snapshot_config(),
        "startup_projects_tab",
        &frame.all_text(),
    )
    .expect("frame snapshot should match baseline");
}

/// Verify that Tab key switches between tabs.
///
/// Starts on Projects tab, presses Tab, and verifies the next tab
/// becomes selected while Projects becomes unselected.
#[test]
#[ignore = "requires agentty binary — run with: cargo test --test e2e -- --ignored"]
fn tab_key_switches_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("tab_switch")
        .wait_for_stable_frame(500, 5000)
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario execution failed");

    // Assert — after pressing Tab, "Sessions" should be selected.
    recipe::expect_selected_tab(&frame, "Sessions");
    recipe::expect_unselected_tab(&frame, "Projects");
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
#[ignore = "requires agentty binary — run with: cargo test --test e2e -- --ignored"]
fn startup_shows_footer_hints() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("footer_hints")
        .wait_for_stable_frame(500, 5000)
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
