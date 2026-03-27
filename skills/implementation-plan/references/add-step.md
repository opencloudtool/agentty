# Add Step

Use this guide when roadmap work is missing entirely and needs a new pending step.

## Goal

Insert one new roadmap step into `## Implementation Steps` with the canonical section layout, using a stable UUID in the `[UUID] Stream: Title` heading.

## Workflow

1. Read `docs/plan/roadmap.md`, the current streams, and the execution diagram before adding anything.
2. Confirm the work is a new atomic acceptance story instead of a revision to an existing step.
3. Prepare one stream name, one step title, one `#### Why now` sentence, one `#### Usable outcome` sentence, and the concrete `#### Substeps`, `#### Tests`, and `#### Docs` bullets for that slice.
4. Insert a new step block in `## Implementation Steps` using the canonical layout from `skills/implementation-plan/SKILL.md`, give it a fresh UUID in the `[UUID] Stream: Title` heading, and place it where the execution order should reflect the new slice.
5. Re-read the inserted step and then manually reconcile any roadmap sections outside `## Implementation Steps` that the new work affects.

## Guardrails

- Adding a step usually also requires manual updates to `## Active Streams`, `## Implementation Approach`, the mermaid diagram, or `## Cross-Stream Notes` when the new work changes roadmap flow.
- Keep the new step at `XL` or smaller and split it before insertion if it would exceed the skill's size budget.
- Prefer `No assignee` for new steps unless the user explicitly wants to claim the work immediately.
