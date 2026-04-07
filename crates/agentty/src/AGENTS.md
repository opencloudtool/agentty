# Source Code

## Local Conventions

- Avoid near-identical local variable names in the same function (for example, `gitdir` and `git_dir`). Use one clear naming style with distinct, descriptive names.
- Persisted keys in `setting` and `project_setting` must come from `domain/setting.rs` `SettingName`; do not introduce ad hoc string keys or legacy aliases.
- Prefer `module.rs` plus `module/` for nested modules. Avoid `mod.rs` module roots.
- Session status flow:
  - Status state machine is: `New` -> (`InProgress` | `Rebasing`), (`Review` | `AgentReview` | `Question`) -> (`InProgress` | `Queued` | `Rebasing` | `Merging` | `Canceled`), (`Review` | `AgentReview`) -> `Done`, `Queued` -> (`Merging` | `Review`), (`InProgress` | `Rebasing`) -> (`Review` | `AgentReview` | `Question`), and `Merging` -> (`Done` | `Review` | `AgentReview`).
  - `New` is set when `create_session()` creates a blank session before the user types a prompt.
  - `InProgress` can be entered from `New` (first prompt) or from `Review`/`Question` (reply).
  - `Question` is set when a completed turn returns structured clarification questions.
  - `Done` can be entered from `Merging` after local merge cleanup succeeds, or from `Review`/`AgentReview` when a review request sync detects an upstream merge.
  - When agent response finishes, all changes are auto-committed and status is set to `Review` or `Question`.
  - While agent is preparing a response, status is `InProgress`.

## Docs Sync

When changing architecture-level behavior under `src/`, update:

- `docs/site/content/docs/architecture/module-map.md` — module/path ownership and boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md` — runtime flow, channel transport, and turn interaction flow.
- `docs/site/content/docs/architecture/testability-boundaries.md` — trait boundaries for external integrations.
- `docs/site/content/docs/architecture/change-recipes.md` — contributor-safe change paths.
- Keep the runtime-mode file list in `module-map.md` aligned with actual files under `runtime/mode/`.
- Keep the key-type tables/field descriptions in `runtime-flow.md` aligned with `infra/channel/contract.rs` (re-exported by `infra/channel.rs`) for `TurnRequest`, `TurnEvent`, and `TurnResult`.
- Keep `testability-boundaries.md` aligned with active `#[cfg_attr(test, mockall::automock)]` trait boundaries that guard external/time/process integrations.

## Major Areas

- `app.rs` and `app/` own orchestration and workflow state.
- `domain.rs` and `domain/` own business entities and enums.
- `infra.rs` and `infra/` own external integrations and persistence.
- `runtime.rs` and `runtime/` own terminal lifecycle and event dispatch.
- `ui.rs` and `ui/` own rendering, layout, and interaction widgets.
