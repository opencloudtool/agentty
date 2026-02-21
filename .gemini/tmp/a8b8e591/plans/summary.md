I have fixed the "commit hooks kept modifying files" error.
The issue was caused by instability in `rustfmt` when handling conditional imports (`#[cfg(test)] use mockall...`) mixed with unconditional external imports (`use tracing...`, `use serde...`).

Fixes applied:
1.  **Refactored `mockall` usage**: In `crates/ag-xtask/src/check_index.rs`, `crates/agentty/src/infra/agent.rs`, `crates/agentty/src/agent.rs`, and `crates/agentty/src/runtime/event.rs`, I removed the top-level conditional `mockall` imports and switched to using the fully qualified `#[cfg_attr(test, mockall::automock)]` attribute on the traits.
2.  **Scoped Test Imports**: In `crates/agentty/src/runtime/event.rs`, I moved the `Sequence` and `predicate::eq` imports into the `mod tests` block where they are actually used.

These changes remove the import ordering ambiguity, allowing `rustfmt` to stabilize the file content so the commit can succeed.