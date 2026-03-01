# Infrastructure Layer

## Overview

Implementations of external interfaces (Database, Git, System).

## Directory Index

- [agent/](agent/) - Provider-specific backend builders and response parsing modules.
- [agent.rs](agent.rs) - Agent module root that wires provider modules under `agent/`.
- [app_server.rs](app_server.rs) - Shared app-server trait and turn request/response event types.
- [channel/](channel/) - Provider-agnostic `AgentChannel` trait and CLI/app-server adapters.
- [channel.rs](channel.rs) - Channel module root with `AgentChannel` trait, types, and factory.
- [app_server_router.rs](app_server_router.rs) - Routes app-server turn execution to the provider-specific client for each model.
- [app_server_transport.rs](app_server_transport.rs) - Shared stdio JSON-RPC transport utilities for app-server protocols.
- [codex_app_server.rs](codex_app_server.rs) - Persistent Codex app-server client and per-session turn execution.
- [db.rs](db.rs) - SQLite database implementation.
- [file_index.rs](file_index.rs) - Gitignore-aware file listing and fuzzy filtering for `@` mentions.
- [gemini_acp.rs](gemini_acp.rs) - Persistent Gemini ACP app-server client and per-session turn execution.
- [git/](git/) - Workflow-focused git operation modules (`merge`, `rebase`, `repo`, `sync`, `worktree`).
- [git.rs](git.rs) - Git module root with public API re-exports and `GitClient` wiring.
- [version.rs](version.rs) - Version checking infrastructure.
