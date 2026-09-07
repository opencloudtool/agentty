# Workspace Crates

`crates/` contains the Rust workspace members. Treat manifests and module roots as the
source of truth for the member and module inventory.

When adding or removing a crate required by a published workspace crate, update
`.github/workflows/publish-crates-io.yml` in the same change and keep dependencies
before dependents in the publish plan.

## Rust Conventions

### Workspace and Modules

- Define all dependencies, including development and build dependencies, under
  `[workspace.dependencies]` in the root `Cargo.toml`. Member manifests use
  `workspace = true` for shared package metadata and dependencies.
- Use singular Rust file names. For nested modules, prefer `module.rs` plus `module/`;
  do not add `mod.rs`.
- A `module.rs` paired with `module/` is router-only: declarations and re-exports, with
  no runtime types, constants, functions, or implementations.

### Readability

- Order code for top-to-bottom reading: public before restricted before private, with
  callees ordered by first use.
- Keep imports at file scope. Prefer module-oriented internal imports, use direct item
  imports only when clearer, and do not mix imported-module and fully qualified styles.
  In tests, prefer `use super::*;`.
- Add `new()` or `Default` only for meaningful initialization. Prefer associated
  constructors over free construction helpers.
- Put an inherent `impl` directly below its struct and trait implementations after it.
  Keep helpers used by one type inside that type's `impl`.
- Put public struct fields before private fields and alphabetize within each group.
- Use descriptive names; avoid single-letter names and near-identical names in one
  scope.
- Separate logical blocks with blank lines, including before explicit or implicit
  returns except in a one-expression block.
- Introduce abstractions only for reuse, reduced complexity, or testability. Inline
  pass-through wrappers that add no behavior or boundary.
- Do not silence Clippy with `#[allow(...)]`; resolve the underlying issue.

### Tests and Boundaries

- Give every touched test explicit `// Arrange`, `// Act`, and `// Assert` sections;
  combine labels only when that improves a very small test.
- Keep test-only code inside `#[cfg(test)] mod tests` unless it belongs to an
  established shared test-support surface. Mockable traits may use
  `#[cfg_attr(test, mockall::automock)]`.
- Keep a real test for an isolated external command. For flows with multiple external
  calls, inject a trait boundary and use deterministic mocks.
- Reuse named fixtures, builders, and expectation helpers instead of duplicating test
  setup. Do not expose production APIs solely to share test fixtures.
- Route process, filesystem, network, terminal, and time access through injected
  boundaries in orchestration code.
- When removing behavior, test the remaining supported path rather than only asserting
  that the old path is absent.

## Tokio

- Keep the codebase async; do not create a runtime merely to call `block_on()`.
- Enable only required Tokio features, never `full`.
- Use `tokio::process::Command` for streamed subprocesses and
  `tokio::task::spawn_blocking` for blocking synchronous work.
- Prefer variable shadowing or a scoped block when cloning values into spawned `move`
  closures.
- Use `#[tokio::test]` for async tests and `tokio::time::sleep` for async delays.

### Mutex Selection

- Default to `std::sync::Mutex`; use `tokio::sync::Mutex` only when the protected
  critical section itself awaits.
- Never hold an async mutex merely to perform synchronous work such as writing to a
  `std::fs::File`.
