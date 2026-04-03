//! Shared helpers and agentty-specific `Journey` builders for E2E tests.
//!
//! Provides [`BuilderEnv`] for isolated test environments and
//! [`save_feature_gif`] for generating high-quality VHS feature GIFs with
//! frame-bytes content hashing to skip regeneration when output is unchanged.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use testty::frame::TerminalFrame;
use testty::journey::Journey;
use testty::proof::report::{ProofCapture, ProofReport};
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;
use testty::step::Step;
use testty::vhs::{VhsTape, VhsTapeSettings, check_vhs_installed};

/// Isolated test environment carrying `agentty_root` and `workdir` paths.
///
/// Use [`BuilderEnv::new`] to create a fresh environment under a temporary
/// directory, [`BuilderEnv::builder`] to get a configured
/// [`PtySessionBuilder`], and [`BuilderEnv::as_vhs_env_pairs`] to export the
/// environment for VHS tape compilation.
pub(crate) struct BuilderEnv {
    /// Path used as `AGENTTY_ROOT` for database and session isolation.
    pub(crate) agentty_root: PathBuf,
    /// Deterministic working directory registered as a project on startup.
    pub(crate) workdir: PathBuf,
}

impl BuilderEnv {
    /// Create a new isolated environment under `temp_root`.
    ///
    /// Creates `agentty_root` and `test-project` subdirectories so each test
    /// gets a fresh database and deterministic project name.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub(crate) fn new(temp_root: &Path) -> std::io::Result<Self> {
        let agentty_root = temp_root.join("agentty_root");
        let workdir = temp_root.join("test-project");

        std::fs::create_dir_all(&agentty_root)?;
        std::fs::create_dir_all(&workdir)?;

        Ok(Self {
            agentty_root,
            workdir,
        })
    }

    /// Return a configured [`PtySessionBuilder`] using this environment.
    ///
    /// Sets `AGENTTY_ROOT`, working directory, and 80×24 terminal size.
    pub(crate) fn builder(&self) -> PtySessionBuilder {
        PtySessionBuilder::new(cargo_bin("agentty"))
            .size(80, 24)
            .env("AGENTTY_ROOT", self.agentty_root.to_string_lossy())
            .workdir(&self.workdir)
    }

    /// Return environment variable pairs for VHS tape compilation.
    ///
    /// These match the variables set by [`BuilderEnv::builder`] so the VHS
    /// recording reproduces the same environment as the PTY session.
    pub(crate) fn as_vhs_env_pairs(&self) -> Vec<(String, String)> {
        vec![(
            "AGENTTY_ROOT".to_string(),
            self.agentty_root.to_string_lossy().into_owned(),
        )]
    }
}

/// Return the feature GIF output directory, creating it if needed.
///
/// Derives the path from `CARGO_MANIFEST_DIR` →
/// `../../docs/site/static/features/`.
fn feature_output_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let output_dir = Path::new(manifest_dir).join("../../docs/site/static/features");

    std::fs::create_dir_all(&output_dir).expect("failed to create feature output dir");

    output_dir
}

/// Compute a content hash from all proof capture frame bytes.
///
/// Uses [`DefaultHasher`] to produce a deterministic u64 hash from the
/// concatenated frame bytes of every capture in the report.
fn compute_frame_hash(report: &ProofReport) -> u64 {
    let mut hasher = DefaultHasher::new();

    for capture in &report.captures {
        capture.frame_bytes.hash(&mut hasher);
    }

    hasher.finish()
}

/// Generate a high-quality VHS feature GIF with frame-bytes content caching.
///
/// Checks VHS availability, computes a content hash from all
/// [`ProofReport`] frame bytes, and skips VHS execution when the hash
/// matches a `.{name}.hash` sidecar file. Uses
/// [`VhsTapeSettings::feature_demo()`] for 3200×1600 `OneDark` recordings.
///
/// Gracefully skips when VHS is not installed.
pub(crate) fn save_feature_gif(
    scenario: &Scenario,
    report: &ProofReport,
    env: &BuilderEnv,
    name: &str,
) {
    // Graceful skip when VHS is not installed.
    if check_vhs_installed().is_err() {
        println!("VHS not installed — skipping feature GIF for {name}");

        return;
    }

    let output_dir = feature_output_dir();
    let hash_path = output_dir.join(format!(".{name}.hash"));
    let gif_path = output_dir.join(format!("{name}.gif"));

    // Content hash from frame bytes.
    let current_hash = compute_frame_hash(report);
    let hash_string = current_hash.to_string();

    // Check sidecar cache — skip VHS when the GIF already matches.
    if gif_path.exists()
        && let Ok(cached) = std::fs::read_to_string(&hash_path)
        && cached.trim() == hash_string
    {
        println!("Feature GIF unchanged — skipping {name}");

        return;
    }

    // Build VHS tape with feature demo settings.
    let screenshot_path = output_dir.join(format!("{name}.png"));
    let owned_pairs = env.as_vhs_env_pairs();
    let env_pairs: Vec<(&str, &str)> = owned_pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();

    let tape = VhsTape::from_scenario_with_settings(
        scenario,
        &cargo_bin("agentty"),
        &screenshot_path,
        &env_pairs,
        &VhsTapeSettings::feature_demo(),
    );

    // Write and execute the tape.
    let tape_path = output_dir.join(format!("{name}.tape"));
    match tape.execute(&tape_path) {
        Ok(_) => {
            // Update the sidecar hash on success.
            std::fs::write(&hash_path, &hash_string).expect("failed to write hash sidecar");

            // Clean up the tape file and screenshot.
            let _ = std::fs::remove_file(&tape_path);
            let _ = std::fs::remove_file(&screenshot_path);

            println!("Feature GIF saved to {}", gif_path.display());
        }
        Err(error) => {
            // Clean up on failure.
            let _ = std::fs::remove_file(&tape_path);
            println!("VHS execution failed for {name}: {error}");
        }
    }
}

/// Reconstruct a [`TerminalFrame`] from a [`ProofCapture`] so full cell-level
/// assertions (highlight, color, style) can be run against intermediate
/// captures.
pub(crate) fn frame_from_capture(capture: &ProofCapture) -> TerminalFrame {
    TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes)
}

/// Header region covering the title bar and tab bar (rows 0-3).
///
/// Agentty renders a title bar at row 0 and a tab bar at row 2. This region
/// is wider than a single row so tab-related assertions match the actual
/// layout.
pub(crate) fn header_region(cols: u16) -> Region {
    Region::new(0, 0, cols, 4)
}

impl BuilderEnv {
    /// Initialize a git repository in the workdir so sessions can create
    /// worktrees.
    ///
    /// Sets up a `main` branch with an empty initial commit and minimal git
    /// config for the test environment.
    ///
    /// # Errors
    ///
    /// Returns an error if any git command fails.
    pub(crate) fn init_git(&self) -> std::io::Result<()> {
        let run = |args: &[&str]| -> std::io::Result<()> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.workdir)
                .output()?;
            if !output.status.success() {
                return Err(std::io::Error::other(format!(
                    "git {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            Ok(())
        };
        run(&["init", "-b", "main"])?;
        run(&["config", "user.email", "test@test.com"])?;
        run(&["config", "user.name", "Test"])?;
        run(&["commit", "--allow-empty", "-m", "init"])
    }
}

// ---------------------------------------------------------------------------
// Agentty-specific Journey builders
// ---------------------------------------------------------------------------

/// Wait for agentty to start up and render a stable initial frame.
///
/// Uses the standard startup timeouts: 500ms stability window with a
/// 10-second maximum wait.
pub(crate) fn wait_for_agentty_startup() -> Journey {
    Journey::new("agentty_startup")
        .with_description("Wait for agentty startup and initial render")
        .step(Step::wait_for_stable_frame(500, 10000))
}

/// Switch to a tab by pressing `Tab` and waiting for the tab label text.
///
/// Useful for navigating forward through the tab bar one step at a time.
pub(crate) fn switch_to_tab(tab_name: &str) -> Journey {
    Journey::new(format!("switch_to_{tab_name}"))
        .with_description(format!("Press Tab and wait for '{tab_name}' to appear"))
        .step(Step::press_key("Tab"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Switch to a tab by pressing `BackTab` and waiting for stability.
///
/// Useful for navigating backward through the tab bar one step at a time.
pub(crate) fn switch_to_tab_reverse(tab_name: &str) -> Journey {
    Journey::new(format!("switch_back_to_{tab_name}"))
        .with_description(format!("Press BackTab and wait for '{tab_name}' to appear"))
        .step(Step::press_key("BackTab"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Open the quit confirmation dialog by pressing `q`.
///
/// Waits for the dialog to render with a stable frame.
pub(crate) fn open_quit_dialog() -> Journey {
    Journey::new("open_quit_dialog")
        .with_description("Press q and wait for quit confirmation dialog")
        .step(Step::press_key("q"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Open the help overlay by pressing `?`.
///
/// Waits for the overlay to render with a stable frame.
pub(crate) fn open_help_overlay() -> Journey {
    Journey::new("open_help_overlay")
        .with_description("Press ? and wait for help overlay")
        .step(Step::press_key("?"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Create a session with a prompt, submit it, and return to the Sessions
/// list.
///
/// Presses `a` to create a non-draft session, types `"test"`, submits with
/// `Enter` (which starts the agent asynchronously while the session
/// persists), and presses `q` from the session view to return to the list.
///
/// Uses fixed sleeps instead of stable-frame waits because the agent may
/// produce continuous output after submit.
///
/// Requires the Sessions tab to be active and a git-initialized workdir.
pub(crate) fn create_session_and_return_to_list() -> Journey {
    Journey::new("create_session")
        .with_description("Create session via a, type test, submit, return to list")
        .step(Step::press_key("a"))
        .step(Step::wait_for_stable_frame(300, 5000))
        .step(Step::write_text("test"))
        .step(Step::wait_for_text("test", 3000))
        .step(Step::press_key("Enter"))
        .step(Step::sleep(Duration::from_secs(2)))
        .step(Step::press_key("q"))
        .step(Step::sleep(Duration::from_secs(1)))
}
