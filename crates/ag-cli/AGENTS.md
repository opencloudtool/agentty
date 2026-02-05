# Agentty CLI

Modular TUI application for managing agent sessions.

## Project Structure

- `src/lib.rs`: Library entry point, exports modules.
- `src/main.rs`: Binary entry point, uses the library.
- `src/agent.rs`: `AgentBackend` trait and concrete backends (`GeminiBackend`, `ClaudeBackend`), plus `AgentKind` enum for selecting a backend via `AGENTTY_AGENT` env var.
- `src/app.rs`: `App` struct holding the application state (`sessions`, `table_state`, `mode`) and business logic.
- `src/lock.rs`: Single-instance session lock using POSIX `flock`, prevents concurrent agentty processes.
- `src/model.rs`: Core domain models (`Session`, `Status`, `AppMode`).
- `src/ui.rs`: Rendering logic using `ratatui`.
- `Cargo.toml`: Crate dependencies and metadata.
