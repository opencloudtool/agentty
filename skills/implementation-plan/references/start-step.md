# Start Step

Use this guide when implementation is about to start for an already-claimed roadmap step.

## Goal

Start the exact planned implementation for one `Ready Now` step without mixing in claim edits or reshaping the scope mid-flight.

## Workflow

1. Read `docs/plan/roadmap.md` and find the target step by the UUID in its `[UUID] Stream: Title` heading.
1. Run `gh api user --jq .login` to confirm the current authenticated GitHub login, then verify the target lives in `## Ready Now` and that its `#### Assignee` already names that user.
1. Re-read that step's `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs` sections before editing code.
1. Implement the step exactly as written. Treat the roadmap text as the current execution contract instead of rewriting the step while work is in progress.
1. If implementation reveals a real scope mistake, or the diff is trending beyond the roadmap's buffered estimate and threatens the `500`-line ceiling, stop the execution flow and handle that roadmap change separately with `references/update-step.md`.
1. Keep tests and docs attached to the same implementation slice described in the step before considering the work complete.
1. Once the step is complete, actualize `docs/plan/roadmap.md` by removing the implemented item from `## Ready Now`, refreshing any changed snapshot rows, and adding or updating a compact queued or parked card if follow-up work still remains.

## Guardrails

- Do not use this flow to claim ownership. Handle claiming first with `references/claim-step.md`.
- Do not quietly expand one step into multiple sibling outcomes during implementation.
- Do not leave roadmap-only claim edits mixed into the implementation diff.
- Do not keep completed implementation detail in `## Ready Now` after the step lands.
