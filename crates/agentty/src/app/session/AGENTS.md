# Session Module

Session domain workflows for app-level orchestration.

## Overview

- Splits session responsibilities by concern to keep workflows focused and testable.
- Keeps app-facing APIs on `App` while sharing internals through module-local helpers.

## Design

- Session workflow responsibilities are split by concern to keep implementations focused and testable.
- `core.rs` holds `SessionManager` state, shared constants, and session-level helpers.
- `workflow.rs` is a router-only module that exposes workflow submodules.
- `workflow/` centralizes lookup, lifecycle, refresh, loading, review replay, task execution, merge/rebase, and worker orchestration.
- Session tests remain with their corresponding source modules (for example `core.rs`) instead of a standalone `test.rs`.

## Directory Index

- [`workflow/`](workflow/) - Session workflow modules and local docs/index.
- [`workflow.rs`](workflow.rs) - Session workflow module router.
- [`core.rs`](core.rs) - Session orchestration implementation (`SessionManager`, clock boundary, constants, and tests).
- [`AGENTS.md`](AGENTS.md) - Local module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
