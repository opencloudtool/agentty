# Update Step

Use this guide when an existing pending roadmap step needs a scoped content update without adding a brand-new step.

## Goal

Revise one existing step while preserving the roadmap's canonical heading-title format (`[UUID] Stream: Title`) and section layout: `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs`.

## Workflow

1. Read `docs/plan/roadmap.md` and locate the target step by the UUID in its heading title.
1. Confirm the change belongs inside the existing step instead of being a separate follow-up slice.
1. Update the minimal set of named sections needed for the change: the step heading, `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs`.
1. Preserve the existing heading format, section order, and markdown shape while editing so the step still matches the roadmap skeleton in `skills/implementation-plan/SKILL.md`.
1. Re-read the edited step and verify the content still describes one atomic, mergeable acceptance story.

## Guardrails

- Split the work into a new step instead of overloading one existing step with multiple sibling outcomes.
- Preserve the UUID unless the old step is being removed and replaced by a genuinely new acceptance story.
- If the revision changes streams, sequencing, or dependencies, manually reconcile `## Active Streams`, `## Suggested Execution Order`, and `## Cross-Stream Notes` after updating the step block.
