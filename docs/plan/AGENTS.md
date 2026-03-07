# Plan

Internal planning documents and UI design notes.

## Plan Document Structure

Use the following structure for every plan document in this directory:

1. `# <Plan Title>`
1. One-sentence scope/context line that references the relevant code area.
1. `## Status Maintenance Rule`

- State that statuses/checklists must be updated immediately after each implemented step.

1. `## Current State Snapshot`

- Include a table with `Area`, `Current state in codebase`, and `Status`.

1. `## Updated Priorities`

- Use numbered priority sections.
- Each priority should include:
  - a short `Why now` rationale
  - a task checklist (`- [ ]` / `- [x]`)
  - primary files to touch

1. `## Suggested Execution Order`

- Ordered implementation sequence.

1. `## Out of Scope for This Pass`

- Explicit non-goals to prevent scope drift.

## Directory Index

- [`test_coverage.md`](test_coverage.md) - Workspace test coverage improvement plan.
