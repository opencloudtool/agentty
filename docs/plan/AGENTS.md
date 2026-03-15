# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for plan structure and implementation-planning requirements.
Keep size budgeting in the skill workflow only; do not render `### Size` sections inside `docs/plan/*.md` files.

## Directory Index

- [`continue_in_progress_sessions_after_exit.md`](continue_in_progress_sessions_after_exit.md) - Implementation plan for keeping active session turns running after the TUI exits and reconnecting on restart.

- [`draft_session_prompt_collection.md`](draft_session_prompt_collection.md) - Implementation plan for draft sessions that collect multiple prompts before the first real agent turn starts.

- [`end_to_end_test_structure.md`](end_to_end_test_structure.md) - Implementation plan for organizing deterministic local scenario tests and thin live smoke suites around git, forge, and agent workflows.

- [`forge_review_request_support.md`](forge_review_request_support.md) - Implementation plan for adding forge-generic review request workflows across GitHub and GitLab.

- [`local_agent_availability.md`](local_agent_availability.md) - Implementation plan for startup-time agent CLI discovery and filtering settings plus `/model` choices to locally available backends.

- [`multi_method_auto_update.md`](multi_method_auto_update.md) - Implementation plan for detecting the installation method and running the appropriate auto-update command for npm, cargo, sh, and npx.

- [`protocol_request_instructions.md`](protocol_request_instructions.md) - Implementation plan for routing every agent request through the shared structured protocol with request-specific instruction injection owned by the protocol layer.

- [`session_in_progress_timer.md`](session_in_progress_timer.md) - Implementation plan for persisting and rendering cumulative session `InProgress` time in chat and list views.

- [`tui_e2e_testing_framework.md`](tui_e2e_testing_framework.md) - Implementation plan for a custom Playwright-inspired TUI end-to-end testing framework with PTY-based interaction, snapshot assertions, and a scriptable scenario DSL.
