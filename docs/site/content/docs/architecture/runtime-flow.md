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
| `crates/ag-forge/` | Shared forge review-request library crate for normalized review-request types, remote detection, and `gh`/`glab` adapter orchestration. |
| `crates/agentty/` | Main TUI application crate (`agentty`) with runtime, app orchestration, domain, infrastructure, and UI modules. |
| `crates/ag-xtask/` | Workspace maintenance commands (index checks, migration checks, automation helpers). |
| `docs/site/content/docs/` | End-user and contributor documentation published at `/docs/`. |

## Main Runtime Flow

<a id="architecture-runtime-flow-main"></a>
Primary foreground path from process start to one event-loop cycle:

```text
main.rs
  ├─ Database::open(...)                    // sqlite open + WAL + FK + migrations
  ├─ App::new(...)
  │    ├─ run one startup-only home-directory project scan, then load project/session snapshots
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
- `reload_projects()` now reloads only persisted project rows; the expensive home-directory repository discovery pass runs only during `App::new()`.
- `BranchPublishActionCompleted` swaps the session-view popup from loading to success or blocked/failure copy after a manual branch push finishes.
- `SessionUpdated` marks touched sessions so reducer can call `sync_session_from_handle()` selectively.
- `SessionProgressUpdated` updates transient progress labels used by UI.
- `AgentResponseReceived` routes question-mode transitions for active view sessions.
- After touched-session sync, terminal statuses (`Done`, `Canceled`) drop per-session worker senders so workers can shut down runtimes.

## Session Turn Data Flow

<a id="architecture-runtime-flow-turn"></a>
From prompt submit to persisted result:

1. Prompt mode submits:
1. `start_session()` for first prompt (`AgentRequestKind::SessionStart`) or `reply()` for follow-up (`AgentRequestKind::SessionResume`).
1. Session command is persisted in `session_operation` before enqueue.
1. `SessionWorkerService` lazily creates or reuses a per-session worker queue.
1. Worker marks operation `running`, checks cancel flags, then runs channel turn.
1. Worker creates `TurnRequest` (reasoning level, model, prompt, `request_kind`, replay output, provider conversation id).
1. Worker spawns `consume_turn_events()` and sets initial progress (`Thinking`).
1. `AgentChannel::run_turn()` streams `TurnEvent` values and returns `TurnResult`.
1. Worker applies final result:
1. Append final assistant transcript output when no assistant chunks were already streamed (`answer` text, fallback `question` text).
1. Persist session questions and emit `AppEvent::AgentResponseReceived`.
1. Persist stats and per-model usage.
1. Persist provider conversation id (app-server providers).
1. Run auto-commit assistance path, which preserves a single evolving commit on the session branch: the first successful file-changing turn creates the commit, later turns regenerate the message from the cumulative diff and amend `HEAD`, and the session `title` is synced from that rewritten commit after success while the structured response `summary` payload remains unchanged.
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
  └─ create_agent_channel(kind, override)
       └─ provider registry (`infra/agent/provider.rs`)
            ├─ transport_mode() -> Cli
            │    └─ CliAgentChannel        (Claude; subprocess per turn)
            └─ transport_mode() -> AppServer
                 ├─ create_app_server_client()
                 └─ AppServerAgentChannel  (Codex/Gemini; persistent runtime per session)
                      └─ AppServerClient
                           ├─ RealCodexAppServerClient
                           └─ RealGeminiAcpClient
```

<a id="architecture-key-types"></a>
Key types (`infra/channel/contract.rs`, re-exported by `infra/channel.rs`):

| Type | Purpose |
|------|---------|
| `TurnRequest` | Input payload: `reasoning_level`, folder, `live_session_output`, model, `request_kind`, prompt, and `provider_conversation_id`. |
| `TurnEvent` | Incremental stream events: `AssistantDelta`, `ThoughtDelta`, `Progress`, `Completed`, `Failed`, `PidUpdate`. |
| `TurnResult` | Normalized output: `assistant_message`, token counts, `provider_conversation_id`. |
| `AgentRequestKind` | `SessionStart`, `SessionResume` (with optional session output replay), or `UtilityPrompt`. |

<a id="architecture-provider-conversation-id-flow"></a>
Provider conversation id flow:

- App-server providers return `provider_conversation_id` in `TurnResult`.
- Worker persists it to DB (`update_session_provider_conversation_id`).
- Future `TurnRequest` loads and forwards it so runtime restarts can resume native provider context.

## Agent Interaction Protocol Flow

<a id="architecture-agent-interaction-protocol"></a>
Provider output is normalized to one structured response protocol:

1. Prompt builders prepend the shared protocol preamble template and the self-descriptive `schemars` document, so every provider sees the same `answer`/`questions`/optional-`summary` schema and transport-enforced `outputSchema` paths can normalize that same contract separately.
   `crates/agentty/src/infra/agent/template/protocol_instruction_prompt.md` owns the normal request wrapper, `crates/agentty/src/infra/agent/prompt.rs` owns shared prompt preparation, and `crates/agentty/src/infra/agent/protocol.rs` routes to the authoritative protocol model/schema/parse submodules.
1. The caller selects one canonical `AgentRequestKind` before transport handoff, and the transport derives the matching `ProtocolRequestProfile` from it. Session turns use `SessionStart` or `SessionResume`, while isolated utility prompts use `UtilityPrompt`.
1. Session discussion turns typically populate `summary.turn` and `summary.session`, while one-shot prompts may leave `summary` unused.
1. Channels stream deltas/progress as `TurnEvent`.
1. Final output is parsed to protocol `answer`, `questions`, and the optional structured summary. The final assistant payload itself must match the shared protocol JSON object, while direct deserialization into the shared wire type still accepts summary-only or otherwise defaulted top-level fields. If a provider prepends prose before one final schema object, parsing now recovers that trailing payload as long as nothing except whitespace follows it.
1. Worker persists final display text, raw summary payload, and question payloads, then emits `AgentResponseReceived`.

<a id="architecture-agent-interaction-streaming"></a>
Streaming behavior differs by transport/provider:

- CLI channel (`CliAgentChannel`): parses stdout lines into `AssistantDelta`
  and `Progress`; keeps raw output for final parse. Claude now uses its
  documented `stream-json` output path here so compaction/tool-use progress can
  surface without waiting for a single final JSON payload.
- CLI prompt submission can stream the fully rendered prompt through stdin for
  providers that would otherwise exceed argv limits on large diffs or one-shot
  utility prompts.
- Shared CLI subprocess helpers under `crates/agentty/src/infra/agent/cli/`
  now own stdin piping and provider-aware exit guidance so session turns and
  one-shot prompts use the same subprocess behavior.
- App-server channel (`AppServerAgentChannel`): bridges `AppServerStreamEvent` to `TurnEvent`.
- One-shot prompt submission asks the concrete backend for its transport path,
  so app-server providers (Codex and Gemini) resolve their own runtime client
  and Claude stays on direct CLI subprocess execution.
- Codex thought phases (`thinking`/`plan`/`reasoning`/`thought`) stream as `ThoughtDelta`.
- Provider capabilities in `crates/agentty/src/infra/agent/provider.rs` centralize whether transports stream assistant chunks live, require strict final protocol validation, classify app-server phase labels as thought output, and construct provider app-server clients.
- Strict providers suppress streamed assistant chunks when needed so malformed first-pass protocol JSON is not persisted.
- Wrapped stream chunks that end in one valid protocol payload are normalized
  down to that payload's `answer`, so recovered schema output does not persist
  any prefatory prose.
- Gemini ACP still accumulates streamed assistant chunks internally for its
  final turn result, but the runtime now prefers the completed
  `session/prompt` payload whenever that payload parses as protocol JSON and
  the streamed accumulation does not.
- Worker persistence behavior: streamed `ThoughtDelta` and `Progress` updates drive transient progress badges and are not appended to session transcript output.

<a id="architecture-agent-interaction-validation"></a>
Final-output validation:

- Claude, Gemini, and Codex use strict protocol parsing and return an error
  immediately when invalid.
- One-shot agent submissions still surface schema errors directly to the
  caller whenever the shared parser rejects the final output, including plain
  text, blank utility responses, non-utility prompts that miss the schema, or
  any output that leaves trailing non-whitespace text after the recovered
  protocol payload.
- App-server restart retries and context-reset transcript replays still preserve the original protocol profile for normal prompt rendering through the shared `infra/app_server/` prompt and retry modules.

## Clarification Question Loop

<a id="architecture-agent-question-loop"></a>
Question-mode loop:

1. Worker receives final parsed response containing clarification questions in `questions`.
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
| Git status poller loop | App startup (if project has git branch), project switch, and session refreshes that change published session branches | `TaskService::spawn_git_status_task` | `AppEvent::GitStatusUpdated` | Periodic fetch plus one repo-level branch-tracking snapshot, then maps that snapshot to the active project branch and all published session branches in that project (about every 30s). |
| Version check one-shot | App startup | `TaskService::spawn_version_check_task` | `AppEvent::VersionAvailabilityUpdated` | Checks npm latest version tag and reports update availability. |
| Per-session worker loop | First command enqueue for a session | `SessionWorkerService::spawn_session_worker` | DB `session_operation` updates, app/session updates | Serializes all turn commands per session and manages channel lifecycle. |
| Per-turn turn-event consumer | Every queued turn execution | `run_channel_turn` | Output append, progress updates, pid slot updates | Consumes `TurnEvent` stream and applies immediate side effects. |
| CLI stdout/stderr readers | Every CLI-backed turn | `CliAgentChannel::run_turn` | `TurnEvent` stream + raw buffers | Reads subprocess streams and emits incremental deltas/progress. |
| App-server stream bridge | Every app-server-backed turn | `AppServerAgentChannel::run_turn` | `TurnEvent` stream | Bridges `AppServerStreamEvent` to unified turn events. |
| Clipboard image persistence | Prompt input `Ctrl+V` or `Alt+V` | `runtime/mode/prompt::handle_prompt_image_paste` | Temp PNG under `AGENTTY_ROOT/tmp/<session-id>/images/`, prompt attachment state | Reads a clipboard image or PNG path via `spawn_blocking`, persists it, and inserts an inline `[Image #n]` placeholder. |
| Session title generation | First `Start` turn, before main turn execution | `spawn_start_turn_title_generation` | DB title + `AppEvent::RefreshSessions` | Runs one-shot title prompt in background and persists generated title if valid. |
| At-mention file indexing | Prompt input or question free-text input activates `@` mention mode | `runtime/mode/prompt::activate_at_mention`, `runtime/mode/question::activate_question_at_mention` | `AppEvent::AtMentionEntriesLoaded` | Lists session files (`spawn_blocking`) and updates mention picker entries for the active composer. |
| Background session-size refresh | Enter on session in list mode | `App::refresh_session_size_in_background` | DB size + `AppEvent::RefreshSessions` | Computes diff-size bucket without blocking key handling path. |
| Session-view branch-publish action | Session view `p` in `Review`, then publish popup `Enter` | `App::start_publish_branch_action` | `AppEvent::BranchPublishActionCompleted` | Collects an optional remote branch name before first publish, locks to the existing upstream after publish, then runs `git push --force-with-lease` for the session branch in the background and updates the session-view popup with success or recovery guidance. |
| Deferred session cleanup | Delete with deferred cleanup path | `delete_selected_session_deferred_cleanup` | Filesystem/git side effects | Removes worktree folder and branch asynchronously after DB deletion. |
| Focused review assist | View mode focused-review open when diff is reviewable | `TaskService::spawn_review_assist_task` | `ReviewPrepared` / `ReviewPreparationFailed` | Runs model review prompt and stores final review text or error. |
| Sync-main workflow task | List-mode sync action (`s`) | `TokioSyncMainRunner::start_sync_main` | `AppEvent::SyncMainCompleted` | Pull-rebase/push selected project branch, with assisted conflict flow. |
| Session merge task | Merge confirmation accepted | `SessionMergeService::merge_session` | Output append, status updates, session metadata updates | Runs rebase, reuses the single evolving session-branch `HEAD` commit message for squash merge, then cleans up the worktree in background. |
| Session rebase task | Rebase action in view mode | `SessionMergeService::rebase_session` | Output append, status updates | Runs assisted rebase and returns session to `Review`/`Question`. |

## Sync, Merge, and Rebase Flows

<a id="architecture-runtime-flow-git-workflows"></a>
Project and session git workflows use shared boundaries (`GitClient`, `FsClient`, assist helpers) but have distinct orchestration paths:

- `sync main`: selected project branch pull/rebase/push, optional assisted conflict resolution, popup result summary.
- session merge: queue-aware workflow, assisted rebase first, reuse the single evolving session-branch `HEAD` commit message for the squash commit into the base branch, then clean up the worktree and set status `Done`.
- session rebase: assisted rebase of session branch onto base branch, returns to `Review` after completion/failure reporting.
- session branch publish: review-ready sessions push the session branch through `GitClient` with `--force-with-lease`; pull request or merge request creation is left to the user's manual forge workflow.

## Persistence and Recovery Boundaries

<a id="architecture-runtime-flow-persistence"></a>
Persistence invariants that shape runtime flow:

- DB opens with SQLite WAL and `foreign_keys = ON`, then embedded migrations run at startup.
- Session snapshots in memory are authoritative for rendering; DB is authoritative for restart recovery.
- Shared session handles (`output`, `status`, `child_pid`) provide low-latency updates between DB reloads.
- Event-driven refresh is primary (`RefreshSessions`); metadata polling is fallback safety only.
- External integrations (`GitClient`, `ReviewRequestClient`, `AppServerClient`, `AgentChannel`, `EventSource`, `FsClient`, `TmuxClient`) isolate side effects and enable deterministic tests.
