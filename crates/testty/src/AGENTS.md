# TUI Test Source

Source modules for the `testty` crate.

## Directory Index

- [`lib.rs`](lib.rs) - Crate root and module declarations.
- [`session.rs`](session.rs) - PTY executor for spawning and driving TUI binaries.
- [`frame.rs`](frame.rs) - Terminal frame parser using `vt100` for cell inspection.
- [`region.rs`](region.rs) - Rectangular region definitions with named anchors.
- [`locator.rs`](locator.rs) - Text locators with style and color filtering.
- [`step.rs`](step.rs) - Scenario step definitions for test automation.
- [`scenario.rs`](scenario.rs) - Scenario builder for composing user journeys.
- [`vhs.rs`](vhs.rs) - VHS tape compiler for visual screenshot capture.
- [`calibration.rs`](calibration.rs) - Cell-to-pixel geometry calibration.
- [`artifact.rs`](artifact.rs) - Artifact directory and capture storage.
- [`overlay.rs`](overlay.rs) - Screenshot overlay rendering for failure reporting.
- [`assertion.rs`](assertion.rs) - Structured matcher APIs for terminal assertions.
- [`recipe.rs`](recipe.rs) - Agent-friendly recipe helpers for common TUI assertions.
- [`snapshot.rs`](snapshot.rs) - Paired snapshot workflow and baseline management.
