# TUI E2E Testing Framework

Rust-native TUI end-to-end testing framework using PTY-driven semantic
assertions and VHS screenshot capture.

## Entry Points

- `src/lib.rs` is the public crate root.
- `src/session.rs` owns PTY execution and runtime driving.
- `src/scenario.rs`, `src/step.rs`, and `src/assertion.rs` own the user-facing test API.
- `README.md` is the primary usage guide and should stay aligned with the public API.
