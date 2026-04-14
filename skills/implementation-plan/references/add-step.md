# Add Step

Use this guide when roadmap work is missing entirely and needs a new backlog item.

## Goal

Insert one new roadmap item into the correct queue in `docs/plan/roadmap.md` using a stable UUID in the `[UUID] Stream: Title` heading.

## Workflow

1. Read `docs/plan/roadmap.md`, the current streams, and the `Ready Now` execution window before adding anything.
1. Confirm the work is a new atomic acceptance story instead of a revision to an existing item.
1. Confirm the work describes a user-facing outcome instead of standalone validation, documentation, cleanup, or other internal-only follow-through.
1. Decide whether the work belongs in `## Ready Now`, `## Queued Next`, or `## Parked`.
1. For `Ready Now`, prepare one stream name, one step title, one `#### Assignee`, one `#### Why now` sentence, one `#### Usable outcome` sentence, concrete `#### Substeps`, and the matching `#### Tests` and `#### Docs` entries for that slice.
1. For `Queued Next` or `Parked`, prepare one stream name, one step title, one `#### Outcome` sentence, one `#### Promote when` sentence, and one `#### Depends on` value.
1. Insert the new item using the canonical layout from `skills/implementation-plan/SKILL.md`, give it a fresh UUID in the `[UUID] Stream: Title` heading, and place it where the execution window or promotion queue should reflect the new work.
1. Re-read the inserted item and then manually reconcile any roadmap sections outside that queue that the new work affects.

## Guardrails

- Adding a `Ready Now` step usually also requires manual updates to `## Active Streams`, `## Planning Model`, `## Ready Now Execution Order`, or `## Context Notes` when the new work changes roadmap flow.
- Treat `500` changed lines as the hard ceiling, but keep new `Ready Now` steps estimated at `350` changed lines or less so the implementation still has room for test, wiring, and docs churn.
- Keep new `Ready Now` steps at `XL` or smaller and split them before insertion if they would exceed the skill's size budget.
- Keep new `Ready Now` steps to `1..=3` implementation bullets under `#### Substeps`; if the slice needs a fourth implementation bullet, queue the follow-up instead of widening the step.
- New `Ready Now` steps must include a concrete `@username` assignee. If the request does not name one, resolve the current promoter with `gh api user --jq .login` and use that `@<login>` value.
- Do not add standalone roadmap cards for tests, docs, cleanup, or similar internal work; keep that follow-through inside the user-facing slice it supports.
- Prefer adding backlog work to `## Queued Next` or `## Parked` instead of expanding `## Ready Now` beyond `5` items.
