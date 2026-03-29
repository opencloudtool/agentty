# Update Step

Use this guide when an existing roadmap item needs a scoped content update without adding a brand-new item.

## Goal

Revise one existing roadmap item while preserving the canonical heading-title format (`[UUID] Stream: Title`) and the section layout for that item's queue.

## Workflow

1. Read `docs/plan/roadmap.md` and locate the target step by the UUID in its heading title.
1. Confirm the change belongs inside the existing item instead of being a separate follow-up slice.
1. Update the minimal set of named sections needed for the change.
1. For `Ready Now`, preserve `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs`.
1. For `Queued Next` and `Parked`, preserve `#### Outcome`, `#### Promote when`, and `#### Depends on`.
1. Preserve the existing heading format, section order, and markdown shape while editing so the item still matches the roadmap skeleton in `skills/implementation-plan/SKILL.md`.
1. Re-read the edited item and verify the content still describes one atomic, mergeable acceptance story at the right planning horizon.

## Guardrails

- Split the work into a new item instead of overloading one existing step or card with multiple sibling outcomes.
- Re-split the step if the revision pushes the estimated scope above `350` changed lines or requires more than `3` implementation bullets under `#### Substeps`.
- Preserve the UUID unless the old item is being removed and replaced by a genuinely new acceptance story.
- If the revision changes queues, sequencing, or dependencies, manually reconcile `## Active Streams`, `## Ready Now Execution Order`, and `## Context Notes` after updating the item block.
