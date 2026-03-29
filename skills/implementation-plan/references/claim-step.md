# Claim Step

Use this guide when an engineer wants to claim an existing `Ready Now` roadmap step before implementation begins.

## Goal

Make ownership visible first by updating only the target `Ready Now` step's `#### Assignee` field for the current user, then land that claim as its own reviewable commit before starting implementation work. Keep roadmap actualization for completion as a separate follow-up step after the implementation lands.

## Workflow

1. Read `docs/plan/roadmap.md` and find the target step by the UUID in its `[UUID] Stream: Title` heading.
1. Verify the target lives in `## Ready Now`. If it lives in `## Queued Next` or `## Parked`, promote it first instead of claiming it there.
1. Run `gh api user --jq .login` and use the returned GitHub login as the current user's canonical username for the claim.
1. Edit only the text inside that step's `#### Assignee` block so the value changes from `No assignee` to the current user's `@username` identifier derived from `gh`.
1. Re-read the touched step and confirm the only content change is the `#### Assignee` value.
1. Land the claim as its own commit before starting implementation so teammates can see ownership in the roadmap diff immediately.

## Guardrails

- Do not use this flow to rewrite `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, or `#### Docs`.
- Stop and clarify if `gh api user --jq .login` is unavailable or returns a login that does not match the intended assignee.
- Stop and clarify if the step is already assigned to someone other than `No assignee`.
- Keep the claim isolated from implementation changes until automated leasing exists.
- Do not remove or reshape the roadmap step during the claim. When the work is complete, actualize the roadmap separately by removing the implemented `Ready Now` step and refreshing any affected snapshot or queued follow-up entries.
