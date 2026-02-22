# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.1.14] - 2026-02-21

### Added

- **Stats:** Add activity heatmap to the Stats tab.
- **Stats:** Track per-model session usage and render usage summaries.
- **Settings:** Add settings tab and persist default model.
- **Diff View:** Split diff view into file list and content panels with file explorer navigation.
- **Diff View:** Render changed files as a tree in the file explorer.
- **Diff View:** Filter diff view content by selected file explorer item.
- **Site:** Add agentty.xyz documentation site with GitHub Pages deployment workflow.

### Changed

- **Architecture:** Refactor codebase into domain, infrastructure, and UI state modules.
- **Architecture:** Move tab state into a dedicated tab manager.
- **Session List:** Group sessions by merge queue and separate archived sessions with placeholders.
- **Session List:** Align session navigation with grouped list order.
- **Session Output:** Render session output and user prompt blocks as markdown.
- **Session Output:** Preserve multiline user prompt block spacing and verbatim rendering.
- **Merge Queue:** Queue session merges in FIFO order and handle queued sessions across app and UI.
- **Merge Queue:** Advance merge queue progression and retry on git index lock failures.
- **Merge:** Treat already-applied squash merges as successful.
- **Rebase:** Harden rebase assist loop against partially resolved conflicts.
- **Output:** Task service batches streamed output before flushing.
- **Output:** Separate streamed response messages for Codex output spacing.
- **Models:** Load default session model from persisted setting.
- **Models:** Use npm semver for version checks and restore version display in status bar.
- **Prompt:** Handle multiline paste and control-key newlines in prompt input.
- **Site:** Redesign landing page with dark terminal theme, Tailwind CSS v4, and theme selector.
- **Deps:** Bump dependency versions.

### Fixed

- **Build:** Fix refactor regressions and restore build stability after module restructure.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.1.13] - 2026-02-19

### Added

- **Session Output:** Render styled markdown in session chat output.
- **Session Output:** Switch to stream-json output and parse Gemini stream events.
- **Session Output:** Extract session output into dedicated UI component.
- **Update Check:** Show update availability in status bar and onboarding page.
- **Models:** Update Gemini Pro to version 3.1 and Claude Sonnet to version 4.6.
- **Models:** Add verbose flag to Claude stream-json commands.

### Changed

- **Session Metadata:** Move session status to output panel title and metadata to chat input border.
- **Session Titles:** Persist session title and summary from squash commit message.
- **Session Titles:** Use full prompt as session title for new sessions.
- **Session Replay:** Replay session transcript once after model switch.
- **Git Actions:** Remove session commit count and always show git actions.
- **Diff View:** Use merge-base for session diff to accurately exclude base branch updates.
- **Rebase:** Refactor rebase logic into a reusable workflow.
- **Database:** Make session token stats non-nullable with zero defaults.
- **NPM:** Update package name to `agentty` in docs and badges.

### Fixed

- **UI:** Fix session list table column layout constraints.
- **Runtime:** Add shutdown signal to event reader thread for cleaner exit.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.12] - 2026-02-19

### Added

- **Session UX:** Added a delete confirmation mode with selectable actions for session deletion.
- **Output Streaming:** Added a live single-line progress indicator in chat and spacing before the first streamed response chunk.
- **Agent Runtime:** Added Codex output streaming during non-interactive runs and follow-up actions for plan mode replies.

### Changed

- **Git Runtime:** Completed async `git` module transition to `spawn_blocking` and updated call sites.
- **Session Model:** Refactored sessions to derive `AgentKind` from `AgentModel`, removed the session `agent` column, and migrated legacy PR statuses to `Review`.
- **Merge/Rebase:** Improved merge and rebase robustness by auto-committing pending changes before merge/rebase and broadening auto-commit assistance handling.
- **UI:** Improved session list layout with minimum-width columns and title truncation, and added spacing around user input in session chat output.
- **Automation:** Split pre-commit workflow into separate autofix and validation phases.
- **Config:** Removed `npm-scope` from `dist-workspace.toml`.

### Removed

- **Pull Requests:** Removed pull request functionality.
- **UI Cleanup:** Removed delete confirmation bottom hints.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.11] - 2026-02-16

### Added

- **Permissions:** Add per-session permission mode toggle and `Plan` permission mode with denial-gated response parsing.
- **Session Control:** Add `Ctrl+c` to stop running agent sessions.
- **Prompt History:** Implement prompt history navigation with up/down arrows.
- **Stats Page:** Add project and model columns to the stats page.
- **Session Size:** Compute session size from diff and display it in the session list.
- **File Listing:** Include directories in `@` mention dropdown with trailing slash.
- **Session Status:** Add `Rebasing` status to session lifecycle.
- **Terminal Summaries:** Persist terminal summaries for session outcomes.

### Changed

- **Architecture:** Refactor app into manager composition with event-driven session state updates and reducer-based routing for git status, PR control, and session mutations.
- **Architecture:** Split session module and centralize lookups; separate session snapshots from runtime handles.
- **Session Defaults:** New sessions inherit the latest session's agent, model, and permission mode.
- **File Listing:** Include non-ignored dotfiles in file listing.
- **Merge Flow:** Run session merges asynchronously, harden merge messaging, and increase merge commit message timeout.
- **Rebase Flow:** Improve assisted rebase continuation flow and auto-commit pending changes before rebasing.
- **Auto-Commit:** Improve auto-commit recovery with agent assistance.
- **Session Summary:** Backfill and use session summary for finished sessions.
- **UI:** Move open worktree keybinding to chat view and update session size color palette.
- **Docs:** Document app module architecture and public API docs; add cargo install instructions to README.

### Removed

- **Health Module:** Remove health check module and wiring.

### Contributors

- @andagaev
- @minev-dev

## [v0.1.10] - 2026-02-15

### Added

- **Review Workflow:** Added an explicit `Merging` session status and a review-session rebase action.
- **Session UX:** Added read-only controls for done sessions and a `/clear` slash command.
- **Help UI:** Added a `?` keybinding with an updated overlay and descriptive slash-command menu.

### Changed

- **Session List:** Split session metadata into `Project`, `Model`, and `Status` columns with dynamic width sizing.
- **Runtime:** Run session commands through per-session workers and restore interrupted sessions into `Review`.
- **Stats:** Accumulate token usage over time and preserve stats after `/clear`.
- **Merge Flow:** Enforce merge commit message formatting and normalize co-author trailer handling.
- **UI Cleanup:** Removed agent labels from session list rows and session chat titles.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.1.9] - 2026-02-13

### Added

- **Diff View:** Added diff content wrapping to render long changed lines without truncation.
- **Diff View:** Added structured parsing with line-number gutters (`old│new`) for unified diffs.
- **Docs:** Added a demo GIF to the README and documented GIF generation with VHS.

### Changed

- **Diff View:** Compare against each session's base branch so review shows all accumulated changes.
- **Workflow:** Simplified commit flow by auto-committing after agent iterations and removing manual commit mode.
- **Release Docs:** Added contributor-list requirements and examples to the release workflow documentation.

### Contributors

- @minev-dev

## [v0.1.8] - 2026-02-13

### Added

- **Onboarding:** Added a full-screen onboarding page shown when no sessions exist.
- **Tests:** Added onboarding behavior coverage for app state, list mode `Enter` handling, and UI rendering conditions.

### Changed

- **UX:** Pressing `Enter` from the onboarding view now creates a new session and opens prompt mode directly.
- **Error Handling:** Session creation errors in list mode are now surfaced instead of being silently ignored.
- **UI:** Kept the footer visible during onboarding and simplified session list rendering to consistently use the table layout.

### Contributors

- @minev-dev

## [v0.1.7] - 2026-02-12

### Added

- **UI:** Show session worktree path and branch in the footer bar for better context awareness.
- **UI:** Display commit count in the session chat title.
- **Stats:** Add session token usage statistics to the Stats page.

### Changed

- **Persistence:** Moved application data directory from `/var/tmp/.agentty` to `~/.agentty` for better persistence and standard compliance.
- **UX:** Renamed "Roadmap" tab to "Stats" to better reflect its content.
- **UX:** Use shortened 8-character UUIDs for session folders and git branches to reduce clutter.
- **Internal:** Standardized session ID variable naming across the codebase.

### Contributors

- @andagaev
- @minev-dev

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

### Contributors

- @andagaev
- @minev-dev

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

### Contributors

- @minev-dev

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

### Contributors

- @minev-dev

## [v0.1.3] - 2026-02-08

### Added

- **Backends:** Added Codex backend support.
- **Project Management:** Added project switching with automatic sibling discovery.
- **Diff View:** Show all file changes in diff view.
- **Status:** Show status as text in session list and chat title.
- **Health:** Added version normalization for agent checks.

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

### Contributors

- @andagaev
- @minev-dev

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

### Contributors

- @minev-dev

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

### Contributors

- @andagaev
- @minev-dev

## [v0.1.0] - 2026-02-08

- Initial release.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev
