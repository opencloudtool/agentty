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

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin;
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::proof::gif::GifBackend;
use testty::proof::report::{ProofCapture, ProofReport};
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;

/// Save a proof report as an animated GIF when `TESTTY_PROOF_OUTPUT` is set.
///
/// When the environment variable `TESTTY_PROOF_OUTPUT` points to a directory,
/// the GIF is written there. Otherwise the call is a no-op, keeping the
/// workspace clean during normal test runs.
fn save_proof_gif(report: &ProofReport, name: &str) {
    let Some(dir) = proof_output_dir() else {
        return;
    };

    let path = dir.join(format!("{name}.gif"));
    report
        .save(&GifBackend::new(), &path)
        .expect("failed to save proof GIF");

    println!("Proof GIF saved to {}", path.display());
}

/// Return the proof output directory from `TESTTY_PROOF_OUTPUT`, creating it
/// if needed. Returns `None` when the variable is unset.
fn proof_output_dir() -> Option<PathBuf> {
    let dir = std::env::var("TESTTY_PROOF_OUTPUT").ok()?;
    let path = Path::new(&dir).to_path_buf();
    std::fs::create_dir_all(&path).expect("failed to create proof output dir");

    Some(path)
}

/// Reconstruct a `TerminalFrame` from a `ProofCapture` so full cell-level
/// assertions (highlight, color, style) can be run against intermediate
/// captures.
fn frame_from_capture(capture: &ProofCapture) -> TerminalFrame {
    TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes)
}

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
        .capture_labeled("startup", "Initial render with Projects tab");

    // Act
    let (frame, report) = scenario
        .run_with_proof(builder)
        .expect("scenario execution failed");

    // Assert
    let header = header_region(frame.cols());
    assertion::assert_text_in_region(&frame, "Projects", &header);
    assertion::assert_span_is_highlighted(&frame, "Projects");

    save_proof_gif(&report, "startup_shows_projects_tab");
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
        .capture_labeled("before", "Projects tab selected")
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("after", "Sessions tab selected");

    // Act
    let (frame, report) = scenario
        .run_with_proof(builder)
        .expect("scenario execution failed");

    // Assert — after pressing Tab, "Sessions" should be selected.
    let header = header_region(frame.cols());
    assertion::assert_text_in_region(&frame, "Sessions", &header);
    assertion::assert_span_is_highlighted(&frame, "Sessions");
    assertion::assert_text_in_region(&frame, "Projects", &header);
    assertion::assert_span_is_not_highlighted(&frame, "Projects");

    save_proof_gif(&report, "tab_key_switches_tabs");
}

/// Verify that pressing Tab cycles through all four tabs in order.
///
/// Starts on Projects, presses Tab three times, and asserts each
/// successive tab becomes selected: Sessions → Stats → Settings.
#[test]
fn tab_cycles_through_all_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("tab_full_cycle")
        .wait_for_stable_frame(500, 10000)
        // Tab 1: Projects → Sessions
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("sessions", "Sessions tab selected")
        // Tab 2: Sessions → Stats
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("stats", "Stats tab selected")
        // Tab 3: Stats → Settings
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("settings", "Settings tab selected");

    // Act
    let (frame, report) = scenario
        .run_with_proof(builder)
        .expect("scenario execution failed");

    // Assert — final frame should have Settings selected.
    let header = header_region(frame.cols());
    assertion::assert_text_in_region(&frame, "Settings", &header);
    assertion::assert_span_is_highlighted(&frame, "Settings");

    // Assert — all three intermediate captures are present.
    assert_eq!(
        report.captures.len(),
        3,
        "Expected 3 captures (sessions, stats, settings)"
    );

    // Assert — each intermediate capture has the correct tab highlighted.
    let sessions_frame = frame_from_capture(&report.captures[0]);
    assertion::assert_span_is_highlighted(&sessions_frame, "Sessions");
    assertion::assert_span_is_not_highlighted(&sessions_frame, "Projects");

    let stats_frame = frame_from_capture(&report.captures[1]);
    assertion::assert_span_is_highlighted(&stats_frame, "Stats");
    assertion::assert_span_is_not_highlighted(&stats_frame, "Sessions");

    save_proof_gif(&report, "tab_cycles_through_all_tabs");
}

/// Verify that pressing `q` opens a quit confirmation dialog.
///
/// The dialog should display the title "Confirm Quit" and the message
/// "Quit agentty?" with selectable options.
#[test]
fn quit_shows_confirmation_dialog() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("quit_confirmation")
        .wait_for_stable_frame(500, 10000)
        .capture_labeled("before", "App running before quit")
        .press_key("q")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("dialog", "Quit confirmation dialog");

    // Act
    let (frame, report) = scenario
        .run_with_proof(builder)
        .expect("scenario execution failed");

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Confirm Quit", &full);
    assertion::assert_text_in_region(&frame, "Quit agentty?", &full);

    save_proof_gif(&report, "quit_shows_confirmation_dialog");
}

/// Verify that the footer shows keybinding hints on startup.
#[test]
fn startup_shows_footer_hints() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let builder = test_builder(temp.path()).expect("failed to create test builder");

    let scenario = Scenario::new("footer_hints")
        .wait_for_stable_frame(500, 10000)
        .capture_labeled("startup", "Footer with keybinding hints");

    // Act
    let (frame, report) = scenario
        .run_with_proof(builder)
        .expect("scenario execution failed");

    // Assert
    let footer = Region::footer(frame.cols(), frame.rows());
    let footer_text = frame.text_in_region(&footer);
    assert!(
        !footer_text.trim().is_empty(),
        "Footer should contain keybinding hints"
    );

    save_proof_gif(&report, "startup_shows_footer_hints");
}
