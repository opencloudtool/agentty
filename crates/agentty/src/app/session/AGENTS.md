# Session Module

Session domain workflows for app-level orchestration.

## Overview

- Splits session responsibilities by concern to keep workflows focused and testable.
- Keeps app-facing APIs on `App` while sharing internals through module-local helpers.

## Design

- Session workflow responsibilities are split by concern to keep implementations focused and testable.
- `access.rs` centralizes session and handle lookups plus canonical lookup errors.
- Lifecycle, refresh, loading, and merge/rebase logic are isolated into dedicated files.
- Tests live alongside session module code (no standalone `test.rs` file).

## Directory Index

- [access.rs](access.rs) - Session lookup helpers and canonical lookup errors.
- [lifecycle.rs](lifecycle.rs) - Session creation, prompt/reply, history, and deletion workflows.
- [load.rs](load.rs) - Session snapshot loading and derived size persistence.
- [merge.rs](merge.rs) - Merge/rebase workflows and worktree cleanup helpers.
- [refresh.rs](refresh.rs) - Periodic refresh scheduling and list-state restoration.
- [worker.rs](worker.rs) - Per-session command queue and worker execution orchestration.
