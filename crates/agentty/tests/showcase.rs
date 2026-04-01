//! Website feature GIF generation using VHS.
//!
//! These tests produce polished animated GIFs for the agentty website
//! (`docs/site/`) using the [VHS](https://github.com/charmbracelet/vhs)
//! terminal recorder. VHS records a real terminal session with proper
//! fonts, colors, and rendering fidelity. Output is rendered at high
//! resolution (3200x1600) for sharp display when downscaled in the
//! browser.
//!
//! The database is pre-seeded with realistic projects and sessions so the
//! UI looks populated and compelling.
//!
//! Each test corresponds to a feature demo on the website. When a test
//! changes, the corresponding GIF must be regenerated.
//!
//! # Prerequisites
//!
//! ```sh
//! brew install vhs
//! ```
//!
//! # Running
//!
//! Tests are marked `#[ignore]` so they do not run during regular
//! `cargo test`. Use `--ignored` to opt in:
//!
//! ```sh
//! cargo test -p agentty --test showcase -- --test-threads=1 --ignored --nocapture
//! ```
//!
//! Override output directory:
//!
//! ```sh
//! FEATURE_OUTPUT=/path/to/dir \
//!     cargo test -p agentty --test showcase -- --test-threads=1 --ignored --nocapture
//! ```

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use agentty::db::{DB_DIR, DB_FILE, Database};
use assert_cmd::cargo::cargo_bin;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Default feature output directory derived from the crate manifest location.
fn default_output_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    manifest_dir.join("../../docs/site/static/features")
}

/// Return the feature output directory, creating it if needed.
///
/// Uses `FEATURE_OUTPUT` if set, otherwise defaults to
/// `docs/site/static/features` relative to the workspace root.
fn feature_output_dir() -> PathBuf {
    let path = std::env::var("FEATURE_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_output_dir());
    std::fs::create_dir_all(&path).expect("failed to create feature output dir");

    path.canonicalize()
        .expect("failed to canonicalize output dir")
}

/// Build a VHS tape that launches agentty with the given environment.
///
/// The tape exports `AGENTTY_ROOT` and `HOME` in a hidden preamble,
/// changes to `workdir`, launches the binary, executes the provided VHS
/// `steps` block, then quits. Output is rendered at high resolution
/// (3200x1600, font size 36) for sharp downscaling in the browser.
fn build_tape(
    output_path: &Path,
    binary_path: &Path,
    agentty_root: &Path,
    workdir: &Path,
    steps: &str,
) -> String {
    let mut tape = String::new();

    // Header settings — high resolution for sharp downscaling.
    let _ = writeln!(tape, "Set Shell \"bash\"");
    let _ = writeln!(tape, "Set FontSize 36");
    let _ = writeln!(tape, "Set Width 3200");
    let _ = writeln!(tape, "Set Height 1600");
    let _ = writeln!(tape, "Set Padding 0");
    let _ = writeln!(tape, "Set Framerate 30");
    let _ = writeln!(tape, "Set TypingSpeed 0");
    let _ = writeln!(tape, "Set Theme \"OneDark\"");
    let _ = writeln!(tape);
    let _ = writeln!(tape, "Output \"{}\"", output_path.display());
    let _ = writeln!(tape);

    // Hidden environment setup.
    let _ = writeln!(tape, "Hide");
    let _ = writeln!(
        tape,
        "Type \"export AGENTTY_ROOT='{}'\"",
        agentty_root.display()
    );
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"export HOME='{}'\"", agentty_root.display());
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"cd '{}'\"", workdir.display());
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"clear\"");
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Show");
    let _ = writeln!(tape);

    // Launch the binary.
    let _ = writeln!(tape, "Type \"'{}'\"", binary_path.display());
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape);

    // Scenario steps.
    let _ = write!(tape, "{steps}");
    let _ = writeln!(tape);

    // Hidden teardown.
    let _ = writeln!(tape, "Hide");
    let _ = writeln!(tape, "Type \"q\"");
    let _ = writeln!(tape, "Sleep 1s");

    tape
}

/// Execute a VHS tape and verify the output was produced.
///
/// Writes the tape to a temporary file, runs `vhs`, and asserts the
/// output file exists at `output_path` on completion.
fn execute_tape(tape_content: &str, name: &str, output_path: &Path) {
    let tape_path = std::env::temp_dir().join(format!("{name}.tape"));
    std::fs::write(&tape_path, tape_content).expect("failed to write tape file");

    println!("VHS tape written to {}", tape_path.display());

    let output = Command::new("vhs")
        .arg(&tape_path)
        .output()
        .expect("failed to run vhs — is it installed? (brew install vhs)");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("VHS execution failed: {stderr}");
    }

    assert!(
        output_path.exists(),
        "VHS did not produce output at {}",
        output_path.display()
    );

    println!("Feature GIF saved to {}", output_path.display());
}

/// Seed the database with realistic projects and sessions.
///
/// Opens the SQLite database, runs migrations, and inserts sample data
/// that makes the UI look populated and visually compelling.
async fn seed_database(agentty_root: &Path, workdir: &Path) {
    let db_path = agentty_root.join(DB_DIR).join(DB_FILE);
    let database = Database::open(&db_path)
        .await
        .expect("failed to open database");

    // Register the project (mirrors what agentty does on startup).
    let project_id = database
        .upsert_project(&workdir.to_string_lossy(), Some("main"))
        .await
        .expect("failed to upsert project");

    // Touch last-opened so the project sorts to the top.
    database
        .touch_project_last_opened(project_id)
        .await
        .expect("failed to touch project");

    // Session data: (id, model, status, title, size).
    let sessions = [
        (
            "a1b2c3d4-0001",
            "claude-opus-4-6",
            "Review",
            "Fix auth token refresh on session expiry",
            "M",
        ),
        (
            "a1b2c3d4-0002",
            "claude-sonnet-4-6",
            "InProgress",
            "Add dark mode toggle to settings page",
            "L",
        ),
        (
            "a1b2c3d4-0003",
            "gemini-3-flash-preview",
            "Review",
            "Optimize database query performance",
            "S",
        ),
        (
            "a1b2c3d4-0004",
            "claude-opus-4-6",
            "Question",
            "Refactor API error handling",
            "M",
        ),
        (
            "a1b2c3d4-0005",
            "claude-sonnet-4-6",
            "Done",
            "Update README with new install steps",
            "XS",
        ),
        (
            "a1b2c3d4-0006",
            "gemini-3-flash-preview",
            "Done",
            "Add unit tests for auth middleware",
            "S",
        ),
        (
            "a1b2c3d4-0007",
            "claude-opus-4-6",
            "Queued",
            "Migrate config to TOML format",
            "L",
        ),
    ];

    let wt_dir = agentty_root.join("wt");

    for (session_id, model, status, title, size) in &sessions {
        database
            .insert_session(session_id, model, "main", status, project_id)
            .await
            .expect("failed to insert session");

        database
            .update_session_title(session_id, title)
            .await
            .expect("failed to update session title");

        database
            .update_session_diff_stats(10, 5, session_id, size)
            .await
            .expect("failed to update session diff stats");

        // Create the worktree directory so the session shows a valid folder.
        let session_wt = wt_dir.join(&session_id[..8]);
        std::fs::create_dir_all(&session_wt).expect("failed to create session worktree dir");
    }
}

// ── Feature Scenarios ────────────────────────────────────────────────────

/// Set up an isolated feature demo environment with a seeded database.
///
/// Returns `(agentty_root, workdir)` with canonicalized paths so the
/// seeded project path matches what the binary resolves (critical on
/// macOS where `/var/folders` is a symlink to `/private/var/folders`).
async fn setup_feature_env(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let agentty_root = temp.path().join("agentty_root");
    let workdir = temp.path().join("my-project");
    std::fs::create_dir_all(&agentty_root).expect("failed to create agentty root");
    std::fs::create_dir_all(&workdir).expect("failed to create workdir");

    // Canonicalize so the seeded path matches the binary's resolved CWD.
    let agentty_root = agentty_root
        .canonicalize()
        .expect("failed to canonicalize agentty root");
    let workdir = workdir
        .canonicalize()
        .expect("failed to canonicalize workdir");

    seed_database(&agentty_root, &workdir).await;

    (agentty_root, workdir)
}

/// Generate a feature GIF of the Sessions tab with populated data.
///
/// Shows the session list with sessions in various statuses (`Review`,
/// `InProgress`, `Question`, `Done`, `Queued`) to demonstrate the core
/// workflow and UI layout.
#[tokio::test]
#[ignore = "requires VHS and a renderable terminal — run explicitly with --ignored"]
async fn feature_sessions_tab() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let (agentty_root, workdir) = setup_feature_env(&temp).await;
    let binary_path = cargo_bin("agentty");
    let gif_path = feature_output_dir().join("sessions.gif");

    let steps = "\
Sleep 2s
Tab
Sleep 1s
Sleep 2s
";

    let tape = build_tape(&gif_path, &binary_path, &agentty_root, &workdir, steps);

    // Act
    execute_tape(&tape, "feature_sessions", &gif_path);
}

/// Generate a feature GIF cycling through all tabs.
///
/// Shows each major view — Projects, Sessions, Stats, Settings — to give
/// a complete overview of the agentty interface.
#[tokio::test]
#[ignore = "requires VHS and a renderable terminal — run explicitly with --ignored"]
async fn feature_tab_tour() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let (agentty_root, workdir) = setup_feature_env(&temp).await;
    let binary_path = cargo_bin("agentty");
    let gif_path = feature_output_dir().join("tab_tour.gif");

    let steps = "\
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
";

    let tape = build_tape(&gif_path, &binary_path, &agentty_root, &workdir, steps);

    // Act
    execute_tape(&tape, "feature_tab_tour", &gif_path);
}
