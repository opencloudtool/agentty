+++
title = "Testability Boundaries"
description = "Trait boundaries around external systems and testing guidance for deterministic orchestration."
weight = 5
+++

<a id="architecture-testability-introduction"></a>
Agentty keeps external systems behind trait boundaries so orchestration logic can
be tested deterministically.

<!-- more -->

## Testability and Boundaries

<a id="architecture-testability-boundaries"></a>
The traits below are mocked with `mockall`. Most use
`#[cfg_attr(test, mockall::automock)]`; shared workspace crates such as
`ag-forge` also expose test mocks through crate features for downstream tests.

| Trait | Module | Boundary |
|-------|--------|----------|
| `SyncMainRunner` | `app/core.rs` | App-level async sync orchestration trigger used by list-mode sync flows. |
| `ReviewRequestClient` | `crates/ag-forge/src/client.rs` | GitHub review-request detection and `gh` orchestration boundary. |
| `ForgeCommandRunner` | `crates/ag-forge/src/command.rs` | Provider CLI command execution boundary used to unit-test the GitHub review-request adapter without a live `gh` binary. |
| `GitClient` | `infra/git/client.rs` | Git/process operations (worktree, merge, rebase, diff, push, pull, and ahead/behind comparisons for both upstream-tracking and session-vs-base-branch status). |
| `FsClient` | `infra/fs.rs` | Async filesystem operations and path probes used by app or runtime orchestration, including non-blocking file reads, existence checks, canonicalization, session worktree cleanup, and prompt-image temp file or directory removal. |
| `TmuxClient` | `infra/tmux.rs` | Tmux subprocess operations for opening session worktrees and dispatching open commands. |
| `TmuxCommandRunner` | `infra/tmux.rs` | Internal tmux command boundary that keeps multi-command `send-keys` flows deterministic in unit tests. |
| `AgentChannel` | `infra/channel.rs` | Provider-agnostic turn execution (session init, run turn, shutdown). |
| `AgentBackend` | `infra/agent/backend.rs` | Per-provider setup and transport command construction. |
| `AgentAvailabilityProbe` | `infra/agent/availability.rs` | Machine-scoped backend discovery used to filter settings defaults and `/model` without shelling out directly from app or runtime orchestration. |
| `AppServerClient` | `infra/app_server/contract.rs` | Provider-specific app-server RPC execution and session runtime lifecycle. |
| `EventSource` | `runtime/event.rs` | Terminal event polling for deterministic event-loop tests. |
| `Clock` | `app/session/core.rs` | Shared wall-clock and monotonic time boundary used by session orchestration and runtime helpers such as pasted-image file naming. |
| `Backend` (generic) | `runtime/core.rs` | Runtime accepts `Terminal<B: Backend>` via `run_with_backend`, enabling in-process TUI tests with `TestBackend` without a real terminal. |
| `TerminalOperation` | `runtime/terminal.rs` | Terminal raw-mode and alternate-screen transitions for deterministic setup and restore failure-path tests. |
| `Sleeper` | `lib.rs` | Wall-clock sleep boundary used by retry/polling flows such as git rebase assistance. |
| `UpdateRunner` | `infra/version.rs` | npm install command execution for background auto-updates. |
| `VersionCommandRunner` | `infra/version.rs` | npm/curl command execution for update checks. |
| `ProjectDiscoveryClient` | `infra/project_discovery.rs` | Home-directory repository discovery used by startup catalog refresh without walking the real filesystem from `app/`. |
| `GitCommandRunner` | `infra/git/rebase.rs` | Rebase command invocation boundary for conflict/retry tests. |
| `SyncAssistClient` | `app/session/workflow/merge.rs` | Sync-rebase assistance execution boundary. |
| `SessionRepository` | `infra/db/session.rs` | Session row persistence, turn metadata storage, and session list queries without binding app workflows to the full `Database` surface. |
| `ProjectRepository` | `infra/db/project.rs` | Project persistence and project-list aggregation behind a narrow mockable boundary. |
| `ReviewRepository` | `infra/db/review.rs` | Session review-request linkage persistence used by branch publish and refresh flows. |
| `UsageRepository` | `infra/db/usage.rs` | Per-session model usage aggregation used by turn persistence and usage views. |
| `ActivityRepository` | `infra/db/activity.rs` | Session-activity history queries and backfill helpers used by startup and session list refresh. |
| `OperationRepository` | `infra/db/operation.rs` | Persisted session-operation lifecycle tracking used by worker restart recovery and cancellation. |
| `SettingRepository` | `infra/db/setting.rs` | Global and project-scoped setting persistence used by startup and settings orchestration. |
| `AppServerClient` retry helpers | `infra/app_server/retry.rs` | Shared restart-and-replay orchestration for provider runtimes without duplicating lifecycle policy in each provider. |
| `CodexRuntimeTransport` | `infra/agent/app_server/codex/transport.rs` | Codex stdio transport boundary for lifecycle, compaction, and turn-stream tests without scripted shell runtimes. |
| `GeminiRuntimeTransport` | `infra/agent/app_server/gemini/transport.rs` | ACP stdio transport boundary for Gemini runtime protocol tests. |

### Typed Error Enums at Infra Boundaries

<a id="architecture-typed-error-enums"></a>
Each infra boundary exposes a typed error enum instead of opaque `String` errors,
so the app layer can discriminate failure causes without parsing formatted messages.

| Error Type | Module | Variants | Wraps |
|------------|--------|----------|-------|
| `DbError` | `infra/db.rs` | `Migration`, `Query`, `Io` | `sqlx::Error` |
| `GitError` | `infra/git/error.rs` | `WorktreeCreate`, `WorktreeRemove`, `BranchDelete`, `Command`, etc. | `std::io::Error`, process exit details |
| `AppServerTransportError` | `infra/app_server_transport.rs` | `Io`, `ProcessTerminated`, `Timeout` | `std::io::Error` |
| `AppServerError` | `infra/app_server/error.rs` | `Transport`, `Provider`, `SessionNotFound`, `Shutdown` | `AppServerTransportError` via `#[from]` |
| `AgentError` | `infra/channel/contract.rs` | `AppServer`, `Backend`, `Io` | `AppServerError` via `#[from]` |

The conversion chain `AppServerTransportError` → `AppServerError::Transport` →
`AgentError::AppServer` allows `?`-propagation through the transport, provider,
and channel layers without collapsing causal context into formatted strings.

### Typed Error Propagation at the App Layer

<a id="architecture-app-layer-typed-errors"></a>
The app layer propagates infra errors through two orchestration-level enums
instead of flattening them to `String`:

| Error Type | Module | Wraps via `#[from]` |
|------------|--------|---------------------|
| `SessionError` | `app/session/error.rs` | `DbError`, `GitError`, `AppServerError`, plus `Workflow(String)` for contextual app-level failures |
| `AppError` | `app/error.rs` | `SessionError`, `DbError`, `GitError`, plus `Workflow(String)` for contextual app-level failures |

App-layer functions that cross infra boundaries return `AppError` or
`SessionError` so callers can discriminate failure causes by variant.
`SessionError::with_context` adds an operation-specific prefix to `Workflow`
messages (for example *"Commit assistance failed: …"*) while passing typed
infrastructure variants through unchanged. At event and display boundaries
(for example `AppEvent` variants and `ReviewCacheEntry`), errors are converted
to `String` via `Display` because those types require `Clone` and `Eq`, which
the infra error types cannot satisfy due to non-cloneable inner types such as
`std::io::Error`.

<a id="architecture-boundary-testing-guidance"></a>
When adding higher-level flows involving multiple external commands, prefer
injectable trait boundaries and `mockall`-based tests over flaky end-to-end
shell-heavy tests. Add a narrower internal command-runner boundary when a
public orchestration trait still needs deterministic coverage of subprocess
sequencing or retry behavior.

Apply the same rule to filesystem discovery and path probes in `app/` and
`runtime/`: route directory walking, `exists` checks, `canonicalize`, and file
copy or persistence helpers through an infra boundary instead of calling
`std::fs` or `Path` helpers directly from orchestration code.

Use the same pattern for time access in `app/` and `runtime/`: if orchestration
logic needs `Instant::now()` or `SystemTime::now()`, route that call through
the shared `Clock` boundary instead of calling the clock API directly in
production logic.

Session review-request publication and refresh follow this rule directly:
`SessionManager` combines `GitClient` with `ReviewRequestClient` so tests can
cover branch publish, duplicate detection, stored-link reuse, and archived
session refresh without live forge auth or network state.

## TUI E2E Testing Framework (`testty`)

<a id="architecture-tui-e2e-framework"></a>
The `testty` workspace crate provides a dual-oracle model for TUI end-to-end
testing. The PTY path (`portable-pty` + `vt100`) is the semantic oracle for text,
style, and location assertions; the VHS path is the visual oracle and review
artifact generator.

| Module | Purpose |
|--------|---------|
| `session` | PTY executor: spawns binaries in a pseudo-terminal, writes input, captures ANSI output. |
| `frame` | Terminal frame parser: converts ANSI bytes into a cell grid with text, color, and style access. |
| `region` | Rectangular region definitions with named anchors (top row, footer, quadrants, percentages). |
| `locator` | Text locators with style/color filtering for identifying TUI controls. |
| `assertion` | Structured matcher APIs: `assert_text_in_region`, `assert_span_is_highlighted`, `assert_match_count`. |
| `recipe` | Agent-friendly helpers: `expect_selected_tab`, `expect_keybinding_hint`, `expect_dialog_title`. |
| `scenario` / `step` | Scenario DSL: compose user journeys from steps, compile to PTY or VHS. |
| `vhs` | VHS tape compiler: generates `.tape` files from scenarios for visual screenshot capture. |
| `calibration` | Cell-to-pixel geometry mapping for screenshot overlays. |
| `overlay` | Bounding box and indicator rendering onto screenshot PNGs. |
| `snapshot` | Paired baseline workflow: visual PNG + semantic frame sidecar with environment-driven update mode. |
| `artifact` | Artifact directory management for test captures and failure diagnostics. |
