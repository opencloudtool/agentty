# Forge Review Request Support Plan

Plan for extending `crates/agentty/src/app`, `crates/agentty/src/infra`, and `crates/agentty/src/ui` so review-ready sessions can publish and track forge review requests across GitHub pull requests and GitLab merge requests.

## Priorities

The manual publish/create/open/refresh workflow in `crates/agentty/src/app/session/workflow`, `crates/agentty/src/infra/forge`, and session view is already landed and is the baseline for the remaining work below.

## 1) Add Background Review-Request Status Reconciliation and Final Architecture Docs

### Why now

Automatic status gathering should extend a workflow users can already trigger and inspect in session view, not precede it.

### Usable outcome

Linked sessions automatically reconcile to `Done` or `Canceled` after the remote review request is observed as merged or closed, and architecture docs describe the final poller and reducer boundaries.

### Substeps

- [ ] Add an app-scoped background job in `crates/agentty/src/app/task.rs` and `crates/agentty/src/app/core.rs` that periodically checks linked review-request state for active sessions with forge metadata.
- [ ] Route poller results through `AppEvent` or an equivalent reducer-driven path instead of mutating session state directly inside the task, keeping the reducer wiring in `crates/agentty/src/app/core.rs`.
- [ ] Reuse the `gh` and `glab` adapter refresh commands inside the poller from `crates/agentty/src/app/session/workflow/refresh.rs` and `crates/agentty/src/app/session/workflow/task.rs` instead of introducing a second direct network client for background reconciliation.
- [ ] Move a session to `Done` when the linked review request is merged and to `Canceled` when it is closed without merge in `crates/agentty/src/domain/session.rs`, while preserving explicit local terminal states when no transition is needed.
- [ ] Define guardrails for polling cadence, unsupported or unauthenticated forge failures, and stale-session behavior so the poller stays low-noise and cheap.

### Tests

- [ ] Add deterministic tests for poll scheduling, event reduction, and status-transition rules for merged, closed, reopened, and unavailable review-request states.

### Docs

- [ ] Update `docs/site/content/docs/usage/workflow.md`, `docs/site/content/docs/architecture/runtime-flow.md`, `docs/site/content/docs/architecture/testability-boundaries.md`, and `docs/site/content/docs/architecture/module-map.md` to describe the automatic reconciliation behavior and its new runtime boundary.

## Cross-Plan Notes

- `docs/plan/coverage_follow_up.md` only adds coverage work and does not change review-request behavior.
- `docs/plan/continue_in_progress_sessions_after_exit.md` also touches `crates/agentty/src/app/core.rs`; detached-session rules own turn lifetime, while this plan owns review-request reconciliation.
- If another active plan conflicts with this plan and the correct resolution is not explicit, stop and ask the user which plan should control the work.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its checklist status in this document and refresh any current-state snapshot rows that changed.
- When a step changes behavior, complete its `### Tests` and `### Docs` work in that same priority before marking it complete.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Manual session review-request workflow | Publish, open, and refresh already persist normalized PR/MR links and can recover archived-session metadata from stored forge URLs. | Healthy |
| Forge adapters and persistence | GitHub and GitLab adapters plus `session_review_request` persistence already cover normalized create, find, refresh, and reload flows. | Healthy |
| Background reconciliation | No app event or background task currently polls linked review requests or reconciles merged or closed remote outcomes back into session status. | Not started |
| Session view UI | Session view already exposes create, open, and refresh actions with popup feedback and inline PR/MR metadata. | Healthy |
| Documentation coverage | Manual session-view review-request docs are landed, but automatic reconciliation docs are still pending. | Partial |

## Implementation Approach

- Keep the already-landed manual review-request workflow as the working baseline: a review-ready session can publish its branch, link or create a review request, refresh stored metadata, and open the linked URL.
- Build background reconciliation as an extension of that visible baseline rather than a prerequisite for publishing or refreshing review requests manually.
- Update usage and architecture docs in the same priority as the reconciliation behavior so the runtime boundary and user expectations stay aligned.

## Suggested Execution Order

```mermaid
graph TD
    B[Completed baseline: manual publish/create/open/refresh] --> P1[1. Background reconciliation and final architecture docs]
```

1. Treat the manual publish/create/open/refresh workflow as the already-landed prerequisite baseline.
1. Start `1) Add Background Review-Request Status Reconciliation and Final Architecture Docs` next, because automatic status gathering should extend the already-discoverable manual workflow instead of redefining it.
1. No top-level priorities are safe to run in parallel right now; the usage docs can trail final user-visible status wording, and the architecture docs can trail the final poller boundary once reducer flow is settled.

## Out of Scope for This Pass

- A repository-wide inbox for browsing arbitrary pull requests or merge requests independent of sessions.
- Inline review-comment authoring, draft review management, or one-click merge parity with forge web UIs.
- Support for forges beyond GitHub and GitLab.
- Webhook-driven reconciliation or any server-side push infrastructure beyond the local CLI-based polling planned here.
