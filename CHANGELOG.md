# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.6.10] - 2026-03-26

### Added

- Add a docs-site blog section.
- Add roadmap metadata requirements for step headings, IDs, assignees, and claim commits.

### Changed

- Rename the Rust-native TUI E2E crate from `ag-tui-test` to `testty` and prepare it for crates.io publishing.
- Remove the manual review-request sync flow and keep `end_turn_no_answer` aligned with `Review`.
- Refocus implementation-roadmap guidance on active follow-up work and simpler step operations.
- Remove `#[ignore]` gates from E2E tests.
- Clarify session commit prompt guidance to consult repository commit conventions.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.9] - 2026-03-25

### Added

- Add `s` keybinding to sync review request status in session view.
- Add `testty` TUI E2E testing framework with PTY-driven semantic assertions.

### Changed

- Migrate git infrastructure module from `Result<..., String>` to typed `GitError`.
- Tolerate extra fields in protocol deserialization while keeping schema strict.
- Improve protocol parse diagnostics.
- Separate summary transcript from streamed output.
- Strip markdown fences in protocol parser.
- Dim question panel when chat input is focused.

### Fixed

- Fix wrapped plain-text utility output test assertion after diagnostics refactor.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.8] - 2026-03-19

### Added

- Add a scrollbar to long diffs.

### Changed

- Change Esc in question mode to end the turn instead of skipping one question.
- Clarify read-only git command guidance in agent prompts.
- Refine Rust-native TUI E2E framework plan.

### Fixed

- Recover trailing protocol payload from wrapped provider output.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.7] - 2026-03-19

### Added

- Add session follow-up task implementation plan.

### Changed

- Require strict protocol JSON for Codex, utility, and all agent responses.
- Prefer structured Gemini completion payload and accept wire-type defaults.
- Track published session branch git statuses across refreshes.
- Support keyboard enhancement flags in terminal runtime.
- Optimize session activity and refresh queries.
- Limit home-directory project scans to startup.
- Clarify that agents do not create commits automatically.

### Contributors

- @minev-dev

## [v0.6.6] - 2026-03-18

### Added

- Add VHS-based E2E testing framework with screenshot comparison.

### Changed

- Refresh session workflow and settings docs.
- Keep tracked upstream branches current in the footer.
- Keep prompt file index unbounded within max depth.
- Keep publish branch shortcut keys in the input field.
- Extract shared input key utilities and add emacs-style editing to question input.
- Document shared-host test thread budget.

### Refactored

- Introduce typed `DbError` for database operations.

### Fixed

- Confirm review session cancellation.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.5] - 2026-03-17

### Added

- Add `AGENTS.md` files for app-server, CLI, and shared modules.
- Add the tech debt error handling implementation plan.

### Changed

- Publish session branches with `git push --force-with-lease`.
- Update agent test expectations for generic agent wording and the current commit message model.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.4] - 2026-03-16

### Refactored

- Refactor provider routing and shared app-server helpers.
- Stream Claude responses with live schema-validated events.

### Contributors

- @minev-dev

## [v0.6.3] - 2026-03-16

### Added

- Add tech-debt and security-audit analysis skills.
- Add at-mention file completion to question mode.
- Pass `--effort` flag to Claude CLI based on reasoning level.
- Generate protocol profiles with self-descriptive schemas.

### Changed

- Refactor agent prompt preparation and split protocol subsystem.
- Move app-server clients under agent backends.
- Append change summaries and preserve summary payloads.
- Format done session summary as markdown sections.
- Parameterize runtime over generic `Backend` for in-process TUI testing.
- Restrict `a` (new session) shortcut to the Sessions tab.
- Finish typed SQLx query mapping cleanup.
- Refine implementation plans for meta-agent skills and execution backends.

### Fixed

- Keep prompt mention state in sync with cursor.
- Fix footer bar branch rendering.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.2] - 2026-03-12

### Added

- Add multi-installer auto-update implementation plans.

### Changed

- Make filesystem reads asynchronous.
- Unify protocol instruction prompt templates.
- Refactor publish branch input mode handling.
- Document setting keys and simplify publish updates.
- Refine session commit coauthor trailer handling.
- Expand session database and app regression test coverage.
- Handle startup app initialization errors gracefully.

### Fixed

- Fix test and rebase mock failures on main.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.1] - 2026-03-12

### Added

- Add background auto-update with status bar progress and `--no-update` flag.
- Add codecov badge to README.
- Add postsubmit coverage workflow.
- Add chat output scrolling to question-answer mode.

### Contributors

- @andagaev
- @minev-dev

## [v0.6.0] - 2026-03-11

### Added

- Support pasted prompt images across all session backends.
- Add auto-update implementation plan.

### Changed

- Session prompt footer uses shared help styling.
- Simplify auto-update plan to background npm install with status bar progress.
- Add test failure protocol to quality gates in AGENTS.md.
- Update test prompt assignments to use `.into()` conversion.
- Plan docs omit rendered size sections.
- Stream Claude and Gemini prompts through stdin.
- Surface Claude auth guidance for command failures.
- Harden prompt image workflow lifecycle.

### Contributors

- @andagaev
- @minev-dev

## [v0.5.11] - 2026-03-11

### Changed

- Track prompt image attachments in prompt mode.
- Add size budgets to implementation plans.
- Clarify implementation-plan AGENTS purpose.
- Rename implementation plan priorities to steps.
- Standardize titled substeps in implementation plans.
- Consolidate architecture guide map into landing page.
- Document single evolving session commit flow.

### Contributors

- @minev-dev

## [v0.5.10] - 2026-03-10

### Changed

- Refine detached session rollout plan.
- Branch push adds forge review request links.
- Refine prompt image paste plan.

### Contributors

- @minev-dev

## [v0.5.9] - 2026-03-10

### Changed

- Align implementation plans with updated skill rules.
- Scope prompt image paste plan to session chat composer.
- Generate session commit messages from cumulative diffs.
- Prefer shallower file index matches.

### Contributors

- @minev-dev

## [v0.5.8] - 2026-03-10

### Fixed

- **Git:** Ignore HTTPS userinfo in remote parsing.

### Contributors

- @minev-dev

## [v0.5.7] - 2026-03-10

### Added

- **Plan:** Add session commit message flow plan.
- **UI:** Add branch publish popup for custom remote targets.
- **Session:** Persist published upstream refs for sessions.
- **Architecture:** Extract forge review-request code into `ag-forge`.

### Changed

- **UI:** Replace review request flow with manual branch publish.
- **Projects:** Skip stale project directories in project list.
- **Docs:** Replace docs plan symlinks with explicit indexes and format plan headings.

### Contributors

- @minev-dev

## [v0.5.6] - 2026-03-09

### Added

- **Agent:** Add predefined answer options for agent questions.
- **Agent:** Add mandatory per-turn change summaries to agent protocol.
- **Infra:** Implement forge CLI review-request adapters (GitHub/GitLab PR/MR workflows).
- **UI:** Add session view review request workflows (create, open, refresh).
- **UI:** Show project scope in list tabs.
- **Settings:** Scope settings per project.
- **Review:** Auto-start focused review generation on session Review transition and cache results.

### Changed

- **UI:** Change focused review navigation to open/regenerate with exit key.
- **UI:** Remove external editor shortcut (`e`).
- **Git:** Remove session worktrees when review sessions are canceled.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.5.5] - 2026-03-07

### Added

- **Skill:** Add `implementation-plan` skill for managing project plans.
- **Docs:** Add Forge review request support plan.
- **Docs:** Add GitHub issue form templates and directory indexing.
- **Infra:** Add default pull request template for the repository.
- **UI:** Add Agentty info panel to the project list.
- **UI:** Chat panel gets polished chrome and unified overlay styling with dimmed backdrop.

### Changed

- **Codex:** Promote `gpt-5.4` as the default Codex model.
- **UI:** Keep question input visible in tight terminal layouts.
- **Docs:** Refine Agentty description.

### Fixed

- **Skill:** Clarify requirements and plan structure for `implementation-plan` skill.

### Contributors

- @minev-dev

## [v0.5.4] - 2026-03-06

### Added

- **Docs:** Define plan template and add test coverage improvement plan.
- **UI:** Add background tints for changed lines in diff view.

### Changed

- **UI:** Tab bar gains separators and muted border styling.

### Contributors

- @minev-dev

## [v0.5.3] - 2026-03-06

### Added

- **UI:** Support `Alt+Enter` and `Shift+Enter` newline entry across settings and prompt.
- **Session:** Generate session titles from user intent when the first start turn begins.

### Changed

- **UI:** Migrate UI color usage to semantic palette tokens.
- **UI:** Refresh table visual styling across list and stats pages.
- **UI:** Render clarification prompts with distinct spacing and styling.
- **UI:** Render footer help as styled keybinding lines.
- **UI:** Session list uses shared page margin.
- **UI:** Stop `@mention` highlighting before trailing punctuation.
- **Session Output:** Improve verbatim markdown wrapping and Unicode width handling.
- **Session Output:** Preserve multiline user prompts across persistence and rendering.
- **Review:** Tighten review suggestion severity criteria and require concise actionable suggestions.
- **Architecture:** Refactor module roots into router-only modules.
- **Claude:** Enforce strict MCP config for Claude backend.
- **Docs:** Reframe UI beautification plan around implementation status.

### Fixed

- **Session Output:** Prevent duplicate final assistant output after streaming.

### Contributors

- @minev-dev

## [v0.5.2] - 2026-03-05

### Added

- **UI:** Support multiline open command editing in the settings tab.
- **UI:** Render session status header directly above the output panel.
- **Session:** Add open command selector when multiple launch commands are configured for a worktree.
- **Docs:** Add `CLAUDE` and `GEMINI` symlinks for module-level `AGENTS.md` files.

### Changed

- **Architecture:** Rename `app` and `ui` module roots to singular names (`app.rs`, `ui.rs`, `domain.rs`, `infra.rs`, `runtime.rs`).
- **Architecture:** Parse merge commit messages from structured protocol output.
- **Architecture:** Harden `AGENTS.md` index validation and normalize directory index links.
- **Docs:** Prevent content and table-of-contents text overflow on the documentation site.
- **Docs:** Expand runtime flow documentation and add doc comments across migration, runtime, UI, and database helpers.

### Contributors

- @minev-dev

## [v0.5.1] - 2026-03-04

### Added

- **UI:** Show active session count in the project list.
- **Review:** Run focused review assist in isolated start mode.
- **Docs:** Split architecture documentation into a dedicated section and document structured response protocol.

### Changed

- **Protocol:** Harden structured protocol handling across providers.
- **Architecture:** Standardize module-oriented imports across the `app` and `ui` layers.
- **Architecture:** Align architecture docs with runtime mode, channel schema, and test boundaries.
- **Quality:** Require explicit user approval to retain legacy behavior during development.

### Fixed

- **UI:** Fix active session count calculation to exclude `Question` status and ensure projects reload on session refresh.

### Contributors

- @minev-dev
- @andagaev

## [v0.5.0] - 2026-03-04

### Added

- **UI:** Handle agent clarification questions in a dedicated question mode with persistent history.
- **UI:** Highlight `@mention` tokens in chat input.
- **UI:** Improve chat input wrapping and viewport scrolling.
- **UI:** Align session output wrapping with panel borders.
- **Architecture:** Switch agent output to schema-validated JSON messages and normalize assist protocol output.
- **Architecture:** Inject `FsClient` and `Clock` dependencies into session workflows for better testability.
- **Architecture:** Route session stats and filesystem workflows through the app layer.
- **Docs:** Add diff-first verification guidance and refine site responsiveness.

### Changed

- **Session:** Move first-turn title generation to the start-turn worker and use plain text.
- **Session:** Filter Codex thought/reasoning text from persisted assistant output and handle it separately during streaming.
- **Session:** Remove plan messages from the agent response protocol.
- **Session:** Prefer active sessions for initial selection in the UI.
- **Protocol:** Prefer agent message content over trailing reasoning payloads.

### Removed

- **UI:** Remove session stop shortcut (`Ctrl+c`) and stop-session flow.
- **Architecture:** Remove unused `nix` dependency.

### Fixed

- **Session:** Ensure merge cleanup (worktree/branch removal) completes before marking session as `Done`.
- **Sync:** Optimize session output synchronization for append-heavy updates.
- **Review:** Parse structured agent responses in focused review assist correctly.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.7] - 2026-03-04

### Added

- **UI:** Add `Ctrl+u` prompt-line deletion and intent-aware confirmations for merge, delete, and quit actions.
- **Settings:** Persist Codex reasoning levels and propagate reasoning identifiers through runtime integrations.
- **Environment:** Allow `AGENTTY_ROOT` to override the default agentty data root.

### Changed

- **UI:** Remove the session list project column and highlight the active project name in the `Sessions` tab.
- **Session:** Preserve live session state during reload and unify child PID propagation across assist/rebase/merge flows.
- **Docs:** Split usage docs into dedicated workflow and keybindings pages and refine rebase conflict guidance.
- **Quality:** Require full validation checks during execution and standardize the full test command to single-threaded runs.

### Fixed

- **Tests:** Refresh stale assertions after settings/help text updates.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.6] - 2026-03-02

### Added

- **UI:** Add structured question-answer flow between agents and users.
- **UI:** Add stable paragraph anchor links and dual sidebars in docs.
- **UI:** Colorize added/removed line counts in diff page title.
- **UI:** Project list highlights active project.
- **Docs:** Add design and architecture documentation page.
- **Docs:** Add GitHub metadata badges and right-side anchor links.
- **Docs:** Add tooling setup instructions for `uv` and `pre-commit`.
- **Session:** Generate session titles once in background and refine generation instructions.
- **Session:** Persist and resume provider-native conversation identifiers across restarts.
- **Architecture:** Add structured agent response protocol with metadata delimiter.
- **Architecture:** Unify session turn execution through agent channels.

### Changed

- **Codex:** Load usage limits lazily and remove usage panel/polling.
- **Codex:** Update model defaults and remove `gpt-5.2-codex`.
- **UI:** Refine info overlay and session list padding.
- **Architecture:** Extract process boundaries (tmux, editor, sync) and inject dependencies (clock, sleeper, git) for better testability.
- **Architecture:** Propagate assistant phase metadata in app-server streams.
- **Sync:** Render sync success details as markdown sections.

### Fixed

- **Session:** Harden runtime shutdown I/O and handle Claude partial streaming.
- **UI:** Harden diff selection fallback and simplify sync commit title formatting.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.5] - 2026-03-01

### Added

- **UI:** Add Cmd+Backspace current-line deletion in prompt.
- **Settings:** Add default review model and separate default smart/fast model settings.
- **Review:** Enforce read-only constraints for focused review assist and refine prompt structure.

### Changed

- **UI:** Show diff line-change totals in diff panel title.
- **UI:** Center loading sync text in info overlay and show only OK action.
- **Sync:** Improve sync completion details and info overlay presentation; show newly pulled commit titles.
- **Settings:** Rename DevServer setting to OpenCommand.
- **Session:** Session manager replays review history after restart.
- **Tokens:** Read turn usage from thread token usage updates.
- **Codex:** Adjust auto-compaction threshold by model.
- **Docs:** Style docs tables with borders, hover states, and responsive scrolling.

### Removed

- **Backend:** Remove env-based backend selection (`AGENTTY_AGENT`).
- **Startup:** Remove lock module from runtime startup.

### Fixed

- **Review:** Prevent runtime leaks and simplify focused review handling.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.4] - 2026-02-28

### Added

- **UI:** Add focused review mode to session view and diff mode.
- **Docs:** Add MCP docs section, Context7 setup guide, and workflow documentation.
- **Codex:** Enable live web search and network access.
- **Docs:** Redirect docs landing page to getting started overview.

### Changed

- **UI:** Keep info overlay action row visible for multiline messages.
- **UI:** Handle shifted J/K scrolling in diff mode and block actions for canceled sessions.
- **Sync:** Improve sync popup guidance for push authentication failures and render sync success metrics on separate lines.
- **Git:** Set non-interactive git prompt defaults for repo commands.
- **Architecture:** Backend command construction uses one build API.

### Contributors

- @minev-dev

## [v0.4.3] - 2026-02-26

### Added

- **ACP:** Integrate typed Gemini ACP protocol transport and tests.
- **ACP:** Send empty Gemini ACP client capabilities on initialize.
- **Session:** Use Askama templates for session prompts and propagate render errors.

### Changed

- **Gemini:** Remove reconnect banner and rename Gemini stdout reader.

### Contributors

- @minev-dev
- @andagaev

## [v0.4.2] - 2026-02-26

### Added

- **Codex:** Add auto-compact support for Codex app-server sessions.

### Contributors

- @minev-dev

## [v0.4.1] - 2026-02-26

### Added

- **UI:** Add `Shift+Arrow` and `Alt/Shift+Backspace` word-wise cursor movement in prompt mode.
- **Models:** Stream Gemini assistant chunks to the UI during turns and handle ACP permission requests.
- **Skills:** Add code review skill.
- **Tests:** Add regression tests and improve coverage with mocked clients.

### Changed

- **UI:** Project switcher supports `j`/`k` navigation, unfiltered list navigation, and visible selection.
- **UI:** Report sync outcome details in completion popup and keep confirmation choices visible.
- **UX:** List mode opens canceled sessions on `Enter`.
- **Models:** Codex resume uses `--last` only without replay history and enforces high reasoning effort.
- **Models:** Rename Gemini Pro preview variant to Gemini 3.1.
- **Session Output:** Keep clean auto-commit silent, report no-op states, and ignore synthetic Codex completion messages.
- **Architecture:** Centralize git command execution and isolate Codex usage-limit loading.
- **Docs:** Update documentation to highlight Agentty self-hosting and align docs page widths.

### Fixed

- **Models:** Handle Gemini `session/new` error responses explicitly and ignore empty assistant chunks.
- **UI:** Stabilize site header across routes.

### Removed

- **UI:** Remove quick project switcher mode and overlay.

### Contributors

- @andagaev
- @minev-dev

## [v0.4.0] - 2026-02-24

### Added

- **Projects:** Add a projects tab with quick project switching.
- **Navigation:** Add backward tab navigation with `Shift+Tab`.
- **Docs:** Add the getting started overview guide.

### Changed

- **App:** Resolve main repository roots via git and exclude session worktrees.
- **UI:** Switch to `Sessions` after project selection and compact footer help actions.
- **Runtime:** Route app-server turns by provider, include root `AGENTS.md` instructions, and pass session folder/model in Codex payloads.
- **Docs:** Reorganize site sections, standardize skill headers, and migrate the docs site to compiled Sass styling.
- **Models:** Add support for the `gpt-5.3-codex-spark` model.

### Fixed

- **Database:** Fix SQLite migration `025` to avoid non-constant defaults.
- **Templates:** Fix malformed Tera block syntax in the base template.
- **Docs:** Remove duplicate front matter delimiters in overview content.

### Removed

- **Onboarding:** Remove the onboarding page from the list-mode flow.
- **Projects:** Remove project favorite controls from the project list.

### Contributors

- @andagaev
- @minev-dev

## [v0.3.0] - 2026-02-23

### Added

- **Docs:** Add copy button to code blocks.
- **Docs:** Add theme selector and favicon to site.
- **Docs:** Add contributing guide and templates.
- **Claude:** Enable Bash tool for Claude agent.
- **Output:** Stream Codex turn events to session output.

### Changed

- **Sync:** Fix pull rebase to target explicit upstream.
- **UI:** Cap chat input panel height and scroll prompt viewport.
- **Architecture:** Generalize app-server session handling.
- **Architecture:** Refactor site templates to use base layout.
- **UI:** Update docs sidebar styling.
- **Project:** Update repository URLs to new organization.
- **Architecture:** Move UI rendering into a dedicated render module.
- **Architecture:** Extract shared stdio JSON-RPC transport utilities.
- **UI:** Adopt Builder Lite pattern for UI components.
- **Project:** Update description to "Agentic Development Environment (ADE)".
- **Runtime:** Track active turn usage from completion and stream events.
- **UX:** Align view mode shortcuts with session state rules.
- **Runtime:** Require strict turn ID matching and make prompt char handling sync.
- **Output:** Filter synthetic completion status lines from chat output.
- **Deps:** Bump pulldown-cmark from 0.13.0 to 0.13.1.

### Removed

- **Command Palette:** Remove command palette and multi-project switching.
- **Docs:** Remove documentation sections and demo assets from README.
- **Slash Commands:** Remove `/clear` slash command and session history clearing logic.

### Contributors

- @andagaev
- @dependabot[bot]
- @minev-dev

## [v0.2.2] - 2026-02-22

### Added

- **Release:** Add crates.io publish workflow for release tags.

### Changed

- **Metadata:** Add full workspace author metadata.

### Contributors

- @minev-dev

## [v0.2.1] - 2026-02-22

### Added

- **Session Output:** Add toggle to switch between summary and full output for completed sessions.
- **Release:** Require explicit confirmation for version bump type in release skill.
- **Runtime:** Track active turn ID to prevent race conditions during turn completion.

### Changed

- **Architecture:** Refactor UI routing and overlays into dedicated modules and centralize frame drawing.
- **Session:** Defer session cleanup and load at-mention entries asynchronously for faster startup.
- **Git:** Retry git commands on index lock contention and simplify session view handling.
- **Settings:** Only persist default model when the "last-used" option is enabled.
- **Rebase:** Improve recovery from stale metadata during rebase assist.
- **Permissions:** Consolidate permission handling into a single "Auto Edit" mode.

### Removed

- **Permissions:** Remove legacy permission mode column from database and UI.
- **Permissions:** Remove non-auto permission modes and plan follow-up functionality.

### Contributors

- @andagaev
- @minev-dev

## [v0.2.0] - 2026-02-22

### Added

- **Plan:** Add iterative plan question flow with per-question answer options.
- **Sync:** Run branch sync in background with loading popup and outcome display.
- **Sync:** Add session branch sync action with sync-blocked popup.
- **Sync:** Add assisted conflict resolution for sync main rebase.
- **Stats:** Add Codex usage limits to stats dashboard.
- **Stats:** Persist session-creation activity and render by local day.
- **Stats:** Persist and display all-time model usage and longest session duration.
- **Help:** Help system uses state-aware action projection.
- **Dev Server:** Add editable Dev Server setting and run when opening session tmux window.
- **UX:** Add `h`/`l` shortcuts for confirmation selection.

### Changed

- **Architecture:** Refactor agent infrastructure into provider modules.
- **Architecture:** Split git infrastructure and UI utilities into focused modules.
- **Architecture:** Inject `GitClient` into app workflows and isolate multi-command git tests.
- **Refactor:** Move file indexing into infra module and parse using `pulldown-cmark`.
- **Refactor:** Rename state, file, and mode modules for clarity.
- **Refactor:** Move module roots from `mod.rs` to sibling files.
- **Sync:** Add project and branch context to sync popups.
- **Sync:** Sync main branch by pushing after rebase.
- **Plan:** Improve plan follow-ups and Codex stats limit rendering.
- **UX:** Use shared confirmation mode for quit and session deletion.
- **UX:** Confirmation prompts default to "No" selection.
- **UX:** Hide open-worktree shortcut for done sessions and restrict view actions while running.
- **Commit:** Preserve a single evolving session commit.
- **Search:** Prioritize basename matches in file list fuzzy scoring.

### Fixed

- **Codex:** Fix app-server error status recovery and wait for responses before parsing limits.
- **Stability:** Fix launch and lint regressions after rebase.
- **UI:** Deduplicate list background rendering and reset grouped session table offset.

### Removed

- **Refactor:** Remove orphaned top-level source files from `src/`.
- **Refactor:** Remove `pr-testing` directory references.

### Contributors

- @andagaev
- @minev-dev

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
