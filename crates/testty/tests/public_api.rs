//! Public-API compatibility tripwire for testty's documented stable surface.
//!
//! This test exists so that any accidental rename, removal, or signature
//! break of a documented stable item fails the build before publication.
//! Update it deliberately whenever an intentional breaking change is made
//! to the published surface, and bump the testty major version in lockstep.
//!
//! Coverage goes beyond symbol presence and exercises the source-compat
//! patterns we want to keep supporting:
//!
//! - struct-literal construction of types whose public field set is part of the
//!   stable contract (for example, [`Region`] and [`CellColor`]),
//! - pattern matching of [`SnapshotError`] variants through the supported `..`
//!   rest-pattern (so future field additions stay non-breaking),
//! - construction of [`SnapshotConfig`] through its public builder so the
//!   `#[non_exhaustive]` lock-down does not regress to struct-literal syntax in
//!   downstream code.

#![allow(unused_imports)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use testty::assertion::{self, AssertionFailure, Expected, MatchResult, SoftAssertions};
use testty::feature::{
    self, FeatureDemo, FeatureMeta, FeatureResult, GifMode, GifStatus, Redaction,
    compute_frame_hash, compute_gif_hash, hash_sidecar_path,
};
use testty::frame::{CellColor, CellStyle, TerminalFrame};
use testty::journey::{Journey, StartupWait};
use testty::locator::MatchedSpan;
use testty::proof::backend::{ProofBackend, RenderContext};
use testty::proof::junit::JunitBackend;
use testty::proof::report::{AssertionResult, ProofCapture, ProofError, ProofReport};
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::{PtySession, PtySessionBuilder, PtySessionError};
use testty::snapshot::{SnapshotConfig, SnapshotError};
use testty::spec::model::{ExpectSpec, RegionSpec, ScenarioSpec, SessionSpec, StepSpec};
use testty::spec::runtime::{LoweredScenario, SpecError};
use testty::step::{FramePredicate, Step};
use testty::vhs::VhsTapeSettings;

/// Minimal `ProofBackend` impl used to exercise the trait through its
/// owning module path without instantiating a real backend.
///
/// The `render` signature pins the stable contract that backends receive a
/// single `&RenderContext` argument, so a downstream implementor's code keeps
/// compiling as long as that shape holds.
struct NoopBackend;

impl ProofBackend for NoopBackend {
    fn render(&self, _context: &RenderContext<'_>) -> Result<(), ProofError> {
        Ok(())
    }
}

fn accept_backend<B: ProofBackend>(_backend: &B) {}

/// Pinned return type of `LoweredScenario::run`, factored out so the tripwire
/// stays within clippy's type-complexity budget.
type LoweredRunResult = Result<(TerminalFrame, Vec<AssertionFailure>), PtySessionError>;

/// Reference every documented stable type so the build breaks if a name
/// is removed or renamed in a backwards-incompatible way.
#[test]
fn public_surface_is_stable() {
    // Arrange, Act, Assert: compile-time references exercise the documented
    // per-module public API.
    let _: Region = Region::new(0, 0, 1, 1);
    let _: CellColor = CellColor::new(0, 0, 0);
    let _: CellStyle = CellStyle::default();
    let _: fn(u16, u16, &[u8]) -> TerminalFrame = TerminalFrame::new;

    let _ = Scenario::new("public-api");
    let _: Option<Step> = None;
    let _: Option<Journey> = None;

    let _: Option<PtySessionBuilder> = None;
    let _: Option<PtySession> = None;
    let _: Option<PtySessionError> = None;

    // `StartupWait` presets and the matching `Journey` constructors are part
    // of the published surface so test authors can pick a documented startup
    // profile instead of hand-tuning `(stable_ms, timeout_ms)` values.
    let _: StartupWait = StartupWait::Default;
    let _: StartupWait = StartupWait::FastNative;
    let _: StartupWait = StartupWait::SlowNode;
    let _: StartupWait = StartupWait::Custom {
        stable_ms: 100,
        timeout_ms: 1_000,
    };
    let _: u32 = StartupWait::Default.stable_ms();
    let _: u32 = StartupWait::Default.timeout_ms();
    let _: fn(StartupWait) -> Journey = Journey::wait_for_startup_preset;
    let _: fn() -> Journey = Journey::wait_for_startup_default;
    // Pin the legacy raw-number constructor too: it is the documented
    // back-compat entry point for callers that pre-date the named presets,
    // so a silent signature drift here would break downstream code that
    // still passes `(stable_ms, timeout_ms)` directly.
    let _: fn(u32, u32) -> Journey = Journey::wait_for_startup;

    // `PtySessionBuilder::args` accepts any `IntoIterator<Item: Into<String>>`
    // and returns the builder by value. Pinning the shape here breaks the
    // build before publication if the signature drifts (for example, if it
    // ever required `&[&str]` or stopped accepting owned `String`s).
    let _ = PtySessionBuilder::new("/bin/echo").args(["--help", "--version"]);
    let _ = PtySessionBuilder::new("/bin/echo")
        .args([String::from("--help"), String::from("--version")]);
    let _ = PtySessionBuilder::new("/bin/echo").args(std::iter::empty::<&str>());

    let _: Option<MatchedSpan> = None;

    let _: Option<ProofCapture> = None;
    let _: Option<ProofReport> = None;
    let _: Option<ProofError> = None;
    let _: Option<AssertionResult> = None;

    // `RenderContext` is the stable input bundle every `ProofBackend::render`
    // receives. Pin construction through the builder and field reads so the
    // documented shape — and its `#[non_exhaustive]` lock-down — cannot
    // regress to a struct literal in downstream backend implementations.
    let render_report = ProofReport::new("render-context");
    let render_context = RenderContext::new(&render_report, Path::new("/tmp/proof"));
    let _: &ProofReport = render_context.report;
    let _: &Path = render_context.output;

    let _: fn(&NoopBackend) = accept_backend::<NoopBackend>;

    // `JunitBackend` is a published external-style backend: it must stay
    // addressable through its owning module and keep satisfying `ProofBackend`
    // so non-Rust CIs can render proof reports to JUnit-XML.
    let _: JunitBackend = JunitBackend;
    accept_backend(&JunitBackend);

    let _: fn(&TerminalFrame, &str) = assertion::assert_not_visible;

    // Result-returning matcher core surface.
    let _: fn(&TerminalFrame, &str) -> MatchResult = assertion::match_not_visible;
    let _: Option<AssertionFailure> = None;
    let _: Option<Expected> = None;

    // SoftAssertions accumulator surface — both standalone and report-bound
    // constructors return the same type so call sites can mix them, and
    // `check` accepts any `MatchResult` from a `match_*` matcher.
    let mut soft: SoftAssertions<'static> = SoftAssertions::new();
    soft.check(Ok(()));
    let failures: Vec<AssertionFailure> = soft.into_failures();
    let _ = failures;
    // `with_report` requires the report to already hold a capture before
    // binding so soft failures cannot be silently dropped from the proof
    // report; pin that contract by adding a capture before the bind here.
    let mut report = ProofReport::new("public-api-soft");
    report.add_capture("only", "Only capture", &TerminalFrame::new(1, 1, b""));
    let mut soft_bound: SoftAssertions<'_> = SoftAssertions::with_report(&mut report);
    soft_bound.check(Ok(()));
    let _ = soft_bound.into_failures();

    // `Step::eventually` constructs an Eventually step from any predicate
    // closure with the documented signature. Pinning the call shape here
    // breaks the build before publication if the constructor signature
    // drifts. The bound predicate can also be referenced through the
    // `FramePredicate` alias exported from `testty::step`.
    let _ = Step::eventually(
        Duration::from_secs(1),
        Duration::from_millis(50),
        |_frame: &TerminalFrame| Ok(()),
    );
    let _: FramePredicate = Arc::new(|_frame: &TerminalFrame| Ok(()));

    // Declarative YAML scenario surface (the language-agnostic `run` front
    // end). Pin the load/lower/run entry points and the spec types so the
    // published shape the CLI and external tooling depend on cannot drift
    // silently.
    let _: fn(&str) -> Result<ScenarioSpec, SpecError> = ScenarioSpec::from_yaml;
    let _: fn(&ScenarioSpec) -> LoweredScenario = ScenarioSpec::lower;
    let _: fn(&LoweredScenario, &TerminalFrame) -> Vec<AssertionFailure> = LoweredScenario::check;
    let _: fn(LoweredScenario) -> LoweredRunResult = LoweredScenario::run;
    let _: RegionSpec = RegionSpec(0, 0, 1, 1);
    let _: Option<ScenarioSpec> = None;
    let _: Option<SessionSpec> = None;
    let _: Option<StepSpec> = None;
    let _: Option<ExpectSpec> = None;
    let _: Option<SpecError> = None;
}

/// Reference always-available stable items that sit alongside the core
/// public surface and are documented as part of the public contract.
#[test]
fn auxiliary_surface_is_stable() {
    // Arrange, Act, Assert: compile-time references exercise documented
    // auxiliary APIs.
    let _: &str = testty::snapshot::DEFAULT_UPDATE_ENV_VAR;

    let config = SnapshotConfig::new("/baselines", "/artifacts")
        .with_update_env_var("MY_VAR")
        .with_update_mode(true);
    let _: bool = config.is_update_mode();

    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_selected_tab;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_unselected_tab;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_instruction_visible;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_keybinding_hint;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_footer_action;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_dialog_title;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_status_message;
    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_not_visible;

    // Result-returning recipe siblings exposed for composition with
    // `SoftAssertions` and `ProofReport` flows.
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_selected_tab;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_unselected_tab;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_instruction_visible;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_keybinding_hint;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_footer_action;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_dialog_title;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_status_message;
    let _: fn(&TerminalFrame, &str) -> MatchResult = testty::recipe::match_not_visible;

    // Feature freshness primitives — exposed so external tooling can build
    // freshness reports without re-running VHS.
    let _: fn(&ProofReport, &[Redaction]) -> u64 = compute_frame_hash;
    let _: fn(&ProofReport, &[Redaction], &VhsTapeSettings) -> u64 = compute_gif_hash;
    let _: fn(&Path, &str) -> PathBuf = hash_sidecar_path;
    let _: GifMode = GifMode::default();
    let _: GifMode = GifMode::CheckOnly;
    let _: GifMode = GifMode::AlwaysGenerate;
    let _: GifMode = GifMode::GenerateIfStale;
    let _: String = Redaction::hex_after("wt/", 8, "<hash>").apply("wt/4175e5af");
    let _: String = Redaction::literal("Agentty v0.13.0", "<version>").apply("Agentty v0.13.0");
    let _ = FeatureDemo::new("public-api")
        .gif_mode(GifMode::CheckOnly)
        .redact(Redaction::hex_after("wt/", 8, "<hash>"));
    let _: Option<FeatureMeta> = None;
    let _: Option<FeatureResult> = None;
}

/// Lock in the supported pattern for matching `GifStatus` variants.
///
/// `GifStatus` is `#[non_exhaustive]` so future variants stay non-breaking.
/// Downstream callers must include a fallback `_` arm and any field
/// destructuring must use the `..` rest-pattern. Compiled (not run) so
/// accidental renames break the build before publication.
#[allow(dead_code)]
fn gif_status_destructuring_is_stable(status: &GifStatus) -> &'static str {
    match status {
        GifStatus::Generated(path) => {
            let _: &PathBuf = path;

            "generated"
        }
        GifStatus::CacheHit(path) => {
            let _: &PathBuf = path;

            "cache-hit"
        }
        GifStatus::VhsNotInstalled => "vhs-not-installed",
        GifStatus::NoOutputDir => "no-output-dir",
        GifStatus::DirCreateFailed(_err) => "dir-create-failed",
        GifStatus::TapeExecutionFailed(_err) => "tape-execution-failed",
        GifStatus::Fresh { gif_path, hash, .. } => {
            let _: (&PathBuf, &u64) = (gif_path, hash);

            "fresh"
        }
        GifStatus::Stale {
            gif_path,
            current,
            committed,
            committed_error,
            ..
        } => {
            let _: (&PathBuf, &u64, &Option<u64>, &Option<String>) =
                (gif_path, current, committed, committed_error);

            "stale"
        }
        _ => "unknown",
    }
}

/// Lock in the supported pattern for matching `Step::Eventually`.
///
/// `Step::Eventually` is destructured with named fields plus a trailing
/// `..` rest-pattern so adding new fields stays non-breaking, and the
/// match includes a fallback `_` arm so adding new variants stays
/// non-breaking. The function is compiled (not run) so accidental
/// renames of the field names or the variant fail the build before
/// publication.
#[allow(dead_code)]
fn step_eventually_destructuring_is_stable(step: &Step) -> &'static str {
    match step {
        Step::Eventually {
            timeout,
            poll,
            predicate,
            ..
        } => {
            let _: (&Duration, &Duration, &FramePredicate) = (timeout, poll, predicate);

            "eventually"
        }
        _ => "other",
    }
}

/// Lock in the supported pattern for matching `ProofError` variants.
///
/// `ProofError` is `#[non_exhaustive]` so future variants stay non-breaking.
/// Downstream callers that match on it must include a fallback `_` arm. This
/// function is compiled (not run) so accidental renames of the documented
/// variants fail the build before publication.
#[allow(dead_code)]
fn proof_error_destructuring_is_stable(error: &ProofError) -> &'static str {
    match error {
        ProofError::Io(_err) => "io",
        ProofError::Format(_message) => "format",
        _ => "unknown",
    }
}

/// Lock in struct-literal construction for public types whose field names
/// are part of the stable contract. Renaming or removing any of these
/// fields will fail this test before publication.
#[test]
fn public_struct_literals_are_stable() {
    // Arrange: construct public structs through their stable field names.
    let region = Region {
        col: 0,
        row: 1,
        width: 2,
        height: 3,
    };
    assert_eq!(region.col, 0);
    assert_eq!(region.row, 1);
    assert_eq!(region.width, 2);
    assert_eq!(region.height, 3);

    let color = CellColor {
        red: 10,
        green: 20,
        blue: 30,
    };

    // Act: read each public field so renames or removals break compilation.
    let region_fields = (region.col, region.row, region.width, region.height);
    let color_fields = (color.red, color.green, color.blue);

    // Assert: the stable field values are preserved.
    assert_eq!(region_fields, (0, 1, 2, 3));
    assert_eq!(color_fields, (10, 20, 30));
}

/// Lock in the supported pattern for matching `AssertionFailure` and the
/// `Expected` variants exposed by the `match_*` matcher core.
///
/// `AssertionFailure` and `Expected` are both `#[non_exhaustive]` so future
/// fields and variants stay non-breaking. Downstream callers must destructure
/// with named fields plus a trailing `..` rest-pattern and must include a
/// fallback `_` arm. This function is compiled (not run) so accidental
/// renames of variants or destructured field names fail the build before
/// publication. The bound values are referenced in each arm so clippy
/// keeps the compatibility check explicit instead of collapsing the named
/// fields into the trailing `..`.
#[allow(dead_code)]
fn assertion_failure_destructuring_is_stable(failure: &AssertionFailure) -> &'static str {
    let AssertionFailure {
        message,
        expected,
        region,
        matched_spans,
        frame_excerpt,
        ..
    } = failure;
    let _: (&String, &Option<Region>, &Vec<MatchedSpan>, &String) =
        (message, region, matched_spans, frame_excerpt);

    match expected {
        Expected::TextInRegion { needle, .. } => {
            let _: &String = needle;

            "text-in-region"
        }
        Expected::NotVisible { needle, .. } => {
            let _: &String = needle;

            "not-visible"
        }
        Expected::MatchCount { needle, count, .. } => {
            let _: (&String, &usize) = (needle, count);

            "match-count"
        }
        Expected::ForegroundColor { needle, color, .. } => {
            let _: (&String, &CellColor) = (needle, color);

            "foreground"
        }
        Expected::BackgroundColor { needle, color, .. } => {
            let _: (&String, &CellColor) = (needle, color);

            "background"
        }
        Expected::Highlighted { needle, .. } => {
            let _: &String = needle;

            "highlighted"
        }
        Expected::NotHighlighted { needle, .. } => {
            let _: &String = needle;

            "not-highlighted"
        }
        _ => "unknown",
    }
}

/// Lock in the supported pattern for destructuring `AssertionResult`.
///
/// `AssertionResult` is a regular (not `#[non_exhaustive]`) struct so
/// downstream crates can both destructure existing entries on
/// [`ProofCapture::assertions`] and push their own entries with struct
/// literals. This function is compiled (not run) so accidental renames
/// of destructured field names fail the build before publication. The
/// bound values are referenced after the destructure so clippy keeps the
/// compatibility check explicit.
#[allow(dead_code)]
fn assertion_result_destructuring_is_stable(result: &AssertionResult) {
    let AssertionResult {
        passed,
        description,
        failure,
    } = result;
    let _: (&bool, &String, &Option<Box<AssertionFailure>>) = (passed, description, failure);
}

/// Lock in the supported pattern for matching `SnapshotError` variants.
///
/// `SnapshotError` and its struct-shaped variants are `#[non_exhaustive]`
/// so future variants and fields stay non-breaking. Downstream callers
/// must match with named fields plus a trailing `..` rest-pattern and
/// must include a fallback `_` arm. This function is compiled (not run)
/// so accidental renames of variants or destructured field names fail
/// the build before publication. The bound values are referenced in each
/// arm so clippy keeps the compatibility check explicit instead of
/// collapsing the named fields into the trailing `..`.
#[allow(dead_code)]
fn snapshot_error_destructuring_is_stable(error: &SnapshotError) -> &'static str {
    match error {
        SnapshotError::MissingBaseline {
            name,
            baseline_path,
            ..
        } => {
            let _: (&String, &PathBuf) = (name, baseline_path);

            "missing-baseline"
        }
        SnapshotError::Mismatch {
            name,
            diff_percent,
            threshold,
            baseline_path,
            actual_path,
            ..
        } => {
            let _: (&String, &f64, &f64, &PathBuf, &PathBuf) =
                (name, diff_percent, threshold, baseline_path, actual_path);

            "mismatch"
        }
        SnapshotError::FrameMismatch {
            name,
            expected,
            actual,
            ..
        } => {
            let _: (&String, &String, &String) = (name, expected, actual);

            "frame-mismatch"
        }
        SnapshotError::IoError(_message) => "io-error",
        SnapshotError::ImageError(_message) => "image-error",
        _ => "unknown",
    }
}

/// Lock the supported pattern for matching `StepSpec`.
///
/// `StepSpec` is `#[non_exhaustive]`, so new step kinds stay non-breaking as
/// long as external matchers keep a fallback `_` arm; struct-variant fields use
/// a trailing `..`. Compiled (not run) so a rename of a documented variant or
/// field fails the build before publication.
#[allow(dead_code)]
fn step_spec_destructuring_is_stable(step: &StepSpec) -> &'static str {
    match step {
        StepSpec::PressKey(key) => {
            let _: &String = key;

            "press_key"
        }
        StepSpec::WriteText(text) => {
            let _: &String = text;

            "write_text"
        }
        StepSpec::SleepMs(ms) => {
            let _: &u64 = ms;

            "sleep_ms"
        }
        StepSpec::WaitForText {
            needle, timeout_ms, ..
        } => {
            let _: (&String, &u32) = (needle, timeout_ms);

            "wait_for_text"
        }
        StepSpec::WaitForStableFrame {
            stable_ms,
            timeout_ms,
            ..
        } => {
            let _: (&u32, &u32) = (stable_ms, timeout_ms);

            "wait_for_stable_frame"
        }
        StepSpec::Eventually {
            matcher,
            timeout_ms,
            poll_ms,
            ..
        } => {
            let _: (&ExpectSpec, &u64, &u64) = (matcher, timeout_ms, poll_ms);

            "eventually"
        }
        StepSpec::Capture => "capture",
        StepSpec::CaptureLabeled {
            label, description, ..
        } => {
            let _: (&String, &String) = (label, description);

            "capture_labeled"
        }
        _ => "unknown",
    }
}

/// Lock the supported pattern for matching `ExpectSpec`.
///
/// `ExpectSpec` is `#[non_exhaustive]`; external matchers keep a `_` arm.
#[allow(dead_code)]
fn expect_spec_destructuring_is_stable(expect: &ExpectSpec) -> &'static str {
    match expect {
        ExpectSpec::SelectedTab(_) => "selected_tab",
        ExpectSpec::UnselectedTab(_) => "unselected_tab",
        ExpectSpec::InstructionVisible(_) => "instruction_visible",
        ExpectSpec::KeybindingHint(_) => "keybinding_hint",
        ExpectSpec::FooterAction(_) => "footer_action",
        ExpectSpec::DialogTitle(_) => "dialog_title",
        ExpectSpec::StatusMessage(_) => "status_message",
        ExpectSpec::NotVisible(_) => "not_visible",
        ExpectSpec::TextInRegion { text, region, .. } => {
            let _: (&String, &RegionSpec) = (text, region);

            "text_in_region"
        }
        _ => "unknown",
    }
}

/// Lock the supported pattern for matching `SpecError`.
///
/// `SpecError` is `#[non_exhaustive]`; external callers keep a `_` arm.
#[allow(dead_code)]
fn spec_error_destructuring_is_stable(error: &SpecError) -> &'static str {
    match error {
        SpecError::Io(_) => "io",
        SpecError::Parse(_) => "parse",
        SpecError::UnsupportedVersion {
            found, supported, ..
        } => {
            let _: (&u32, &u32) = (found, supported);

            "unsupported-version"
        }
        _ => "unknown",
    }
}

/// Lock struct-literal construction for the scenario spec types so their
/// public field names (which are also the YAML contract) cannot be renamed or
/// removed without breaking this build.
#[test]
fn spec_struct_literals_are_stable() {
    // Arrange: construct the spec structs through their stable field names.
    let session = SessionSpec {
        bin: PathBuf::from("./app"),
        size: Some([80, 24]),
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        workdir: None,
    };
    let scenario = ScenarioSpec {
        version: 1,
        name: None,
        session,
        steps: Vec::new(),
        expect: Vec::new(),
    };

    // Act / Assert: the stable fields are readable.
    assert_eq!(scenario.version, 1);
    assert_eq!(scenario.session.bin, PathBuf::from("./app"));
    assert_eq!(scenario.session.size, Some([80, 24]));
    assert!(scenario.steps.is_empty());
    assert!(scenario.expect.is_empty());
}
