# AG-XTASK

Workspace maintenance tasks. This crate serves as the central hub for project automation, replacing fragile shell scripts with robust Rust code.

## How to Extend

1.  **Create a Module:** Add a new module in `src/` for your task (e.g., `src/version_bump.rs`).
2.  **Implement Logic:** Use the `CommandRunner` trait for testable shell interactions.
3.  **Register Command:** Update `main.rs` to dispatch to your new module based on CLI arguments.

## TODOs & Recommendations

- [ ] **Refactor Argument Parsing:** Adopt `clap` when adding the next task to handle subcommands (e.g., `cargo xtask check-indexes`, `cargo xtask lint`) cleanly.
- [ ] **Modularize:** Move the current index-checking logic into `src/tasks/check_index.rs` to keep `main.rs` as a clean entry point.
- [ ] **Shared Library:** If tasks share significant logic, extract a `lib.rs` to house common utilities like git wrappers and file system helpers.

## Directory Index
- [Cargo.toml](Cargo.toml) - Crate configuration and dependencies.
- [src/](src/) - Source code for the xtask binary.
