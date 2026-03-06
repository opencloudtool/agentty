# Session Workflow Module

Session workflow modules used by `core.rs` session orchestration.

## Directory Index

- [`access.rs`](access.rs) - Session lookup helpers and canonical lookup errors.
- [`lifecycle.rs`](lifecycle.rs) - Session creation, prompt/reply, history, and deletion workflows.
- [`load.rs`](load.rs) - Session snapshot loading and derived size persistence.
- [`merge.rs`](merge.rs) - Merge/rebase workflows and worktree cleanup helpers.
- [`refresh.rs`](refresh.rs) - Periodic refresh scheduling and list-state restoration.
- [`review.rs`](review.rs) - Review-session transcript replay tracking helpers.
- [`task.rs`](task.rs) - Session process execution, streaming output capture, and status persistence helpers.
- [`worker.rs`](worker.rs) - Per-session command queue and worker execution orchestration.
- [`AGENTS.md`](AGENTS.md) - Local module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
