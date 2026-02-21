# AG-XTASK

Workspace maintenance tasks. This crate serves as the central hub for project automation, replacing fragile shell scripts with robust Rust code.

## How to Extend

1. **Create a Module:** Add a new module in `src/` for your task (e.g., `src/version_bump.rs`).
1. **Implement Logic:** Use the `CommandRunner` trait for testable shell interactions.
1. **Register Command:** Add a new variant to the `Command` enum in `main.rs` and dispatch to your module.

## TODOs & Recommendations

- [ ] **Shared Library:** If tasks share significant logic, extract a `lib.rs` to house common utilities like git wrappers and file system helpers.

## Directory Index

- [Cargo.toml](Cargo.toml) - Crate configuration and dependencies.
- [src/](src/) - Source code for the xtask binary.
