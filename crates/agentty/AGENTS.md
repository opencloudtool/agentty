# Agentty CLI

Modular TUI application for managing agent sessions.
The main binary is named `agentty`.

## Entry Points

- `src/main.rs` is the binary composition root.
- `src/lib.rs` exposes the crate modules.
- `src/` contains the layered application code.
- `migrations/` contains embedded SQLx migrations.
- `tests/` contains integration tests for live provider behavior and protocol compliance.

## Architecture References

- `docs/site/content/docs/architecture/module-map.md` is the canonical path-ownership map.
- `docs/site/content/docs/architecture/runtime-flow.md` documents runtime orchestration and channel flow.
- `docs/site/content/docs/architecture/testability-boundaries.md` tracks external trait boundaries.
