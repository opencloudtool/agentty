---
name: implementation-plan
description: Create and maintain actionable implementation plans for this repository. Use when asked to draft a new plan in docs/plan, revise an existing plan document, or convert a request into a prioritized execution checklist with clear scope, status tracking, and file targets.
---

# Implementation Plan Workflow

Use this skill when producing or updating implementation plans in `docs/plan/`.

## Workflow

1. **Collect planning context**

   - Read `docs/plan/AGENTS.md` to enforce the required plan structure.
   - Review related source files and existing plan documents before writing.
   - Capture concrete constraints from the user request (scope, deadlines, quality gates, excluded work).

1. **Define scope and success boundaries**

   - Write one concise scope/context line tied to the relevant code area.
   - Identify what is in scope for this pass and what must remain out of scope.
   - Preserve only behavior required by the current request; remove stale or legacy plan items unless explicitly requested to keep them.

1. **Build the current-state snapshot**

   - Add a table with `Area`, `Current state in codebase`, and `Status`.
   - Base each row on observable code or command output.
   - Use precise status wording such as `Not started`, `Partial`, `Healthy`, or `Baseline captured`.

1. **Create prioritized execution sections**

   - Use numbered priorities with a short `Why now` rationale.
   - Add task checklists with `- [ ]` / `- [x]` and make each item implementation-ready.
   - List the primary files for each priority using repository-root-relative paths.

1. **Define execution sequence and guardrails**

   - Add `## Suggested Execution Order` with an ordered sequence.
   - Add `## Out of Scope for This Pass` with explicit non-goals.
   - Add `## Status Maintenance Rule` that requires immediate updates after each implemented step.

1. **Quality check before handing off**

   - Confirm the plan structure exactly matches `docs/plan/AGENTS.md`.
   - Remove duplicated or contradictory checklist items.
   - Ensure every priority can be executed independently and validated.

## Plan Skeleton

Use this skeleton when creating a new file in `docs/plan/`:

```markdown
# <Plan Title>

<One-sentence scope/context line tied to the relevant code area.>

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its status in this document.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| <area> | <observation> | <status> |

## Updated Priorities

## 1) <Priority Title>

**Why now:** <rationale>

- [ ] <task>
- [ ] <task>

Primary files:

- `<path>`
- `<path>`

## Suggested Execution Order

1. <step>
1. <step>

## Out of Scope for This Pass

- <non-goal>
- <non-goal>
```
