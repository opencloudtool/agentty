# Agentty

TUI tool to manage agents.

## Project Facts
- Project is a Rust workspace.
- The `crates/` directory contains all workspace members.
- All workspace crates use the `ag-` prefix (e.g., `ag-cli`).
- `ag-cli`: A binary crate providing the CLI interface using Ratatui.

## Rust Project Style Guide
- **Dependency Management:** ALL dependencies (including `dev-dependencies` and `build-dependencies`) must be defined in the root `Cargo.toml` under `[workspace.dependencies]`.
- All workspace crates must use `workspace = true` for shared package metadata and dependencies. Never define a version number inside a crate's `Cargo.toml`.
- **Release Profile:** Maintain optimized release settings in `Cargo.toml` (`codegen-units=1`, `lto=true`, `opt-level="s"`, `strip=true`) to minimize binary size.
- Use `ratatui` for terminal UI development.

## Quality Gates
To ensure code quality, you must pass both automated and manual gates.

### Automated Checks
Run these commands with autofix enabled:
- **Test:** `cargo test`
- **Lint:** `cargo clippy --fix --allow-dirty -- -D warnings`
- **Format:** `cargo fmt --all`
- **Coverage:** `cargo tarpaulin` (install with `cargo install cargo-tarpaulin`)

### Manual Verification
- **Test Style:** Verify *every* test function uses explicit `// Arrange`, `// Act`, and `// Assert` comments.
- **Dependencies:** Verify all dependencies (including dev/build) are defined in the root `Cargo.toml` and referenced via `workspace = true`.

## Git Conventions
- Before committing, review the recent commit history using an optimized command (e.g., `git log -n 5 --format="---%n%B"`) to ensure consistency with the established tone, phrasing, and level of detail while being token-efficient.
- Follow the "commit title and description" style:
  - The first line should be a concise summary (the "title") in present simple tense (e.g., "Fix cursor offset" not "Fixed cursor offset").
  - Use a blank line between the title and the body.
  - The body (the "description") should provide more detail on *why* and *how* in present simple tense. It is not needed when the title is self-explanatory.
- Do not use conventional commit prefixes (e.g., `feat:`, `fix:`).
- Do not add `Co-Authored-By` trailers or any AI attribution to commit messages.

## Agent Instructions
- **MANDATORY:** After every user instruction that establishes a preference, convention, or workflow change (e.g., "run checks with autofix", "use X instead of Y", "always do Z"), immediately update the relevant `AGENTS.md` file so the instruction persists across sessions. If unsure whether something qualifies, update anyway — over-documenting is better than losing context. Both `CLAUDE.md` and `GEMINI.md` are symlinks to `AGENTS.md`, so a single update keeps all AI assistants in sync.
- Always cover all touched code with auto tests to prevent regressions and ensure stability.
- Structure tests using "Arrange, Act, Assert" comments to clearly separate setup, execution, and verification phases.
- When creating a new `AGENTS.md` file in any directory, always create corresponding symlinks: `ln -s AGENTS.md CLAUDE.md && ln -s AGENTS.md GEMINI.md` in the same directory.
- Keep the root `README.md` up to date whenever new information is relevant to end users (e.g., new crates, features, usage instructions, or prerequisites).
