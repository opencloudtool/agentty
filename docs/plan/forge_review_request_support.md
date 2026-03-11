# Forge Review Request Support Plan

Plan for extending `crates/agentty/src/app`, `crates/agentty/src/infra`, and `crates/agentty/src/ui` so review-ready sessions can publish and track forge review requests across GitHub pull requests and GitLab merge requests.

## Priorities

The manual review-request workflow in `crates/agentty/src/app/session/workflow` and `crates/agentty/src/infra/forge` is already landed and remains the prerequisite baseline for the priorities below.

## 1) Ship Session-View PR/MR Actions and Manual Usage Docs

### Why now

The baseline workflow already supports publish/create/open/refresh, so the next smallest user-visible iteration is surfacing that behavior in the main session view.

### Usable outcome

A user can create a PR/MR when missing, open an existing PR/MR, and refresh linked metadata directly from session view with clear help/footer guidance and updated usage docs.

### Substeps

- [x] Add a session-view review-request action in `crates/agentty/src/runtime/mode/session_view.rs` that creates a PR/MR when no link exists and otherwise opens or refreshes the linked review request based on state.
- [x] Extend `AppMode` in `crates/agentty/src/ui/state/app_mode.rs`, help/footer projections in `crates/agentty/src/ui/state/help_action.rs`, and view-mode key handling to show loading, success, and blocked states without leaving session context.
- [x] Render normalized review-request metadata in `crates/agentty/src/ui/page/session_chat.rs` without displacing existing diff and focused-review behavior.
- [x] Show actionable blocked states when `gh` or `glab` is missing or unauthenticated so users can fix local CLI setup from the same review flow.
- [x] Add UI-focused tests that keep the new action availability, footer/help text, and session rendering aligned with session status and linked review-request state.
- [x] Update `docs/site/content/docs/usage/workflow.md` and `docs/site/content/docs/usage/keybindings.md` to document the manual session-view review-request flow and its local CLI prerequisites.

## 2) Add Background Review-Request Status Reconciliation and Final Architecture Docs

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
- [ ] Add deterministic tests for poll scheduling, event reduction, and status-transition rules for merged, closed, reopened, and unavailable review-request states.
- [ ] Update `docs/site/content/docs/usage/workflow.md`, `docs/site/content/docs/architecture/runtime-flow.md`, `docs/site/content/docs/architecture/testability-boundaries.md`, and `docs/site/content/docs/architecture/module-map.md` to describe the automatic reconciliation behavior and its new runtime boundary.

## Cross-Plan Notes

- `docs/plan/coverage_follow_up.md` only adds coverage work and does not change review-request behavior.
- `docs/plan/continue_in_progress_sessions_after_exit.md` also touches `crates/agentty/src/app/core.rs`; detached-session rules own turn lifetime, while this plan owns review-request reconciliation.
- If another active plan conflicts with this plan and the correct resolution is not explicit, stop and ask the user which plan should control the work.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its checklist status in this document and refresh any current-state snapshot rows that changed.
- When a step changes user-visible behavior or contributor guidance, update the corresponding documentation in that same step before marking it complete.

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
- Surface session-view actions and rendered metadata next so users can drive the manual workflow from the primary TUI without leaving session context.
- Keep the first UI slice compatible with explicit refresh; automatic reconciliation can land afterward as an extension of the visible workflow rather than a prerequisite for it.
- Update usage docs in the same iteration as the session-view action, then update architecture docs when the background reconciliation boundary and reducer flow are stable.

## Suggested Execution Order

```mermaid
graph TD
    B[Completed baseline: manual publish/create/open/refresh] --> P1[1. Session-view PR/MR actions and manual docs]
    P1 --> P2[2. Background reconciliation and final architecture docs]
```

1. Treat the manual publish/create/open/refresh workflow as the already-landed prerequisite baseline.
1. Start `1) Ship Session-View PR/MR Actions and Manual Usage Docs` first so users can create and manage PR/MR links from the primary TUI flow.
1. Start `2) Add Background Review-Request Status Reconciliation and Final Architecture Docs` only after `1)` is merged, because status gathering should extend the already-discoverable UI workflow.
1. No top-level priorities are safe to run in parallel right now; the usage-doc tasks inside `1)` can trail final keybinding and action names, and the architecture-doc tasks inside `2)` can trail the final poller boundary once reducer flow is settled.

## Out of Scope for This Pass

- A repository-wide inbox for browsing arbitrary pull requests or merge requests independent of sessions.
- Inline review-comment authoring, draft review management, or one-click merge parity with forge web UIs.
- Support for forges beyond GitHub and GitLab.
- Webhook-driven reconciliation or any server-side push infrastructure beyond the local CLI-based polling planned here.
