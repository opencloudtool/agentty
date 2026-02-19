# Source Code

## Local Conventions
- Avoid near-identical local variable names in the same function (for example, `gitdir` and `git_dir`). Use one clear naming style with distinct, descriptive names.
- Session status flow:
  - Status state machine is: `New` -> `InProgress`, `Review` -> (`InProgress` | `Done` | `Canceled`), `InProgress` -> `Review`.
  - `New` is set when `create_session()` creates a blank session before the user types a prompt.
  - `InProgress` can be entered from `New` (first prompt) or `Review` (reply).
  - `Done` can only be entered from `Review` (local merge).
  - When agent response finishes, all changes are auto-committed and status is set to `Review`.
  - While agent is preparing a response, status is `InProgress`.

## Directory Index
- [app/](app/) - Application state and workflows split by concern (`session`, `project`, `task`, `title`).
- [runtime/](runtime/) - Runtime event loop, terminal integration, and mode key handling.
- [ui/](ui/) - User Interface module.
- [agent.rs](agent.rs) - `AgentBackend` trait and concrete backends (`GeminiBackend`, `ClaudeBackend`).
- [db.rs](db.rs) - `Database` struct wrapping SQLx for session metadata persistence.
- [file_list.rs](file_list.rs) - Gitignore-aware file listing for `@` mention dropdown.
- [git.rs](git.rs) - Git integration and worktree management.
- [icon.rs](icon.rs) - Centralized `Icon` enum for consistent Unicode symbols.
- [lib.rs](lib.rs) - Library entry point, exports modules.
- [lock.rs](lock.rs) - Single-instance session lock using POSIX `flock`.
- [main.rs](main.rs) - Binary composition root for lock, DB bootstrap, and runtime launch.
- [model.rs](model.rs) - Core domain models (`Session`, `Status`, `AppMode`).
