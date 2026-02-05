# Agentty

TUI tool to manage agents.

## Project Facts
- Project is a Rust workspace.
- The `crates/` directory contains all workspace members.
- All workspace crates use the `ag-` prefix (e.g., `ag-cli`).
- `ag-cli`: A binary crate providing the CLI interface using Ratatui.
- **Workflow**: Agents are run in isolated git worktrees.
- **Review**: Users review changes using the Diff view (`d` key in chat) which shows the output of `git diff` in the session's worktree.
- **Output**: Agent `stdout` and `stderr` are captured in parallel using multiple threads to ensure prompts and errors are visible.

## Rust Project Style Guide
- **Dependency Management:** ALL dependencies (including `dev-dependencies` and `build-dependencies`) must be defined in the root `Cargo.toml` under `[workspace.dependencies]`.
- All workspace crates must use `workspace = true` for shared package metadata and dependencies. Never define a version number inside a crate's `Cargo.toml`.
- **Release Profile:** Maintain optimized release settings in `Cargo.toml` (`codegen-units=1`, `lto=true`, `opt-level="s"`, `strip=true`) to minimize binary size.
- Use `ratatui` for terminal UI development.
- **Constructors:** Only add `new()` and `Default` when there is actual initialization logic or fields with meaningful defaults. For unit structs or zero-field structs, construct directly (e.g., `MyStruct`) — do not add boilerplate `new()` / `Default` impls.
- **Function Ordering:** Order functions to allow reading from top to bottom (caller before callee):
    - Public functions first.
    - Followed by less public functions (e.g., `pub(crate)`).
    - Private functions last.
    - If a function has multiple callees, they should appear in the order they are first called within that function.
- **Imports:** Always place imports at the top of the file. Do not use local `use` statements within functions or other blocks.

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
- **Test Ordering:** Verify tests follow the same order as the functions they test.
- **Dependencies:** Verify all dependencies (including dev/build) are defined in the root `Cargo.toml` and referenced via `workspace = true`.

## Documentation Conventions
- **Code Element Formatting:** Always wrap code elements in backticks (`) when referencing them in documentation, commit messages, PR descriptions, or bullet points:
  - Enum variants: `Sessions`, `Roadmap`
  - Struct/Type names: `RoadmapPage`, `Tab`, `AppMode`
  - Function names: `next_tab()`, `render()`
  - Field names: `current_tab`, `table_state`
  - Key bindings: `Tab`, `Enter`, `Esc`
  - File names: `model.rs`, `AGENTS.md`
  - Configuration values: `workspace = true`
- This improves readability and clearly distinguishes code from prose.

## Git Conventions
- Before committing, review the recent commit history using an optimized command (e.g., `git log -n 5 --format="---%n%B"`) to ensure consistency with the established tone, phrasing, and level of detail while being token-efficient.
- Follow the "commit title and description" style:
  - The first line should be a concise summary (the "title") in present simple tense (e.g., "Fix cursor offset" not "Fixed cursor offset").
  - Use a blank line between the title and the body.
  - The body (the "description") should provide more detail on *why* and *how* in present simple tense. It is not needed when the title is self-explanatory.
- Do not use conventional commit prefixes (e.g., `feat:`, `fix:`).
- Do not add `Co-Authored-By` trailers or any AI attribution to commit messages.

## Git Worktree Integration
Agentty automatically creates isolated git worktrees for sessions when launched from within a git repository:

- **Automatic Behavior:** When `agentty` is launched from a git repository, each new session automatically gets its own git worktree with a dedicated branch.
- **Branch Naming:** Worktree branches follow the pattern `agentty/<hash>`, where `<hash>` is the 16-character session identifier (e.g., `agentty/a1b2c3d4e5f6a7b8`).
- **Base Branch:** The worktree is based on the branch that was active when `agentty` was launched.
- **Location:** Worktrees are created in the session folder (under `/var/tmp/.agentty/<hash>/`), separate from the main repository.
- **Session Creation:** If worktree creation fails (e.g., git not installed, permission errors), session creation fails atomically and displays an error message.
- **Cleanup:** When a session is deleted, its worktree is automatically removed using `git worktree remove --force` and the corresponding branch is deleted.
- **Non-Git Directories:** Sessions in non-git directories work normally without worktrees.

### Cleanup Commands
To manually clean up all agentty branches (if needed):
```bash
# List all agentty branches
git branch | grep agentty/

# Delete all agentty branches
git branch | grep agentty/ | xargs git branch -D

# Prune stale worktree references
git worktree prune
```

## Agent Instructions
- **MANDATORY:** After every user instruction that establishes a preference, convention, or workflow change (e.g., "run checks with autofix", "use X instead of Y", "always do Z"), immediately update the relevant `AGENTS.md` file so the instruction persists across sessions. If unsure whether something qualifies, update anyway — over-documenting is better than losing context. Both `CLAUDE.md` and `GEMINI.md` are symlinks to `AGENTS.md`, so a single update keeps all AI assistants in sync.
- Always cover all touched code with auto tests to prevent regressions and ensure stability.
- Structure tests using "Arrange, Act, Assert" comments to clearly separate setup, execution, and verification phases.
- When creating a new `AGENTS.md` file in any directory, always create corresponding symlinks: `ln -s AGENTS.md CLAUDE.md && ln -s AGENTS.md GEMINI.md` in the same directory.
- Keep the root `README.md` up to date whenever new information is relevant to end users (e.g., new crates, features, usage instructions, or prerequisites).
