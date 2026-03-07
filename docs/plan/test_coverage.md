# Test Coverage Improvement Plan

Workspace-level test coverage uplift plan aligned with the current Rust workspace in `crates/agentty` and `crates/ag-xtask`.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its status in this document (for example, mark checklist items, update snapshot rows, and adjust target metrics).

## Current State Snapshot

Baseline measured on March 6, 2026 using `cargo llvm-cov --workspace --json --summary-only`.

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Workspace line coverage | 85.23% (`32038/37589`) | Baseline captured |
| Workspace function coverage | 83.11% (`3606/4339`) | Baseline captured |
| `crates/agentty` | 85.08% line coverage (`31368/36870`) | Baseline captured |
| `crates/ag-xtask` | 93.18% line coverage (`670/719`) | Healthy |
| Zero-coverage files | Added focused tests for `ui/router.rs`, `ui/render.rs`, and `runtime/core.rs`; coverage refresh pending | Implemented |
| Large uncovered totals | Added targeted lifecycle/error-path tests for `infra/codex_app_server.rs`, `infra/gemini_acp.rs`, `infra/app_server_transport.rs`, and `app/session/workflow/merge.rs`; coverage refresh pending | Implemented |
| External boundary error paths | Added deterministic retry/error-path coverage for `infra/git/rebase.rs` and command sequencing/failure coverage for `infra/tmux.rs`; coverage refresh pending | Implemented |
| Coverage policy in CI | No ratcheting line/function threshold in routine checks | Not started |

## Updated Priorities

## 1) Add tests for zero-coverage entry wiring

**Why now:** These files represent core orchestration surfaces currently unexercised by tests.

- [x] Add focused tests for `crates/agentty/src/ui/router.rs`.
- [x] Add focused tests for `crates/agentty/src/ui/render.rs`.
- [x] Add focused tests for `crates/agentty/src/runtime/core.rs`.
- [x] Assert behavior at integration boundaries instead of only smoke-compiling paths.

Primary files:

- `crates/agentty/src/ui/router.rs`
- `crates/agentty/src/ui/render.rs`
- `crates/agentty/src/runtime/core.rs`

## 2) Close high-impact uncovered lines in protocol/runtime integrations

**Why now:** These modules have the largest absolute uncovered line totals and carry high behavior risk.

- [x] Expand turn lifecycle and error-path tests in `crates/agentty/src/infra/codex_app_server.rs`.
- [x] Expand session and permission-flow tests in `crates/agentty/src/infra/gemini_acp.rs`.
- [x] Add transport parsing/error handling tests in `crates/agentty/src/infra/app_server_transport.rs`.
- [x] Increase branch-path coverage for complex merge assistance in `crates/agentty/src/app/session/workflow/merge.rs`.

Primary files:

- `crates/agentty/src/infra/codex_app_server.rs`
- `crates/agentty/src/infra/gemini_acp.rs`
- `crates/agentty/src/infra/app_server_transport.rs`
- `crates/agentty/src/app/session/workflow/merge.rs`

## 3) Raise runtime UI mode coverage on edge transitions

**Why now:** Prompt/session-view mode logic is high-traffic and has many conditional branches.

- [ ] Add edge-case tests for `crates/agentty/src/runtime/mode/prompt.rs` (history navigation, mixed modifiers, slash flows).
- [ ] Add additional state-transition tests for `crates/agentty/src/runtime/mode/session_view.rs` (status gating, focused review toggles, diff/open actions).
- [ ] Increase key handling fallback-path assertions in `crates/agentty/src/runtime/key_handler.rs`.

Primary files:

- `crates/agentty/src/runtime/mode/prompt.rs`
- `crates/agentty/src/runtime/mode/session_view.rs`
- `crates/agentty/src/runtime/key_handler.rs`

## 4) Strengthen external command boundary tests with deterministic mocks

**Why now:** Lower-coverage external boundary modules are risk-prone and can be tested cheaply with DI/mocks.

- [x] Increase failure/retry path coverage in `crates/agentty/src/infra/git/rebase.rs`.
- [x] Add additional command failure and parsing tests for `crates/agentty/src/infra/tmux.rs`.
- [x] Ensure multi-command flows rely on trait boundaries and `mockall`-based mocks for deterministic behavior.

Primary files:

- `crates/agentty/src/infra/git/rebase.rs`
- `crates/agentty/src/infra/tmux.rs`

## 5) Introduce coverage ratchet policy

**Why now:** Prevents regression while incremental improvements land.

- [ ] Add a coverage check command in project automation with `--fail-under-lines` and `--fail-under-functions`.
- [ ] Start thresholds at current baseline (for example lines `85`, functions `83`).
- [ ] Increase thresholds gradually per release cycle after each improvement batch.
- [ ] Document the coverage workflow in contributor-facing docs if command usage changes.

Primary files:

- `Cargo.toml`
- `.pre-commit-config.yaml`
- `CONTRIBUTING.md`

## Suggested Execution Order

1. Add coverage for zero-coverage files (`ui/router.rs`, `ui/render.rs`, `runtime/core.rs`).
1. Tackle highest uncovered-count modules (`infra/codex_app_server.rs`, `app/session/workflow/merge.rs`, `runtime/mode/prompt.rs`, `runtime/mode/session_view.rs`).
1. Expand deterministic tests for external command boundaries (`infra/git/rebase.rs`, `infra/tmux.rs`).
1. Add/enable coverage ratchet thresholds in automation.
1. Re-run full coverage and refresh this snapshot table with new metrics.

## Out of Scope for This Pass

- Refactoring production architecture solely to improve coverage metrics.
- Adding fragile network-dependent end-to-end tests that require live provider credentials.
- Pursuing 100% line coverage for low-risk display-only branches when risk-adjusted value is low.
