# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for plan structure and implementation-planning requirements.
Keep size budgeting in the skill workflow only; do not render `### Size` sections inside `docs/plan/*.md` files.

## Directory Index

- [`session_execution_backends.md`](session_execution_backends.md) - Implementation plan for backend-owned session execution that survives TUI exit and restart, with `LocalProcess` first and OCI backends reserved for follow-up work.

- [`draft_session_prompt_collection.md`](draft_session_prompt_collection.md) - Implementation plan for draft sessions that collect multiple prompts before the first real agent turn starts.

- [`end_to_end_test_structure.md`](end_to_end_test_structure.md) - Implementation plan for organizing deterministic local scenario tests and thin live smoke suites around git, forge, and agent workflows.

- [`forge_review_request_support.md`](forge_review_request_support.md) - Implementation plan for adding forge-generic review request workflows across GitHub and GitLab.

- [`local_agent_availability.md`](local_agent_availability.md) - Implementation plan for startup-time agent CLI discovery and filtering settings plus `/model` choices to locally available backends.

- [`multi_method_auto_update.md`](multi_method_auto_update.md) - Implementation plan for detecting the installation method and running the appropriate auto-update command for npm, cargo, sh, and npx.

- [`session_in_progress_timer.md`](session_in_progress_timer.md) - Implementation plan for persisting and rendering cumulative session `InProgress` time in chat and list views.

- [`tech_debt_error_handling.md`](tech_debt_error_handling.md) - Implementation plan for replacing 167 `Result<..., String>` functions with typed error enums, documenting ~170 silent `let _ =` discards, filling test coverage gaps, and fixing minor convention violations.

- [`tui_e2e_testing_framework.md`](tui_e2e_testing_framework.md) - Implementation plan for a Rust-native Playwright-inspired TUI end-to-end testing framework with in-process `TestBackend` harness, `insta` snapshot assertions, and PTY-based smoke validation.
