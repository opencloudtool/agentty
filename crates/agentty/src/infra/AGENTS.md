# Infrastructure Layer

## Overview

Implementations of external interfaces (Database, Git, System).

## Entry Points

- `db.rs` owns SQLite persistence and query execution.
- `git.rs` and `git/` own git operations behind `GitClient`.
- `channel.rs` and `agent.rs` own provider transport and prompt execution.
- `app_server.rs` and `app_server/` own shared app-server runtime infrastructure.
- `file_index.rs` owns gitignore-aware file traversal used by `@` mentions and explorer features.

## Change Guidance

- Keep new external integrations behind trait boundaries.
- Route subprocess, filesystem, and time access through existing infrastructure boundaries instead of introducing direct calls in orchestration layers.
