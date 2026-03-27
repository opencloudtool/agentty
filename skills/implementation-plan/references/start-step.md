# Start Step

Use this guide when an engineer is claiming an existing roadmap step before implementation begins.

## Goal

Change only the target step's `#### Assignee` field so ownership becomes visible without mixing in implementation edits.

## Workflow

1. Read `docs/plan/roadmap.md` and find the target step by the UUID in its `[UUID] Stream: Title` heading.
2. Verify the claim should only change ownership. If the request also changes scope, handle that separately with `references/update-step.md`.
3. Confirm the requested assignee uses the exact `@assignee` GitHub-handle format required by the roadmap.
4. Edit only the text inside that step's `#### Assignee` block so the value changes from `No assignee` to `@assignee`.
5. Re-read the touched step and confirm the only content change is the `#### Assignee` value.

## Guardrails

- Do not use this flow to rewrite `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, or `#### Docs`.
- Stop and clarify if the step is already assigned to someone other than `No assignee`.
- Keep the claim isolated from implementation changes for the direct-to-`main` workflow.
