# Source Code

## Directory Index
- [ui/](ui/) - User Interface module.
- [agent.rs](agent.rs) - `AgentBackend` trait and concrete backends (`GeminiBackend`, `ClaudeBackend`).
- [app.rs](app.rs) - `App` struct holding application state (`sessions`, `table_state`, `mode`) and business logic.
- [git.rs](git.rs) - Git integration and worktree management.
- [lib.rs](lib.rs) - Library entry point, exports modules.
- [lock.rs](lock.rs) - Single-instance session lock using POSIX `flock`.
- [main.rs](main.rs) - Binary entry point, uses the library.
- [model.rs](model.rs) - Core domain models (`Session`, `Status`, `AppMode`).
