+++
title = "Module Map"
description = "Path-by-path ownership map for composition root, application, domain, infrastructure, runtime, and UI modules."
weight = 3
+++

<a id="architecture-module-map-introduction"></a>
This guide maps source paths to responsibilities so contributors can quickly
choose the correct module when implementing changes.

<!-- more -->

## Composition Root

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/main.rs` | Binary entry point: database bootstrap, `RoutingAppServerClient` creation, `App` construction, runtime launch. |
| `crates/agentty/src/lib.rs` | Public module exports and crate-level re-exports. |

## Application Layer (`app/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/app.rs` | `App` facade, event reducer, startup loading, background task wiring. |
| `crates/agentty/src/app/assist.rs` | Shared assistance helpers for commit and rebase recovery loops. |
| `crates/agentty/src/app/merge_queue.rs` | Merge queue state machine (`Queued`/`Merging` progression rules). |
| `crates/agentty/src/app/project.rs` | `ProjectManager` - project CRUD and selection orchestration. |
| `crates/agentty/src/app/service.rs` | `AppServices` dependency container (`Database`, `FsClient`, `GitClient`, app-server client, event sender). |
| `crates/agentty/src/app/session_state.rs` | `SessionState` - per-session runtime state container. |
| `crates/agentty/src/app/settings.rs` | `SettingsManager` - settings management and persistence. |
| `crates/agentty/src/app/tab.rs` | `TabManager` - top-level tab definitions and tab selection state. |
| `crates/agentty/src/app/task.rs` | App-scoped background tasks (git status polling, version checks, review assists, app-server turns). |
| `crates/agentty/src/app/session/` | Session-specific orchestration split by concern: |
| - `access.rs` | Session lookup helpers. |
| - `lifecycle.rs` | Session creation, prompt/reply workflows. |
| - `load.rs` | Session snapshot loading. |
| - `merge.rs` | Merge/rebase workflows. |
| - `refresh.rs` | Periodic refresh scheduling. |
| - `review.rs` | Review transcript replay and review-mode restoration helpers. |
| - `task.rs` | Session process execution and status persistence. |
| - `worker.rs` | Per-session command queue orchestration, `AgentChannel` turn dispatch. |

## Domain Layer (`domain/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/domain/agent.rs` | Agent kinds, models, model metadata, and agent-related enums. |
| `crates/agentty/src/domain/input.rs` | Input state management. |
| `crates/agentty/src/domain/permission.rs` | `PermissionMode` and permission logic. |
| `crates/agentty/src/domain/project.rs` | Project entities and display helpers. |
| `crates/agentty/src/domain/session.rs` | Session entities, statuses, sizes, and stats-focused domain types. |
| `crates/agentty/src/domain/setting.rs` | Shared persisted setting keys used across app and infrastructure layers. |

## Infrastructure Layer (`infra/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/infra/db.rs` | SQLite persistence and queries; database open config enables `WAL` and foreign keys. |
| `crates/agentty/src/infra/fs.rs` | `FsClient` trait and production async filesystem adapter used by app orchestration. |
| `crates/agentty/src/infra/git.rs` + `infra/git/` | `GitClient` trait and async git workflow commands (`merge.rs`, `rebase.rs`, `repo.rs`, `sync.rs`, `worktree.rs`). |
| `crates/agentty/src/infra/channel.rs` + `infra/channel/` | `AgentChannel` trait and provider-agnostic turn execution: |
| - `cli.rs` | `CliAgentChannel` - CLI subprocess adapter (Claude). |
| - `app_server.rs` | `AppServerAgentChannel` - app-server RPC adapter (Codex/Gemini). |
| `crates/agentty/src/infra/agent/` | Per-provider backend command builders and response parsing: |
| - `backend.rs` | `AgentBackend` trait, transport mode selection, prompt templates. |
| - `claude.rs` | Claude backend implementation. |
| - `codex.rs` | Codex backend implementation. |
| - `gemini.rs` | Gemini backend implementation. |
| - `protocol.rs` | Structured response contract (`AgentResponse`, `AgentResponseMessage`, `AgentResponseMessageKind`), JSON-schema generation, parsing, stream normalization, and repair prompt helpers. |
| - `response_parser.rs` | Provider-specific final/stream output parsing and usage extraction for Claude, Gemini, and Codex. |
| `crates/agentty/src/infra/app_server.rs` | `AppServerClient` trait and shared request/response stream types. |
| `crates/agentty/src/infra/app_server_router.rs` | `RoutingAppServerClient` - provider routing for app-server models (Codex/Gemini). |
| `crates/agentty/src/infra/app_server_transport.rs` | Shared stdio JSON-RPC transport utilities for app-server processes. |
| `crates/agentty/src/infra/codex_app_server.rs` | Codex app-server transport/session integration. |
| `crates/agentty/src/infra/gemini_acp.rs` | Gemini ACP transport/session integration. |
| `crates/agentty/src/infra/file_index.rs` | Gitignore-aware file indexing and fuzzy filtering for `@` mentions in prompts. |
| `crates/agentty/src/infra/tmux.rs` | `TmuxClient` trait and tmux subprocess adapter used by `App` worktree-open orchestration. |
| `crates/agentty/src/infra/version.rs` | Version checking infrastructure. |

## Runtime Layer (`runtime/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/runtime.rs` | Terminal lifecycle, event/render loop orchestration, `TerminalGuard`. |
| `crates/agentty/src/runtime/terminal.rs` | Terminal setup/cleanup plus `EditorLauncher` trait boundary for external editor subprocesses. |
| `crates/agentty/src/runtime/event.rs` | `EventSource` trait, event reader spawn, tick processing, and app-event integration. |
| `crates/agentty/src/runtime/key_handler.rs` | Mode dispatch for key events. |
| `crates/agentty/src/runtime/mode/` | Key handlers for each `AppMode`: |
| - `list.rs` | Session list mode. |
| - `session_view.rs` | Session view mode navigation. |
| - `prompt.rs` | Prompt mode editing and submit. |
| - `question.rs` | Clarification question input mode handling and follow-up reply submission. |
| - `diff.rs` | Diff mode handling. |
| - `help.rs` | Help overlay mode. |
| - `confirmation.rs` | Shared yes/no confirmation mode. |
| - `sync_blocked.rs` | Sync-blocked popup key handling. |

## UI Layer (`ui/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/ui/render.rs` | Frame composition and render context. |
| `crates/agentty/src/ui/router.rs` | Mode-to-page routing for content rendering. |
| `crates/agentty/src/ui/layout.rs` | Layout helper utilities. |
| `crates/agentty/src/ui/overlays.rs` | Overlay rendering dispatch (help, info, confirmation). |
| `crates/agentty/src/ui/markdown.rs` | Markdown rendering utilities. |
| `crates/agentty/src/ui/diff_util.rs` | Diff parsing and rendering helpers. |
| `crates/agentty/src/ui/icon.rs` | Icon constants and helpers. |
| `crates/agentty/src/ui/text_util.rs` | Text manipulation helpers. |
| `crates/agentty/src/ui/activity_heatmap.rs` | Activity heatmap visualization. |
| `crates/agentty/src/ui/util.rs` | General UI utilities. |
| `crates/agentty/src/ui/pages/` | Full-screen page implementations: |
| - `diff.rs` | Diff view page. |
| - `project_list.rs` | Project list page. |
| - `session_chat.rs` | Session chat page (new sessions and replies). |
| - `session_list.rs` | Session list page. |
| - `settings.rs` | Settings page. |
| - `stats.rs` | Stats/analytics page. |
| `crates/agentty/src/ui/components/` | Reusable widgets and overlays: |
| - `chat_input.rs` | Chat input widget. |
| - `confirmation_overlay.rs` | Confirmation dialog overlay. |
| - `file_explorer.rs` | Diff file explorer component. |
| - `footer_bar.rs` | Footer bar widget. |
| - `help_overlay.rs` | Help overlay component. |
| - `info_overlay.rs` | Info overlay component. |
| - `session_output.rs` | Session output display widget. |
| - `status_bar.rs` | Status bar widget. |
| - `tab.rs` | Tabs navigation widget. |
| `crates/agentty/src/ui/state/` | UI state types: |
| - `app_mode.rs` | `AppMode` enum and mode transitions. |
| - `help_action.rs` | Help content definitions. |
| - `prompt.rs` | Prompt editor state. |
