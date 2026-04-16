//! Shared helpers and agentty-specific `Journey` builders for E2E tests.
//!
//! Provides [`BuilderEnv`] for isolated test environments and
//! [`FeatureTest`] for declarative feature demo tests with optional Zola
//! page generation.

use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use testty::feature::{FeatureDemo, GifStatus};
use testty::frame::TerminalFrame;
use testty::journey::Journey;
use testty::proof::report::{ProofCapture, ProofReport};
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;
use testty::step::Step;

/// Isolated test environment carrying `agentty_root` and `workdir` paths.
///
/// Use [`BuilderEnv::new`] to create a fresh environment under a temporary
/// directory, [`BuilderEnv::builder`] to get a configured
/// [`PtySessionBuilder`], and [`BuilderEnv::as_vhs_env_pairs`] to export the
/// environment for VHS tape compilation.
pub(crate) struct BuilderEnv {
    /// Path used as `AGENTTY_ROOT` for database and session isolation.
    pub(crate) agentty_root: PathBuf,
    /// Directory containing stub agent executables so the app passes startup
    /// availability validation even when no real agent CLI is installed.
    pub(crate) stub_bin: PathBuf,
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
        let stub_bin = temp_root.join("stub-bin");

        std::fs::create_dir_all(&agentty_root)?;
        std::fs::create_dir_all(&workdir)?;
        std::fs::create_dir_all(&stub_bin)?;

        // Create a stub `claude` executable so the app passes startup agent
        // availability validation on machines without real agent CLIs (CI).
        let stub_agent_path = stub_bin.join("claude");
        std::fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;
        }

        Ok(Self {
            agentty_root,
            stub_bin,
            workdir,
        })
    }

    /// Return a configured [`PtySessionBuilder`] using this environment.
    ///
    /// Sets `AGENTTY_ROOT`, working directory, 80×24 terminal size, and
    /// prepends the stub agent bin directory to `PATH` so the app passes
    /// startup agent availability validation.
    pub(crate) fn builder(&self) -> PtySessionBuilder {
        let path_with_stub_bin = self.path_with_stub_bin();

        PtySessionBuilder::new(cargo_bin("agentty"))
            .size(80, 24)
            .env("AGENTTY_ROOT", self.agentty_root.to_string_lossy())
            .env("PATH", path_with_stub_bin)
            .workdir(&self.workdir)
    }

    /// Return environment variable pairs for VHS tape compilation.
    ///
    /// These match the variables set by [`BuilderEnv::builder`] so the VHS
    /// recording reproduces the same environment as the PTY session.
    pub(crate) fn as_vhs_env_pairs(&self) -> Vec<(String, String)> {
        vec![
            (
                "AGENTTY_ROOT".to_string(),
                self.agentty_root.to_string_lossy().into_owned(),
            ),
            ("PATH".to_string(), self.path_with_stub_bin()),
        ]
    }

    /// Build a `PATH` value with the stub bin directory prepended to the
    /// inherited system `PATH`.
    fn path_with_stub_bin(&self) -> String {
        let system_path = std::env::var("PATH").unwrap_or_default();
        let mut paths = vec![self.stub_bin.clone()];
        paths.extend(std::env::split_paths(&system_path));

        match std::env::join_paths(paths) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(_) => self.stub_bin.to_string_lossy().into_owned(),
        }
    }
}

/// Return the feature GIF output directory, creating it if needed.
///
/// Derives the path from `CARGO_MANIFEST_DIR` →
/// `../../docs/site/static/features/`.
fn feature_output_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let output_dir = Path::new(manifest_dir).join("../../docs/site/static/features");

    let _ = std::fs::create_dir_all(&output_dir);

    output_dir
}

/// Reconstruct a [`TerminalFrame`] from a [`ProofCapture`] so full cell-level
/// assertions (highlight, color, style) can be run against intermediate
/// captures.
pub(crate) fn frame_from_capture(capture: &ProofCapture) -> TerminalFrame {
    TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes)
}

// ---------------------------------------------------------------------------
// Zola feature page generation
// ---------------------------------------------------------------------------

/// Return the Zola feature content directory, creating it if needed.
///
/// Derives the path from `CARGO_MANIFEST_DIR` →
/// `../../docs/site/content/features/`.
fn feature_content_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let content_dir = Path::new(manifest_dir).join("../../docs/site/content/features");

    let _ = std::fs::create_dir_all(&content_dir);

    content_dir
}

/// Metadata for generating a Zola feature content page.
///
/// When passed to [`FeatureTest::zola`], the test runner writes a minimal
/// `.md` frontmatter page to `docs/site/content/features/{name}.md` if the
/// file does not already exist.
pub(crate) struct ZolaFeaturePage {
    /// Human-readable title shown on the features page.
    pub(crate) title: String,
    /// Short description shown below the title.
    pub(crate) description: String,
    /// Ordering weight for the Zola features section (lower = first).
    pub(crate) weight: u32,
}

impl ZolaFeaturePage {
    /// Write the Zola frontmatter page if it does not already exist.
    ///
    /// The generated page uses TOML frontmatter with `title`, `description`,
    /// `weight`, and `[extra] gif` fields matching the Zola feature page
    /// conventions.
    fn ensure(&self, name: &str) {
        let content_dir = feature_content_dir();
        let page_path = content_dir.join(format!("{name}.md"));

        if page_path.exists() {
            return;
        }

        let content = format!(
            "+++\ntitle = \"{title}\"\ndescription = \"{description}\"\nweight = \
             {weight}\n\n[extra]\ngif = \"{name}.gif\"\n+++\n",
            title = self.title,
            description = self.description,
            weight = self.weight,
        );

        let _ = std::fs::write(&page_path, content);
    }
}

// ---------------------------------------------------------------------------
// FeatureTest builder
// ---------------------------------------------------------------------------

/// Declarative feature test builder for agentty E2E tests.
///
/// Owns the full test lifecycle: `TempDir` + [`BuilderEnv`] creation,
/// optional git init, scenario execution via [`FeatureDemo`], assertions,
/// GIF generation with hash caching, and optional Zola page creation.
///
/// # Example
///
/// ```ignore
/// #[test]
/// fn session_creation() {
///     FeatureTest::new("session_creation")
///         .with_git()
///         .zola("Session creation", "Start a new agent session.", 30)
///         .run(
///             |scenario| {
///                 scenario
///                     .compose(&common::wait_for_agentty_startup())
///                     .press_key("a")
///                     .capture_labeled("prompt", "Prompt mode")
///             },
///             |frame, _report| {
///                 let full = Region::full(frame.cols(), frame.rows());
///                 assertion::assert_text_in_region(frame, "Enter", &full);
///             },
///         );
/// }
/// ```
pub(crate) struct FeatureTest {
    /// Feature name used for GIF filename and Zola page filename.
    name: String,
    /// Optional environment setup hook that can seed database state or files
    /// before the PTY session starts.
    setup: Option<FeatureSetupHook>,
    /// Whether to initialize a git repository in the workdir.
    with_git: bool,
    /// Optional Zola page metadata for auto-generation.
    zola_page: Option<ZolaFeaturePage>,
}

/// Boxed setup hook used by [`FeatureTest`] before launching the PTY session.
type FeatureSetupHook = Box<dyn Fn(&BuilderEnv) -> Result<(), Box<dyn std::error::Error>>>;

impl FeatureTest {
    /// Create a new feature test builder with the given name.
    ///
    /// The name is used as the GIF filename stem and Zola page filename.
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            setup: None,
            with_git: false,
            zola_page: None,
        }
    }

    /// Configure an environment setup hook that runs after optional git
    /// initialization and before the PTY session starts.
    pub(crate) fn setup(
        mut self,
        setup: impl Fn(&BuilderEnv) -> Result<(), Box<dyn std::error::Error>> + 'static,
    ) -> Self {
        self.setup = Some(Box::new(setup));

        self
    }

    /// Enable git initialization in the test workdir.
    ///
    /// Required for tests that exercise worktree-dependent features like
    /// session creation.
    pub(crate) fn with_git(mut self) -> Self {
        self.with_git = true;

        self
    }

    /// Configure Zola feature page auto-generation.
    ///
    /// When set, the test runner writes a minimal `.md` frontmatter page
    /// to `docs/site/content/features/{name}.md` if it does not already
    /// exist.
    pub(crate) fn zola(mut self, title: &str, description: &str, weight: u32) -> Self {
        self.zola_page = Some(ZolaFeaturePage {
            title: title.to_string(),
            description: description.to_string(),
            weight,
        });

        self
    }

    /// Run the feature test: build scenario, execute, assert, generate GIF.
    ///
    /// The `build_scenario` closure receives a fresh [`Scenario`] with the
    /// feature name and should return it after composing journeys and steps.
    /// The `assert` closure receives the final frame and proof report for
    /// semantic assertions.
    pub(crate) fn run(
        self,
        build_scenario: impl FnOnce(Scenario) -> Scenario,
        assert: impl FnOnce(&TerminalFrame, &ProofReport),
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::TempDir::new()?;
        let env = BuilderEnv::new(temp.path())?;

        if self.with_git {
            env.init_git()?;
        }

        if let Some(setup) = &self.setup {
            setup(&env)?;
        }

        let scenario = build_scenario(Scenario::new(&self.name));
        let owned_pairs = env.as_vhs_env_pairs();
        let env_pairs: Vec<(&str, &str)> = owned_pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();

        let result = FeatureDemo::new(&self.name)
            .gif_output_dir(feature_output_dir())
            .run(&scenario, env.builder(), &cargo_bin("agentty"), &env_pairs)
            .map_err(|error| std::io::Error::other(format!("feature demo failed: {error}")))?;

        // Surface GIF generation diagnostics — fail on unexpected errors.
        match &result.gif_status {
            GifStatus::Generated(_)
            | GifStatus::CacheHit(_)
            | GifStatus::VhsNotInstalled
            | GifStatus::NoOutputDir => {}
            GifStatus::DirCreateFailed(err) => {
                return Err(std::io::Error::other(format!(
                    "Feature GIF dir creation failed for {}: {err}",
                    self.name
                ))
                .into());
            }
            GifStatus::TapeExecutionFailed(err) => {
                return Err(std::io::Error::other(format!(
                    "VHS tape execution failed for {}: {err}",
                    self.name
                ))
                .into());
            }
        }

        assert(&result.frame, &result.report);

        if let Some(zola_page) = self.zola_page {
            zola_page.ensure(&self.name);
        }

        Ok(())
    }
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
/// Waits for the initial TUI frame to appear and then settle briefly before
/// the scenario starts interacting with the app.
pub(crate) fn wait_for_agentty_startup() -> Journey {
    Journey::new("agentty_startup")
        .with_description("Wait for agentty startup and initial render")
        .step(Step::wait_for_text("Agentty", 30000))
        .step(Step::wait_for_stable_frame(300, 5000))
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
    create_session_with_prompt_and_return_to_list("test")
}

/// Create a session with a caller-provided prompt, submit it, and return to
/// the Sessions list.
///
/// Uses fixed sleeps instead of stable-frame waits because the agent may
/// produce continuous output after submit.
///
/// Requires the Sessions tab to be active and a git-initialized workdir.
pub(crate) fn create_session_with_prompt_and_return_to_list(prompt: &str) -> Journey {
    Journey::new("create_session")
        .with_description(format!(
            "Create session via a, type {prompt}, submit, return to list"
        ))
        .step(Step::press_key("a"))
        .step(Step::wait_for_stable_frame(300, 5000))
        .step(Step::write_text(prompt))
        .step(Step::wait_for_text(prompt, 3000))
        .step(Step::press_key("Enter"))
        .step(Step::sleep(Duration::from_secs(2)))
        .step(Step::press_key("q"))
        .step(Step::sleep(Duration::from_secs(1)))
}
