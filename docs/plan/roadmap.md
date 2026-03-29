# Agentty Roadmap

Single-file roadmap for the active project backlog. Humans keep priorities and guardrails here, while only `Ready Now` work carries full execution detail and everything else stays intentionally lighter.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Follow-up task workflow | Persisted follow-up tasks now launch sibling sessions from session view, and launched/open state survives refresh, reopen, and restart flows. | Landed |
| Review request publish flow | Review sessions still expose one generic `p` branch-publish action; GitHub only gets a post-push pull-request creation link, while GitLab uses the same publish flow. | Partial |
| Model availability scoping | `/model` and settings still cycle through the full static backend catalog even when one or more agent CLIs are not installed on the current machine. | Missing |
| Draft session workflow | `Shift+A` now creates explicit draft sessions that persist ordered staged draft messages, while `a` keeps the immediate-start first-prompt flow. | Landed |
| Session activity timing | `session` persists cumulative `InProgress` timing fields, and both chat and the grouped session list now show the same cumulative active-work timer. | Landed |
| Deterministic scenario coverage | Local git tests exist; five PTY-driven E2E tests in `crates/agentty/tests/e2e.rs` cover startup, tab cycling, quit dialog, and footer hints using testty, but live in a single file with no session-lifecycle or feature-area coverage. | Partial |
| Typed errors and hygiene | `DbError`, `GitError`, `AppServerTransportError`, `AppServerError`, `AgentError`, `SessionError`, and `AppError` enums propagate typed errors through all infra and app-layer boundaries; module-level regression tests cover session access, review replay, agent backend, CLI error formatting, and stdin write helpers; convention cleanup remains open. | Partial |
| Testty proof pipeline | PTY-driven sessions, VT100 frame parsing, VHS tape compilation, snapshot baselines, overlay renderer, recipe layer, proof reports (labeled captures, four backends: frame-text, PNG strip, GIF, HTML), native bitmap renderer, frame diffing, journey composition, and scale tooling are all landed. | Landed |
| Project delivery strategy | Review-ready sessions can already merge into the base branch or publish a session branch, but projects configured in Agentty still cannot declare whether their normal landing path should be direct merge to `main` or a pull-request flow. | Missing |

## Active Streams

- `Agents`: machine-scoped model availability for settings and slash-model selection.
- `Forge`: GitHub review-request creation/update from session view while preserving GitLab branch publishing.
- `Workflow`: draft-session staging before the first agent turn.
- `Quality`: deterministic local session coverage, typed-error migration, and hygiene follow-up.
- `Delivery`: project-level landing strategy for review-ready sessions, including direct-merge vs. pull-request expectations.

## Planning Model

- Keep no more than `5` fully expanded steps in `Ready Now`.
- Keep `Queued Next` as the compact promotion queue for the next few outcomes, not as a second fully detailed backlog.
- Keep `Parked` for strategic work that matters, but should not consume active planning attention yet.
- Treat `500` changed lines as the hard implementation ceiling and keep `Ready Now` slices estimated at `350` changed lines or less so normal implementation drift still stays reviewable.
- Run `cargo run -q -p ag-xtask -- roadmap context-digest` before promoting queued or parked work so the decision uses fresh repository context.
- When a `Ready Now` step lands and queued work remains, promote the next queued card into `Ready Now` instead of leaving the execution window short.
- Until lease automation exists, only `Ready Now` items can carry an assignee and only `Ready Now` items should be claimed.
- Claim ownership in a dedicated roadmap-only commit before starting implementation so the roadmap diff advertises who is taking the step, and resolve that assignee with `gh api user --jq .login` before writing `@<login>`.
- Keep tests and documentation attached to the same `Ready Now` step that changes behavior.
- Keep `Ready Now` implementation scopes to `1..=3` bullets under `#### Substeps`; when a step needs broader adoption, copy polish, or a second peer surface, queue the follow-up instead of widening the current slice.

## Ready Now

### [28de2b07-70a0-442a-821b-8b1946a1cea4] Agents: Scope model lists to locally available backends

#### Assignee

`@minev-dev`

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

### [ca014af3-5cd0-4567-bf11-3495765dcf6f] Forge: Replace GitHub branch publish with create or update pull request

#### Assignee

`@minev-dev`

#### Why now

The current `p` flow already has the git push path, the `ag-forge` review-request client, and session-side review-request persistence; promoting this slice now closes the remaining GitHub-specific review gap without waiting on unrelated workflow work.

#### Usable outcome

Pressing `p` in `Review` creates or refreshes the session pull request for GitHub remotes, while GitLab and other non-GitHub remotes keep the existing branch-publish behavior.

#### Substeps

- [ ] **Route `p` through a forge-aware publish action.** Update `crates/agentty/src/domain/session.rs`, `crates/agentty/src/runtime/mode/session_view.rs`, `crates/agentty/src/runtime/key_handler.rs`, and `crates/agentty/src/app/core.rs` so review sessions choose between the existing push-only path and one GitHub review-request action without splitting the current session-view popup flow into parallel entry points.
- [ ] **Reuse the existing review-request workflow for GitHub sessions.** Wire `crates/agentty/src/app/session/workflow/lifecycle.rs`, `crates/agentty/src/app/session/workflow/refresh.rs`, and the `ag-forge` client boundary so GitHub publication pushes the branch, finds any existing pull request for the session branch, refreshes it when present, and creates it when absent while persisting the normalized review-request summary back onto the session.
- [ ] **Align publish UI copy with the new GitHub outcome.** Update `crates/agentty/src/ui/component/publish_branch_overlay.rs`, `crates/agentty/src/ui/component/info_overlay.rs`, `crates/agentty/src/ui/state/help_action.rs`, and `crates/agentty/src/ui/page/session_chat.rs` so the popup title, help text, and success messaging describe create-or-update pull-request behavior for GitHub while preserving the current branch-publish copy for GitLab and other remotes.

#### Tests

- [ ] Add or extend coverage in `crates/agentty/src/app/core.rs`, `crates/agentty/src/app/session/workflow/lifecycle.rs`, `crates/agentty/src/app/session/workflow/refresh.rs`, `crates/agentty/src/runtime/mode/session_view.rs`, and `crates/agentty/src/ui/component/publish_branch_overlay.rs` for GitHub vs. GitLab action selection, existing-pull-request refresh, first-time pull-request creation, and the resulting popup copy/state.

#### Docs

- [ ] Update `docs/site/content/docs/usage/workflow.md`, `docs/site/content/docs/usage/keybindings.md`, and `docs/site/content/docs/architecture/runtime-flow.md` to explain that `p` creates or refreshes GitHub pull requests while non-GitHub remotes continue to use the branch-publish flow.

### [01c37d54-bd1a-467f-bf9e-8c8d6cabaac3] Quality: Restructure E2E tests and add navigation coverage

#### Assignee

`@andagaev`

#### Why now

The testty proof pipeline is fully landed and five E2E tests already prove the PTY-driven approach works, but they live in a single flat file with no shared helpers and only cover tab cycling and quit dialog. Splitting into a multi-file layout now creates the scaffolding that all subsequent feature-area E2E tests build on, while the first batch of new tests covers navigation and confirmation gaps that need no agent backend.

#### Usable outcome

E2E tests live in a `crates/agentty/tests/e2e/` multi-file layout with shared helpers, reusable `Journey` builders, and new tests for reverse tab cycling, help overlay toggling, confirmation dialog completion, session list empty state, and project page rendering.

#### Substeps

- [ ] **Restructure into multi-file test layout with journey builders.** Convert `crates/agentty/tests/e2e.rs` into `crates/agentty/tests/e2e/main.rs` with a `mod common;` shared helpers module (extracting `test_builder`, `save_proof_gif`, `proof_output_dir`, `frame_from_capture`, `header_region`) and agentty-specific `Journey` builders (startup-wait, switch-to-tab, open-quit-dialog) that compose testty's generic `Journey` API. Move existing tests into `crates/agentty/tests/e2e/navigation.rs`, verify `cargo test -p agentty --test e2e` still passes, and confirm tests run in parallel (each test uses its own `TempDir` and `AGENTTY_ROOT`).
- [ ] **Add reverse tab cycling and help overlay tests.** Add `backtab_cycles_tabs_reverse` (Shift+Tab cycles backward through all four tabs) and `help_overlay_toggle` (`?` opens help overlay with keybinding content, `Esc` closes it and restores the previous view) to `crates/agentty/tests/e2e/navigation.rs`.
- [ ] **Add confirmation dialog tests.** Create `crates/agentty/tests/e2e/confirmation.rs` with `quit_confirm_yes_exits` (assert PTY process exits with code 0 after `q` → `y`) and `quit_confirm_dismiss_returns` (app continues after `q` → `n` and after `q` → `Esc`, covering both dismiss paths in one test).
- [ ] **Add session list and project page empty-state tests.** Create `crates/agentty/tests/e2e/session.rs` with `session_list_empty_state` (Sessions tab shows empty-state message when no sessions exist) and `crates/agentty/tests/e2e/project.rs` with `projects_page_shows_cwd` (Projects tab lists the registered project name from the temp workdir).

#### Tests

- [ ] Each substep above produces its own E2E tests. Run `cargo test -p agentty --test e2e` to validate the full restructured suite passes.

#### Docs

- [ ] No user-facing behavior changes — no doc updates needed.

## Ready Now Execution Order

```mermaid
flowchart TD
    R1["[28de2b07] Agents: local model availability"]
    R2["[ca014af3] Forge: GitHub review request publish"]
    R3["[01c37d54] Quality: E2E test restructure + navigation"]
```

## Queued Next

### [4f491812-f373-4ac5-bd57-b46c4f9d91e3] Workflow: Polish draft-session editing after baseline staging lands

#### Outcome

Refine the draft-session UX with edit/remove affordances and any transcript/title cleanup that proves necessary once the explicit-start baseline is in place.

#### Promote when

Promote after the explicit `Shift+A` draft-session baseline sees enough use to clarify which edit/remove affordances are worth standardizing.

#### Depends on

`[64c9bb7f] Workflow: Stage draft session messages and start them explicitly`

### [33150cab-7c16-4eea-b5a2-34f316243709] Quality: Add session lifecycle and prompt E2E tests

#### Outcome

E2E tests cover session creation via `a` key, opening a session with `Enter`, session list `j`/`k` navigation, deleting a session with confirmation, prompt input basics (typing text, multiline via Alt+Enter, cancel via Esc), and returning to list from session view, all driven through full UI flows without pre-seeded database state.

#### Promote when

Promote after `Quality: Restructure E2E tests and add navigation coverage` lands and the multi-file layout with shared journey builders is in place.

#### Depends on

`[01c37d54] Quality: Restructure E2E tests and add navigation coverage`

### [5a84d7a9-3346-4e01-90be-ce5d3783b32f] Quality: Add settings, stats, and resize E2E tests

#### Outcome

E2E tests cover settings page navigation and edit overlay (`Enter` opens, `Esc` cancels), stats page empty-state rendering, project selection switching to sessions tab, small terminal graceful rendering (40x12), and wide terminal layout adaptation (200x50).

#### Promote when

Promote after `Quality: Restructure E2E tests and add navigation coverage` lands and the multi-file layout with shared journey builders is in place. Can promote independently of `[33150cab]`.

#### Depends on

`[01c37d54] Quality: Restructure E2E tests and add navigation coverage`

### [17a9e2ba-0b7d-407d-9cd4-72807ef7bc1f] Delivery: Add project commit strategy selection

#### Outcome

Let each project stored in Agentty choose its expected landing path so review-ready sessions can steer users toward either direct merge to `main` or a pull-request workflow instead of treating both paths as an undifferentiated default.

#### Promote when

Promote when maintainers want review and publish actions to respect the target project's expected delivery flow rather than leaving that decision entirely manual.

#### Depends on

`[ca014af3] Forge: Replace GitHub branch publish with create or update pull request`

## Parked

### [282012e4-d4c0-4a83-8d24-a5d137f40111] Quality: Refresh discard-path documentation

#### Outcome

Bring discard-path documentation and comments back in sync after the typed-error and workflow changes settle.

#### Promote when

Promote when the active quality slices stop changing the discard behavior and wording.

#### Depends on

`[ed9de74b] Quality: Propagate typed errors through the app layer` (landed)

### [d2e6ee6c-e784-4d54-aad6-559c2c580101] Quality: Sweep convention cleanup follow-up

#### Outcome

Finish the remaining convention cleanup after active behavior work is no longer changing the same files.

#### Promote when

Promote when the active `Workflow`, `Platform`, and `Quality` steps stop rewriting the same modules.

#### Depends on

`[832c9729] Quality: Fill the missing module-level regression tests` (landed)

### [1c7b7080-deaf-4e2c-8e3c-df24e01d9251] Quality: Ship one deterministic local session workflow slice

#### Outcome

Add one deterministic local-session scenario plus the minimal reusable harness so the default in-process workflow path can be validated without live credentials.

#### Promote when

Promote when a `Ready Now` slot opens and the active workflow and model-availability slices stop competing for the same session lifecycle boundaries.

#### Depends on

`None`

## Context Notes

- `Forge: Replace GitHub branch publish with create or update pull request` should reuse the existing `ag-forge` review-request create/refresh flow and keep GitLab on the current push-only branch-publish path.
- `Agents: Scope model lists to locally available backends` should reuse one shared availability snapshot across Settings and `/model` instead of probing CLIs separately in render paths.
- `Workflow: Stage draft session messages and start them explicitly` should keep using `Status::New` for draft sessions instead of introducing a second pre-start lifecycle status.
- The parked local session harness slice should come back only when the active workflow and model-availability changes stop churning the same lifecycle seams.
- The typed-error migration and module-test backfill are both complete. Convention cleanup remains open in the parked sweep card.
- `Delivery: Add project commit strategy selection` should define the landing policy at the Agentty project level so merge and publish actions can present the right default path for each managed repository.
- Testty proof pipeline is fully landed in `crates/testty/`. Future enhancements (e.g., additional proof backends, CI integration, or new recipe types) should be queued as new parked cards referencing that crate.
- E2E tests use the testty framework and drive the full UI flow (no pre-seeded database). Tests that require agent-dependent states (diff view from `Review`, question mode, follow-up task navigation) are deferred until an in-process mock agent channel can be wired into the PTY binary or the parked local session workflow harness lands.
- The parked `[1c7b7080] Quality: Ship one deterministic local session workflow slice` covers in-process session testing with mock agent channels. The E2E test stream covers PTY-driven TUI testing. These are complementary, not overlapping.
- Run `cargo run -q -p ag-xtask -- roadmap context-digest` before promoting queued or parked work to `Ready Now`.

## Status Maintenance Rule

- Keep no more than `5` items in `## Ready Now`.
- Keep only `Ready Now` items fully expanded with `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs`.
- Keep `## Queued Next` and `## Parked` as compact promotion cards with `#### Outcome`, `#### Promote when`, and `#### Depends on`.
- Claim work only from `## Ready Now` by updating that step's `#### Assignee` field in a dedicated commit before implementation starts, using `gh api user --jq .login` to determine the `@<login>` value.
- Keep each `Ready Now` step estimated at `350` changed lines or less so implementation remains below the `500`-line hard ceiling, and split any wider follow-up into `## Queued Next`.
- After a `Ready Now` step lands, remove it from `## Ready Now`, refresh any changed snapshot rows, and promote the next queued card whenever `## Queued Next` still has work.
- If follow-up work remains after a step lands, add or update a compact queued or parked card instead of preserving the completed step.
