# Source Code

## Local Conventions

- Avoid near-identical local variable names in the same function (for example, `gitdir` and `git_dir`). Use one clear naming style with distinct, descriptive names.
- Prefer `module.rs` plus `module/` for nested modules. Avoid `mod.rs` module roots.
- Session status flow:
  - Status state machine is: `New` -> `InProgress`, `Review` -> (`InProgress` | `Done` | `Canceled`), `InProgress` -> `Review`.
  - `New` is set when `create_session()` creates a blank session before the user types a prompt.
  - `InProgress` can be entered from `New` (first prompt) or `Review` (reply).
  - `Done` can only be entered from `Review` (local merge).
  - When agent response finishes, all changes are auto-committed and status is set to `Review`.
  - While agent is preparing a response, status is `InProgress`.

## Docs Sync

When changing architecture-level behavior under `src/`, update:

- `docs/site/content/docs/architecture/module-map.md` — module/path ownership and boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md` — runtime flow, channel transport, and turn interaction flow.
- `docs/site/content/docs/architecture/testability-boundaries.md` — trait boundaries for external integrations.
- `docs/site/content/docs/architecture/change-recipes.md` — contributor-safe change paths.
- Keep the runtime-mode file list in `module-map.md` aligned with actual files under `runtime/mode/`.
- Keep the key-type tables/field descriptions in `runtime-flow.md` aligned with `infra/channel.rs` (`TurnRequest`, `TurnEvent`, `TurnResult`, `TurnMode`).
- Keep `testability-boundaries.md` aligned with active `#[cfg_attr(test, mockall::automock)]` trait boundaries that guard external/time/process integrations.

## Directory Index

- [`app/`](app/) - Application state and workflows split by concern (`session`, `project`, `task`).
- [`app.rs`](app.rs) - App module router and public re-exports for app orchestration APIs.
- [`domain/`](domain/) - Domain layer entities and logic.
- [`domain.rs`](domain.rs) - Domain module root and submodule declarations.
- [`infra/`](infra/) - Infrastructure layer implementations.
- [`infra.rs`](infra.rs) - Infrastructure module root and submodule declarations.
- [`runtime/`](runtime/) - Runtime event loop, terminal integration, and mode key handling.
- [`runtime.rs`](runtime.rs) - Runtime module router with public runtime entry exports.
- [`ui/`](ui/) - User Interface module.
- [`ui.rs`](ui.rs) - UI module root with shared exports and submodule declarations.
- [`lib.rs`](lib.rs) - Library entry point, exports modules.
- [`main.rs`](main.rs) - Binary composition root for DB bootstrap and runtime launch.
