# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for plan structure and implementation-planning requirements.
Keep size budgeting in the skill workflow only; do not render `### Size` sections inside `docs/plan/*.md` files.

## Directory Index

- [`continue_in_progress_sessions_after_exit.md`](continue_in_progress_sessions_after_exit.md) - Implementation plan for keeping active session turns running after the TUI exits and reconnecting on restart.

- [`end_to_end_test_structure.md`](end_to_end_test_structure.md) - Implementation plan for organizing deterministic local scenario tests and thin live smoke suites around git, forge, and agent workflows.

- [`forge_review_request_support.md`](forge_review_request_support.md) - Implementation plan for adding forge-generic review request workflows across GitHub and GitLab.

- [`prompt_image_paste.md`](prompt_image_paste.md) - Implementation plan for pasting clipboard images into session prompts and rendering pending attachments in the chat composer.

- [`auto_update.md`](auto_update.md) - Implementation plan for automatic self-update on startup with re-exec restart.
