# Session Module

Session domain workflows for app-level orchestration.

## Overview

- Splits session responsibilities by concern to keep workflows focused and testable.
- Keeps app-facing APIs on `App` while sharing internals through module-local helpers.

## Design

- Session workflow responsibilities are split by concern to keep implementations focused and testable.
- `access.rs` centralizes session and handle lookups plus canonical lookup errors.
- Lifecycle, refresh, loading, review replay, Codex usage-limit loading, and merge/rebase logic are isolated into dedicated files.
- Session tests remain with their corresponding source modules (for example `session.rs`) instead of a standalone `test.rs`.

## Directory Index

- [access.rs](access.rs) - Session lookup helpers and canonical lookup errors.
- [codex_usage.rs](codex_usage.rs) - Codex app-server usage-limit loading and response parsing.
- [lifecycle.rs](lifecycle.rs) - Session creation, prompt/reply, history, and deletion workflows.
- [load.rs](load.rs) - Session snapshot loading and derived size persistence.
- [merge.rs](merge.rs) - Merge/rebase workflows and worktree cleanup helpers.
- [review.rs](review.rs) - Review-session transcript replay tracking helpers.
- [refresh.rs](refresh.rs) - Periodic refresh scheduling and list-state restoration.
- [task.rs](task.rs) - Session process execution, streaming output capture, and status persistence helpers.
- [worker.rs](worker.rs) - Per-session command queue and worker execution orchestration.
