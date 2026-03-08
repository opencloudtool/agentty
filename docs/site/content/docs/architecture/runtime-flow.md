+++
title = "Runtime Flow"
description = "Goals, workspace map, runtime event flow, background tasks, and agent channel transport model."
weight = 2
+++

<a id="architecture-runtime-flow-introduction"></a>
This guide documents Agentty's runtime data flows end to end: the foreground event loop, reducer/event buses, session-worker turn execution, merge/rebase/sync orchestration, and every background task with trigger points and side effects.

<!-- more -->

## Architecture Goals

<a id="architecture-runtime-flow-goals"></a>
Agentty runtime design is built around these constraints:

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

## Main Runtime Flow

<a id="architecture-runtime-flow-main"></a>
Primary foreground path from process start to one event-loop cycle:

```text
main.rs
  ├─ Database::open(...)                    // sqlite open + WAL + FK + migrations
  ├─ RoutingAppServerClient::new()          // Codex/Gemini router
  ├─ App::new(...)
  │    ├─ load startup project/session snapshots
  │    ├─ fail unfinished operations from previous run
  │    └─ spawn app background tasks
  └─ runtime::run(&mut app)
       ├─ terminal::setup_terminal()
       ├─ event::spawn_event_reader(...)    // dedicated OS thread
       └─ run_main_loop(...)
            ├─ sessions.sync_from_handles() // pull Arc<Mutex> runtime state into snapshots
            ├─ ui::render::draw(...)
            └─ event::process_events(...)
                 ├─ key events -> mode handlers -> app/session orchestration
                 ├─ app events -> App::apply_app_events reducer
                 └─ tick -> refresh_sessions_if_needed safety poll
```

<a id="architecture-runtime-flow-notes"></a>
Foreground loop details:

- `run_main_loop()` renders every cycle and applies snapshot sync before draw.
- `process_events()` waits on terminal events, app events, or tick (`tokio::select!`).
- After one event, it drains queued terminal events immediately to avoid one-key-per-frame lag.
- Tick interval is `50ms`; metadata-based session reload fallback is `5s` (`SESSION_REFRESH_INTERVAL`).

## Data Channels

<a id="architecture-runtime-flow-channels"></a>
Agentty uses four primary runtime data channels:

| Channel | Producer(s) | Consumer(s) | Payload | Purpose |
|---------|-------------|-------------|---------|---------|
| Terminal `Event` channel (`runtime/event.rs`) | Event-reader thread | `runtime::process_events()` | `crossterm::Event` | User input and terminal events. |
| App event bus (`AppEvent`) | App background tasks, workers, task helpers | `App::apply_app_events()` reducer | `AppEvent` variants | Safe cross-task app-state mutation. |
| Turn event stream (`TurnEvent`) | `AgentChannel` implementations | Session worker `consume_turn_events()` | Stream deltas/progress/pid | Real-time turn output and progress updates. |
| Session handles (`SessionHandles`) | Workers/session task helpers | `SessionState::sync_from_handles()` | Shared `Arc<Mutex<...>>` output/status/pid | Fast snapshot sync without full DB reload. |

## App Event Reducer Flow

<a id="architecture-runtime-flow-app-events"></a>
`App::apply_app_events()` is the single reducer path for async app events.

Flow:

1. Drain queued events (`first_event` + `try_recv` loop).
1. Reduce into `AppEventBatch` (coalesces refresh, git status, model/progress updates).
1. Apply side effects in stable order.

Reducer behaviors that matter for data flow:

- `RefreshSessions` sets `should_force_reload`, which triggers `refresh_sessions_now()` and `reload_projects()`.
- `ReviewRequestActionCompleted` persists refreshed PR/MR linkage when needed, then swaps the session-view popup from loading to success or blocked/failure copy.
- `SessionUpdated` marks touched sessions so reducer can call `sync_session_from_handle()` selectively.
- `SessionProgressUpdated` updates transient progress labels used by UI.
- `AgentResponseReceived` routes question-mode transitions for active view sessions.
- After touched-session sync, terminal statuses (`Done`, `Canceled`) drop per-session worker senders so workers can shut down runtimes.

## Session Turn Data Flow

<a id="architecture-runtime-flow-turn"></a>
From prompt submit to persisted result:

1. Prompt mode submits:
1. `start_session()` for first prompt (`TurnMode::Start`) or `reply()` for follow-up (`TurnMode::Resume`).
1. Session command is persisted in `session_operation` before enqueue.
1. `SessionWorkerService` lazily creates or reuses a per-session worker queue.
1. Worker marks operation `running`, checks cancel flags, then runs channel turn.
1. Worker creates `TurnRequest` (reasoning level, model, prompt, replay output, provider conversation id).
1. Worker spawns `consume_turn_events()` and sets initial progress (`Thinking`).
1. `AgentChannel::run_turn()` streams `TurnEvent` values and returns `TurnResult`.
1. Worker applies final result:
1. Append final assistant transcript output when no assistant chunks were already streamed (`answer` text, fallback `question` text).
1. Persist session questions and emit `AppEvent::AgentResponseReceived`.
1. Persist stats and per-model usage.
1. Persist provider conversation id (app-server providers).
1. Run auto-commit assistance path.
1. Refresh persisted session size.
1. Update final status (`Review` or `Question`; on failure -> `Review`).

### Operation Lifecycle and Recovery

<a id="architecture-session-operation-lifecycle"></a>
Turn execution is durable and restart-safe:

- Before enqueue: insert `session_operation` row (`queued`).
- Worker transitions: `queued -> running -> done/failed/canceled`.
- Cancel requests are persisted and checked before command execution.
- On startup, unfinished operations are failed with reason `Interrupted by app restart`, and impacted sessions are reset to `Review`.

### Status Transition Rules

<a id="architecture-runtime-flow-status"></a>
Runtime status transitions enforced by `Status::can_transition_to()`:

- `New -> InProgress` (first prompt)
- `Review/Question -> InProgress` (reply)
- `Review -> Queued -> Merging -> Done` (merge queue path)
- `Review -> Rebasing -> Review/Question` (rebase path)
- `Review/Question -> Canceled`
- `InProgress/Rebasing -> Review/Question` (post-turn or post-rebase)

## Agent Channel Architecture

<a id="architecture-agent-channel"></a>
Session workers are transport-agnostic through `AgentChannel`:

```text
app/session/workflow/worker.rs
  └─ AgentChannel::run_turn(session_id, TurnRequest, event_tx)
       ├─ CliAgentChannel        (Claude; subprocess per turn)
       └─ AppServerAgentChannel  (Codex/Gemini; persistent runtime per session)
            └─ AppServerClient
                 └─ RoutingAppServerClient
                      ├─ RealCodexAppServerClient
                      └─ RealGeminiAcpClient
```

<a id="architecture-key-types"></a>
Key types (`infra/channel.rs`):

| Type | Purpose |
|------|---------|
| `TurnRequest` | Input payload: `reasoning_level`, folder, `live_session_output`, model, mode (start/resume), prompt, `provider_conversation_id`. |
| `TurnEvent` | Incremental stream events: `AssistantDelta`, `ThoughtDelta`, `Progress`, `Completed`, `Failed`, `PidUpdate`. |
| `TurnResult` | Normalized output: `assistant_message`, token counts, `provider_conversation_id`. |
| `TurnMode` | `Start` (fresh turn) or `Resume` (with optional session output replay). |

<a id="architecture-provider-conversation-id-flow"></a>
Provider conversation id flow:

- App-server providers return `provider_conversation_id` in `TurnResult`.
- Worker persists it to DB (`update_session_provider_conversation_id`).
- Future `TurnRequest` loads and forwards it so runtime restarts can resume native provider context.

## Agent Interaction Protocol Flow

<a id="architecture-agent-interaction-protocol"></a>
Provider output is normalized to one structured response protocol:

1. Prompt builders prepend protocol instructions (`answer`/`question` schema).
1. Session discussion prompts also require every turn to end with a markdown `## Change Summary` containing `### Current Turn` and `### Session Changes`.
1. One-shot prompts keep the same JSON envelope but omit the change-summary footer so internal utility calls can parse commit messages, generated titles, focused review text, and assist-task transcript output directly from `answer` messages.
1. Channels stream deltas/progress as `TurnEvent`.
1. Final output is parsed to protocol `messages`.
1. Worker persists final display text and question payloads, then emits `AgentResponseReceived`.

<a id="architecture-agent-interaction-streaming"></a>
Streaming behavior differs by transport/provider:

- CLI channel (`CliAgentChannel`): parses stdout lines into `AssistantDelta` and `Progress`; keeps raw output for final parse.
- App-server channel (`AppServerAgentChannel`): bridges `AppServerStreamEvent` to `TurnEvent`.
- Codex thought phases (`thinking`/`plan`/`reasoning`/`thought`) stream as `ThoughtDelta`.
- Strict providers suppress streamed assistant chunks when needed so malformed first-pass protocol JSON is not persisted.
- Worker persistence behavior: streamed `ThoughtDelta` and `Progress` updates drive transient progress badges and are not appended to session transcript output.

<a id="architecture-agent-interaction-validation"></a>
Final-output validation:

- Claude and Gemini use strict protocol parsing with up to three repair retries when invalid.
- Codex uses permissive parse fallback (schema already supplied via app-server `outputSchema` path).
- One-shot agent submissions use strict protocol parsing and the same repair prompt for every backend before returning utility results to app/session workflows, including auto-commit and rebase conflict assistance.

## Clarification Question Loop

<a id="architecture-agent-question-loop"></a>
Question-mode loop:

1. Worker receives final parsed response containing `question` messages.
1. Worker persists question list and sets session status `Question`.
1. Reducer switches active view to `AppMode::Question` when that session is focused.
1. User answers each question.
1. Runtime builds one follow-up prompt:

```text
Clarifications:
1. Q: <question 1>
   A: <response 1>
2. Q: <question 2>
   A: <response 2>
```

6. Runtime submits this as a normal reply turn; flow returns to standard worker path.

## Background Task Catalog

<a id="architecture-runtime-flow-background-tasks"></a>
Detached/background execution paths and their trigger conditions:

| Task | Trigger | Spawn site | Emits / Writes | What it does |
|------|---------|------------|----------------|--------------|
| Terminal event reader thread | Runtime startup | `runtime/event::spawn_event_reader` | Terminal `Event` channel | Polls crossterm and forwards terminal events into the runtime loop. |
| Git status poller loop | App startup (if project has git branch), and project switch | `TaskService::spawn_git_status_task` | `AppEvent::GitStatusUpdated` | Periodic fetch + ahead/behind snapshot (about every 30s). |
| Version check one-shot | App startup | `TaskService::spawn_version_check_task` | `AppEvent::VersionAvailabilityUpdated` | Checks npm latest version tag and reports update availability. |
| Per-session worker loop | First command enqueue for a session | `SessionWorkerService::spawn_session_worker` | DB `session_operation` updates, app/session updates | Serializes all turn commands per session and manages channel lifecycle. |
| Per-turn turn-event consumer | Every queued turn execution | `run_channel_turn` | Output append, progress updates, pid slot updates | Consumes `TurnEvent` stream and applies immediate side effects. |
| CLI stdout/stderr readers | Every CLI-backed turn | `CliAgentChannel::run_turn` | `TurnEvent` stream + raw buffers | Reads subprocess streams and emits incremental deltas/progress. |
| App-server stream bridge | Every app-server-backed turn | `AppServerAgentChannel::run_turn` | `TurnEvent` stream | Bridges `AppServerStreamEvent` to unified turn events. |
| Session title generation | First `Start` turn, before main turn execution | `spawn_start_turn_title_generation` | DB title + `AppEvent::RefreshSessions` | Runs one-shot title prompt in background and persists generated title if valid. |
| At-mention file indexing | Prompt input activates `@` mention mode | `runtime/mode/prompt::activate_at_mention` | `AppEvent::AtMentionEntriesLoaded` | Lists session files (`spawn_blocking`) and updates mention picker entries. |
| Background session-size refresh | Enter on session in list mode | `App::refresh_session_size_in_background` | DB size + `AppEvent::RefreshSessions` | Computes diff-size bucket without blocking key handling path. |
| Session-view review-request action | Session view `p` for create/refresh | `App::start_review_request_action` | `AppEvent::ReviewRequestActionCompleted` | Runs forge CLI publish/refresh work in the background and updates the session-view popup plus persisted PR/MR metadata. |
| Deferred session cleanup | Delete with deferred cleanup path | `delete_selected_session_deferred_cleanup` | Filesystem/git side effects | Removes worktree folder and branch asynchronously after DB deletion. |
| Focused review assist | View mode focused-review open when diff is reviewable | `TaskService::spawn_focused_review_assist_task` | `FocusedReviewPrepared` / `FocusedReviewPreparationFailed` | Runs model review prompt and stores final review text or error. |
| Sync-main workflow task | List-mode sync action (`s`) | `TokioSyncMainRunner::start_sync_main` | `AppEvent::SyncMainCompleted` | Pull-rebase/push selected project branch, with assisted conflict flow. |
| Session merge task | Merge confirmation accepted | `SessionMergeService::merge_session` | Output append, status updates, session metadata updates | Runs rebase + squash merge + worktree cleanup in background. |
| Session rebase task | Rebase action in view mode | `SessionMergeService::rebase_session` | Output append, status updates | Runs assisted rebase and returns session to `Review`/`Question`. |

## Sync, Merge, and Rebase Flows

<a id="architecture-runtime-flow-git-workflows"></a>
Project and session git workflows use shared boundaries (`GitClient`, `FsClient`, assist helpers) but have distinct orchestration paths:

- `sync main`: selected project branch pull/rebase/push, optional assisted conflict resolution, popup result summary.
- session merge: queue-aware workflow, assisted rebase first, squash merge into base branch, worktree cleanup, status `Done` on success.
- session rebase: assisted rebase of session branch onto base branch, returns to `Review` after completion/failure reporting.
- review-request publish/refresh: review-ready sessions push the session branch through `GitClient`, resolve the forge adapter through `ReviewRequestClient`, persist normalized PR/MR linkage, and can refresh archived links from stored forge URLs after worktree cleanup.

## Persistence and Recovery Boundaries

<a id="architecture-runtime-flow-persistence"></a>
Persistence invariants that shape runtime flow:

- DB opens with SQLite WAL and `foreign_keys = ON`, then embedded migrations run at startup.
- Session snapshots in memory are authoritative for rendering; DB is authoritative for restart recovery.
- Shared session handles (`output`, `status`, `child_pid`) provide low-latency updates between DB reloads.
- Event-driven refresh is primary (`RefreshSessions`); metadata polling is fallback safety only.
- External integrations (`GitClient`, `ReviewRequestClient`, `AppServerClient`, `AgentChannel`, `EventSource`, `FsClient`, `TmuxClient`) isolate side effects and enable deterministic tests.
