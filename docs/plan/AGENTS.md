# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for roadmap structure and implementation-planning requirements.
Keep active planning consolidated in `docs/plan/roadmap.md`.
Keep `docs/plan/roadmap.md` split into `## Ready Now`, `## Queued Next`, and `## Parked`.
Keep only `Ready Now` steps fully expanded; keep queued and parked work intentionally compact.
Remove implemented `Ready Now` steps from the roadmap instead of preserving completed execution detail.
Keep size budgeting in the skill workflow only; do not render `### Size` sections inside `docs/plan/*.md` files, and plan `Ready Now` slices with headroom so estimated scope stays at or below `350` changed lines even though the implementation hard ceiling remains `500`.
Require every roadmap step heading title to use the exact format `[UUID] Stream: Title`.
Require `Ready Now` steps to start with `#### Assignee` using `@username` or `No assignee`.
Claim `Ready Now` work through `skills/implementation-plan/references/claim-step.md`, resolve the current GitHub login with `gh api user --jq .login`, and land that claim in its own commit before implementation begins.
Run `cargo run -q -p ag-xtask -- roadmap context-digest` before promoting queued or parked work into `Ready Now`.
When a `Ready Now` step is completed and `Queued Next` still has items, promote the next queued card into `Ready Now` instead of leaving the slot open.
Keep `Ready Now` steps to `1..=3` implementation checklist items under `#### Substeps`; when a slice needs more than that or spans multiple peer surfaces, split the follow-up into `Queued Next` instead of widening the active step.

## Planning Surface

- `roadmap.md` is the canonical active roadmap.
- Keep planning guidance semantic and process-focused; do not reintroduce local file inventories here.
