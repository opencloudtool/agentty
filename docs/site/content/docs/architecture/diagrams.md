+++
title = "Architecture Diagrams"
description = "Comprehensive Mermaid diagrams for workspace crates, module layers, pipelines, state machines, and data flows."
weight = 6
+++

<a id="architecture-diagrams-introduction"></a>
Visual reference diagrams for every major architectural surface in the Agentty
workspace. Each section targets one crate, layer, or cross-cutting pipeline.

<!-- more -->

## 1. Workspace Crate Dependency Graph

```mermaid
graph TD
    subgraph Workspace["agentty Workspace"]
        AGENTTY["agentty<br/><i>Main TUI Binary</i>"]
        FORGE["ag-forge<br/><i>Forge Review-Request Library</i>"]
        TESTTY["testty<br/><i>E2E TUI Testing Framework</i>"]
        XTASK["ag-xtask<br/><i>Workspace Automation</i>"]
    end

    AGENTTY -->|"uses"| FORGE
    TESTTY -.->|"tests"| AGENTTY

    subgraph External["Key External Dependencies"]
        RATATUI["ratatui"]
        SQLX["sqlx (SQLite)"]
        TOKIO["tokio"]
        CROSSTERM["crossterm"]
        CLAP["clap"]
        ASKAMA["askama"]
        PORTABLE_PTY["portable-pty"]
    end

    AGENTTY --> RATATUI
    AGENTTY --> SQLX
    AGENTTY --> TOKIO
    AGENTTY --> CROSSTERM
    AGENTTY --> ASKAMA
    TESTTY --> PORTABLE_PTY
    XTASK --> CLAP
```

## 2. Main Crate Layered Architecture

```mermaid
graph TD
    subgraph CompositionRoot["Composition Root"]
        MAIN["main.rs<br/>DB bootstrap, App construction, runtime launch"]
        LIB["lib.rs<br/>Module exports"]
    end

    subgraph RuntimeLayer["Runtime Layer"]
        RT_CORE["runtime/core.rs<br/>Event loop, render cycle"]
        RT_EVENT["runtime/event.rs<br/>EventSource, AppEvent, polling"]
        RT_KEY["runtime/key_handler.rs<br/>Mode dispatch"]
        RT_MODE["runtime/mode/*<br/>Per-mode key handlers"]
        RT_CLIP["runtime/clipboard_image.rs<br/>Image paste"]
    end

    subgraph UILayer["UI Layer"]
        UI_RENDER["ui/render.rs<br/>Frame composition, RenderContext"]
        UI_ROUTER["ui/router.rs<br/>Mode-to-Page routing"]
        UI_PAGES["ui/page/*<br/>Full-screen pages"]
        UI_COMP["ui/component/*<br/>Reusable widgets"]
        UI_STATE["ui/state/*<br/>AppMode, prompt state"]
        UI_STYLE["ui/style.rs<br/>Semantic color palette"]
    end

    subgraph AppLayer["App Layer"]
        APP_CORE["app/core.rs<br/>App facade, event reducer"]
        APP_SVC["app/service.rs<br/>AppServices DI container"]
        APP_SESS["app/session/*<br/>SessionManager, workflows"]
        APP_PROJ["app/project.rs<br/>ProjectManager"]
        APP_TAB["app/tab.rs<br/>TabManager"]
        APP_TASK["app/task.rs<br/>Background tasks"]
        APP_MQ["app/merge_queue.rs<br/>Merge queue state machine"]
        APP_SET["app/setting.rs<br/>SettingsManager"]
    end

    subgraph DomainLayer["Domain Layer"]
        DOM_AGENT["domain/agent.rs<br/>AgentKind, AgentModel"]
        DOM_SESS["domain/session.rs<br/>Session, Status, SessionSize"]
        DOM_PROJ["domain/project.rs<br/>Project entity"]
        DOM_SET["domain/setting.rs<br/>SettingName keys"]
        DOM_PERM["domain/permission.rs<br/>PermissionMode"]
        DOM_INPUT["domain/input.rs<br/>InputState"]
    end

    subgraph InfraLayer["Infrastructure Layer"]
        INF_DB["infra/db.rs<br/>SQLite persistence"]
        INF_GIT["infra/git/*<br/>GitClient trait + ops"]
        INF_CH["infra/channel/*<br/>AgentChannel trait + impls"]
        INF_AGENT["infra/agent/*<br/>Backend, provider, protocol"]
        INF_FS["infra/fs.rs<br/>FsClient trait"]
        INF_AS["infra/app_server/*<br/>App-server infra"]
        INF_TMUX["infra/tmux.rs<br/>TmuxClient"]
        INF_FI["infra/file_index.rs<br/>File indexing"]
    end

    MAIN --> RT_CORE
    MAIN --> APP_CORE
    RT_CORE --> RT_EVENT
    RT_CORE --> UI_RENDER
    RT_EVENT --> RT_KEY
    RT_KEY --> RT_MODE
    RT_MODE --> APP_CORE
    UI_RENDER --> UI_ROUTER
    UI_ROUTER --> UI_PAGES
    UI_PAGES --> UI_COMP
    UI_PAGES --> UI_STYLE
    APP_CORE --> APP_SVC
    APP_CORE --> APP_SESS
    APP_CORE --> APP_PROJ
    APP_CORE --> APP_TAB
    APP_CORE --> APP_MQ
    APP_SESS --> INF_CH
    APP_SESS --> INF_DB
    APP_SESS --> INF_GIT
    APP_SVC --> INF_DB
    APP_SVC --> INF_GIT
    APP_SVC --> INF_FS
    APP_TASK --> INF_GIT
    APP_TASK --> INF_AGENT
    INF_CH --> INF_AGENT
    INF_CH --> INF_AS
    AppLayer --> DomainLayer
    UILayer --> DomainLayer
    RuntimeLayer --> AppLayer
```

## 3. Session Status State Machine

```mermaid
stateDiagram-v2
    [*] --> New : create_session()

    New --> InProgress : start_session(prompt)
    New --> Rebasing : rebase triggered

    InProgress --> Review : turn complete + auto-commit
    InProgress --> AgentReview : review assist running
    InProgress --> Question : turn returns questions

    Rebasing --> Review : rebase succeeded
    Rebasing --> Question : rebase needs input

    Review --> AgentReview : review assist starts
    AgentReview --> Review : review assist completes

    Review --> InProgress : reply(prompt)
    Review --> Queued : merge requested (queue busy)
    Review --> Merging : merge requested (queue idle)
    Review --> Rebasing : rebase requested
    Review --> Canceled : cancel requested

    AgentReview --> InProgress : reply(prompt)
    AgentReview --> Queued : merge requested
    AgentReview --> Merging : merge requested
    AgentReview --> Rebasing : rebase requested
    AgentReview --> Canceled : cancel requested

    Question --> InProgress : answer submitted
    Question --> Queued : merge requested
    Question --> Rebasing : rebase requested
    Question --> Merging : merge requested
    Question --> Canceled : cancel requested

    Queued --> Merging : queue advances
    Queued --> Review : dequeued
    Queued --> AgentReview : dequeued with review

    Merging --> Done : merge succeeded
    Merging --> Review : merge failed
    Merging --> AgentReview : merge failed with review

    Done --> [*]
    Canceled --> [*]
```

## 4. Runtime Event Loop

```mermaid
sequenceDiagram
    participant Main as main.rs
    participant Guard as TerminalGuard
    participant Terminal as Terminal (CrosstermBackend)
    participant EventReader as Event Reader Thread
    participant Loop as run_main_loop()
    participant App as App
    participant UI as ui::render()

    Main->>Guard: TerminalGuard::new()
    Main->>Terminal: setup_terminal()
    Main->>EventReader: spawn_event_reader(event_tx, shutdown)

    loop Every FRAME_INTERVAL tick
        Loop->>App: sessions.sync_from_handles()
        Loop->>Terminal: render_frame(app)
        Terminal->>App: app.draw(frame)
        App->>UI: build RenderContext + route_frame()

        Loop->>Loop: process_events()
        alt Terminal KeyEvent received
            EventReader-->>Loop: crossterm::Event via event_rx
            Loop->>App: key_handler::handle_key_event()
            App-->>Loop: EventResult::Continue or Quit
        else AppEvent received
            App-->>Loop: AppEvent via app.next_app_event()
            Loop->>App: apply_app_events()
        else Tick fires
            Loop->>App: apply_app_events()
        end
    end

    Loop-->>Main: EventResult::Quit
    Main->>EventReader: shutdown.store(true)
    Main->>Terminal: show_cursor()
```

## 5. AppMode Dispatch and UI Routing

```mermaid
graph TD
    KEY["Key Event"] --> KH["key_handler.rs"]

    KH -->|"AppMode::List"| LIST_H["mode/list.rs"]
    KH -->|"AppMode::View"| VIEW_H["mode/session_view.rs"]
    KH -->|"AppMode::Prompt"| PROMPT_H["mode/prompt.rs"]
    KH -->|"AppMode::Question"| QUESTION_H["mode/question.rs"]
    KH -->|"AppMode::Diff"| DIFF_H["mode/diff.rs"]
    KH -->|"AppMode::Help"| HELP_H["mode/help.rs"]
    KH -->|"AppMode::Confirmation"| CONF_H["mode/confirmation.rs"]
    KH -->|"AppMode::SyncBlockedPopup"| SYNC_H["mode/sync_blocked.rs"]
    KH -->|"AppMode::OpenCommandSelector"| OPEN_H["key_handler (inline)"]
    KH -->|"AppMode::PublishBranchInput"| PUB_H["key_handler (inline)"]
    KH -->|"AppMode::ViewInfoPopup"| INFO_H["key_handler (inline)"]

    subgraph UIRouting["ui/router.rs - Page Rendering"]
        LIST_P["SessionListPage<br/>ProjectListPage<br/>StatPage<br/>SettingPage<br/>TaskPage"]
        CHAT_P["SessionChatPage"]
        DIFF_P["DiffPage"]
        OVERLAY["Overlays<br/>Help / Confirmation /<br/>Info / OpenCommand /<br/>PublishBranch"]
    end

    LIST_H -.->|"renders"| LIST_P
    VIEW_H -.->|"renders"| CHAT_P
    PROMPT_H -.->|"renders"| CHAT_P
    QUESTION_H -.->|"renders"| CHAT_P
    DIFF_H -.->|"renders"| DIFF_P
    HELP_H -.->|"renders"| OVERLAY
    CONF_H -.->|"renders"| OVERLAY
```

## 6. App Facade and Event Reducer

```mermaid
graph TD
    subgraph AppStruct["App Struct"]
        MODE["mode: AppMode"]
        TABS["tabs: TabManager"]
        SESSIONS["sessions: SessionManager"]
        PROJECTS["projects: ProjectManager"]
        SERVICES["services: AppServices"]
        MQ["merge_queue: MergeQueue"]
        SETTINGS["settings: SettingsManager"]
        REVIEW_CACHE["review_cache"]
        EVENT_RX["event_rx"]
    end

    subgraph AppServices["AppServices DI Container"]
        DB["db: Database"]
        GIT["git_client: Arc dyn GitClient"]
        FS["fs_client: Arc dyn FsClient"]
        FORGE_C["review_request_client: Arc dyn ReviewRequestClient"]
        CLOCK["clock: Arc dyn Clock"]
        EVENT_TX["event_tx: mpsc::UnboundedSender"]
        AGENTS["available_agent_kinds: Arc Vec AgentKind"]
    end

    subgraph Reducer["Event Reducer Pipeline"]
        DRAIN["1. Drain all queued events"]
        COALESCE["2. Coalesce per-session (latest-wins)"]
        APPLY["3. Apply batch mutations"]
    end

    EVENT_RX --> DRAIN
    DRAIN --> COALESCE
    COALESCE --> APPLY

    APPLY -->|"RefreshSessions"| SESSIONS
    APPLY -->|"GitStatusUpdated"| PROJECTS
    APPLY -->|"AgentResponseReceived"| SESSIONS
    APPLY -->|"ReviewPrepared"| REVIEW_CACHE
    APPLY -->|"SessionSizeUpdated"| SESSIONS
    APPLY -->|"VersionAvailabilityUpdated"| AppStruct
    APPLY -->|"merge queue progress"| MQ
```

## 7. AppEvent Variants

```mermaid
graph LR
    subgraph Metadata["Metadata Updates"]
        E1["AtMentionEntriesLoaded"]
        E2["GitStatusUpdated"]
        E3["SessionModelUpdated"]
        E4["SessionReasoningLevelUpdated"]
        E5["SessionProgressUpdated"]
    end

    subgraph Refresh["Refresh Triggers"]
        E6["RefreshSessions"]
        E7["RefreshGitStatus"]
    end

    subgraph Completion["Completion Events"]
        E8["SyncMainCompleted"]
        E9["BranchPublishActionCompleted"]
        E10["ReviewPrepared"]
        E11["ReviewPreparationFailed"]
        E12["SessionTitleGenerationFinished"]
        E13["SessionSizeUpdated"]
        E14["SessionUpdated"]
        E15["AgentResponseReceived"]
    end

    subgraph Status["Status Changes"]
        E16["VersionAvailabilityUpdated"]
        E17["UpdateStatusChanged"]
    end

    E15 -->|"carries"| TAS["TurnAppliedState<br/>summary, questions,<br/>follow_up_tasks, usage"]
```

## 8. Turn Execution Pipeline

```mermaid
sequenceDiagram
    participant User as User Input
    participant Mode as runtime/mode/*
    participant App as App / SessionManager
    participant Worker as SessionWorkerService
    participant Channel as AgentChannel
    participant Backend as AgentBackend / AppServer
    participant Events as AppEvent bus

    User->>Mode: Keypress (Enter in Prompt)
    Mode->>App: start_session(id, prompt) or reply(id, prompt)
    App->>App: persist operation_id in DB
    App->>Worker: enqueue_session_command(Run {...})
    Worker->>Worker: ensure_session_worker()

    alt Worker not running
        Worker->>Worker: create AgentChannel
        Worker->>Worker: spawn_session_worker() background loop
    end

    Worker->>Channel: start_session(folder, session_id)
    Channel-->>Worker: SessionRef

    Worker->>Channel: run_turn(session_id, TurnRequest, event_tx)

    alt CLI Transport (Claude)
        Channel->>Backend: build_command() then Command
        Backend-->>Channel: spawned subprocess
        loop Streaming
            Channel-->>Events: TurnEvent::ThoughtDelta(text)
        end
        Channel->>Channel: parse stdout then AgentResponse
    else App-Server Transport (Codex/Gemini)
        Channel->>Backend: AppServerTurnRequest
        Backend-->>Channel: AppServerStreamEvent stream
        loop Streaming
            Channel-->>Events: TurnEvent::ThoughtDelta(text)
        end
        Channel-->>Channel: AppServerTurnResponse
    end

    Channel-->>Worker: TurnResult with answer, questions, follow_ups, summary
    Worker->>Worker: persist turn metadata to DB
    Worker->>Worker: handle_auto_commit()

    alt Has changes
        Worker->>Worker: commit_changes_with_assist()
        Worker->>Worker: update session title
    end

    Worker->>Events: AgentResponseReceived
    Events->>App: apply_app_events() via reducer
    App->>App: update SessionState, mode transitions
```

## 9. Infrastructure Trait Boundaries

```mermaid
graph TD
    subgraph Traits["Mockable Trait Boundaries"]
        T_GIT["GitClient<br/>30+ async methods<br/>worktree, merge, rebase,<br/>diff, commit, push"]
        T_FS["FsClient<br/>create_dir_all, remove_dir_all,<br/>read_file, write_file,<br/>remove_file, is_dir, is_file"]
        T_CH["AgentChannel<br/>start_session()<br/>run_turn()<br/>shutdown_session()"]
        T_AS["AppServerClient<br/>run_turn()<br/>shutdown_session()"]
        T_RR["ReviewRequestClient<br/>detect_remote, find_by_source_branch,<br/>create_review_request,<br/>refresh_review_request"]
        T_TMUX["TmuxClient<br/>open_window_for_folder()<br/>run_command_in_window()"]
        T_BACK["AgentBackend<br/>setup(folder)<br/>build_command(request)"]
        T_EVT["EventSource<br/>poll(timeout)<br/>read()"]
        T_CLK["Clock<br/>wall_clock_unix_seconds()<br/>monotonic_now()"]
        T_SLEEP["Sleeper<br/>sleep(duration)"]
    end

    subgraph Impls["Production Implementations"]
        I_GIT["RealGitClient"]
        I_FS["RealFsClient"]
        I_CLI["CliAgentChannel<br/>(Claude)"]
        I_APP["AppServerAgentChannel<br/>(Codex, Gemini)"]
        I_RR["RealReviewRequestClient"]
        I_TMUX["RealTmuxClient"]
        I_CLAUDE["ClaudeBackend"]
        I_CODEX["CodexBackend"]
        I_GEMINI["GeminiBackend"]
    end

    T_GIT --> I_GIT
    T_FS --> I_FS
    T_CH --> I_CLI
    T_CH --> I_APP
    T_RR --> I_RR
    T_TMUX --> I_TMUX
    T_BACK --> I_CLAUDE
    T_BACK --> I_CODEX
    T_BACK --> I_GEMINI
    I_APP --> T_AS
```

## 10. Agent Provider Routing

```mermaid
graph TD
    AK["AgentKind"] -->|"Claude"| CLAUDE_PATH
    AK -->|"Codex"| CODEX_PATH
    AK -->|"Gemini"| GEMINI_PATH

    subgraph CLAUDE_PATH["Claude Pipeline"]
        C_BACK["ClaudeBackend<br/>build_command()"]
        C_TRANS["Transport: CLI"]
        C_PROMPT["Prompt: Argv"]
        C_CHAN["CliAgentChannel"]
        C_PARSE["claude::parse_response()"]
    end

    subgraph CODEX_PATH["Codex Pipeline"]
        X_BACK["CodexBackend<br/>build_command()"]
        X_TRANS["Transport: AppServer"]
        X_PROMPT["Prompt: Stdin"]
        X_CHAN["AppServerAgentChannel"]
        X_CLIENT["CodexAppServerClient"]
        X_PARSE["codex::parse_response()"]
    end

    subgraph GEMINI_PATH["Gemini Pipeline"]
        G_BACK["GeminiBackend<br/>build_command()"]
        G_TRANS["Transport: AppServer"]
        G_PROMPT["Prompt: Stdin"]
        G_CHAN["AppServerAgentChannel"]
        G_CLIENT["GeminiAppServerClient"]
        G_PARSE["gemini::parse_response()"]
    end

    subgraph Registry["provider.rs - AgentProviderDescriptor"]
        REG_BF["backend_factory"]
        REG_T["transport"]
        REG_PT["prompt_transport"]
        REG_ASF["app_server_client_factory"]
        REG_PR["parse_response"]
        REG_PSL["parse_stream_output_line"]
        REG_ATP["app_server_thought_policy"]
    end

    AK --> Registry
    Registry -->|"creates"| C_BACK
    Registry -->|"creates"| X_BACK
    Registry -->|"creates"| G_BACK
```

## 11. Domain Entity Relationships

```mermaid
erDiagram
    Project ||--o{ Session : "has many"
    Session ||--o{ SessionFollowUpTask : "has many"
    Session ||--o| ReviewRequest : "may have"
    Session }o--|| AgentModel : "uses"
    AgentModel }o--|| AgentKind : "belongs to"
    ReviewRequest }o--|| ReviewRequestSummary : "wraps"
    ReviewRequestSummary }o--|| ForgeKind : "from"
    ReviewRequestSummary }o--|| ReviewRequestState : "has"
    Session }o--|| Status : "current"
    Session }o--|| SessionSize : "calculated"
    Session ||--|| SessionStats : "has"
    Session ||--|| SessionHandles : "runtime"
    Session }o--o| ReasoningLevel : "override"

    Project {
        i64 id PK
        string path
        string display_name
        string git_branch
        bool is_favorite
        i64 created_at
        i64 updated_at
        i64 last_opened_at
    }

    Session {
        string id PK
        string project_name
        string base_branch
        string folder
        string prompt
        string output
        string title
        string summary
        bool is_draft
        i64 created_at
        i64 updated_at
    }

    SessionFollowUpTask {
        i64 id PK
        string text
        usize position
        string launched_session_id
    }

    AgentKind {
        enum Claude
        enum Codex
        enum Gemini
    }

    AgentModel {
        enum ClaudeOpus46
        enum ClaudeSonnet46
        enum ClaudeHaiku4520251001
        enum Gpt54
        enum Gpt53CodexSpark
        enum Gemini3FlashPreview
        enum Gemini31ProPreview
    }

    Status {
        enum New
        enum InProgress
        enum Review
        enum AgentReview
        enum Question
        enum Queued
        enum Rebasing
        enum Merging
        enum Done
        enum Canceled
    }

    SessionSize {
        enum Xs_0to10
        enum S_11to30
        enum M_31to80
        enum L_81to200
        enum Xl_201to500
        enum Xxl_501plus
    }
```

## 12. Session Manager and Worker Architecture

```mermaid
graph TD
    subgraph SessionManager["SessionManager"]
        SM_STATE["state: SessionState<br/>sessions list, handles map, table_state,<br/>git_statuses map, follow_up_positions map"]
        SM_WORKER["worker_service: SessionWorkerService<br/>workers map of session_id to mpsc::Sender,<br/>test_agent_channels map"]
        SM_MERGE["merge_service: SessionMergeService"]
        SM_TITLE["title_generation_tasks map"]
        SM_REPLAY["pending_history_replay set"]
        SM_OUTPUT["active_prompt_outputs map"]
    end

    subgraph Worker["Per-Session Worker (background task)"]
        W_LOOP["command queue loop"]
        W_CHAN["Arc dyn AgentChannel"]
        W_CTX["SessionWorkerContext<br/>db, fs, git, event_tx"]
    end

    SM_WORKER -->|"ensure_session_worker()"| Worker
    SM_WORKER -->|"enqueue"| W_LOOP
    W_LOOP -->|"SessionCommand::Run"| W_CHAN
    W_CHAN -->|"TurnResult"| W_LOOP
    W_LOOP -->|"AppEvent"| EVENT_BUS["event_tx"]
    W_LOOP -->|"auto-commit"| TASK_SVC["SessionTaskService"]

    TASK_SVC -->|"commit_changes_with_assist()"| GIT_CLIENT["GitClient"]
    TASK_SVC -->|"generate commit msg"| W_CHAN

    EVENT_BUS --> REDUCER["App::apply_app_events()"]
    REDUCER --> SM_STATE
```

## 13. Merge Queue State Machine

```mermaid
stateDiagram-v2
    [*] --> Idle : queue empty

    Idle --> Active : enqueue(session_id)

    state Active {
        [*] --> Merging
        Merging --> MergeDone : merge succeeded
        Merging --> MergeFailed : merge failed
    }

    Active --> PopNext : active session exits Merging
    PopNext --> Active : queue has next session
    PopNext --> Idle : queue empty

    state QueuedSessions {
        [*] --> Waiting
        Waiting --> Waiting : more enqueued
    }

    QueuedSessions --> Active : pop_next()
```

## 14. UI Frame Composition

```mermaid
graph TD
    FRAME["Terminal Frame"]

    FRAME --> LAYOUT["Vertical Split"]
    LAYOUT --> STATUS_BAR["StatusBar (1 row)<br/>version, update status"]
    LAYOUT --> CONTENT["Content Area (flex)"]
    LAYOUT --> FOOTER["FooterBar (1 row)<br/>git branch, working dir"]

    CONTENT --> ROUTER["ui/router.rs<br/>route_frame()"]

    ROUTER -->|"List modes"| LIST_BG["render_list_background()"]

    LIST_BG --> TAB_W["Tabs component<br/>Projects / Sessions / Tasks / Stats / Settings"]

    TAB_W -->|"Projects"| PROJ_PAGE["ProjectListPage"]
    TAB_W -->|"Sessions"| SESS_PAGE["SessionListPage"]
    TAB_W -->|"Tasks"| TASK_PAGE["TaskPage"]
    TAB_W -->|"Stats"| STAT_PAGE["StatPage<br/>activity_heatmap"]
    TAB_W -->|"Settings"| SET_PAGE["SettingPage"]

    ROUTER -->|"Prompt / View / Question"| CHAT_PAGE["SessionChatPage"]
    CHAT_PAGE --> SESS_OUT["SessionOutput<br/>transcript, follow-up tasks"]
    CHAT_PAGE --> CHAT_IN["ChatInput<br/>prompt editor, attachments"]

    ROUTER -->|"Diff"| DIFF_PAGE["DiffPage"]
    DIFF_PAGE --> FILE_EXP["FileExplorer<br/>file tree"]
    DIFF_PAGE --> DIFF_VIEW["Diff viewer<br/>syntax-highlighted"]

    ROUTER -->|"Overlays"| OVERLAYS["Overlay Layer"]
    OVERLAYS --> HELP_OV["HelpOverlay"]
    OVERLAYS --> CONF_OV["ConfirmationOverlay"]
    OVERLAYS --> INFO_OV["InfoOverlay"]
    OVERLAYS --> OPEN_OV["OpenCommandOverlay"]
    OVERLAYS --> PUB_OV["PublishBranchOverlay"]
```

## 15. ag-forge Crate Internals

```mermaid
graph TD
    subgraph PublicAPI["Public API"]
        RRC_TRAIT["ReviewRequestClient trait<br/>detect_remote()<br/>find_by_source_branch()<br/>create_review_request()<br/>refresh_review_request()<br/>review_request_web_url()"]
        REAL_RRC["RealReviewRequestClient"]
    end

    subgraph Model["Domain Types (model.rs)"]
        FK["ForgeKind<br/>GitHub"]
        FR["ForgeRemote<br/>host, namespace, project"]
        RRS["ReviewRequestSummary<br/>display_id, state, title, web_url"]
        RRSTATE["ReviewRequestState<br/>Open / Merged / Closed"]
        CRI["CreateReviewRequestInput<br/>source_branch, target_branch, title"]
        RRE["ReviewRequestError<br/>CliNotInstalled / AuthRequired /<br/>HostResolutionFailed /<br/>UnsupportedRemote / OperationFailed"]
    end

    subgraph Adapters["Provider Adapters"]
        GH["GitHubReviewRequestAdapter<br/>find, create, refresh"]
    end

    subgraph Infra["Command Infrastructure"]
        CMD_TRAIT["ForgeCommandRunner trait<br/>run(ForgeCommand) then ForgeCommandOutput"]
        REAL_CMD["RealForgeCommandRunner"]
        REMOTE["remote.rs<br/>detect_remote()<br/>parse_remote_url()"]
    end

    RRC_TRAIT --> REAL_RRC
    REAL_RRC --> GH
    REAL_RRC --> REMOTE
    GH --> CMD_TRAIT
    CMD_TRAIT --> REAL_CMD
    REAL_CMD -->|"spawns"| GH_CLI["gh CLI subprocess"]
    GH --> FK
    GH --> RRS
    REMOTE --> FR
```

## 16. testty Crate Testing Pipeline

```mermaid
graph TD
    subgraph TestAPI["Test Authoring API"]
        SCENARIO["Scenario<br/>name, steps list<br/>write_text(), press_key(),<br/>wait_for_text(), capture()"]
        JOURNEY["Journey<br/>composable building blocks<br/>wait_for_startup(),<br/>navigate_with_key(),<br/>type_and_confirm()"]
        STEP["Step enum<br/>WriteText / PressKey / Sleep /<br/>WaitForText / WaitForStableFrame /<br/>Capture / CaptureLabeled"]
    end

    subgraph Execution["PTY Execution"]
        BUILDER["PtySessionBuilder<br/>binary_path, cols, rows,<br/>env_vars, workdir"]
        SESSION["PtySession<br/>spawn, write_bytes, press_key,<br/>drain_output, capture_frame,<br/>wait_for_text, execute_steps"]
    end

    subgraph Assertion["Assertion Layer"]
        ASSERT["assertion.rs<br/>assert_text_in_region()<br/>assert_not_visible()<br/>assert_match_count()<br/>assert_text_has_fg_color()<br/>assert_span_is_highlighted()"]
        REGION["Region<br/>row, col, width, height"]
        TFRAME["TerminalFrame<br/>cell grid, text extraction"]
    end

    subgraph Proof["Proof Pipeline"]
        REPORT["ProofReport<br/>captures list, diffs list"]
        CAPTURE["ProofCapture<br/>label, frame_text, frame_bytes"]
        RESULT["AssertionResult<br/>passed, description"]
        BACKEND["ProofBackend trait"]
        FT["FrameTextBackend"]
        STRIP["StripBackend (PNG)"]
        GIF["GifBackend"]
        HTML["HtmlBackend"]
    end

    SCENARIO -->|"compose()"| JOURNEY
    SCENARIO -->|"step()"| STEP
    SCENARIO -->|"run()"| BUILDER
    BUILDER -->|"spawn()"| SESSION
    SESSION -->|"execute_steps()"| STEP
    SESSION -->|"capture_frame()"| TFRAME
    TFRAME --> ASSERT
    ASSERT --> REGION

    SCENARIO -->|"run_with_proof()"| REPORT
    REPORT --> CAPTURE
    CAPTURE --> RESULT
    REPORT -->|"save()"| BACKEND
    BACKEND --> FT
    BACKEND --> STRIP
    BACKEND --> GIF
    BACKEND --> HTML
```

## 17. ag-xtask Commands

```mermaid
graph TD
    CLI["ag-xtask CLI (clap)"]

    CLI -->|"check-migrations"| CM["check_migration::run()<br/>Validates SQL migration<br/>numbering sequence"]
    CLI -->|"workspace-map"| WM["workspace_map::run()<br/>Generates<br/>target/agentty/workspace-map.json"]
    CLI -->|"roadmap lint"| RL["roadmap::lint()<br/>Validates roadmap structure<br/>and queue rules"]
    CLI -->|"roadmap context-digest"| RCD["roadmap::context_digest()<br/>Prints roadmap-oriented digest"]
```

## 18. Git Operations Hierarchy

```mermaid
graph TD
    subgraph GitClientTrait["GitClient trait"]
        GC["30+ async methods"]
    end

    subgraph Modules["infra/git/ Implementation Modules"]
        WORKTREE["worktree.rs<br/>create_worktree()<br/>remove_worktree()<br/>detect_git_info()<br/>find_git_repo_root()"]
        MERGE["merge.rs<br/>squash_merge_diff()<br/>squash_merge()"]
        REBASE["rebase.rs<br/>rebase(), rebase_start()<br/>rebase_continue()<br/>abort_rebase()<br/>list_conflicted_files()"]
        REPO["repo.rs<br/>commit_all()<br/>commit_all_preserving_single_commit()<br/>stage_all(), diff()<br/>head_short_hash()<br/>is_worktree_clean()"]
        SYNC["sync.rs<br/>pull_rebase()<br/>push_current_branch()"]
    end

    subgraph Outcomes["Result Types"]
        SQO["SquashMergeOutcome<br/>Committed / AlreadyPresentInTarget"]
        RSR["RebaseStepResult<br/>Completed / Conflict"]
        PRR["PullRebaseResult"]
    end

    GC --> WORKTREE
    GC --> MERGE
    GC --> REBASE
    GC --> REPO
    GC --> SYNC
    MERGE --> SQO
    REBASE --> RSR
    SYNC --> PRR

    WORKTREE -->|"spawns"| GIT_CMD["git CLI subprocess<br/>(via spawn_blocking)"]
    MERGE --> GIT_CMD
    REBASE --> GIT_CMD
    REPO --> GIT_CMD
    SYNC --> GIT_CMD
```

## 19. Database Schema (Logical)

```mermaid
erDiagram
    project ||--o{ session : "has"
    project ||--o{ project_setting : "has"
    session ||--o{ session_follow_up_task : "has"
    session ||--o{ session_review_request : "has"

    project {
        INTEGER id PK
        TEXT path
        TEXT display_name
        TEXT git_branch
        INTEGER is_favorite
        INTEGER created_at
        INTEGER updated_at
        INTEGER last_opened_at
    }

    session {
        TEXT id PK
        INTEGER project_id FK
        TEXT base_branch
        TEXT folder
        TEXT model
        TEXT output
        TEXT prompt
        TEXT reasoning_level_override
        TEXT published_upstream_ref
        TEXT questions_json
        TEXT status
        TEXT summary
        TEXT title
        INTEGER is_draft
        INTEGER in_progress_started_at
        INTEGER in_progress_total_seconds
        INTEGER added_lines
        INTEGER deleted_lines
        INTEGER input_tokens
        INTEGER output_tokens
        INTEGER created_at
        INTEGER updated_at
    }

    session_follow_up_task {
        INTEGER id PK
        TEXT session_id FK
        TEXT text
        INTEGER position
        TEXT launched_session_id
    }

    session_review_request {
        INTEGER id PK
        TEXT session_id FK
        TEXT display_id
        TEXT forge_kind
        TEXT state
        TEXT title
        TEXT web_url
        INTEGER last_refreshed_at
    }

    setting {
        INTEGER id PK
        TEXT key
        TEXT value
    }

    project_setting {
        INTEGER id PK
        INTEGER project_id FK
        TEXT key
        TEXT value
    }
```

## 20. End-to-End Data Flow

```mermaid
graph TD
    USER["User"] -->|"keypress"| CROSSTERM["crossterm events"]
    CROSSTERM --> EVENT_READER["Event Reader Thread"]
    EVENT_READER -->|"mpsc channel"| RUNTIME["Runtime Event Loop<br/>runtime/core.rs"]

    RUNTIME -->|"render"| TERMINAL["Terminal<br/>ratatui"]
    RUNTIME -->|"key dispatch"| KEY_HANDLER["key_handler.rs"]
    KEY_HANDLER -->|"mode dispatch"| MODE_HANDLERS["runtime/mode/*"]
    MODE_HANDLERS -->|"actions"| APP["App facade<br/>app/core.rs"]

    APP -->|"session ops"| SESSION_MGR["SessionManager"]
    APP -->|"project ops"| PROJECT_MGR["ProjectManager"]
    APP -->|"settings"| SETTINGS_MGR["SettingsManager"]

    SESSION_MGR -->|"turn requests"| WORKER_SVC["SessionWorkerService"]
    WORKER_SVC -->|"per-session queue"| WORKER["Background Worker"]

    WORKER -->|"CLI (Claude)"| CLI_CHAN["CliAgentChannel"]
    WORKER -->|"RPC (Codex/Gemini)"| AS_CHAN["AppServerAgentChannel"]

    CLI_CHAN -->|"subprocess"| CLAUDE["claude CLI"]
    AS_CHAN -->|"JSON-RPC"| APP_SERVER["codex / gemini<br/>app-server process"]

    CLAUDE -->|"stdout stream"| CLI_CHAN
    APP_SERVER -->|"stream events"| AS_CHAN

    CLI_CHAN --> PROTOCOL["Protocol Parser<br/>infra/agent/protocol/*"]
    AS_CHAN --> PROTOCOL
    PROTOCOL -->|"AgentResponse"| WORKER

    WORKER -->|"auto-commit"| GIT["GitClient<br/>infra/git/*"]
    GIT -->|"subprocess"| GIT_CLI["git CLI"]

    WORKER -->|"AppEvent"| EVENT_BUS["Event Bus<br/>mpsc channel"]
    EVENT_BUS --> REDUCER["Event Reducer<br/>app/core.rs"]
    REDUCER -->|"state mutations"| APP

    APP -->|"RenderContext"| UI_RENDER["ui/render.rs"]
    UI_RENDER -->|"route"| ROUTER["ui/router.rs"]
    ROUTER --> PAGES["Pages + Components"]
    PAGES --> TERMINAL

    SESSION_MGR -->|"persistence"| DB["SQLite<br/>infra/db.rs"]
    SESSION_MGR -->|"forge ops"| FORGE["ReviewRequestClient<br/>ag-forge"]
    FORGE -->|"subprocess"| GH["gh CLI"]
```
