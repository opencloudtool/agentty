# App Module

Application-layer workflows and orchestration.

## Overview
- The app layer coordinates session lifecycle, prompt/reply submission, async worker execution, persistence, and UI state refresh.
- UI mode handlers are enqueue-first for long-running work: they persist user intent, append immediate high-level output, and return control without blocking rendering.
- Each session is isolated by `session_id` and has independent in-memory handles (`output`, `status`, `commit_count`) mirrored to the database.
- Session refresh is metadata-driven (`row_count`, `updated_at_max`) to avoid reloading rows when nothing changed.

## Design
- Session creation flow:
  - Creating a session provisions an isolated worktree, initializes metadata, persists a `New` session record, and refreshes list state.
- Prompt execution flow:
  - First prompts and follow-up replies update persisted prompt context, append immediate UI feedback, and enqueue background work.
  - Each queued unit is tracked as a durable session operation so lifecycle state survives refreshes and restarts.
- Per-session worker model:
  - The app maintains one command queue per session so commands for the same session execute sequentially.
  - Different sessions use separate queues/workers, so long-running operations do not block each other.
  - Workers persist operation lifecycle transitions (`queued`, `running`, `done`, `failed`, `canceled`) for observability and recovery.
- Command execution path:
  - Background execution runs the agent process, captures both output streams concurrently, parses response content and stats, appends output, and performs commit attempts when applicable.
  - Session status changes are persisted as work progresses and settle into review state after successful execution.
- Output and single-writer discipline:
  - Session output writes are routed through a single execution path per session and persisted to both disk-backed logs and database output state.
  - This keeps UI-visible output recoverable across refreshes/restarts.
- Cancellation and deletion:
  - Session deletion requests cancellation for unfinished operations, detaches worker routing, removes persisted session data, and cleans worktree/branch resources.
  - Worker-side validity checks prevent canceled or invalid operations from continuing execution.
- Startup recovery:
  - On app startup, unfinished operations from a previous run are marked failed with a restart reason to avoid stuck `running` states.
- Session data/runtime-handle split:
  - `Session` is a pure data snapshot (`output: String`, `status: Status`, `commit_count: i64`) with no `Arc`/`Mutex`.
  - `SessionHandles` owns runtime channels (`Arc<Mutex<...>>`) for output/status/commit-count updates shared with background tasks.
  - `SessionState` stores both `sessions: Vec<Session>` (render data) and `handles: HashMap<String, SessionHandles>` (live runtime state).
  - `SessionState::sync_from_handles()` copies handle values into `Session` snapshots once per tick before rendering.
  - Background tasks must clone arcs from `SessionState.handles`, not from `Session`.
  - Handle identity is preserved across session reloads so existing workers keep valid references.

## Directory Index
- [mod.rs](mod.rs) - Shared app state and module wiring.
- [pr.rs](pr.rs) - Pull request workflow orchestration.
- [project.rs](project.rs) - Project discovery and switching logic.
- [session.rs](session.rs) - Session lifecycle, persistence, and git worktree flow.
- [task.rs](task.rs) - Background task spawning and output handling.
- [title.rs](title.rs) - Session title summarization helpers.
- [worker.rs](worker.rs) - Per-session worker queue and operation lifecycle orchestration.
