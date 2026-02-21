# Agentty

TUI tool to manage agents.

# MANDATORY

> **STOP! Read this section before proceeding.**
> These rules are absolute and take precedence over all others.

- **Document Code:** Document all added or updated code using docstrings. When touching existing code, add or refresh docstrings so the changed behavior is clearly described.
- **Update AGENTS.md:** Update the relevant `AGENTS.md` file only when a user instruction establishes a critical, persistent preference, convention, or workflow rule. Do not update it for one-off tasks.
- **Directory Indexing:** Maintain the "Directory Index" section in the local `AGENTS.md`. If you create, rename, or delete a file/directory, update the index immediately.
- **Context First:** Before listing a directory or reading source code, ALWAYS read the local `AGENTS.md` first. This provides immediate context on the folder structure and file purposes, reducing the need for broad discovery actions.
- **Context7 First:** If Context7 is connected as an MCP server, use it to retrieve the latest documentation and API details for the tools and libraries used in the task.

## Project Facts

- Project is a Rust workspace.
- The `crates/` directory contains all workspace members.
- All workspace crates use the `ag-` prefix (e.g., `ag-xtask`).
- `agentty`: A binary crate providing the CLI interface using Ratatui.
- **Workflow**: Agents are run in isolated git worktrees.
- **Review**: Users review changes using the Diff view (`d` key in chat) which shows the output of `git diff` in the session's worktree.
- **Output**: Agent `stdout` and `stderr` are captured in parallel using `tokio` tasks to ensure prompts and errors are visible.

## Rust Project Style Guide

- **Dependency Management:** ALL dependencies (including `dev-dependencies` and `build-dependencies`) must be defined in the root `Cargo.toml` under `[workspace.dependencies]`.
- All workspace crates must use `workspace = true` for shared package metadata and dependencies. Never define a version number inside a crate's `Cargo.toml`.
- **Release Profile:** Maintain optimized release settings in `Cargo.toml` (`codegen-units=1`, `lto=true`, `opt-level="s"`, `strip=true`) to minimize binary size.
- Use `ratatui` for terminal UI development.
- **Constructors:** Only add `new()` and `Default` when there is actual initialization logic or fields with meaningful defaults. For unit structs or zero-field structs, construct directly (e.g., `MyStruct`) — do not add boilerplate `new()` / `Default` impls.
- **Constructors:** Prefer `Type::new(...)` associated constructors over standalone helper functions when constructing that type.
- **Function Ordering:** Order functions to allow reading from top to bottom (caller before callee):
  - Public functions first.
  - Followed by less public functions (e.g., `pub(crate)`).
  - Private functions last.
  - If a function has multiple callees, they should appear in the order they are first called within that function.
- **File Naming:** Use **singular** names for Rust source files (e.g., `model.rs`, `icon.rs`, `agent.rs`). Do not use plural forms.
- **Imports:** Always place imports at the top of the file. Do not use local `use` statements within functions or other blocks.
  - In test modules, prefer `use super::*;` where practical.
- **Test-only code placement:** Do not add `#[cfg(test)]` to top-level imports/functions in production modules. Keep test-only helpers inside `#[cfg(test)] mod tests` (duplicate code there if needed). Exception: `#[cfg_attr(test, mockall::automock)]` on traits used for mocking.
- **Struct Fields:** Order fields in structs as follows:
  - Public fields first.
  - Private fields second.
  - Within each group, sort fields alphabetically.
- **Clippy Compliance:** Do not bypass clippy rules with `#[allow()]`. Adopt the solution that complies with the rule.
- **Code Grouping:** Within functions, separate related code blocks with empty lines. Group lines that belong together logically and add blank lines between distinct groups.
- **Return Spacing:** Always add an empty line before return statements, both explicit (`return`) and implicit (last expression). Exception: single-line blocks where the return is the only statement.
- **Impl Placement:** Place each standalone/inherent `impl StructName { ... }` block immediately below its `struct` declaration, then place trait impls (e.g., `impl Trait for StructName`) after it.
- **Helper Placement:** Place helper functions used by only one struct inside that struct’s `impl`; keep only shared helpers at file scope.
- **Testability via DI:** Prefer trait-based dependency injection for external boundaries (terminal events, git/process calls, clocks/timers, filesystem, network) so logic can be tested deterministically.
  - Keep runtime wiring in production implementations and inject trait objects/generics into orchestration functions.
  - Use `#[cfg_attr(test, mockall::automock)]` on internal traits where mocking is needed.
  - Prefer testing behavior through injected fakes/mocks over end-to-end terminal/process dependencies when unit coverage is the goal.

## Database Standards (SQLx + SQLite)

### 1. Stack & Pattern

- **Driver:** `sqlx` (Feature: `sqlite`).
- **Runtime:** `tokio`.
- **Pattern:** Repository pattern or direct service-layer queries. **No ORM**.
- **Safety:** Prefer compile-time checked macros (`query!`, `query_as!`).
  - *Requirement:* `.sqlx` directory must be committed for offline compilation (CI/CD).
- **Concurrency:** Must enable **WAL Mode** (Write-Ahead Logging) for concurrent readers/writers.

### 2. Naming Conventions (Strict)

- **Tables:** `snake_case`, **SINGULAR** (e.g., `user`, `order_item`).
  - *Rationale:* Matches Rust struct names exactly (`User` -> `user`).
- **Columns:** `snake_case`.
  - **PK:** `id` (`INTEGER PRIMARY KEY AUTOINCREMENT`).
  - **FK:** `{table}_id` (e.g., `user_id`).
  - **Booleans:** Prefix with `is_`, `has_` (Stored as `INTEGER`, mapped to `bool`).
  - **Timestamps:** `{action}_at` (Stored as `INTEGER` (Unix) or `TEXT` (ISO8601)).
- **Rust Structs:**
  - Name: Singular, PascalCase (e.g., `User`).
  - Fields: `snake_case` (Matches DB columns 1:1).

### 3. Implementation Guidelines

1. **Configuration:**
   - Set `PRAGMA foreign_keys = ON;` (SQLite defaults to OFF).
   - Set `PRAGMA journal_mode = WAL;` (Crucial for performance).
1. **Migrations:** Embedded at compile time via `sqlx::migrate!()`.
   - Place SQL files in `crates/<crate>/migrations/` named `NNN_description.sql`.
   - Migrations run automatically on database open; no external CLI required.
   - Never modify existing migration files. Always add a new migration file for every schema change.
   - If `SQLite` cannot alter a structure in place (for example, changing a primary key), use a new migration that drops and recreates the table.
1. **Dependency Injection:** Pass `&sqlx::SqlitePool` to functions.
   - *Note:* SQLite handles cloning the pool cheaply.
1. **Error Handling:** Map `sqlx::Error` to domain-specific errors.

## Async Runtime (Tokio)

The project uses `tokio` as its async runtime. The binary entry point uses `#[tokio::main]` and all I/O-bound operations are async.

### Feature Selection

- **NEVER** use `features = ["full"]`. The project optimizes for binary size — only enable the specific features you need.
- When adding a new tokio API, check which feature flag it requires and add only that flag.

### Mutex Selection: `std::sync::Mutex` vs `tokio::sync::Mutex`

- **Default to `std::sync::Mutex`** unless you need to hold the lock across an `.await` point.
- `tokio::sync::Mutex` is only needed when the critical section itself contains `.await` calls (e.g., async file I/O, async network calls).
- If the critical section is purely synchronous (e.g., `writeln!` to a `std::fs::File`, pushing to a `String`), use `std::sync::Mutex` even inside async functions. It is cheaper and avoids unnecessary async overhead.
- **Wrong:** `Arc<tokio::sync::Mutex<std::fs::File>>` with `file.lock().await` followed by sync `writeln!`.
- **Right:** `Arc<std::sync::Mutex<std::fs::File>>` with `file.lock().ok()` followed by sync `writeln!`.

### Blocking Operations

- Use `tokio::task::spawn_blocking` for operations that block the thread (e.g., shelling out to `git` via `std::process::Command`).
- Do **not** call blocking functions directly in async contexts — it starves the tokio worker threads.
- For subprocess management where you need async streaming of stdout/stderr, use `tokio::process::Command` instead.

### Variable Cloning for `move` Closures

- When cloning variables for `spawn_blocking` or `tokio::spawn` closures, prefer **variable shadowing** or **scoped blocks** over `_clone` suffixes.
- **Wrong:** `let folder_clone = folder.clone(); let root_clone = root.clone();`
- **Right (shadowing):** `let folder = folder.clone();`
- **Right (scoped block):** Wrap the `spawn_blocking` call in a block so the originals remain available after:
  ```rust
  {
      let source = source_branch.clone();
      tokio::task::spawn_blocking(move || do_work(&source)).await??;
  }
  // source_branch is still usable here
  ```

### Tests

- Use `#[tokio::test]` for async test functions, not `#[test]`.
- All `sqlx` operations are async and require `.await`.
- For sleep/delays in tests, use `tokio::time::sleep` instead of `std::thread::sleep`.

### Anti-Patterns to Avoid

- **No sync wrappers:** Do not wrap async code in `Runtime::new()` + `block_on()`. The codebase is fully async — keep it that way.
- **No `features = ["full"]`:** Always specify individual tokio features.
- **No `tokio::sync::Mutex` for sync-only guards:** Only use it when the critical section contains `.await`.

## Quality Gates

To ensure code quality, you must pass both automated and manual gates.

### Automated Checks

Run these commands after making changes:

1. **Autofix:** `pre-commit run rustfmt-fix --all-files --hook-stage manual && pre-commit run clippy-fix --all-files --hook-stage manual`
1. **Validate:** `pre-commit run --all-files`
1. **Test:** `cargo test -q`

The manual-stage autofix hooks apply formatting and fixable clippy lints. The
validation command then runs non-mutating checks (including formatting and clippy
lint gates), dependency checks, compilation, and directory index checks with
minimal output (errors only).

### Manual Verification

- **Test Style:** Verify *every* test function uses explicit `// Arrange`, `// Act`, and `// Assert` comments.
  - Combining `Arrange`, `Act`, and `Assert` is allowed when it improves clarity (for very small tests).
- **Test Ordering:** Verify tests follow the same order as the functions they test.
- **Dependencies:** Verify all dependencies (including dev/build) are defined in the root `Cargo.toml` and referenced via `workspace = true`.

## Documentation Conventions

- **Code Element Formatting:** Always wrap code elements in backticks (\`) when referencing them in documentation, commit messages, PR descriptions, or bullet points:
  - Enum variants: `Sessions`, `Roadmap`
  - Struct/Type names: `RoadmapPage`, `Tab`, `AppMode`
  - Function names: `next_tab()`, `render()`
  - Field names: `current_tab`, `table_state`
  - Key bindings: `Tab`, `Enter`, `Esc`
  - File names: `model.rs`, `AGENTS.md`
  - Configuration values: `workspace = true`
- This improves readability and clearly distinguishes code from prose.
- **Rust Docs:** Add `///` doc comments to structs and all public functions/types in touched Rust files.
- **Contextual Docs:** When touching a file for code changes and updating docs, also add or refresh missing/stale doc comments for related sibling and parent elements (for example `struct`, `enum`, `impl`, and closely related items) when needed for clarity.

## Git Conventions

- For all commit preparation and commit message work, use `skills/git-commit/SKILL.md`.
- **Tagging:** Always use the `v` prefix for version tags (e.g., `v0.1.0`).

## Git Worktree Integration

Agentty automatically creates isolated git worktrees for sessions when launched from within a git repository:

- **Automatic Behavior:** When `agentty` is launched from a git repository, each new session automatically gets its own git worktree with a dedicated branch.
- **Branch Naming:** Worktree branches follow the pattern `agentty/<hash>`, where `<hash>` is the first 8 characters of the session UUID (e.g., `agentty/a1b2c3d4`).
- **Base Branch:** The worktree is based on the branch that was active when `agentty` was launched.
- **Location:** Worktrees are created in the session folder (under `~/.agentty/wt/<hash>/`), separate from the main repository.
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

- **Pragmatic Abstractions:** Introduce new abstractions only when they provide clear payoff (reuse, reduced complexity, or materially better testability). For straightforward changes, prefer direct in-place edits with minimal diff.
- **Test Coverage:** Try to maintain 100% test coverage when it makes sense. Ensure critical logic is always covered, but pragmatic exceptions are allowed for boilerplate or untestable I/O.
- **Readability:** Use descriptive variable names. Do NOT use single-letter variables (e.g., `f`, `p`, `c`) or single-letter prefixes. Code should be self-documenting.
- Always cover all touched code with auto tests to prevent regressions and ensure stability.
- Structure tests using "Arrange, Act, Assert" comments to clearly separate setup, execution, and verification phases.
- When creating a new `AGENTS.md` file in any directory, always create corresponding symlinks: `ln -s AGENTS.md CLAUDE.md && ln -s AGENTS.md GEMINI.md` in the same directory.
- Keep the root `README.md` up to date whenever new information is relevant to end users (e.g., new crates, features, usage instructions, or prerequisites).
- **Changelog:** Update `CHANGELOG.md` when releasing a new version. Follow the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.

## Skills

- Skills are available under `skills/`, with the canonical index in `skills/AGENTS.md`.
- Read `skills/AGENTS.md` to discover available skills before selecting one.
- Activate a skill when the user explicitly names it or the task intent matches the skill description.
- Use the minimal set of skills needed for the current turn.
- Do not carry a skill across turns unless it is explicitly requested again or clearly re-triggered by intent.

## Directory Index

- [.claude/](.claude/) - Claude AI specific settings.
- [.codex/](.codex/) - Codex AI specific settings.
- [.gemini/](.gemini/) - Gemini AI specific settings.
- [.git-town.toml](.git-town.toml) - Git Town configuration.
- [.github/](.github/) - GitHub configuration files.
- [.gitignore](.gitignore) - Git ignore patterns.
- [.pre-commit-config.yaml](.pre-commit-config.yaml) - Pre-commit hooks configuration.
- [.rustfmt.toml](.rustfmt.toml) - Rustfmt configuration.
- [clippy.toml](clippy.toml) - Clippy lint configuration.
- [crates/](crates/) - Workspace member crates.
- [docs/](docs/) - Documentation and assets.
- [AGENTS.md](AGENTS.md) - Context and instructions for AI agents.
- [Cargo.lock](Cargo.lock) - Exact version pins for dependencies.
- [Cargo.toml](Cargo.toml) - Workspace root configuration and dependency definitions.
- [CHANGELOG.md](CHANGELOG.md) - Project changelog.
- [CLAUDE.md](CLAUDE.md) - Symlink to AGENTS.md.
- [dist-workspace.toml](dist-workspace.toml) - Release configuration for cargo-dist.
- [GEMINI.md](GEMINI.md) - Symlink to AGENTS.md.
- [LICENSE](LICENSE) - Project license file.
- [pr-testing/](pr-testing/) - Files for pull request testing workflows.
- [README.md](README.md) - Main project documentation.
- [rust-toolchain.toml](rust-toolchain.toml) - Rust toolchain version pinning.
- [skills/](skills/) - Shared agent skills.
