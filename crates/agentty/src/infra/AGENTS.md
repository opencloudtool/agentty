# Infrastructure Layer

## Overview

Implementations of external interfaces (Database, Git, System).

## Directory Index

- [agent/](agent/) - Provider-specific backend builders and response parsing modules.
- [agent.rs](agent.rs) - Agent module root that wires provider modules under `agent/`.
- [codex_app_server.rs](codex_app_server.rs) - Persistent Codex app-server client and per-session turn execution.
- [db.rs](db.rs) - SQLite database implementation.
- [file_index.rs](file_index.rs) - Gitignore-aware file listing and fuzzy filtering for `@` mentions.
- [git.rs](git.rs) - Git operations.
- [lock.rs](lock.rs) - File locking mechanisms.
- [mod.rs](mod.rs) - Module definition.
- [version.rs](version.rs) - Version checking infrastructure.
