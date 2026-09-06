//! Language-agnostic command-line front end for the `testty` framework.
//!
//! This binary ships inside the `testty` crate so non-Rust projects can drive
//! TUI end-to-end scenarios without writing Rust test harnesses. Installing the
//! crate (`cargo install testty`) provides the `testty` executable, which Cargo
//! auto-detects from this `src/main.rs` alongside the library `src/lib.rs`.
//!
//! The binary exposes the full `testty` verb tree (`run`, `schema`,
//! `proof open`, `proof gallery`, `update`).
//!
//! `run` is implemented: it loads a YAML scenario, lowers it onto the engine
//! via [`testty::spec`], drives the binary under test, and reports pass/fail
//! through the process exit code. The remaining verbs are still stubbed — each
//! reports "not yet implemented" on stderr and exits non-zero so callers and CI
//! can detect that the behavior is not wired up yet. Later tasks replace each
//! remaining [`Command::dispatch`] arm with real logic that delegates into the
//! `testty` library API.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use testty::spec::model::ScenarioSpec;

/// Top-level command-line interface for the `testty` runner.
#[derive(Parser)]
#[command(name = "testty", about, version)]
struct Cli {
    /// Selected verb to execute.
    #[command(subcommand)]
    command: Command,
}

/// Supported `testty` verbs.
///
/// Every variant is dispatched through [`Command::dispatch`], which currently
/// returns [`ExitCode::FAILURE`] for all verbs until the behavior is filled in.
#[derive(Subcommand)]
enum Command {
    /// Runs a scenario file against a TUI binary.
    Run {
        /// Path to the scenario definition (for example, `scenario.yaml`).
        scenario: PathBuf,
        /// Path to the binary under test, overriding the scenario default.
        #[arg(long)]
        bin: Option<PathBuf>,
        /// Output directory for generated proof artifacts.
        #[arg(long)]
        proof: Option<PathBuf>,
    },
    /// Prints the JSON schema for scenario files.
    Schema,
    /// Inspects proof artifacts produced by a run.
    Proof {
        /// Selected proof subcommand.
        #[command(subcommand)]
        command: ProofCommand,
    },
    /// Updates stored scenario snapshots to match current output.
    Update,
}

impl Command {
    /// Executes the selected verb and returns the process exit code.
    ///
    /// `run` executes a YAML scenario via [`run_scenario`]; the remaining arms
    /// are stubbed and return [`ExitCode::FAILURE`] with a "not yet
    /// implemented" notice. Replace the stubbed arms as the verbs are
    /// implemented.
    fn dispatch(self) -> ExitCode {
        match self {
            Command::Run {
                scenario,
                bin,
                proof,
            } => run_scenario(&scenario, bin.as_deref(), proof.as_deref()),
            Command::Schema => not_implemented("schema"),
            Command::Proof {
                command: ProofCommand::Open { .. },
            } => not_implemented("proof open"),
            Command::Proof {
                command: ProofCommand::Gallery { .. },
            } => not_implemented("proof gallery"),
            Command::Update => not_implemented("update"),
        }
    }
}

/// Subcommands for inspecting proof artifacts.
#[derive(Subcommand)]
enum ProofCommand {
    /// Opens a single proof report in the browser.
    Open {
        /// Path to the proof HTML report.
        html: PathBuf,
    },
    /// Builds a gallery from a directory of proof reports.
    Gallery {
        /// Directory containing proof reports.
        dir: PathBuf,
    },
}

/// Parses arguments and dispatches the selected verb.
fn main() -> ExitCode {
    let cli = Cli::parse();

    cli.command.dispatch()
}

/// Loads a YAML scenario, runs it against the binary, and reports the outcome.
///
/// Binds the production diagnostic sink (locked stderr) and delegates to
/// [`run_scenario_reporting`]. The process exit code is the pass/fail signal
/// for non-Rust CI.
fn run_scenario(
    scenario_path: &Path,
    bin_override: Option<&Path>,
    proof: Option<&Path>,
) -> ExitCode {
    let mut stderr = io::stderr().lock();

    run_scenario_reporting(scenario_path, bin_override, proof, &mut stderr)
}

/// Runs a YAML scenario and writes every diagnostic to `out`.
///
/// `bin_override` replaces the scenario's `session.bin` (the `--bin` flag).
/// Routing diagnostics through an injected writer (instead of writing to
/// stderr directly) keeps the messages observable in tests while still
/// avoiding the `print_stderr` lint. The exit code is `SUCCESS` when every
/// expectation passes, `FAILURE` on any failed expectation, parse error, or
/// run error.
fn run_scenario_reporting(
    scenario_path: &Path,
    bin_override: Option<&Path>,
    proof: Option<&Path>,
    out: &mut dyn Write,
) -> ExitCode {
    let text = match fs::read_to_string(scenario_path) {
        Ok(text) => text,
        Err(err) => {
            let _ = writeln!(
                out,
                "testty: cannot read {}: {err}",
                scenario_path.display()
            );

            return ExitCode::FAILURE;
        }
    };

    let mut spec = match ScenarioSpec::from_yaml(&text) {
        Ok(spec) => spec,
        Err(err) => {
            let _ = writeln!(out, "testty: {err}");

            return ExitCode::FAILURE;
        }
    };

    if let Some(bin) = bin_override {
        spec.session.bin = bin.to_path_buf();
    }

    if proof.is_some() {
        let _ = writeln!(
            out,
            "testty: --proof is not yet supported; running without proof output"
        );
    }

    let (_frame, failures) = match spec.lower().run() {
        Ok(result) => result,
        Err(err) => {
            let _ = writeln!(out, "testty: scenario failed to run: {err}");

            return ExitCode::FAILURE;
        }
    };

    if failures.is_empty() {
        let _ = writeln!(out, "testty: scenario passed");

        return ExitCode::SUCCESS;
    }

    for failure in &failures {
        let _ = writeln!(out, "FAIL: {}", failure.message);
    }
    let _ = writeln!(out, "testty: {} expectation(s) failed", failures.len());

    ExitCode::FAILURE
}

/// Reports an unimplemented verb on stderr and returns a failing exit code.
///
/// Writing directly to stderr (instead of `eprintln!`) keeps the stub free of
/// the `print_stderr` lint while still surfacing the notice to callers.
fn not_implemented(verb: &str) -> ExitCode {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "testty: `{verb}` is not yet implemented");

    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn parses_run_with_scenario_path() {
        // Arrange
        let argv = ["testty", "run", "scenario.yaml"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Run { scenario, bin: None, proof: None }
                if scenario.as_path() == Path::new("scenario.yaml")
        ));
    }

    #[test]
    fn parses_run_with_bin_and_proof_options() {
        // Arrange
        let argv = [
            "testty",
            "run",
            "scenario.yaml",
            "--bin",
            "./app",
            "--proof",
            "out",
        ];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Run { bin: Some(bin), proof: Some(proof), .. }
                if bin.as_path() == Path::new("./app") && proof.as_path() == Path::new("out")
        ));
    }

    #[test]
    fn parses_schema_verb() {
        // Arrange
        let argv = ["testty", "schema"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(cli.command, Command::Schema));
    }

    #[test]
    fn parses_proof_open_with_html_path() {
        // Arrange
        let argv = ["testty", "proof", "open", "report.html"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Proof { command: ProofCommand::Open { html } }
                if html.as_path() == Path::new("report.html")
        ));
    }

    #[test]
    fn parses_proof_gallery_with_dir_path() {
        // Arrange
        let argv = ["testty", "proof", "gallery", "proofs"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Proof { command: ProofCommand::Gallery { dir } }
                if dir.as_path() == Path::new("proofs")
        ));
    }

    #[test]
    fn parses_update_verb() {
        // Arrange
        let argv = ["testty", "update"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(cli.command, Command::Update));
    }

    #[test]
    fn run_without_scenario_is_rejected() {
        // Arrange
        let argv = ["testty", "run"];

        // Act
        let result = Cli::try_parse_from(argv);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn every_remaining_stub_reports_unimplemented_and_fails() {
        // Arrange — verbs whose behavior is not yet wired up.
        let verbs: [Command; 4] = [
            Command::Schema,
            Command::Proof {
                command: ProofCommand::Open {
                    html: PathBuf::from("r.html"),
                },
            },
            Command::Proof {
                command: ProofCommand::Gallery {
                    dir: PathBuf::from("d"),
                },
            },
            Command::Update,
        ];

        // Act + Assert
        for verb in verbs {
            assert_eq!(verb.dispatch(), ExitCode::FAILURE);
        }
    }

    #[test]
    fn run_scenario_fails_when_file_missing() {
        // Arrange
        let missing = Path::new("/nonexistent/testty-scenario.yaml");

        // Act
        let code = run_scenario(missing, None, None);

        // Assert
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn run_scenario_fails_on_unsupported_version() {
        // Arrange — an otherwise valid scenario with an unknown version.
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("bad.yaml");
        std::fs::write(&path, "version: 999\nsession:\n  bin: /bin/echo\n").expect("write");

        // Act
        let code = run_scenario(&path, None, None);

        // Assert
        assert_eq!(code, ExitCode::FAILURE);
    }

    /// The `Run` dispatch arm actually routes through `run_scenario`: a valid
    /// scenario against a real fixture reports success, which a stub that
    /// merely returned `FAILURE` could not produce.
    #[cfg(unix)]
    #[test]
    fn dispatch_runs_scenario_for_run_verb() {
        use std::os::unix::fs::PermissionsExt;

        // Arrange — a fixture that renders deterministic text and a scenario
        // that expects it, wired through the `Run` verb.
        let temp = tempfile::tempdir().expect("temp dir");
        let script = temp.path().join("greet.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
        let scenario = temp.path().join("scenario.yaml");
        std::fs::write(
            &scenario,
            format!(
                "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
                 stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \
                 \"Hello World\", region: [0, 0, 80, 1] }}\n",
                bin = script.display()
            ),
        )
        .expect("write scenario");
        let verb = Command::Run {
            scenario,
            bin: None,
            proof: None,
        };

        // Act
        let code = verb.dispatch();

        // Assert
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn run_scenario_fails_when_binary_cannot_spawn() {
        // Arrange — a parseable scenario whose binary does not exist, so the
        // engine errors while spawning rather than producing expectations.
        let temp = tempfile::tempdir().expect("temp dir");
        let scenario = temp.path().join("scenario.yaml");
        std::fs::write(
            &scenario,
            "session:\n  bin: /nonexistent/testty-missing-binary\n  size: [80, 24]\nsteps:\n  - \
             wait_for_stable_frame: { stable_ms: 100, timeout_ms: 1000 }\n",
        )
        .expect("write scenario");

        // Act
        let code = run_scenario(&scenario, None, None);

        // Assert
        assert_eq!(code, ExitCode::FAILURE);
    }

    /// End-to-end: the `run` verb drives a real binary and reports success.
    #[cfg(unix)]
    #[test]
    fn run_scenario_passes_against_fixture_binary() {
        use std::os::unix::fs::PermissionsExt;

        // Arrange — a fixture that renders deterministic text and stays alive.
        let temp = tempfile::tempdir().expect("temp dir");
        let script = temp.path().join("greet.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
        let scenario = temp.path().join("scenario.yaml");
        std::fs::write(
            &scenario,
            format!(
                "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
                 stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \
                 \"Hello World\", region: [0, 0, 80, 1] }}\n",
                bin = script.display()
            ),
        )
        .expect("write scenario");

        // Act
        let code = run_scenario(&scenario, None, None);

        // Assert
        assert_eq!(code, ExitCode::SUCCESS);
    }

    /// `bin_override` (the `--bin` flag) replaces the scenario's `session.bin`:
    /// the scenario points at a binary that cannot spawn, so success is only
    /// possible when the override binary is the one actually driven.
    #[cfg(unix)]
    #[test]
    fn run_scenario_honors_bin_override() {
        use std::os::unix::fs::PermissionsExt;

        // Arrange — the real fixture is supplied through `bin_override`, while
        // the scenario points at a placeholder binary that must never spawn.
        let temp = tempfile::tempdir().expect("temp dir");
        let script = temp.path().join("greet.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
        let scenario = temp.path().join("scenario.yaml");
        std::fs::write(
            &scenario,
            "session:\n  bin: /nonexistent/placeholder\n  size: [80, 24]\nsteps:\n  - \
             wait_for_stable_frame: { stable_ms: 300, timeout_ms: 5000 }\nexpect:\n  - \
             text_in_region: { text: \"Hello World\", region: [0, 0, 80, 1] }\n",
        )
        .expect("write scenario");

        // Act
        let code = run_scenario(&scenario, Some(&script), None);

        // Assert
        assert_eq!(code, ExitCode::SUCCESS);
    }

    /// A `--proof` directory is not yet supported, so the run emits a notice on
    /// the diagnostic sink and still drives the scenario to completion.
    #[cfg(unix)]
    #[test]
    fn run_scenario_warns_when_proof_requested() {
        use std::os::unix::fs::PermissionsExt;

        // Arrange — a passing fixture scenario plus a requested proof
        // directory.
        let temp = tempfile::tempdir().expect("temp dir");
        let script = temp.path().join("greet.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
        let scenario = temp.path().join("scenario.yaml");
        std::fs::write(
            &scenario,
            format!(
                "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
                 stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \
                 \"Hello World\", region: [0, 0, 80, 1] }}\n",
                bin = script.display()
            ),
        )
        .expect("write scenario");
        let proof = temp.path().join("proof-out");
        let mut out = Vec::new();

        // Act
        let code = run_scenario_reporting(&scenario, None, Some(&proof), &mut out);

        // Assert
        let log = String::from_utf8(out).expect("utf8 log");
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(log.contains("--proof is not yet supported"));
    }

    /// A scenario whose expectation does not match the rendered frame reports
    /// the failed expectation on the diagnostic sink and exits with failure.
    #[cfg(unix)]
    #[test]
    fn run_scenario_reports_failed_expectations() {
        use std::os::unix::fs::PermissionsExt;

        // Arrange — the fixture renders "Hello World" but the scenario expects
        // different text.
        let temp = tempfile::tempdir().expect("temp dir");
        let script = temp.path().join("greet.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
        let scenario = temp.path().join("scenario.yaml");
        std::fs::write(
            &scenario,
            format!(
                "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
                 stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \
                 \"Goodbye Moon\", region: [0, 0, 80, 1] }}\n",
                bin = script.display()
            ),
        )
        .expect("write scenario");
        let mut out = Vec::new();

        // Act
        let code = run_scenario_reporting(&scenario, None, None, &mut out);

        // Assert
        let log = String::from_utf8(out).expect("utf8 log");
        assert_eq!(code, ExitCode::FAILURE);
        assert!(log.contains("expectation(s) failed"));
    }
}
