# Agent Manager

Project to manage agents.

## Project Facts
- Project is a Rust workspace.
- The `crates/` directory contains all workspace members.
- `am-cli`: A binary crate providing the CLI interface using Ratatui.

## Rust Project Style Guide
- Dependency versions and project information (version, authors) are managed in the root `Cargo.toml`.
- All workspace crates must use `workspace = true` for shared package metadata and dependencies.
- Use `ratatui` for terminal UI development.

## Quality Gates
To ensure code quality, run the following commands:
- **Test:** `cargo test`
- **Lint:** `cargo clippy -- -D warnings`
- **Format:** `cargo +nightly fmt --all -- --check`

## Git Conventions
- Before committing, review the recent commit history using an optimized command (e.g., `git log -n 5 --format="---%n%B"`) to ensure consistency with the established tone, phrasing, and level of detail while being token-efficient.
- Follow the "commit title and description" style:
  - The first line should be a concise summary (the "title").
  - Use a blank line between the title and the body.
  - The body (the "description") should provide more detail on *why* and *how* the change was made when it's not immediately obvious from the title.
- Do not use conventional commit prefixes (e.g., `feat:`, `fix:`).

## Agent Instructions
- Always update this file and other `GEMINI.md` files (e.g., in sub-crates) whenever new architectural insights, project facts, or significant conventions are established or discovered during conversations. This ensures the project context remains up-to-date for future interactions.
