# Source Code

## Local Conventions
- Avoid near-identical local variable names in the same function (for example, `gitdir` and `git_dir`). Use one clear naming style with distinct, descriptive names.
- PR lifecycle status flow:
  - Status state machine is: `New` -> `InProgress`, `Review` -> (`InProgress` | `Committing` | `CreatingPullRequest` | `PullRequest` | `Done`), (`InProgress` | `Committing`) -> `Review`, `CreatingPullRequest` -> (`PullRequest` | `Review`), and `PullRequest` -> `Done`.
  - `New` is set when `create_session()` creates a blank session before the user types a prompt.
  - `InProgress` can be entered from `New` (first prompt) or `Review` (reply).
  - `Committing` can only be entered from `Review` while `spawn_commit_session()` is running.
  - `CreatingPullRequest` can only be entered from `Review` while `create_pr_session()` is running.
  - `PullRequest` is entered after successful PR creation and remains active while merge polling runs.
  - `Done` can only be entered from `Review` (local merge) or `PullRequest` (merged PR).
  - When agent response finishes, set status to `Review`.
  - While agent is preparing a response, status is `InProgress`.
  - While commit creation runs, status is `Committing`.
  - While PR creation runs, status is `CreatingPullRequest`.
  - Do not stop polling PR status for previously tracked sessions on project switch.

## Directory Index
- [app/](app/) - Application state and workflows split by concern (`session`, `project`, `pr`, `task`, `title`).
- [runtime/](runtime/) - Runtime event loop, terminal integration, and mode key handling.
- [ui/](ui/) - User Interface module.
- [acp.rs](acp.rs) - ACP (Agent Client Protocol) connection management and streaming.
- [agent.rs](agent.rs) - `AgentKind` enum, model definitions, and ACP command helpers.
- [db.rs](db.rs) - `Database` struct wrapping SQLx for session metadata persistence.
- [gh.rs](gh.rs) - GitHub CLI integration and PR response parsing.
- [git.rs](git.rs) - Git integration and worktree management.
- [health.rs](health.rs) - Health check domain logic.
- [icon.rs](icon.rs) - Centralized `Icon` enum for consistent Unicode symbols.
- [lib.rs](lib.rs) - Library entry point, exports modules.
- [lock.rs](lock.rs) - Single-instance session lock using POSIX `flock`.
- [main.rs](main.rs) - Binary composition root for lock, DB bootstrap, and runtime launch.
- [model.rs](model.rs) - Core domain models (`Session`, `Status`, `AppMode`).
