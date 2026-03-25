# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for plan structure and implementation-planning requirements.

## Directory Index

- [`session_execution_backends.md`](session_execution_backends.md) - Implementation plan for backend-owned session execution that survives TUI exit and restart, with `LocalProcess` first and OCI backends reserved for follow-up work.
- [`coverage_follow_up.md`](coverage_follow_up.md) - Follow-up implementation plan for improving post-ratchet coverage hot spots.
- [`end_to_end_test_structure.md`](end_to_end_test_structure.md) - Implementation plan for organizing deterministic local scenario tests and thin live smoke suites around git, forge, and agent workflows.
- [`forge_review_request_support.md`](forge_review_request_support.md) - Implementation plan for adding forge-generic review request workflows across GitHub and GitLab.
- [`sync_review_request.md`](sync_review_request.md) - Implementation plan for `s` keybinding that syncs review request status and transitions to `Done` on merge (completed).
