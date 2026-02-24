# Source Code

## Local Conventions

- Avoid near-identical local variable names in the same function (for example, `gitdir` and `git_dir`). Use one clear naming style with distinct, descriptive names.
- Keep `mod.rs` files declaration-only. Move non-declaration module logic into sibling `*.rs` files.
- Session status flow:
  - Status state machine is: `New` -> `InProgress`, `Review` -> (`InProgress` | `Done` | `Canceled`), `InProgress` -> `Review`.
  - `New` is set when `create_session()` creates a blank session before the user types a prompt.
  - `InProgress` can be entered from `New` (first prompt) or `Review` (reply).
  - `Done` can only be entered from `Review` (local merge).
  - When agent response finishes, all changes are auto-committed and status is set to `Review`.
  - While agent is preparing a response, status is `InProgress`.

## Directory Index

- [app/](app/) - Application state and workflows split by concern (`session`, `project`, `task`).
- [app.rs](app.rs) - App module root, shared app state, and orchestration APIs.
- [domain/](domain/) - Domain layer entities and logic.
- [infra/](infra/) - Infrastructure layer implementations.
- [runtime/](runtime/) - Runtime event loop, terminal integration, and mode key handling.
- [runtime.rs](runtime.rs) - Runtime entry point and main event/render loop wiring.
- [ui/](ui/) - User Interface module.
- [ui.rs](ui.rs) - UI module root with shared exports and submodule declarations.
- [lib.rs](lib.rs) - Library entry point, exports modules.
- [main.rs](main.rs) - Binary composition root for lock, DB bootstrap, and runtime launch.
