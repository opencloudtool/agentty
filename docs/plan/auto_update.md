# Auto-Update

Plan for extending the background version check in `crates/agentty/src/infra/version.rs` and the status bar so `agentty` automatically runs `npm i -g agentty@latest` in the background and shows progress in the header. The new version takes effect on next launch — no re-exec.

## Steps

## 1) Run background npm update when a newer version is detected (single step)

### Why now

The version check already runs asynchronously on startup and the status bar already shows a manual `npm i -g agentty@latest` hint. This step replaces the manual hint with an automatic background `npm i -g agentty@latest` and shows update progress in the status bar.

### Usable outcome

When `agentty` launches and detects a newer npm version, the status bar shows "Updating to vX.Y.Z..." while `npm i -g agentty@latest` runs in the background. On success, it shows "Updated to vX.Y.Z — restart to use new version". On failure, it falls back to the existing manual update hint. No user intervention required.

### Substeps

- [x] **Add `UpdateRunner` trait boundary.** Define `#[cfg_attr(test, mockall::automock)] trait UpdateRunner: Send + Sync` in `crates/agentty/src/infra/version.rs` with `fn run_update(&self, command: &str, args: Vec<String>) -> Result<String, String>`. The production implementation shells out via `std::process::Command` inside `spawn_blocking`.
- [x] **Add update status to `AppEvent`.** Extend the version/update event model in `crates/agentty/src/app/core.rs` so the reducer can track three states: `UpdateInProgress { version }`, `UpdateComplete { version }`, and `UpdateFailed { version }`. These drive the status bar text.
- [x] **Run background update in version check task.** Extend `TaskService::spawn_version_check_task()` in `crates/agentty/src/app/task.rs`: after detecting a newer version, emit `UpdateInProgress`, then run `npm i -g agentty@latest` via `UpdateRunner` in the same background task, then emit `UpdateComplete` or `UpdateFailed`.
- [x] **Update status bar to show update progress.** Modify `StatusBar` in `crates/agentty/src/ui/component/status_bar.rs` to render three states: "Updating to vX.Y.Z..." (in progress), "Updated to vX.Y.Z — restart to use new version" (success), or the existing manual update hint (failure).
- [x] **Add `--no-update` CLI flag.** Add the flag to the CLI args in `crates/agentty/src/main.rs`. When set, skip the update step (still check version, but do not auto-run npm install).

### Tests

- [x] Unit tests for `UpdateRunner` production implementation with a mock command.
- [x] Unit tests for the version check task emitting the correct sequence of events (in-progress → complete/failed) using `MockUpdateRunner` and `MockVersionCommandRunner`.
- [x] Update existing status bar render tests to verify all three update states render correctly.
- [x] Unit test verifying `--no-update` flag prevents the update command from running while still showing the manual hint.

### Docs

- [x] Update `docs/site/content/docs/usage/workflow.md` with auto-update behavior: background check, status bar progress, restart to use new version.
- [x] Update `docs/site/content/docs/getting-started/overview.md` mentioning auto-update capability.
- [x] Update `docs/site/content/docs/architecture/testability-boundaries.md` with the `UpdateRunner` trait boundary.
- [x] Update `README.md` with `--no-update` flag documentation.

## Cross-Plan Notes

- No conflicts with active plans. The version check infrastructure in `infra/version.rs` is touched only by this plan.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its checklist status and refresh the snapshot rows that changed.
- When a step changes behavior, complete its `### Tests` and `### Docs` work in that same step before marking it complete.
- When the full plan is complete, remove the implemented plan file; if more work remains, move that work into a new follow-up plan file before deleting the completed one.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Version detection | `infra/version.rs` checks npm for latest version | Exists, extend |
| Update notification | Status bar shows hardcoded `npm i -g agentty@latest` text | Exists, replace with progress states |
| Update execution | Not implemented | New |

## Implementation Approach

- Single step: add `UpdateRunner`, wire background npm install into the existing version check task, and update the status bar to show progress.
- No cooldown needed — the version check and update run in a background async task, so they never block startup or the TUI. The npm registry handles typical launch frequency without issues.

## Out of Scope for This Pass

- Non-npm installation methods (cargo-install, cargo-dist). Can be added later.
- Re-exec / automatic restart (user relaunches manually).
- In-TUI update confirmation overlay or `/update` slash command.
- Update channels (stable/beta/nightly).
- Rollback to previous version on update failure.
- Windows support (no current target platform).
- Update cooldown (background async approach makes it unnecessary).
