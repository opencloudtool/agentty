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
All traits below use `#[cfg_attr(test, mockall::automock)]`:

| Trait | Module | Boundary |
|-------|--------|----------|
| `SyncMainRunner` | `app/core.rs` | App-level async sync orchestration trigger used by list-mode sync flows. |
| `ReviewRequestClient` | `infra/forge.rs` | Cross-forge review-request detection and provider-specific `gh`/`glab` orchestration boundary. |
| `ForgeCommandRunner` | `infra/forge/command.rs` | Provider CLI command execution boundary used to unit-test GitHub and GitLab review-request adapters without live `gh` or `glab` binaries. |
| `GitClient` | `infra/git/client.rs` | Git/process operations (worktree, merge, rebase, diff, push, pull). |
| `FsClient` | `infra/fs.rs` | Filesystem operations used by app orchestration (create/remove/read workflow files). |
| `TmuxClient` | `infra/tmux.rs` | Tmux subprocess operations for opening session worktrees and dispatching open commands. |
| `AgentChannel` | `infra/channel.rs` | Provider-agnostic turn execution (session init, run turn, shutdown). |
| `AgentBackend` | `infra/agent/backend.rs` | Per-provider CLI command construction and one-time setup. |
| `AppServerClient` | `infra/app_server.rs` | App-server RPC execution (provider routing, JSON-RPC transport). |
| `EventSource` | `runtime/event.rs` | Terminal event polling for deterministic event-loop tests. |
| `Sleeper` | `lib.rs` | Wall-clock sleep boundary used by the event-reader thread. |
| `EditorLauncher` | `runtime/terminal.rs` | External editor process launch boundary used by runtime key handlers. |
| `VersionCommandRunner` | `infra/version.rs` | npm/curl command execution for update checks. |
| `GitCommandRunner` | `infra/git/rebase.rs` | Rebase command invocation boundary for conflict/retry tests. |
| `SyncAssistClient` | `app/session/workflow/merge.rs` | Sync-rebase assistance execution boundary. |
| `GeminiRuntimeTransport` | `infra/gemini_acp.rs` | ACP stdio transport boundary for Gemini runtime protocol tests. |

<a id="architecture-boundary-testing-guidance"></a>
When adding higher-level flows involving multiple external commands, prefer
injectable trait boundaries and `mockall`-based tests over flaky end-to-end
shell-heavy tests.
