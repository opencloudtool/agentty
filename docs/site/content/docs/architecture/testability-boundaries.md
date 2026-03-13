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
| `ReviewRequestClient` | `crates/ag-forge/src/client.rs` | Cross-forge review-request detection and provider-specific `gh`/`glab` orchestration boundary. |
| `ForgeCommandRunner` | `crates/ag-forge/src/command.rs` | Provider CLI command execution boundary used to unit-test GitHub and GitLab review-request adapters without live `gh` or `glab` binaries. |
| `GitClient` | `infra/git/client.rs` | Git/process operations (worktree, merge, rebase, diff, push, pull). |
| `FsClient` | `infra/fs.rs` | Async filesystem operations used by app orchestration, including non-blocking file reads plus session worktree cleanup and prompt-image temp file and directory removal. |
| `TmuxClient` | `infra/tmux.rs` | Tmux subprocess operations for opening session worktrees and dispatching open commands. |
| `TmuxCommandRunner` | `infra/tmux.rs` | Internal tmux command boundary that keeps multi-command `send-keys` flows deterministic in unit tests. |
| `AgentChannel` | `infra/channel.rs` | Provider-agnostic turn execution (session init, run turn, shutdown). |
| `AgentBackend` | `infra/agent/backend.rs` | Per-provider CLI command construction and one-time setup. |
| `AppServerClient` | `infra/app_server.rs` | App-server RPC execution (provider routing, JSON-RPC transport). |
| `EventSource` | `runtime/event.rs` | Terminal event polling for deterministic event-loop tests. |
| `TerminalOperation` | `runtime/terminal.rs` | Terminal raw-mode and alternate-screen transitions for deterministic setup and restore failure-path tests. |
| `Sleeper` | `lib.rs` | Wall-clock sleep boundary used by retry/polling flows such as git rebase assistance. |
| `UpdateRunner` | `infra/version.rs` | npm install command execution for background auto-updates. |
| `VersionCommandRunner` | `infra/version.rs` | npm/curl command execution for update checks. |
| `GitCommandRunner` | `infra/git/rebase.rs` | Rebase command invocation boundary for conflict/retry tests. |
| `SyncAssistClient` | `app/session/workflow/merge.rs` | Sync-rebase assistance execution boundary. |
| `GeminiRuntimeTransport` | `infra/gemini_acp.rs` | ACP stdio transport boundary for Gemini runtime protocol tests. |

<a id="architecture-boundary-testing-guidance"></a>
When adding higher-level flows involving multiple external commands, prefer
injectable trait boundaries and `mockall`-based tests over flaky end-to-end
shell-heavy tests. Add a narrower internal command-runner boundary when a
public orchestration trait still needs deterministic coverage of subprocess
sequencing or retry behavior.

Session review-request publication and refresh follow this rule directly:
`SessionManager` combines `GitClient` with `ReviewRequestClient` so tests can
cover branch publish, duplicate detection, stored-link reuse, and archived
session refresh without live forge auth or network state.
