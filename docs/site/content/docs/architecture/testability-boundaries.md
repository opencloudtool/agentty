+++
title = "Testability Boundaries"
description = "Trait boundaries around external systems and testing guidance for deterministic orchestration."
weight = 5
+++

<a id="architecture-testability-introduction"></a> Agentty keeps external systems behind
trait boundaries so orchestration logic can be tested deterministically.

<!-- more -->

## Testability and Boundaries

<a id="architecture-testability-boundaries"></a> External-boundary traits are mocked
with `mockall`, usually via `#[cfg_attr(test, mockall::automock)]`; shared workspace
crates such as `ag-agent`, `ag-forge`, and `ag-git` expose test mocks through crate-root
exports gated by test features or test-only exports. The major boundaries and
application ports:

| Trait                      | Module                                       | Boundary                                                                                                                                                                                                                                                                                                                                                                                                                           |
| -------------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GitClient`                | `crates/ag-git/src/client.rs`                | Git and worktree operations (hook readiness and execution, merge, merge-conflict probes, rebase, diff, bounded preview-file reads, push, status, ahead/behind).                                                                                                                                                                                                                                                                    |
| `FsClient`                 | `infra/fs.rs`                                | Async filesystem operations and path probes.                                                                                                                                                                                                                                                                                                                                                                                       |
| `AgentChannel`             | `crates/ag-agent/src/channel.rs`             | Provider-agnostic turn execution.                                                                                                                                                                                                                                                                                                                                                                                                  |
| `OneShotClient`            | `crates/ag-agent/src/agent/submission.rs`    | Isolated structured prompts, including transport routing, protocol repair, runtime cleanup, and usage aggregation.                                                                                                                                                                                                                                                                                                                 |
| `AgentBackend`             | `crates/ag-agent/src/agent/backend.rs`       | Per-provider setup and transport command construction.                                                                                                                                                                                                                                                                                                                                                                             |
| `AppServerClient`          | `crates/ag-agent/src/app_server/contract.rs` | Provider-managed runtime execution and session lifecycle, including app-server RPC and persistent NDJSON processes.                                                                                                                                                                                                                                                                                                                |
| `ReviewRequestClient`      | `crates/ag-forge/src/client.rs`              | Review-request orchestration, comment loading, and thread reply/resolution through `gh`/`glab`.                                                                                                                                                                                                                                                                                                                                    |
| `SessionBackend`           | `crates/ag-session/src/service.rs`           | Clone-safe frontend-neutral session creation, complete by-id lookup, messaging, structured question answers, cancellation, merge, and review-request operations implemented by host applications.                                                                                                                                                                                                                                  |
| `EventSource`              | `runtime/event.rs`                           | Terminal event polling for deterministic event-loop tests.                                                                                                                                                                                                                                                                                                                                                                         |
| `Clock`                    | `infra/clock.rs`                             | Wall-clock, UTC-offset, and monotonic time for session orchestration, activity timestamps/day grouping, and render throttling; fixed clocks pin the timestamp and offset so application state and `FrameTime` remain deterministic.                                                                                                                                                                                                |
| `TmuxClient`               | `infra/tmux.rs`                              | Tmux subprocess operations for opening worktrees.                                                                                                                                                                                                                                                                                                                                                                                  |
| `ClipboardImageClient`     | `infra/clipboard_image.rs`                   | Clipboard image capture and temp-file persistence; host clipboard reads are isolated in `ag-clipboard`.                                                                                                                                                                                                                                                                                                                            |
| `PersonalityCatalogClient` | `infra/personality.rs`                       | Discovers and resolves enabled personality definitions from the current session worktree's `.agents/agents` directory.                                                                                                                                                                                                                                                                                                             |
| Repository traits          | `crates/ag-store/src/*.rs`                   | Narrow persistence boundaries (`SessionRepository`, `ProjectRepository`, `ReviewRepository`, `UsageRepository`, `ActivityRepository`, `OperationRepository`, `OrchestrationRepository`, `SettingRepository`); activity persistence returns raw timestamps for clock-aware app aggregation. `OrchestrationRepository` owns its own pool so orchestration reconciliation never contends with the foreground session-runtime mailbox. |
| `TimestampSource`          | `crates/ag-store/src/timestamp.rs`           | Unix timestamps for persistence writes; Agentty adapts its environment-selected `Clock` at the composition root while standalone store constructors use the system clock.                                                                                                                                                                                                                                                          |

`ag-orchestration` injects `OrchestrationEventSink` for campaign refresh/progress
notifications and `OrchestrationSchedule` for reconciliation wakeups. Tests can combine
an in-memory event channel, deterministic schedule, repository mocks, and a
`SessionBackend` without constructing the TUI. Agentty tests its event translation at
the application boundary.

`OrchestrationRepository` also bulk-loads session-list progress and controller-child
adjacency per project, and atomically claims roll-up submission before the session API
uses its stable operation identifier.

`ResourceClient` supplies process-table snapshots to the session manager. Its production
adapter runs a locale-stable, time-limited `ps` command between native creation-identity
queries for the tracked roots. Native queries run off the async executor. Deterministic
snapshots test process-tree attribution, same-second PID reuse, refresh throttling, and
invalidation without host-dependent resource assertions. Isolated host tests check both
accounting and native identity access.

Beyond these, narrower internal command-runner boundaries (for example
`ForgeCommandRunner`, `GitCommandRunner`, `CompatibilityMergeRunner`,
`TmuxCommandRunner`, `UpdateRunner`, the `ag-harness` repository-inspection runner, and
the provider transport traits) keep subprocess sequencing and retry behavior
deterministic in unit tests. The runtime also accepts `Terminal<B: Backend>` via
`run_with_backend`, enabling in-process TUI tests with `TestBackend`.

The `ag-agent` crate keeps provider routers, parsers, and concrete transport adapters
private. Application workflows that submit isolated utility prompts inject
`OneShotClient`; provider and transport tests use the feature-gated crate-root mocks and
helper factories rather than deep module paths. CLI-backed session turns, one-shot
prompts, and protocol-repair retries share one crate-private raw subprocess executor for
command construction, stdin delivery, PID lifetime, stream collection, and exit
classification. Adapter-specific observers translate those raw events into session
updates, while one-shot callers consume the collected raw output; response parsing and
repair policy stay in the owning adapter.

## Typed Errors Across Layers

<a id="architecture-typed-error-enums"></a> Each infra boundary exposes a typed error
enum (`DbError`, `GitError`, `AppServerError`, `AgentError`, `OneShotError`,
`ClipboardError`, and so on) instead of opaque `String` errors. The private app-server
transport error is wrapped by `AppServerError::Transport`, then by
`AgentError::AppServer`, allowing `?`-propagation through the transport, provider, and
channel layers without collapsing causal context into formatted strings.

<a id="architecture-app-layer-typed-errors"></a> The app layer propagates infra errors
through `SessionError` (`app/session/error.rs`) and `AppError` (`app/error.rs`), both of
which wrap infra and `OneShotError` values via `#[from]` plus a `Workflow(String)`
variant for contextual app-level failures. At event and display boundaries, errors are
converted to `String` via `Display` because those types require `Clone` and `Eq`.

## Testing Guidance

<a id="architecture-boundary-testing-guidance"></a> When adding higher-level flows
involving multiple external commands, prefer injectable trait boundaries and
`mockall`-based tests over flaky end-to-end shell-heavy tests. Add a narrower internal
command-runner boundary when a public orchestration trait still needs deterministic
coverage of subprocess sequencing or retry behavior. Repository command boundaries must
also select executables outside repository scope, neutralize inherited process
configuration and configured filesystem monitors, verify canonical scope, and drain
subprocess streams into complete bounded records.

Apply the same rule to filesystem discovery and path probes in `app/` and `runtime/`:
route directory walking, `exists` checks, `canonicalize`, and file copy or persistence
helpers through an infra boundary instead of calling `std::fs` or `Path` helpers
directly from orchestration code. Likewise, route `Instant::now()` and
`SystemTime::now()` through the shared `Clock` boundary.

## TUI E2E Testing Framework (`testty`)

<a id="architecture-tui-e2e-framework"></a> The `testty` workspace crate provides a
dual-oracle model for TUI end-to-end testing. The PTY path (`portable-pty` + `vt100`) is
the semantic oracle for text, style, and location assertions; the VHS path is the visual
oracle and review artifact generator.

| Module                          | Purpose                                                            |
| ------------------------------- | ------------------------------------------------------------------ |
| `session`                       | PTY executor: spawns binaries, writes input, captures ANSI output. |
| `frame`                         | Terminal frame parser: ANSI bytes to a cell grid.                  |
| `region` / `locator`            | Rectangular regions and style-aware text locators.                 |
| `assertion` / `recipe`          | Structured matchers and agent-friendly expectation helpers.        |
| `scenario` / `step` / `journey` | Scenario DSL compiled to PTY or VHS.                               |
| `vhs` / `snapshot` / `proof`    | VHS tape compilation, paired baselines, proof backends.            |
| `feature`                       | `FeatureDemo` builder with hash-cached VHS GIF generation.         |

testty has no crate-root re-export module: every public item is addressable only through
its owning module path (for example, `use testty::scenario::Scenario;`). The
`tests/public_api.rs` tripwire pins those per-module items as the documented stable
surface.

Agentty feature runs pin the wall clock, UTC offset, agent executables, and rendered
version label before the PTY frame is captured. Hash redactions then normalize generated
worktree names and the pinned version label without relying on their runtime width.
