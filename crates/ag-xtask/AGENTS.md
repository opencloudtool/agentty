# AG-XTASK

Workspace maintenance tasks. This crate serves as the central hub for project automation, replacing fragile shell scripts with robust Rust code.

## Key Commands

- `check-migrations` validates SQL migration numbering across workspace crates.
- `workspace-map` writes `target/agentty/workspace-map.json` for tooling and agent exploration.

## How to Extend

1. **Create a Module:** Add a new module in `src/` for your task (e.g., `src/version_bump.rs`).
1. **Implement Logic:** Use the `CommandRunner` trait for testable shell interactions.
1. **Register Command:** Add a new variant to the `Command` enum in `main.rs` and dispatch to your module.

## Change Guidance

- Prefer generated outputs for raw structure over hand-maintained inventories.
- Keep maintenance tasks deterministic and suitable for local developer tooling.
