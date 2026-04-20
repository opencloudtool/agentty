+++
title = "Module Map"
description = "Path-by-path ownership map for composition root, application, domain, infrastructure, runtime, and UI modules."
weight = 3
+++

<a id="architecture-module-map-introduction"></a>
This guide maps source paths to responsibilities so contributors can quickly
choose the correct module when implementing changes.

For tooling and agent exploration, `cargo run -p ag-xtask -- workspace-map`
also writes a machine-readable workspace summary to
`target/agentty/workspace-map.json`.

<!-- more -->

## Workspace Crates

- `crates/ag-forge/`: Shared forge review-request library crate with
  normalized review-request types, GitHub remote detection, and the `gh`
  adapter.
- `crates/agentty/`: Main TUI application crate with composition root,
  application, domain, infrastructure, runtime, and UI layers.
- `crates/testty/`: Rust-native TUI end-to-end testing framework with
  PTY-driven semantic assertions, VHS screenshot capture, calibration, overlay
  rendering, and snapshot workflows.
- `crates/ag-xtask/`: Workspace maintenance commands and automation helpers,
  including generated workspace-map output for tooling.

## Composition Root

- `crates/agentty/src/main.rs`: Binary entry point for database bootstrap,
  `App` construction, and runtime launch.
- `crates/agentty/src/lib.rs`: Public module exports and crate-level re-exports.
- `crates/ag-forge/src/lib.rs`: Shared workspace crate for forge review-request
  types, CLI boundaries, GitHub remote detection, and the GitHub adapter.

## Application Layer (`app/`)

- `crates/agentty/src/app.rs`: App module router and public re-exports for app
  orchestration APIs.
- `crates/agentty/src/app/core.rs`: Router-only `App` core module that
  re-exports the facade and focused child modules under `app/core/`.
- `crates/agentty/src/app/core/state.rs`: `App`, `AppClients`,
  `SyncMainRunner`, shared state structs, and remaining workflow glue that does
  not belong to startup, draw, reducer, or roadmap slices.
- `crates/agentty/src/app/core/new.rs`: `App` construction, startup project
  loading, service wiring, and startup-only app helpers.
- `crates/agentty/src/app/core/draw.rs`: Frame rendering plus render-facing app
  accessors used to assemble `ui::RenderContext`.
- `crates/agentty/src/app/core/events.rs`: `AppEvent`, reducer batch
  coalescing, app-event application, and sync or branch-publish popup helpers.
- `crates/agentty/src/app/core/roadmap.rs`: Active-project roadmap cache state,
  `Tasks` tab availability, and roadmap loading or scrolling helpers.
- `crates/agentty/src/app/startup.rs`: `AppStartup` for startup project
  resolution, initial session hydration, and project catalog refresh helpers.
- `crates/agentty/src/app/reducer.rs`: `AppEventReducer` and `AppEventBatch`
  for app-event draining and batch coalescing during one runtime tick.
- `crates/agentty/src/app/review.rs`: Focused review cache updates plus
  background review-assist orchestration helpers.
- `crates/agentty/src/app/review_request.rs`: Shared review-request commit
  message parsing used by branch-publish and session lifecycle workflows.
- `crates/agentty/src/app/branch_publish.rs`: Branch-publish task payloads,
  git-push auth guidance, and branch publish background helpers.
- `crates/agentty/src/app/assist.rs`: Shared assistance helpers for commit and
  rebase recovery loops.
- `crates/agentty/src/app/merge_queue.rs`: Merge queue state machine for
  `Queued` and `Merging` progression rules.
- `crates/agentty/src/app/project.rs`: `ProjectManager` for project CRUD and
  selection orchestration.
- `crates/agentty/src/app/service.rs`: `AppServices` dependency container for
  `AppRepositories`, `FsClient`, `GitClient`, `ReviewRequestClient`, the
  optional app-server test override, and the event sender.
- `crates/agentty/src/app/session_state.rs`: `SessionState`, the per-session
  runtime state container.
- `crates/agentty/src/app/session/workflow.rs`: Router-only session workflow
  module that re-exports shared workflow state and exposes child workflow
  modules.
- `crates/agentty/src/app/setting.rs`: `SettingsManager` for settings
  management and persistence.
- `crates/agentty/src/app/tab.rs`: `TabManager` for top-level tab definitions
  and tab selection state.
- `crates/agentty/src/app/task.rs`: App-scoped background tasks for git status
  polling, version checks, review assists, and app-server turns.
- `crates/agentty/src/app/session/core.rs`: `SessionManager`, session clock
  boundary, shared constants, and session module tests.
- `crates/agentty/src/app/session/workflow/access.rs`: Session lookup helpers.
- `crates/agentty/src/app/session/workflow/lifecycle.rs`: Session creation,
  prompt/reply workflows, and forge review-request publication or open helpers.
- `crates/agentty/src/app/session/workflow/load.rs`: Session snapshot loading,
  including persisted question and summary hydration.
- `crates/agentty/src/app/session/workflow/merge.rs`: Merge and rebase
  workflows.
- `crates/agentty/src/app/session/workflow/refresh.rs`: Periodic refresh
  scheduling plus on-demand forge review-request refresh.
- `crates/agentty/src/app/session/workflow/review.rs`: Review transcript replay
  and review-mode restoration helpers.
- `crates/agentty/src/app/session/workflow/task.rs`: Session process
  execution, session commit-message generation, auto-commit orchestration that
  keeps one evolving session-branch commit, and status persistence.
- `crates/agentty/src/app/session/workflow/worker.rs`: Per-session command
  queue orchestration, `AgentChannel` turn dispatch, and post-turn persistence
  for summaries and questions.

## Domain Layer (`domain/`)

- `crates/agentty/src/domain/agent.rs`: Agent kinds, models, model metadata,
  and agent-related enums.
- `crates/agentty/src/domain/composer.rs`: Shared prompt-composer logic for
  slash-menu derivation, attachment placeholder tracking, prompt submission
  draining, agent-facing `@path` normalization, and image-token-aware deletion
  helpers.
- `crates/agentty/src/domain/input.rs`: Input state management.
- `crates/agentty/src/domain/permission.rs`: `PermissionMode` and permission
  logic.
- `crates/agentty/src/domain/project.rs`: Project entities and display helpers.
- `crates/agentty/src/domain/session.rs`: Session entities, statuses, sizes,
  stats, review-request linkage wrappers,
  and re-exports of shared forge review-request types from `ag-forge`.
- `crates/agentty/src/domain/setting.rs`: Shared persisted setting keys used
  across app and infrastructure layers.

## Infrastructure Layer (`infra/`)

- `crates/agentty/src/infra/db.rs`: SQLite database open and pool wiring,
  shared repository bundle construction, and row or repository re-exports.
- `crates/agentty/src/infra/db/session.rs`: `SessionRepository`,
  `SqliteSessionRepository`, session row models, turn-metadata persistence,
  and session query helpers.
- `crates/agentty/src/infra/db/project.rs`: `ProjectRepository`,
  `SqliteProjectRepository`, project row models, and project list queries.
- `crates/agentty/src/infra/db/review.rs`: `ReviewRepository`,
  `SqliteReviewRepository`, and persisted session review-request linkage.
- `crates/agentty/src/infra/db/usage.rs`: `UsageRepository`,
  `SqliteUsageRepository`, and per-model session usage aggregation.
- `crates/agentty/src/infra/db/activity.rs`: `ActivityRepository`,
  `SqliteActivityRepository`, and session-activity history queries.
- `crates/agentty/src/infra/db/operation.rs`: `OperationRepository`,
  `SqliteOperationRepository`, and persisted session-operation lifecycle
  queries.
- `crates/agentty/src/infra/db/setting.rs`: `SettingRepository`,
  `SqliteSettingRepository`, and global or project-scoped settings queries.
- `crates/agentty/src/infra/fs.rs`: `FsClient` trait and production async
  filesystem adapter used by app orchestration.
- `crates/agentty/src/infra/git.rs` and `crates/agentty/src/infra/git/`: Git
  module router plus async git workflow commands in `merge.rs`, `rebase.rs`,
  `repo.rs`, `sync.rs`, and `worktree.rs`, including the single-session-commit
  sync path that stages changes and amends `HEAD` after the first session
  commit exists.
- `crates/agentty/src/infra/git/client.rs`: `GitClient` trait boundary,
  `RealGitClient` production adapter, and git client integration tests.
- `crates/agentty/src/infra/channel.rs` and
  `crates/agentty/src/infra/channel/`: `AgentChannel` trait and
  provider-agnostic turn execution.
- `crates/agentty/src/infra/channel/contract.rs`: Shared `AgentChannel` trait
  plus turn request, event, and result types.
- `crates/agentty/src/infra/channel/factory.rs`: Provider-to-channel routing
  factory via `create_agent_channel()`.
- `crates/agentty/src/infra/channel/cli.rs`: `CliAgentChannel`, the CLI
  subprocess adapter for Claude.
- `crates/agentty/src/infra/channel/app_server.rs`:
  `AppServerAgentChannel`, the app-server RPC adapter for Codex and Gemini.
- `crates/agentty/src/infra/agent/`: Per-provider backend command builders and
  response parsing.
- `crates/agentty/src/infra/agent/availability.rs`: `AgentAvailabilityProbe`
  plus machine-scoped CLI discovery used to cache which agent kinds are
  locally runnable.
- `crates/agentty/src/infra/agent/backend.rs`: `AgentBackend` trait and shared
  backend request or error types.
- `crates/agentty/src/infra/agent/provider.rs`: Central provider registry for
  transport mode, parser policy, stdin strategy, app-server client factories,
  and shared schema-mismatch error formatting.
- `crates/agentty/src/infra/agent/cli.rs` and
  `crates/agentty/src/infra/agent/cli/`: Router plus shared CLI subprocess
  stdin or error helpers reused by session turns and one-shot prompts.
- `crates/agentty/src/infra/agent/claude.rs`: Claude backend implementation.
- `crates/agentty/src/infra/agent/app_server.rs` and
  `crates/agentty/src/infra/agent/app_server/`: Router plus provider-specific
  app-server client trees kept private to the agent backend module. The Codex
  and Gemini trees both split `client.rs` orchestration from focused
  `lifecycle.rs`, `transport.rs`, `stream_parser.rs`, `policy.rs`, and
  `usage.rs` helpers.
- `crates/agentty/src/infra/agent/codex.rs`: Codex backend runtime command
  construction.
- `crates/agentty/src/infra/agent/gemini.rs`: Gemini backend runtime command
  construction.
- `crates/agentty/src/infra/agent/prompt.rs`: Shared prompt preparation via
  `prepare_prompt_text()` for transcript replay and protocol preamble
  injection.
- `crates/agentty/src/infra/agent/protocol.rs` and
  `crates/agentty/src/infra/agent/protocol/`: Router plus focused protocol
  submodules. `model.rs` owns the wire contract with `answer`, `questions`,
  and `summary`; `schema.rs` owns prompt and transport
  schema generation; `parse.rs` owns final and stream parsing plus shared
  debug diagnostics for schema mismatches.
- `crates/agentty/src/infra/agent/response_parser.rs`: Provider-specific final
  and stream output parsing plus usage extraction for Claude, Gemini, and
  Codex.
- `crates/agentty/src/infra/agent/submission.rs`: Shared one-shot prompt
  execution and strict protocol validation for generated titles, session
  commit messages, assist prompts, and review text. Concrete backends provide
  either an app-server client or a direct CLI execution path.
- `crates/agentty/src/infra/app_server.rs` and
  `crates/agentty/src/infra/app_server/`: Router plus shared app-server
  contract, prompt shaping, runtime registry, and restart or retry modules.
- `crates/agentty/src/infra/app_server_router.rs`: `RoutingAppServerClient`,
  the reusable app-server router kept for tests and integration entry points
  that want one shared client across providers.
- `crates/agentty/src/infra/app_server_transport.rs`: Shared stdio JSON-RPC
  transport utilities for app-server processes, including common child-process
  spawn wiring for piped stdin or stdout runtimes.
- `crates/agentty/src/infra/file_index.rs`: Gitignore-aware file indexing and
  fuzzy filtering for `@` mentions in prompts.
- `crates/agentty/src/infra/project_discovery.rs`: `ProjectDiscoveryClient`
  trait plus the home-directory git-repository scan used by startup catalog
  refresh without leaking directory walking into `app/`.
- `crates/agentty/src/infra/tmux.rs`: `TmuxClient` trait and tmux subprocess
  adapter used by `App` worktree-open orchestration.
- `crates/agentty/src/infra/version.rs`: Version checking infrastructure.

## Forge Library (`ag-forge`)

- `crates/ag-forge/src/lib.rs`: Forge crate router and public re-exports for
  review-request APIs shared with `agentty`.
- `crates/ag-forge/src/client.rs`: `ReviewRequestClient` trait and
  `RealReviewRequestClient` provider dispatch.
- `crates/ag-forge/src/command.rs`: `ForgeCommandRunner`, command output
  normalization, and subprocess execution boundary.
- `crates/ag-forge/src/github.rs`: GitHub pull-request adapter routed through
  `gh`.
- `crates/ag-forge/src/model.rs`: Shared forge domain types including
  `ForgeKind`, `ReviewRequestSummary`, errors, and create input.
- `crates/ag-forge/src/remote.rs`: Repository remote parsing and forge
  detection helpers.

## Runtime Layer (`runtime/`)

- `crates/agentty/src/runtime.rs`: Runtime module router and public runtime
  entry re-exports.
- `crates/agentty/src/runtime/clipboard_image.rs`: Clipboard image capture and
  temporary PNG persistence helpers for prompt-mode attachments.
- `crates/agentty/src/runtime/core.rs`: Terminal lifecycle, event and render
  loop orchestration, and `TerminalGuard`.
- `crates/agentty/src/runtime/terminal.rs`: Terminal setup, cleanup, and
  raw-mode lifecycle helpers.
- `crates/agentty/src/runtime/event.rs`: `EventSource` trait, event-reader
  spawn, tick processing, and app-event integration.
- `crates/agentty/src/runtime/key_handler.rs`: Mode dispatch for key events.
- `crates/agentty/src/runtime/mode.rs`: Router-only runtime-mode module that
  exposes per-`AppMode` handlers.
- `crates/agentty/src/runtime/timing.rs`: Shared runtime frame-timing
  constants.
- `crates/agentty/src/runtime/mode/list.rs`: Session list mode.
- `crates/agentty/src/runtime/mode/session_view.rs`: Session view mode
  navigation.
- `crates/agentty/src/runtime/mode/prompt.rs`: Prompt mode editing and submit.
- `crates/agentty/src/runtime/mode/question.rs`: Clarification question input
  mode handling and follow-up reply submission.
- `crates/agentty/src/runtime/mode/input_key.rs`: Shared input-key utilities
  for modifier predicates, word movement, word deletion, and text
  normalization.
- `crates/agentty/src/runtime/mode/diff.rs`: Diff mode handling.
- `crates/agentty/src/runtime/mode/help.rs`: Help overlay mode.
- `crates/agentty/src/runtime/mode/confirmation.rs`: Shared yes or no
  confirmation mode.
- `crates/agentty/src/runtime/mode/sync_blocked.rs`: Sync-blocked popup key
  handling.

## UI Layer (`ui/`)

- `crates/agentty/src/ui/render.rs`: Frame composition and render context.
- `crates/agentty/src/ui/router.rs`: Mode-to-page routing for content
  rendering.
- `crates/agentty/src/ui/component.rs`: Router-only component module exposing
  reusable widgets and overlays.
- `crates/agentty/src/ui/layout.rs`: Layout helper utilities.
- `crates/agentty/src/ui/overlay.rs`: Overlay rendering dispatch for help,
  info, and confirmation flows.
- `crates/agentty/src/ui/markdown.rs`: Markdown rendering utilities.
- `crates/agentty/src/ui/diff_util.rs`: Diff parsing and rendering helpers.
- `crates/agentty/src/ui/icon.rs`: Icon constants and helpers.
- `crates/agentty/src/ui/page.rs`: Router-only page module exposing full-screen
  pages.
- `crates/agentty/src/ui/state.rs`: Router-only UI-state module exposing mode,
  help, and prompt state.
- `crates/agentty/src/ui/style.rs`: Shared semantic color palette and session
  status styling helpers.
- `crates/agentty/src/ui/text_util.rs`: Text manipulation helpers.
- `crates/agentty/src/ui/activity_heatmap.rs`: Activity heatmap visualization.
- `crates/agentty/src/ui/util.rs`: General UI utilities.
- `crates/agentty/src/ui/page/diff.rs`: Diff view page.
- `crates/agentty/src/ui/page/project_list.rs`: Project list page.
- `crates/agentty/src/ui/page/session_chat.rs`: Session chat page for new
  sessions and replies.
- `crates/agentty/src/ui/page/session_list.rs`: Session list page.
- `crates/agentty/src/ui/page/setting.rs`: Settings page.
- `crates/agentty/src/ui/page/stat.rs`: Stats and analytics page.
- `crates/agentty/src/ui/page/task.rs`: Tasks page that renders roadmap queue
  summaries for projects with `docs/plan/roadmap.md`.
- `crates/agentty/src/ui/task_roadmap.rs`: Roadmap parsing and formatting
  helpers used by the Tasks page.
- `crates/agentty/src/ui/component/chat_input.rs`: Chat input widget.
- `crates/agentty/src/ui/component/confirmation_overlay.rs`: Confirmation
  dialog overlay.
- `crates/agentty/src/ui/component/file_explorer.rs`: Diff file explorer
  component.
- `crates/agentty/src/ui/component/footer_bar.rs`: Footer bar widget.
- `crates/agentty/src/ui/component/help_overlay.rs`: Help overlay component.
- `crates/agentty/src/ui/component/info_overlay.rs`: Info overlay component.
- `crates/agentty/src/ui/component/open_command_overlay.rs`: Open-command
  selector overlay.
- `crates/agentty/src/ui/component/publish_branch_overlay.rs`: Session
  branch-publish overlay.
- `crates/agentty/src/ui/component/session_output.rs`: Session output display
  widget, including synthetic summary and review rendering layered on top of
  the persisted transcript.
- `crates/agentty/src/ui/component/status_bar.rs`: Status bar widget.
- `crates/agentty/src/ui/component/tab.rs`: Tabs navigation widget.
- `crates/agentty/src/ui/state/app_mode.rs`: `AppMode` enum and mode
  transitions.
- `crates/agentty/src/ui/state/help_action.rs`: Help content definitions.
- `crates/agentty/src/ui/state/prompt.rs`: Prompt UI mention state plus
  re-exports of shared prompt-composer state and helpers from
  `domain/composer.rs`.
