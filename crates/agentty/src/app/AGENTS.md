# App Module

Application-layer workflows and orchestration.

## Overview

- The app layer coordinates session lifecycle, prompt/reply submission, async worker execution, persistence, and UI state refresh.
- Mode handlers prefer enqueue-first behavior for long-running work so the UI remains responsive.

## Design

- Composition model:
  - `App` is a facade/orchestrator.
  - `SessionManager` owns session snapshots, runtime handles, and session worker queues.
  - `ProjectManager` owns project list, active project context, and git status tracking state.
  - `AppServices` holds shared dependencies (`Database`, base path, app-event sender).
- Session state model:
  - `Session` is a render-friendly data snapshot.
  - `SessionHandles` stores shared runtime channels used by background tasks.
  - `SessionState` performs handle-to-snapshot sync before render.
- Event model:
  - `AppEvent` is the internal bus between background workflows and the runtime loop.
  - `apply_app_events()` is the reducer for app-side async mutations.
  - Background tasks and manager workflows emit events through `AppServices`.
  - Foreground `App` wrappers process queued events to keep reducer-driven state coherent.
- Execution model:
  - Work is serialized per session through worker queues.
  - Merge workflows run in background tasks and report progress through events and persisted status/output.
- Refresh model:
  - List reloads are event-driven (`RefreshSessions`) at lifecycle boundaries.
  - A low-frequency metadata poll remains as a safety fallback.
- Recovery model:
  - Operation state is persisted so interrupted work can be reconciled on startup.

## Docs Sync

When app orchestration or session lifecycle behavior changes, update:

- `docs/site/content/docs/usage/workflow.md` — statuses, transitions, question flow, and slash-command behavior.
- `docs/site/content/docs/usage/keybindings.md` — user-visible actions available per mode/state.
- `docs/site/content/docs/architecture/runtime-flow.md` — app orchestration and worker/channel runtime flow.

## Entry Points

- `core.rs` owns the main `App` facade and reducer wiring.
- `session.rs` and `session/` own session lifecycle and worker orchestration.
- `project.rs`, `setting.rs`, and `tab.rs` own project, settings, and top-level navigation concerns.
- `task.rs` and `merge_queue.rs` own detached workflows and queue orchestration.
