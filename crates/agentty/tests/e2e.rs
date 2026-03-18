//! VHS-based end-to-end tests for the agentty TUI.
//!
//! These tests launch the real `agentty` binary inside a VHS virtual
//! terminal, capture PNG screenshots, and compare them against stored
//! reference images using pixel-level tolerance.
//!
//! # Prerequisites
//!
//! - VHS must be installed: `brew install vhs`
//!
//! # Running
//!
//! ```sh
//! cargo test -p agentty --test e2e -- --ignored
//! ```
//!
//! # Updating reference screenshots
//!
//! ```sh
//! AGENTTY_E2E_UPDATE=1 cargo test -p agentty --test e2e -- --ignored
//! ```

mod e2e_support;

use e2e_support::harness;

/// Verify that agentty startup renders the Projects tab.
///
/// Launches agentty in a clean environment (empty database,
/// deterministic working directory named `test-project`) and captures
/// a VHS screenshot after the TUI renders. Compares the screenshot
/// against the stored reference using pixel-level tolerance.
///
/// Requires VHS installed. Run with `--ignored` flag.
#[test]
#[ignore = "requires VHS installed — run with: cargo test --test e2e -- --ignored"]
fn startup_shows_projects_tab() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let test = harness::VhsTest::new()?;

    // Act
    let screenshot = test.run_and_screenshot()?;

    // Assert
    harness::assert_screenshot_matches(&screenshot, "startup_projects_tab")?;

    Ok(())
}
