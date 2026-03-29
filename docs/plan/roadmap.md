# Agentty Roadmap

Single-file roadmap for the active project backlog. Humans keep priorities and guardrails here, while only `Ready Now` work carries full execution detail and everything else stays intentionally lighter.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Follow-up task workflow | Persisted follow-up tasks now flow through the protocol, SQLite storage, and session UI, but they still cannot launch sibling sessions or retain launched/open state. | Partial |
| Model availability scoping | `/model` and settings still cycle through the full static backend catalog even when one or more agent CLIs are not installed on the current machine. | Missing |
| Draft session workflow | `New` sessions are still blank placeholders whose first submitted prompt starts the agent immediately, so users cannot stage multiple draft messages and explicitly launch the session later. | Missing |
| Session activity timing | `session` persists cumulative `InProgress` timing fields, chat shows the timer, and the session list still has no time column. | Partial |
| Deterministic scenario coverage | Local git tests exist, but there is no shared app-level scenario harness for a full local session workflow. | Partial |
| Typed errors and hygiene | `DbError` is landed, but git, app-server, remaining infra surfaces, and the app layer still expose string errors; discard comments, missing module tests, and convention cleanup remain open. | Partial |
| Testty proof pipeline | PTY-driven sessions, VT100 frame parsing, VHS tape compilation, snapshot baselines, overlay renderer, and recipe layer exist. Proof reports, native frame rendering, and scale tooling remain backlog work. | Partial |

## Active Streams

- `Agents`: machine-scoped model availability for settings and slash-model selection.
- `Workflow`: follow-up task launch behavior and draft-session staging before the first agent turn.
- `Platform`: session timing surfaces.
- `Quality`: deterministic local session coverage, typed-error migration, and hygiene follow-up.
- `Testty`: proof-driven TUI testing framework and scale tooling for `crates/testty/`.

## Planning Model

- Keep no more than `5` fully expanded steps in `Ready Now`.
- Keep `Queued Next` as the compact promotion queue for the next few outcomes, not as a second fully detailed backlog.
- Keep `Parked` for strategic work that matters, but should not consume active planning attention yet.
- Run `cargo run -q -p ag-xtask -- roadmap context-digest` before promoting queued or parked work so the decision uses fresh repository context.
- Until lease automation exists, only `Ready Now` items can carry an assignee and only `Ready Now` items should be claimed.
- Claim ownership in a dedicated roadmap-only commit before starting implementation so the roadmap diff advertises who is taking the step.
- Keep tests and documentation attached to the same `Ready Now` step that changes behavior.

## Ready Now

### [8f4402cd-beff-4b4d-b9f7-00efd834249b] Workflow: Launch sibling sessions from follow-up tasks and retain task state

#### Assignee

`No assignee`

#### Why now

The follow-up task persistence slice is already landed, so the next workflow step should deliver the remaining user-visible action instead of leaving stored tasks as read-only output.

#### Usable outcome

Selecting a persisted follow-up task can launch it into a sibling session, and the original session keeps the launched and open task state stable across reopen and refresh flows.

#### Substeps

- [ ] **Add the sibling-session launch path.** Wire one persisted follow-up task through `crates/agentty/src/app/core.rs`, `crates/agentty/src/app/session_state.rs`, and `crates/agentty/src/app/session/workflow/load.rs` so the app can create a sibling session from stored task content without inventing a parallel session-creation path.
- [ ] **Expose the launch action in session view.** Update `crates/agentty/src/runtime/mode/session_view.rs`, `crates/agentty/src/ui/page/session_chat.rs`, and `crates/agentty/src/ui/state/help_action.rs` so the selected follow-up task can be launched from the existing session UI with a clear launched/open affordance.
- [ ] **Persist launched-task state through reloads.** Extend `crates/agentty/src/infra/db.rs`, `crates/agentty/src/domain/session.rs`, and any required migration under `crates/agentty/migrations/` so launched and open task state survives refresh, app restart, and session reopen.

#### Tests

- [ ] Add or extend coverage in `crates/agentty/src/app/core.rs`, `crates/agentty/src/app/session/workflow/load.rs`, `crates/agentty/src/infra/db.rs`, and `crates/agentty/src/runtime/mode/session_view.rs` for sibling-session launch, persisted task-state reload, and the session-view action path.

#### Docs

- [ ] Update `docs/site/content/docs/usage/workflow.md` and `docs/site/content/docs/usage/keybindings.md` for launching follow-up tasks into sibling sessions and the resulting task-state behavior.

### [9f115af0-a382-46f4-8bf9-25886936e252] Platform: Add the timer to the grouped session list

#### Assignee

`@minev-dev`

#### Why now

The active-work timer already persists and renders in session chat, so the next platform slice should reuse that same timing path in the list view instead of leaving the grouped list behind.

#### Usable outcome

The grouped session list shows the same cumulative active-work timer that session chat shows, including live updates for in-progress sessions and frozen totals for completed intervals.

#### Substeps

- [ ] **Add the timer column to the grouped list.** Update `crates/agentty/src/ui/page/session_list.rs` to render a timer column and reuse duration helpers from `crates/agentty/src/domain/session.rs` and `crates/agentty/src/ui/text_util.rs` instead of introducing list-specific timing math.
- [ ] **Thread the render-time clock through list rendering.** Keep `crates/agentty/src/ui/render.rs` and `crates/agentty/src/ui/router.rs` aligned so the grouped session list reads the same `wall_clock_unix_seconds` render context already used by session chat.
- [ ] **Preserve grouped-table behavior with the new column.** Extend the grouped-row layout logic and tests in `crates/agentty/src/ui/page/session_list.rs` so selection, placeholders, truncation, and width calculations stay stable with the timer present.

#### Tests

- [ ] Extend `crates/agentty/src/ui/page/session_list.rs` and `crates/agentty/src/ui/text_util.rs` tests to cover active and completed timers in grouped session rows.

#### Docs

- [ ] Update `docs/site/content/docs/usage/workflow.md` to note that the session list now surfaces cumulative active-work time.

### [1c7b7080-deaf-4e2c-8e3c-df24e01d9251] Quality: Ship one deterministic local session workflow slice

#### Assignee

`No assignee`

#### Why now

The quality stream needs one full app-level scenario that exercises the default local path before the remaining cleanup work keeps landing around it.

#### Usable outcome

A deterministic scenario test can create a disposable repo, run one scripted local agent turn through the app-facing workflow, and verify the resulting commit, worktree, transcript output, and terminal session state.

#### Substeps

- [ ] **Add the minimal local-session harness.** Create the smallest reusable harness under `crates/agentty/tests/support/` for temp repos, fake CLIs, and workflow assertions.
- [ ] **Add one deterministic local-session scenario.** Add `crates/agentty/tests/local_session_workflow.rs` to exercise a full local session journey without live credentials.
- [ ] **Refactor only the boundaries the scenario needs.** Keep any workflow refactors constrained to explicit boundaries rather than shell-heavy test-only helpers.

#### Tests

- [ ] Run the new local-session scenario and the touched workflow-module tests to confirm the harness covers the full local path.

#### Docs

- [ ] Update `CONTRIBUTING.md` with the deterministic local-session scenario command and the expectation that fake CLIs cover the default workflow path.

### [7b743a5a-ee48-48ed-a9d6-689a50440a87] Quality: Introduce `GitError` for `infra/git/` and `GitClient`

#### Assignee

`@andagaev`

#### Why now

The git boundary is the largest remaining source of `Result<..., String>` signatures and should set the typed-error pattern for the rest of the pending infra work.

#### Usable outcome

The git modules and `GitClient` return typed `GitError` variants instead of strings, while app-layer bridges remain only where later steps still need them.

#### Substeps

- [ ] **Define and re-export `GitError`.** Add `crates/agentty/src/infra/git/error.rs` and re-export the enum from `crates/agentty/src/infra/git.rs`.
- [ ] **Migrate the git modules.** Convert `sync.rs`, `rebase.rs`, `repo.rs`, `merge.rs`, and `worktree.rs` to return `GitError`.
- [ ] **Update `GitClient` and `RealGitClient`.** Move the trait and production implementation to typed git errors and keep temporary app bridges only where still required.
- [ ] **Maintain touched docs and semantic guides.** Add `///` doc comments for the new error type and refresh the nearest semantic `AGENTS.md` guidance when the module boundary changes.

#### Tests

- [ ] Run the existing git tests with `GitError` return types and add at least one assertion for a simulated `GitError::CommandFailed` path.

#### Docs

- [ ] Keep the new error type documented in code and the touched semantic guidance aligned with the new file layout.

### [28de2b07-70a0-442a-821b-8b1946a1cea4] Agents: Scope model lists to locally available backends

#### Assignee

`No assignee`

#### Why now

Agentty already documents that each backend depends on a locally installed CLI, but the current static model lists still expose unavailable choices in `/model` and Settings, which makes first-run setup and cross-machine use noisier than the runtime actually supports.

#### Usable outcome

The `/model` picker and persisted default-model selectors only offer models whose backend is locally runnable on the user's machine, while stored but unavailable model values fall back predictably instead of trapping the UI on hidden choices.

#### Substeps

- [ ] **Add one machine-scoped agent-availability boundary.** Introduce a focused availability module under `crates/agentty/src/infra/agent/` plus the matching router exports in `crates/agentty/src/infra/agent.rs` so backend discovery lives behind a trait-based subprocess boundary instead of direct orchestration calls; wire one startup/background refresh path through `crates/agentty/src/app/task.rs` and `crates/agentty/src/app/core.rs` to cache which `AgentKind` values are locally runnable.
- [ ] **Scope settings defaults and fallback resolution.** Update the model-selection helpers in `crates/agentty/src/app/setting.rs` and any needed domain helpers in `crates/agentty/src/domain/agent.rs` so smart, fast, and review defaults cycle only through available models, and persisted unavailable values resolve to a stable fallback path instead of remaining silently selectable-but-unrunnable.
- [ ] **Scope prompt model switching and empty-state messaging.** Update `crates/agentty/src/runtime/mode/prompt.rs` and `crates/agentty/src/ui/page/session_chat.rs` so `/model` shows only locally available agent kinds and models, preserves the current session model when it is still valid, and surfaces explicit guidance when no supported backend CLI is installed.

#### Tests

- [ ] Add or extend coverage in `crates/agentty/src/infra/agent/`, `crates/agentty/src/app/setting.rs`, `crates/agentty/src/runtime/mode/prompt.rs`, and `crates/agentty/src/ui/page/session_chat.rs` for mixed installed/missing CLIs, persisted unavailable defaults, and the no-backend-installed empty state, keeping the external-command checks behind mockable trait boundaries.

#### Docs

- [ ] Update `docs/site/content/docs/agents/backends.md` and `docs/site/content/docs/usage/workflow.md` to explain that model choices are filtered by locally available backend CLIs and to describe the fallback behavior for stored defaults that are unavailable on the current machine.

## Ready Now Execution Order

```mermaid
flowchart TD
    R1["[8f4402cd] Workflow: sibling-session launch"]
    R2["[9f115af0] Platform: session-list timer"]
    R3["[1c7b7080] Quality: deterministic local session harness"]
    R4["[7b743a5a] Quality: GitError migration"]
    R5["[28de2b07] Agents: local model availability"]
```

## Queued Next

### [64c9bb7f-4d11-4c3c-b2ad-4a86db9bd6c9] Workflow: Stage draft session messages and start them explicitly

#### Outcome

Let `New` sessions retain ordered draft messages across reloads and start the first agent turn only when the staged draft bundle is explicitly launched.

#### Promote when

Promote after a `Ready Now` slot opens and the active workflow/model-availability slices stop competing for the same session lifecycle files.

#### Depends on

`None`

### [7608043e-3ae8-44b4-bcb4-341f8070d0d2] Quality: Introduce typed errors for the remaining infra boundaries

#### Outcome

Replace stringly typed errors in the remaining infra boundaries so the app layer can consume typed failures consistently.

#### Promote when

Promote after `Quality: Introduce GitError for infra/git/ and GitClient` lands and the git boundary pattern is reusable.

#### Depends on

`[7b743a5a] Quality: Introduce GitError for infra/git/ and GitClient`

### [ed9de74b-64c0-4ca6-86b5-d29c8bc26591] Quality: Propagate typed errors through the app layer

#### Outcome

Replace app-layer string error bridges with typed errors after the infra boundaries expose stable enums.

#### Promote when

Promote after the remaining infra typed-error step lands and the app-facing shape is clear.

#### Depends on

`[7608043e] Quality: Introduce typed errors for the remaining infra boundaries`

### [832c9729-acde-45c0-93d8-d31511100082] Quality: Fill the missing module-level regression tests

#### Outcome

Backfill missing module tests once the active workflow and typed-error slices stop moving underneath them.

#### Promote when

Promote after the current `Ready Now` behavioral steps settle enough that the new tests will not churn immediately.

#### Depends on

`[cbf025d6] Workflow: Persist and render emitted follow-up tasks`, `[ed9de74b] Quality: Propagate typed errors through the app layer`

### [4f491812-f373-4ac5-bd57-b46c4f9d91e3] Workflow: Polish draft-session editing after baseline staging lands

#### Outcome

Refine the draft-session UX with edit/remove affordances and any transcript/title cleanup that proves necessary once the explicit-start baseline is in place.

#### Promote when

Promote after `Workflow: Stage draft session messages and start them explicitly` lands and real usage clarifies which draft-editing actions are worth standardizing.

#### Depends on

`[64c9bb7f] Workflow: Stage draft session messages and start them explicitly`

## Parked

### [282012e4-d4c0-4a83-8d24-a5d137f40111] Quality: Refresh discard-path documentation

#### Outcome

Bring discard-path documentation and comments back in sync after the typed-error and workflow changes settle.

#### Promote when

Promote when the active quality slices stop changing the discard behavior and wording.

#### Depends on

`[ed9de74b] Quality: Propagate typed errors through the app layer`

### [d2e6ee6c-e784-4d54-aad6-559c2c580101] Quality: Sweep convention cleanup follow-up

#### Outcome

Finish the remaining convention cleanup after active behavior work is no longer changing the same files.

#### Promote when

Promote when the active `Workflow`, `Platform`, and `Quality` steps stop rewriting the same modules.

#### Depends on

`[832c9729] Quality: Fill the missing module-level regression tests`

### [3e7f1a92-4b8d-4c6e-9a15-d2f8e0b71c34] Testty: Land proof report fundamentals

#### Outcome

Add labeled captures and the proof report core so the proof pipeline can generate reviewable artifacts.

#### Promote when

Promote when the active product-facing streams no longer dominate planning attention or when `Testty` becomes the primary investment stream.

#### Depends on

`None`

### [b8e4a6d2-1f3c-4d7e-a952-c6b0d8e3f419] Testty: Add native rendering and visual proof backends

#### Outcome

Render terminal frames natively and use that renderer to unlock screenshot, GIF, and HTML proof outputs.

#### Promote when

Promote after `Testty: Land proof report fundamentals` lands and the proof object model is stable.

#### Depends on

`[3e7f1a92] Testty: Land proof report fundamentals`

### [4c9f2e68-d1a5-4b7c-8e34-a6b0c3d9e271] Testty: Add scale tooling for high-volume scenarios

#### Outcome

Add scenario tiering and reusable journey helpers so the proof pipeline scales without manual test choreography.

#### Promote when

Promote after the proof fundamentals land and there is enough scenario volume to justify scale tooling.

#### Depends on

`[3e7f1a92] Testty: Land proof report fundamentals`

## Context Notes

- `Workflow: Launch sibling sessions from follow-up tasks and retain task state` should reuse the same stored task content that the persistence slice lands.
- `Agents: Scope model lists to locally available backends` should reuse one shared availability snapshot across Settings and `/model` instead of probing CLIs separately in render paths.
- `Workflow: Stage draft session messages and start them explicitly` should treat `Status::New` as the persisted draft container instead of introducing a second pre-start lifecycle status.
- `Platform: Add the timer to the grouped session list` should reuse `Session::in_progress_duration_seconds()` and the shared render-time wall clock instead of inventing a second timer source.
- The local session harness should keep validating the default in-process workflow path that `Workflow` and `Platform` depend on.
- The typed-error sequence should stay linear so each layer learns from the previous enum shape instead of reworking multiple error surfaces at once.
- `Testty` remains strategically important, but it is independent of the active `agentty` product work and should stay parked until a human intentionally rebalances the queue.
- Run `cargo run -q -p ag-xtask -- roadmap context-digest` before promoting queued or parked work to `Ready Now`.

## Status Maintenance Rule

- Keep no more than `5` items in `## Ready Now`.
- Keep only `Ready Now` items fully expanded with `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs`.
- Keep `## Queued Next` and `## Parked` as compact promotion cards with `#### Outcome`, `#### Promote when`, and `#### Depends on`.
- Claim work only from `## Ready Now` by updating that step's `#### Assignee` field in a dedicated commit before implementation starts.
- After a `Ready Now` step lands, remove it from `## Ready Now`, refresh any changed snapshot rows, and either promote a queued card or leave the slot open.
- If follow-up work remains after a step lands, add or update a compact queued or parked card instead of preserving the completed step.
