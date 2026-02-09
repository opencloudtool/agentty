# Source Code

## Local Conventions
- Avoid near-identical local variable names in the same function (for example, `gitdir` and `git_dir`). Use one clear naming style with distinct, descriptive names.
- PR lifecycle status flow:
  - Status state machine is: `New` -> `InProgress` -> `Review` -> (`InProgress` or `PullRequest` or `Done`) and `PullRequest` -> `Done`.
  - `New` is set when `create_session()` creates a blank session before the user types a prompt.
  - `create_pr_session()` is allowed only while session status is `Review`.
  - `InProgress` can be entered from `New` (first prompt) or `Review` (reply).
  - `PullRequest` can only be entered from `Review`.
  - `Done` can only be entered from `Review` (local merge) or `PullRequest` (merged PR).
  - When agent response finishes, set status to `Review`.
  - While agent is preparing a response, status is `InProgress`.
  - Do not stop polling PR status for previously tracked sessions on project switch.

## Directory Index
- [app/](app/) - Application state and workflows split by concern (`session`, `project`, `pr`, `task`, `title`).
- [runtime/](runtime/) - Runtime event loop, terminal integration, and mode key handling.
- [ui/](ui/) - User Interface module.
- [agent.rs](agent.rs) - `AgentBackend` trait and concrete backends (`GeminiBackend`, `ClaudeBackend`).
- [db.rs](db.rs) - `Database` struct wrapping SQLx for session metadata persistence.
- [git.rs](git.rs) - Git integration and worktree management.
- [health.rs](health.rs) - Health check domain logic.
- [icon.rs](icon.rs) - Centralized `Icon` enum for consistent Unicode symbols.
- [lib.rs](lib.rs) - Library entry point, exports modules.
- [lock.rs](lock.rs) - Single-instance session lock using POSIX `flock`.
- [main.rs](main.rs) - Binary composition root for lock, DB bootstrap, and runtime launch.
- [model.rs](model.rs) - Core domain models (`Session`, `Status`, `AppMode`).
