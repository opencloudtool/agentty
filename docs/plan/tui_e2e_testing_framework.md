# TUI End-to-End Testing Framework

VHS-based E2E testing framework that launches the real `agentty` binary in a virtual terminal, captures PNG screenshots, and compares them against stored references with pixel-level tolerance.

## Architecture

VHS layer (`e2e.rs` + `e2e_support/`): Runs the real binary in a VHS virtual terminal, captures PNG screenshots, and compares against stored references using pixel-level Euclidean RGB distance. Tests the full user experience including terminal rendering, font layout, and startup behavior.

## Steps

## 1) Make the runtime generic over backend

### Why now

The runtime hardcodes `TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>`. Before the test harness crate can drive the app with `TestBackend`, the runtime must accept any `ratatui::backend::Backend` implementation.

### Usable outcome

Tests can construct a `Terminal<TestBackend>` that drives the same render/event code paths as production.

### Substeps

- [x] **Parameterize the runtime core over `Backend`.** `run_main_loop`, `render_frame` in `crates/agentty/src/runtime/core.rs` accept `Terminal<B>` where `B: Backend`. Added `backend_err` helper. Production entry point `run()` still uses `CrosstermBackend<io::Stdout>`.

- [x] **Propagate the generic through event processing and key handling.** Updated `process_events`/`process_event` in `event.rs`, `handle_key_event` in `key_handler.rs`, and mode handlers in `session_view.rs` and `prompt.rs`.

- [x] **Decouple event injection from the crossterm poller.** Added `run_with_backend` in `crates/agentty/src/runtime/core.rs` that accepts an `mpsc::UnboundedReceiver<crossterm::event::Event>` and a `Terminal<B>` directly.

### Tests

- [x] All existing runtime tests pass. Added `run_with_backend_exits_on_quit_key` test.

### Docs

- [x] Updated doc comments and `docs/site/content/docs/architecture/testability-boundaries.md`.

## 2) VHS E2E framework with startup screenshot test

### Why now

In-memory tests exercise the rendering code paths but never test the real binary in a real terminal. VHS provides a virtual terminal that renders the actual TUI, capturing true visual output as PNG screenshots.

### Usable outcome

One E2E test launches the real `agentty` binary via VHS, captures a screenshot, and compares it against a stored reference using pixel-level tolerance. Running `cargo test --test e2e -- --ignored` verifies the startup UI renders the Projects tab correctly.

### Architecture

- **`VhsTest`** (`crates/agentty/tests/e2e_support/harness.rs`): Creates an isolated environment (`AGENTTY_ROOT` set to a temp dir, deterministic working directory named `test-project`), generates a VHS tape, runs it, and captures a screenshot.
- **Pixel comparison**: Uses the `image` crate to compare screenshots pixel-by-pixel. Each pixel's Euclidean RGB distance is computed; pixels differing by more than 30 units count as "different". If more than 10% of pixels differ, the test fails.
- **Retry mechanism**: VHS intermittently fails to produce screenshots. The harness retries up to 3 times before failing.
- **Update workflow**: `AGENTTY_E2E_UPDATE=1 cargo test --test e2e -- --ignored` saves the current screenshot as the new reference.
- **Reference storage**: PNG files stored in `crates/agentty/tests/e2e_screenshots/`, committed to the repo. Actual screenshots from failed comparisons are saved as `*_actual.png` (git-ignored).

### Substeps

- [x] **Create VHS harness.** `crates/agentty/tests/e2e_support/harness.rs` with `VhsTest`, tape generation, pixel comparison, and `assert_screenshot_matches`. 3 unit tests for `pixel_distance`.

- [x] **Write startup screenshot test.** `crates/agentty/tests/e2e.rs` with `startup_shows_projects_tab` test. Launches agentty in a clean environment and verifies the rendered screenshot matches the reference.

- [x] **Store reference screenshot.** `crates/agentty/tests/e2e_screenshots/startup_projects_tab.png` committed as the baseline.

### Tests

- [x] `cargo test --test e2e -- --ignored` — passes consistently (3/3 comparison runs after reference generation).

### Docs

- [x] Updated `crates/agentty/tests/AGENTS.md` with E2E test entries.

## 3) Add navigation and tab-switching VHS tests

### Why now

The startup test proves the framework works. Adding navigation tests (switching tabs, opening sessions) exercises user interactions through the real terminal.

### Usable outcome

VHS tests verify that pressing `Tab` switches between Projects/Sessions/Stats/Settings, and that the correct content appears for each tab.

### Substeps

- [ ] **Write tab-switching test.** Add `tab_switching_shows_sessions` test that presses `Tab` to switch to Sessions tab and captures a screenshot.

- [ ] **Write session navigation test.** Add a test that creates sessions via the database, navigates to one, and captures the session view screenshot.

### Tests

- [ ] Both tests pass with stable reference screenshots.

## 4) Add agent interaction VHS tests

### Why now

The highest-value E2E test is the full user flow: send a prompt, get a response, see it rendered. This requires launching agentty with a real or mock agent backend.

### Usable outcome

A VHS test verifies the complete prompt → response → render cycle through the real terminal.

### Substeps

- [ ] **Design agent interaction approach.** Decide between: (a) using a mock agent binary that returns scripted responses, or (b) using the `test-utils` feature to inject mock channels, or (c) testing against real APIs with controlled prompts.

- [ ] **Write single-turn conversation test.** Launch agentty, open a session, type a prompt, wait for the response, and capture the final screenshot.

## 5) CI integration

### Why now

VHS E2E tests need to run in CI to catch regressions automatically.

### Usable outcome

GitHub Actions workflow installs VHS and runs E2E tests on every PR.

### Substeps

- [ ] **Add VHS installation step to CI.** Use `brew install vhs` or the official Charmbracelet GitHub Action.

- [ ] **Upload screenshots as artifacts.** On test failure, upload the actual and reference screenshots for review.

## Cross-Plan Notes

- `docs/plan/end_to_end_test_structure.md` owns workflow-level tests (fake CLIs, session state assertions, git/forge scenarios). This plan owns the visual E2E layer.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its status in this document.
- When the full plan is complete, remove the implemented plan file; if more work remains, move that work into a new follow-up plan file before deleting the completed one.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Runtime backend generics | `run_with_backend` accepts `Terminal<B: Backend>`. | Complete |
| VHS E2E framework | `VhsTest` harness with tape generation, pixel comparison, retry logic, and `assert_screenshot_matches`. 3 unit tests + 1 integration test. | Complete |
| Startup screenshot test | `startup_shows_projects_tab` verifies Projects tab renders. Reference PNG committed. | Complete |
| Navigation VHS tests | No VHS tests for tab switching or session navigation. | Not started |
| Agent interaction VHS tests | No VHS tests for prompt → response flow. | Not started |
| CI integration | No CI workflow for VHS E2E tests. | Not started |

## Research References

| Tool | Type | Language | Key Takeaway |
|------|------|----------|-------------|
| [VHS](https://github.com/charmbracelet/vhs) | Terminal recorder | Go | Virtual terminal, scripted via tape files, PNG screenshots. |
| [Playwright](https://playwright.dev/) | Browser E2E | TypeScript | Design model — screenshot comparison with pixel tolerance. |
