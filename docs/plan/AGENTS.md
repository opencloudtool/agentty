# Plan

Internal planning documents and UI design notes.

Use `skills/implementation-plan/SKILL.md` for roadmap structure and implementation-planning requirements.
Keep active planning consolidated in `docs/plan/roadmap.md`.
Keep `docs/plan/roadmap.md` to one shared execution diagram and one shared `## Implementation Steps` section.
Remove implemented steps from the roadmap instead of preserving completed execution detail.
Keep size budgeting in the skill workflow only; do not render `### Size` sections inside `docs/plan/*.md` files.
Require every roadmap step heading title to use the exact format `[UUID] Stream: Title`, then start the body with `#### Assignee` using a GitHub handle in `@assignee` format or `No assignee` before `#### Why now`.
For the current direct-to-`main` workflow, an engineer claims a step by landing and pushing a dedicated commit that changes only that step's exact `#### Assignee` field, then starts implementation in later commits.

## Directory Index

- [`roadmap.md`](roadmap.md) - Canonical single-file project roadmap that consolidates the active implementation planning inventory.
