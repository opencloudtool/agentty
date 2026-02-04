# Agent Manager CLI

Modular TUI application for managing agents.

## Project Structure

- `src/lib.rs`: Library entry point, exports modules.
- `src/main.rs`: Binary entry point, uses the library.
- `src/app.rs`: `App` struct holding the application state (`agents`, `table_state`, `mode`) and business logic.
- `src/model.rs`: Core domain models (`Agent`, `Status`, `AppMode`).
- `src/ui.rs`: Rendering logic using `ratatui`.
- `Cargo.toml`: Crate dependencies and metadata.
