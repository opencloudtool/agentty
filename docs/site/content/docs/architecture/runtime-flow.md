+++
title = "Runtime Flow"
description = "Goals, workspace map, runtime event flow, and agent channel transport model."
weight = 2
+++

<a id="architecture-runtime-flow-introduction"></a>
This guide covers how Agentty is structured at runtime, from process bootstrap to
mode dispatch and turn execution.

<!-- more -->

## Architecture Goals

<a id="architecture-runtime-flow-goals"></a>
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

<a id="architecture-runtime-flow-main"></a>
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
