# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.1.6] - 2026-02-10

### Added
- **Session Status:** Added a `Committing` status to make commit progress explicit in the session lifecycle.

### Changed
- **Persistence:** Persist session prompt/output history in SQLite and load it on startup so chat history survives app reloads.
- **Session Output:** Parse agent JSON output and display only the response message in session output.
- **GitHub Integration:** Parse GitHub PR responses using typed serde structs and move GitHub CLI logic into a dedicated `gh` module.
- **PR Workflow:** Treat closed pull requests as canceled sessions and show a loader while PR creation is in flight.
- **Commit Flow:** Improve asynchronous session commit handling and remove placeholder commit output in view mode.
- **Documentation:** Extract git commit guidance into the shared skills documentation.

### Fixed
- **Tests:** Stabilized merge cleanup testing to avoid environment-dependent blocking during release verification.

## [v0.1.5] - 2026-02-08

### Added
- **Tests:** Added runtime mode handler coverage tests.
- **Documentation:** Added local `AGENTS.md` files and enforced folder index checks.
- **Documentation:** Added Context7-first rule for retrieving latest tool info.
- **Documentation:** Documented dependency injection testability guidance.

### Changed
- **Architecture:** Modularized app and runtime into focused modules (`app/` and `runtime/`).
- **Runtime:** Injected event source into the runtime event loop for better testability.
- **Session:** Made agent and model configurations session-scoped.
- **Linting:** Refined clippy lint configuration, tightening policies and re-enabling pedantic rules.
- **Skills:** Symlinked the entire skills directory for agents and refactored release skill.
- **Refactor:** Refactored long handlers to enforce clippy line limits.

## [v0.1.4] - 2026-02-08

### Added
- **Session Identity:** Migrated session IDs to UUIDs for stable identification.
- **Session Management:** Added a forward-only migration system for schema changes.
- **UI:** Added nullable title support to sessions.
- **UI:** Improved chat input with indentation preservation on wrapped lines.

### Changed
- **Session Ordering:** Sessions are now strictly ordered by `updated_at` (latest first).
- **Performance:** Implemented incremental session state refresh to reduce database load.
- **UX:** Moved prompt cursor by visual wrapped lines for better navigation.
- **Internal:** Use `String` directly for session IDs in `AppMode` and command flows.
- **Internal:** Refactored health checks into flat pass/fail checks.
- **Database:** Manage session timestamps directly in SQLite.
- **Database:** Use multiline SQL strings for better query readability.

### Removed
- Removed project-filtered session loader.
- Removed git worktree suffix from initial session prompt.
- Removed Reply mode; unified into session chat page.

## [v0.1.3] - 2026-02-08

### Added
- **Backends:** Added Codex backend support.
- **Project Management:** Added project switching with automatic sibling discovery.
- **Diff View:** Show all file changes in diff view.
- **Status:** Show status as text in session list and chat title.
- **Health:** Added version normalization for agent checks.
- **Testing:** Added `pr-testing` directory to root index.

### Changed
- **Concurrency:** Converted event loop to async to fix TUI freezing on macOS.
- **Input:** Improved multiline input editing.
- **Workflow:** Enforced review-based session status transitions.
- **Performance:** Reduced tick rate to 50ms for smoother output.
- **Locking:** Replaced `fs2` with `std` file locking.
- **Formatting:** Added code formatting rules and applied to `ag-xtask`.

### Fixed
- Fixed UI freezing on macOS during agent execution.
- Clarified git worktree requirements in README.

## [v0.1.2] - 2026-02-08

### Added
- **GitHub Integration:** Added 'p' command to create GitHub Pull Requests (draft by default).
- **GitHub Integration:** Added GitHub CLI health check with nested auth sub-check.
- **UI:** Centralized icons into a reusable `Icon` enum.
- **UI:** Improve command palette with arrow navigation and auto-select.
- **Database:** Persist session status to the database.

### Changed
- **UX:** Use `/` selector in command palette dropdowns.
- **UX:** Ensure exactly one blank line before the spinner in chat view.
- **Health:** Rename Claude health check label to Claude Code.
- **Internal:** Refactor PR creation logic and tests.
- **Internal:** Optimize quality gates for AI agents.

### Removed
- Remove custom Gemini configuration creation.

## [v0.1.1] - 2026-02-08

### Added
- **Database:** Introduce SQLite via SQLx for session metadata.
- **UI:** Add command palette with agents selection.
- **UI:** Add health check splash screen via `/health` command.
- **UI:** Add git status indicator to footer bar.
- **Docs:** Add installation guide to README.

### Changed
- **Async:** Convert sync DB wrapper and thread spawns to native async.
- **Tooling:** Replace `cargo-machete` with `cargo-shear` in quality gates.
- **UI:** Use tilde for home directory in footer.
- **Internal:** Reorder struct fields by visibility and name.

## [v0.1.0] - 2026-02-08

- Initial release.
