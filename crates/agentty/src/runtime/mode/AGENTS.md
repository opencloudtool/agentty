# Runtime Mode Handlers

Key handling logic split by `AppMode`.

## Directory Index

- [confirmation.rs](confirmation.rs) - Shared yes/no confirmation mode key handling.
- [diff.rs](diff.rs) - Diff mode key handling.
- [help.rs](help.rs) - Help overlay mode key handling.
- [list.rs](list.rs) - Session list mode key handling.
- [prompt.rs](prompt.rs) - Prompt mode editing and submit handling.
- [question.rs](question.rs) - Model-question mode key handling and response submission flow.
- [sync_blocked.rs](sync_blocked.rs) - Sync-blocked popup key handling.
- [session_view.rs](session_view.rs) - Session view mode navigation and actions.

## Docs

Keep usage docs synchronized with mode behavior:

- Key handling or shortcut changes require updating `docs/site/content/docs/usage/keybindings.md`.
- Question-mode flow or session-state behavior changes require updating `docs/site/content/docs/usage/workflow.md`.
- Adding, removing, or renaming a mode handler file under `runtime/mode/` requires updating the runtime-mode list in `docs/site/content/docs/architecture/module-map.md`.
