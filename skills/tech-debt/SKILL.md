---
name: tech-debt
description: Sweep the codebase for tech debt and return a prioritized markdown task list of findings.
---

# Tech Debt Skill

Use this skill when asked to find tech debt, stale patterns, or maintenance issues in a codebase.

The user may scope the sweep to specific directories or modules (e.g., "sweep `crates/agentty/src/app`"). When a scope is given, restrict traversal to those paths. When no scope is given, sweep the full codebase.

## Workflow

1. **Read Project Context**
   - Read the root `AGENTS.md` for project conventions, architecture references, and style rules.
   - Identify which directories and modules are relevant to the sweep (respecting any user-provided scope).

2. **Traverse Target Directories**
   - Navigate into each target directory and read the nearest available `AGENTS.md` for local conventions, entry points, and change guidance.
   - Use the architecture docs and module routers to prioritize which files and modules to inspect when no deeper local guide exists.

3. **Analyze for Tech Debt**
   - **TODOs and FIXMEs:** Find `TODO`, `FIXME`, `HACK`, and `XXX` comments that indicate deferred work.
   - **Outdated Patterns:** Identify deprecated API usage, legacy code paths retained without justification, and patterns that conflict with current project conventions.
   - **Inconsistent Error Handling:** Flag mixed error handling strategies (e.g., `unwrap()` alongside proper `Result` propagation), swallowed errors, and missing error context.
   - **Missing Documentation:** Note public types, traits, and functions lacking doc comments, especially in areas with complex logic.
   - **Stale Dependencies:** Look for pinned dependency versions that lag behind current releases, unused dependencies, or feature flags that are no longer needed.
   - **Dead Code:** Identify unused functions, modules, imports, or feature gates that can be removed.
   - **Test Gaps:** Flag critical logic paths that lack test coverage or tests that are marked `#[ignore]`.
   - **Convention Violations:** Check code against the project conventions discovered in Step 1 (e.g., naming rules, module layout, import style, constructor patterns) and flag deviations.

4. **Return Findings as a Task List**
   - Structure your answer as a prioritized markdown task list.
   - Use the format below for each finding.

### Task List Format

```markdown
# Tech Debt Report

## Summary
[Brief overview: total finding count, highest-risk area, and overall codebase health impression.]

## Critical
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and why it is critical.]

## High
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and recommended action.]

## Medium
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and recommended action.]

## Low
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and recommended action.]
```

### Priority Guidelines

| Priority | Criteria |
|----------|----------|
| **Critical** | Causes runtime failures, data loss, or blocks other work. |
| **High** | Significant maintenance burden, outdated patterns actively causing confusion, or missing error handling in critical paths. |
| **Medium** | Style inconsistencies, missing documentation, minor dead code, or deferred TODOs with clear scope. |
| **Low** | Cosmetic issues, minor naming improvements, or optional cleanup with no immediate impact. |
