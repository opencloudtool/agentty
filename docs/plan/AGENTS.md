# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for plan structure and implementation-planning requirements.
Keep the `## Priorities` section near the top of each plan, immediately after the title and scope/context line. Do not rename it to `Updated Priorities`.
Priority sections should render `Why now`, `Usable outcome`, and `Substeps` as separate subtopics on their own lines, not as inline bold labels.
Do not add `Primary files` blocks inside priorities. Mention every file needed for a priority directly in the checklist text for its substeps.

## Directory Index

- [`continue_in_progress_sessions_after_exit.md`](continue_in_progress_sessions_after_exit.md) - Implementation plan for keeping active session turns running after the TUI exits and reconnecting on restart.
- [`coverage_follow_up.md`](coverage_follow_up.md) - Follow-up implementation plan for improving post-ratchet coverage hot spots.
- [`end_to_end_test_structure.md`](end_to_end_test_structure.md) - Implementation plan for organizing deterministic local scenario tests and thin live smoke suites around git, forge, and agent workflows.
- [`forge_review_request_support.md`](forge_review_request_support.md) - Implementation plan for adding forge-generic review request workflows across GitHub and GitLab.
- [`session_commit_message_flow.md`](session_commit_message_flow.md) - Implementation plan for making the session branch commit message authoritative and reusing it during merge.
