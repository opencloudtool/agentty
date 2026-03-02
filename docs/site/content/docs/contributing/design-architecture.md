+++
title = "Design & Architecture"
description = "Architecture map and change-path guidance for contributors working in the Agentty workspace."
weight = 2
+++

<a id="architecture-introduction"></a>
This guide maps Agentty's architecture to concrete paths so contributors can
change the right module on the first pass.

<!-- more -->

## Architecture Goals

<a id="architecture-goals"></a>
Agentty is structured around clear boundaries:

- Keep domain logic independent from infrastructure and UI.
- Keep long-running or external operations behind trait boundaries for testability.
- Keep runtime event handling responsive by offloading background work to async tasks.
- Keep AI-session changes isolated in git worktrees and reviewable as diffs.
- Decouple agent transport (CLI subprocess vs app-server RPC) behind a unified channel abstraction.

## Workspace Map

| Path | Responsibility |
|------|----------------|
| `crates/agentty/` | Main TUI application crate (`agentty`) with runtime, app orchestration, domain, infrastructure, and UI modules. |
| `crates/ag-xtask/` | Workspace maintenance commands (index checks, migration checks, automation helpers). |
| `docs/site/content/docs/` | End-user and contributor documentation published at `/docs/`. |

## Runtime Flow (Top to Bottom)

<a id="architecture-runtime-flow"></a>
The main runtime path is:

```text
main.rs
  ├─ Database::open(...)
  ├─ RoutingAppServerClient::new()
  ├─ App::new(base_path, working_dir, git_branch, db, app_server_client)
  └─ runtime::run(&mut app)
       ├─ terminal::setup_terminal()
       ├─ event::spawn_event_reader(...)
       └─ run_main_loop(...)
            ├─ event::process_events(...)
            │    └─ key_handler::handle_key_event(...)
            │         └─ runtime/mode/* handlers
            │              └─ app/* orchestration
            │                   └─ infra/* boundaries
            └─ ui::render::draw(...)
                 └─ ui::router (mode-to-page dispatch)
```

<a id="architecture-runtime-flow-notes"></a>
This flow keeps UI and key handling thin while `app/` owns state transitions and
workflow orchestration.

## Agent Channel Architecture

<a id="architecture-agent-channel"></a>
Agent turns are executed through the provider-agnostic `AgentChannel` trait,
which decouples session workers from transport details:

```text
app/session/worker.rs
  └─ AgentChannel::run_turn(session_id, TurnRequest, event_tx)
       ├─ CliAgentChannel        (Claude: spawns CLI subprocess)
       └─ AppServerAgentChannel  (Codex/Gemini: delegates to AppServerClient)
```

<a id="architecture-key-types"></a>
**Key types** (all in `infra/channel.rs`):

| Type | Purpose |
|------|---------|
| `TurnRequest` | Input payload: folder, model, mode (start/resume), prompt, `provider_conversation_id`. |
| `TurnEvent` | Incremental stream events: `AssistantDelta`, `Progress`, `Completed`, `Failed`, `PidUpdate`. |
| `TurnResult` | Normalized output: `assistant_message`, token counts, `provider_conversation_id`. |
| `TurnMode` | `Start` (fresh turn) or `Resume` (with optional session output replay). |

<a id="architecture-provider-conversation-id-flow"></a>
**Provider conversation ID flow**: app-server providers return a
`provider_conversation_id` after each turn. Session workers persist this to the
database so a future runtime restart can resume the provider's native context
instead of replaying the full transcript.

## Core Module Paths

### Composition Root

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/main.rs` | Binary entry point: database bootstrap, `RoutingAppServerClient` creation, `App` construction, runtime launch. |
| `crates/agentty/src/lib.rs` | Public module exports and crate-level re-exports. |

### Application Layer (`app/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/app.rs` | `App` facade, event reducer, startup loading, background task wiring. |
| `crates/agentty/src/app/assist.rs` | Shared assistance helpers for commit and rebase recovery loops. |
| `crates/agentty/src/app/merge_queue.rs` | Merge queue state machine (`Queued`/`Merging` progression rules). |
| `crates/agentty/src/app/project.rs` | `ProjectManager` — project CRUD and selection orchestration. |
| `crates/agentty/src/app/service.rs` | `AppServices` dependency container (`Database`, `GitClient`, app-server client, event sender). |
| `crates/agentty/src/app/session_state.rs` | `SessionState` — per-session runtime state container. |
| `crates/agentty/src/app/settings.rs` | `SettingsManager` — settings management and persistence. |
| `crates/agentty/src/app/tab.rs` | `TabManager` — top-level tab definitions and tab selection state. |
| `crates/agentty/src/app/task.rs` | App-scoped background tasks (git status polling, version checks, focused review assists, app-server turns). |
| `crates/agentty/src/app/session/` | Session-specific orchestration split by concern: |
| — `access.rs` | Session lookup helpers. |
| — `codex_usage.rs` | Codex app-server usage-limit loading. |
| — `lifecycle.rs` | Session creation, prompt/reply workflows. |
| — `load.rs` | Session snapshot loading. |
| — `merge.rs` | Merge/rebase workflows. |
| — `refresh.rs` | Periodic refresh scheduling. |
| — `task.rs` | Session process execution and status persistence. |
| — `worker.rs` | Per-session command queue orchestration, `AgentChannel` turn dispatch. |

### Domain Layer (`domain/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/domain/agent.rs` | Agent kinds, models, model metadata, and agent-related enums. |
| `crates/agentty/src/domain/input.rs` | Input state management. |
| `crates/agentty/src/domain/permission.rs` | `PermissionMode` and permission logic. |
| `crates/agentty/src/domain/project.rs` | Project entities and display helpers. |
| `crates/agentty/src/domain/session.rs` | Session entities, statuses, sizes, and stats-focused domain types. |

### Infrastructure Layer (`infra/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/infra/db.rs` | SQLite persistence and queries; database open config enables `WAL` and foreign keys. |
| `crates/agentty/src/infra/git.rs` + `infra/git/` | `GitClient` trait and async git workflow commands (`merge.rs`, `rebase.rs`, `repo.rs`, `sync.rs`, `worktree.rs`). |
| `crates/agentty/src/infra/channel.rs` + `infra/channel/` | `AgentChannel` trait and provider-agnostic turn execution: |
| — `cli.rs` | `CliAgentChannel` — CLI subprocess adapter (Claude). |
| — `app_server.rs` | `AppServerAgentChannel` — app-server RPC adapter (Codex/Gemini). |
| `crates/agentty/src/infra/agent/` | Per-provider backend command builders and response parsing: |
| — `backend.rs` | `AgentBackend` trait, transport mode selection, prompt templates. |
| — `claude.rs` | Claude backend implementation. |
| — `codex.rs` | Codex backend implementation. |
| — `gemini.rs` | Gemini backend implementation. |
| — `response_parser.rs` | Streaming response parsing. |
| `crates/agentty/src/infra/app_server.rs` | `AppServerClient` trait and shared request/response stream types. |
| `crates/agentty/src/infra/app_server_router.rs` | `RoutingAppServerClient` — provider routing for app-server models (Codex/Gemini). |
| `crates/agentty/src/infra/app_server_transport.rs` | Shared stdio JSON-RPC transport utilities for app-server processes. |
| `crates/agentty/src/infra/codex_app_server.rs` | Codex app-server transport/session integration. |
| `crates/agentty/src/infra/gemini_acp.rs` | Gemini ACP transport/session integration. |
| `crates/agentty/src/infra/file_index.rs` | Gitignore-aware file indexing and fuzzy filtering for `@` mentions in prompts. |
| `crates/agentty/src/infra/version.rs` | Version checking infrastructure. |

### Runtime Layer (`runtime/`)

| Path | What lives here |
|------|------------------|
| `crates/agentty/src/runtime.rs` | Terminal lifecycle, event/render loop orchestration, `TerminalGuard`. |
| `crates/agentty/src/runtime/terminal.rs` | Terminal setup and cleanup guard. |
| `crates/agentty/src/runtime/event.rs` | `EventSource` trait, event reader spawn, tick processing, and app-event integration. |
| `crates/agentty/src/runtime/key_handler.rs` | Mode dispatch for key events. |
| `crates/agentty/src/runtime/mode/` | Key handlers for each `AppMode`: |
| — `list.rs` | Session list mode. |
| — `session_view.rs` | Session view mode navigation. |
| — `prompt.rs` | Prompt mode editing and submit. |
| — `diff.rs` | Diff mode handling. |
| — `help.rs` | Help overlay mode. |
| — `confirmation.rs` | Shared yes/no confirmation mode. |
| — `sync_blocked.rs` | Sync-blocked popup key handling. |

### UI Layer (`ui/`)

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
| — `diff.rs` | Diff view page. |
| — `project_list.rs` | Project list page. |
| — `session_chat.rs` | Session chat page (new sessions and replies). |
| — `session_list.rs` | Session list page. |
| — `settings.rs` | Settings page. |
| — `stats.rs` | Stats/analytics page. |
| `crates/agentty/src/ui/components/` | Reusable widgets and overlays: |
| — `chat_input.rs` | Chat input widget. |
| — `confirmation_overlay.rs` | Confirmation dialog overlay. |
| — `file_explorer.rs` | Diff file explorer component. |
| — `footer_bar.rs` | Footer bar widget. |
| — `help_overlay.rs` | Help overlay component. |
| — `info_overlay.rs` | Info overlay component. |
| — `session_output.rs` | Session output display widget. |
| — `status_bar.rs` | Status bar widget. |
| — `tab.rs` | Tabs navigation widget. |
| `crates/agentty/src/ui/state/` | UI state types: |
| — `app_mode.rs` | `AppMode` enum and mode transitions. |
| — `help_action.rs` | Help content definitions. |
| — `prompt.rs` | Prompt editor state. |

## Change Path Recipes

### Add or Modify a Session Workflow

1. Update orchestration in `crates/agentty/src/app/session/` (`lifecycle.rs`, `worker.rs`, `task.rs`, etc.).
1. Keep persistence in `crates/agentty/src/infra/db.rs`.
1. Keep git operations behind `GitClient` in `crates/agentty/src/infra/git.rs`.
1. Update docs when lifecycle/status behavior changes: `docs/site/content/docs/usage/usage.md`.

### Add a New Agent Backend or Model

1. Update domain model declarations in `crates/agentty/src/domain/agent.rs`.
1. Add backend behavior in `crates/agentty/src/infra/agent/` and wiring in `backend.rs`.
1. If app-server-based, extend routing in `crates/agentty/src/infra/app_server_router.rs`.
1. Register transport mode in `crates/agentty/src/infra/agent/backend.rs` (`transport_mode()`).
1. The channel layer (`infra/channel.rs`) routes automatically based on transport mode — no change needed there.
1. Update `docs/site/content/docs/agents/backends.md` with backend/model documentation.

### Add a Keybinding or Mode Interaction

1. Update the handler in `crates/agentty/src/runtime/mode/`.
1. If a new mode/state is needed, extend `crates/agentty/src/ui/state/app_mode.rs`.
1. If help content changes, update `crates/agentty/src/ui/state/help_action.rs` as needed.
1. Update `docs/site/content/docs/usage/usage.md`.

### Add or Change Database Schema

1. Add a new migration file in `crates/agentty/migrations/` (`NNN_description.sql`).
1. Never modify existing migration files.
1. Keep query changes in `crates/agentty/src/infra/db.rs`.
1. Ensure any status/model behavior changes are reflected in docs pages affected by user-facing behavior.

### Add a New UI Page or Component

1. Add the page in `crates/agentty/src/ui/pages/` or component in `crates/agentty/src/ui/components/`.
1. Wire the page into `crates/agentty/src/ui/router.rs`.
1. If a new `AppMode` is needed, extend `crates/agentty/src/ui/state/app_mode.rs` and add a key handler in `crates/agentty/src/runtime/mode/`.

## Testability and Boundaries

<a id="architecture-testability-boundaries"></a>
Agentty intentionally uses trait boundaries around external systems so
orchestration can be unit-tested with mocks. All traits below use
`#[cfg_attr(test, mockall::automock)]`:

| Trait | Module | Boundary |
|-------|--------|----------|
| `GitClient` | `infra/git.rs` | Git/process operations (worktree, merge, rebase, diff, push, pull). |
| `AgentChannel` | `infra/channel.rs` | Provider-agnostic turn execution (session init, run turn, shutdown). |
| `AgentBackend` | `infra/agent/backend.rs` | Per-provider CLI command construction and one-time setup. |
| `AppServerClient` | `infra/app_server.rs` | App-server RPC execution (provider routing, JSON-RPC transport). |
| `EventSource` | `runtime/event.rs` | Terminal event polling for deterministic event-loop tests. |

<a id="architecture-boundary-testing-guidance"></a>
When adding higher-level flows involving multiple external commands, prefer
injectable trait boundaries and `mockall`-based tests over flaky end-to-end
shell-heavy tests.

## Contributor Checklist for Architecture-Safe Changes

1. Keep workflow/state transitions in `app/`, not in UI rendering modules.
1. Keep external integrations in `infra/` behind traits.
1. Keep business entities and enums in `domain/`.
1. New external boundaries should get a trait with `#[cfg_attr(test, mockall::automock)]`.
1. Update docs in `docs/site/content/docs/` whenever user-facing behavior changes.
1. Update this page when changing module boundaries, runtime flow, or trait boundaries.
1. Run quality gates from `AGENTS.md` before opening a PR.
