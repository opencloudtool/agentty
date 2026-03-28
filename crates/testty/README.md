# testty

Rust-native TUI end-to-end testing framework. Drives a real TUI binary in a
pseudo-terminal, captures location-aware terminal state with `vt100`, and
provides a semantic assertion API for text, style, color, and region checks.
Scenarios can also be compiled into [VHS](https://github.com/charmbracelet/vhs)
tapes for visual screenshot capture.

## Quick start

Add `testty` as a dev-dependency in the crate that contains your E2E
tests:

```toml
[dev-dependencies]
testty = "0.6"
tempfile = "3"
```

Inside this workspace, keep using shared workspace dependencies instead:

```toml
# crates/my-app/Cargo.toml
[dev-dependencies]
testty = { workspace = true }
tempfile = { workspace = true }
```

> **Note:** `workspace = true` requires matching entries in the root
> `Cargo.toml` under `[workspace.dependencies]`.

Write a test that launches your binary, interacts with it, and asserts on
the terminal state:

```rust
use testty::recipe;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;

#[test]
fn startup_shows_welcome() {
    // Arrange
    let temp = tempfile::TempDir::new().unwrap();
    let builder = PtySessionBuilder::new("/path/to/my-binary")
        .size(80, 24)
        .env("MY_APP_ROOT", temp.path().to_string_lossy())
        .workdir(temp.path());

    let scenario = Scenario::new("startup")
        .wait_for_stable_frame(500, 5000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario failed");

    // Assert
    recipe::expect_instruction_visible(&frame, "Welcome");
}
```

Run the test:

```sh
cargo test -p my-app --test e2e
```

## Core concepts

### Scenario

A `Scenario` is an ordered sequence of `Step` actions that describe a user
journey. Built with a fluent API, then executed in a PTY or compiled into a
VHS tape.

```rust
use std::time::Duration;
use testty::scenario::Scenario;

let scenario = Scenario::new("tab_navigation")
    .wait_for_stable_frame(500, 5000)   // Wait for app to render
    .press_key("Tab")                    // Switch tab
    .wait_for_stable_frame(300, 3000)   // Wait for re-render
    .capture();                          // Snapshot the terminal
```

### Steps

Each `Step` represents a single user action or wait condition:

| Step | Description | Example |
|------|-------------|---------|
| `WriteText` | Type text into the terminal | `.write_text("hello world")` |
| `PressKey` | Send a named key press | `.press_key("Enter")` |
| `Sleep` | Pause for a fixed duration | `.sleep_ms(200)` |
| `WaitForText` | Poll until text appears | `.wait_for_text("Ready", 5000)` |
| `WaitForStableFrame` | Wait for rendering to stabilize | `.wait_for_stable_frame(500, 5000)` |
| `Capture` | Snapshot the current terminal state | `.capture()` |
| `CaptureLabeled` | Labeled snapshot for proof reports | `.capture_labeled("init", "App launched")` |

**Supported key names:** `Enter`, `Tab`, `Escape` / `Esc`, `Backspace`,
`Up`, `Down`, `Left`, `Right`, `Home`, `End`, `Delete`, `PageUp`, `PageDown`,
`Space`, `Ctrl+<letter>` (e.g., `Ctrl+C`). Unknown keys are sent as raw bytes.

### PtySession and PtySessionBuilder

`PtySession` spawns the binary in a real PTY using `portable-pty`. Configure
it with `PtySessionBuilder`:

```rust
use testty::session::PtySessionBuilder;

let builder = PtySessionBuilder::new("/path/to/binary")
    .size(120, 40)                          // Terminal dimensions (default: 80x24)
    .env("DATABASE_URL", "sqlite::memory:") // Environment variables
    .env("LOG_LEVEL", "debug")
    .workdir("/tmp/test-workspace");        // Working directory

// Option 1: Use with a Scenario
let frame = scenario.run(builder).expect("failed");

// Option 2: Manual control
let mut session = builder.spawn().expect("spawn failed");
session.write_text("hello")?;
session.press_key("Enter")?;
let frame = session.wait_for_text("response", std::time::Duration::from_secs(5))?;
```

### TerminalFrame

A `TerminalFrame` is a snapshot of the terminal state parsed through `vt100`.
It provides structured access to text, colors, and styles:

```rust
use testty::frame::TerminalFrame;

// Create from raw ANSI bytes (done automatically by PtySession)
let frame = TerminalFrame::new(80, 24, b"\x1b[1mBold Title\x1b[0m\r\nBody text");

// Read text
let all_text = frame.all_text();          // All visible text
let row_text = frame.row_text(0);         // Text from row 0
let region_text = frame.text_in_region(&region); // Text in a region

// Search for text (returns Vec<MatchedSpan>)
let matches = frame.find_text("Title");   // Find everywhere
let matches = frame.find_text_in_region("Title", &region); // Find in region
```

### Region

A `Region` defines a rectangular area for scoped assertions:

```rust
use testty::region::Region;

// Named constructors
let header  = Region::top_row(80);           // First row, full width
let footer  = Region::footer(80, 24);        // Last row, full width
let full    = Region::full(80, 24);          // Entire terminal
let left    = Region::left_panel(80, 24);    // Left half
let right   = Region::right_panel(80, 24);   // Right half
let top_l   = Region::top_left(80, 24);      // Top-left quadrant
let top_r   = Region::top_right(80, 24);     // Top-right quadrant

// Percentage-based (col%, row%, width%, height%, cols, rows)
let upper = Region::percent(0, 0, 100, 60, 80, 24); // Top 60%

// Explicit coordinates
let custom = Region::new(10, 5, 30, 3);      // col=10, row=5, 30x3
```

### MatchedSpan

When text is found in a frame, you get a `MatchedSpan` with position, color,
and style metadata:

```rust
let matches = frame.find_text("Projects");
let span = &matches[0];

span.text;           // "Projects"
span.rect;           // Region { col, row, width, height }
span.foreground;     // Option<CellColor>
span.background;     // Option<CellColor>
span.style;          // CellStyle (bold, italic, underline, inverse)
span.is_bold();      // true if bold
span.is_highlighted(); // true if bold, inverse, or has background color
span.has_fg(&color); // true if foreground matches
span.has_bg(&color); // true if background matches
```

## Assertion API

### Low-level assertions (`assertion` module)

```rust
use testty::assertion;
use testty::frame::CellColor;
use testty::region::Region;

let region = Region::top_row(80);

// Text presence
assertion::assert_text_in_region(&frame, "Projects", &region);
assertion::assert_not_visible(&frame, "Error");
assertion::assert_match_count(&frame, "Tab", 2);

// Style checks
assertion::assert_span_is_highlighted(&frame, "Selected");
assertion::assert_span_is_not_highlighted(&frame, "Inactive");

// Color checks
assertion::assert_text_has_fg_color(&frame, "Error", &CellColor::new(128, 0, 0));
assertion::assert_text_has_bg_color(&frame, "Active", &CellColor::new(0, 0, 128));
```

All assertion functions panic with detailed messages on failure, including
the match position, actual colors/styles, and region contents.

### Recipe helpers (`recipe` module)

High-level, composable helpers for common TUI patterns. Prefer these over
raw assertions:

```rust
use testty::recipe;

// Tabs
recipe::expect_selected_tab(&frame, "Projects");     // In header, highlighted
recipe::expect_unselected_tab(&frame, "Sessions");    // In header, not highlighted

// Footer
recipe::expect_keybinding_hint(&frame, "Tab");        // Hint in footer row
recipe::expect_footer_action(&frame, "Quit");         // Action in footer row

// Content
recipe::expect_instruction_visible(&frame, "Press Enter to start");
recipe::expect_dialog_title(&frame, "Confirm Delete"); // Upper 60% of terminal
recipe::expect_status_message(&frame, "Saved");        // Anywhere in frame

// Absence
recipe::expect_not_visible(&frame, "Loading...");
```

## Snapshot testing

The framework supports two snapshot modes: **frame text** (semantic) and
**visual screenshot** (pixel-level via VHS).

### Frame text snapshots

Compare the terminal text content against a committed baseline:

```rust
use testty::snapshot::{self, SnapshotConfig};

let config = SnapshotConfig::new(
    "tests/e2e_baselines",  // Committed baseline directory
    "tests/e2e_artifacts",  // Failure artifact output (gitignored)
);

snapshot::assert_frame_snapshot_matches(
    &config,
    "startup_projects_tab",  // Snapshot name
    &frame.all_text(),
).expect("frame snapshot should match");
```

### Visual snapshots (VHS)

Compile a scenario into a VHS tape for pixel-level screenshot capture:

```rust
let tape = scenario.to_vhs_tape(
    &binary_path,
    Path::new("/tmp/screenshot.png"),
    &[("MY_ENV", "value")],
);

// Write tape to file
tape.write_to(Path::new("/tmp/test.tape"))?;

// Execute the tape (requires VHS installed)
tape.execute(Path::new("/tmp/test.tape"))?;
```

### Updating baselines

Set `TUI_TEST_UPDATE=1` to overwrite baselines with the current output:

```sh
TUI_TEST_UPDATE=1 cargo test -p my-app --test e2e
```

### Snapshot config tuning

For visual snapshots, configure pixel-level comparison thresholds:

```rust
let config = SnapshotConfig::new("tests/baselines", "tests/artifacts")
    .with_thresholds(
        30.0,  // Per-pixel color distance threshold (Euclidean RGB)
        10.0,  // Maximum percentage of differing pixels allowed
    );
```

## Proof pipeline

The proof pipeline captures labeled terminal states during scenario execution
and renders them through swappable backends. Use `capture_labeled()` steps
and `run_with_proof()` to collect a `ProofReport`:

```rust
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;
use testty::proof::frame_text::FrameTextBackend;
use testty::proof::strip::ScreenshotStripBackend;
use testty::proof::gif::GifBackend;
use testty::proof::html::HtmlBackend;

let scenario = Scenario::new("startup_proof")
    .wait_for_stable_frame(300, 5000)
    .capture_labeled("launched", "App reached stable state")
    .press_key("Tab")
    .wait_for_stable_frame(200, 3000)
    .capture_labeled("navigated", "Switched to second tab");

let builder = PtySessionBuilder::new("/path/to/binary").size(80, 24);
let (_frame, report) = scenario.run_with_proof(builder).expect("failed");

// Render through any backend:
report.save(&FrameTextBackend, Path::new("proof.txt")).unwrap();
report.save(&ScreenshotStripBackend, Path::new("proof.png")).unwrap();
report.save(&GifBackend::new(), Path::new("proof.gif")).unwrap();
report.save(&HtmlBackend, Path::new("proof.html")).unwrap();
```

### Proof formats

| Format | Backend | Output | Use case |
|--------|---------|--------|----------|
| Frame text | `FrameTextBackend` | `.txt` | CI logs, quick inspection |
| PNG strip | `ScreenshotStripBackend` | `.png` | Review comments, docs |
| Animated GIF | `GifBackend` | `.gif` | PR descriptions, demos |
| HTML report | `HtmlBackend` | `.html` | Detailed review with diffs and assertions |

## Composable journeys

`Journey` provides reusable building blocks for declarative scenario authoring.
Instead of repeating low-level step sequences, compose scenarios from
pre-built journeys:

```rust
use testty::journey::Journey;
use testty::scenario::Scenario;

let startup = Journey::wait_for_startup(300, 5000);
let navigate = Journey::navigate_with_key("Tab", "Settings", 3000);
let input = Journey::type_and_confirm("search query");
let snapshot = Journey::capture_labeled("final", "End state");

let scenario = Scenario::new("settings_search")
    .compose(&startup)
    .compose(&navigate)
    .compose(&input)
    .compose(&snapshot);
```

### Built-in journeys

| Journey | Steps | Description |
|---------|-------|-------------|
| `wait_for_startup(stable_ms, timeout_ms)` | 1 | Wait for app to render and stabilize |
| `navigate_with_key(key, expected, timeout)` | 2 | Press key, wait for expected text |
| `type_and_confirm(text)` | 2 | Type text and press Enter |
| `press_and_wait(key, ms)` | 2 | Press key and sleep briefly |
| `capture_labeled(label, desc)` | 1 | Capture with label for proofs |

## Frame diffing

The diff engine computes cell-level differences between terminal frames
and generates human-readable change summaries:

```rust
use testty::diff::FrameDiff;
use testty::frame::TerminalFrame;

let before = TerminalFrame::new(80, 24, b"Counter: 0");
let after = TerminalFrame::new(80, 24, b"Counter: 42");

let diff = FrameDiff::compute(&before, &after);
assert!(!diff.is_identical());

// Get changed regions with row/col spans
for region in diff.changed_regions() {
    println!("Row {}, cols {}..{}: {:?}",
        region.region.row, region.region.col,
        region.region.col + region.region.width,
        region.change_type);
}

// Human-readable summaries
for line in diff.summary() {
    println!("{line}");
}
```

Diffs are automatically computed between consecutive captures in a
`ProofReport` and displayed in the HTML proof backend.

## Module overview

| Module | Purpose |
|--------|---------|
| `scenario` | Fluent builder for composing test scenarios from steps |
| `step` | Step enum: `WriteText`, `PressKey`, `Sleep`, `WaitForText`, `WaitForStableFrame`, `Capture`, `CaptureLabeled` |
| `session` | PTY executor: `PtySession` + `PtySessionBuilder` |
| `frame` | Terminal state parser: `TerminalFrame`, `CellColor`, `CellStyle` |
| `region` | Rectangular region definitions with named anchors |
| `locator` | `MatchedSpan` with text, position, color, and style metadata |
| `assertion` | Structured assertion functions with detailed failure messages |
| `recipe` | High-level helpers for tabs, footer, dialogs, status messages |
| `snapshot` | Baseline management: frame text and visual image comparison |
| `vhs` | VHS tape compiler for visual screenshot capture |
| `calibration` | Cell-to-pixel geometry mapping for overlay rendering |
| `artifact` | Failure artifact directory and capture storage |
| `overlay` | Pixel-level drawing on screenshots for visual debugging |
| `proof` | Proof pipeline: report collector, backend trait, and format implementations |
| `renderer` | Native bitmap font renderer for terminal frames to PNG images |
| `diff` | Cell-level frame diffing with changed region detection |
| `journey` | Composable journey building blocks for declarative test authoring |

## Full example

A complete E2E test exercising tab navigation:

```rust
use testty::recipe;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;
use testty::snapshot::{self, SnapshotConfig};

#[test]
fn tab_key_switches_tabs() {
    // Arrange
    let temp = tempfile::TempDir::new().unwrap();
    let builder = PtySessionBuilder::new(env!("CARGO_BIN_EXE_myapp"))
        .size(80, 24)
        .env("APP_ROOT", temp.path().to_string_lossy())
        .workdir(temp.path());

    let scenario = Scenario::new("tab_switch")
        .wait_for_stable_frame(500, 5000)
        .press_key("Tab")
        .wait_for_stable_frame(300, 3000)
        .capture();

    // Act
    let frame = scenario.run(builder).expect("scenario failed");

    // Assert
    recipe::expect_selected_tab(&frame, "Sessions");
    recipe::expect_unselected_tab(&frame, "Projects");

    // Optional: frame snapshot
    let config = SnapshotConfig::new("tests/e2e_baselines", "tests/e2e_artifacts");
    snapshot::assert_frame_snapshot_matches(&config, "tab_switch", &frame.all_text())
        .expect("frame snapshot should match");
}
```

## Examples

Run the showcase examples to see the framework features in action:

```sh
# Full proof pipeline: captures → frame-text, PNG strip, GIF, HTML
cargo run --example proof_pipeline -p testty

# Frame diffing with changed region detection
cargo run --example frame_diffing -p testty

# Composable journeys and scenario building
cargo run --example journey_composition -p testty
```

## Tips

- **Use `wait_for_stable_frame`** instead of `sleep_ms` when waiting for
  rendering. It adapts to actual render speed rather than hard-coding delays.
- **Use recipe helpers** over raw assertions. They encode common TUI layout
  patterns (header tabs, footer hints) so you don't rebuild locator logic.
- **Set deterministic terminal size** (e.g., 80x24) to keep frame snapshots
  stable across machines.
- **Isolate state** with `tempfile::TempDir` and environment variables so
  tests don't interfere with each other or the real app data.
- **E2E tests run automatically** with `cargo test`. Cargo builds the binary
  before running integration tests, so `CARGO_BIN_EXE_*` is always available.
