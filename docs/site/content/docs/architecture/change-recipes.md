+++
title = "Change Recipes"
description = "Concrete change paths for common contribution scenarios, plus a contributor checklist."
weight = 4
+++

<a id="architecture-change-recipes-introduction"></a> Use these recipes to route changes
through the correct modules without crossing layer boundaries.

<!-- more -->

## Add or Modify a Session Workflow

1. Keep frontend-neutral request/result models and programmatic operations in
   `crates/ag-session/`.
1. Update Agentty orchestration in `crates/agentty/src/app/session/` (`lifecycle.rs`,
   `worker.rs`, `task.rs`, etc.) and adapt it through
   `crates/agentty/src/app/session_api.rs`.
1. Route background-callable operations through the bounded actor in
   `crates/agentty/src/app/session_runtime.rs`; do not share `App` behind an async
   mutex.
1. Keep persistence in `crates/ag-store/src/` domain modules. Use `repository.rs` for
   repository bundle composition and `connection.rs` for pool wiring; keep Agentty's
   `crates/agentty/src/infra/db.rs` limited to database-location and clock composition.
1. Keep git operations behind `GitClient` in `crates/ag-git/src/client.rs`.
1. Preserve the session-branch invariant: one evolving commit per session branch, with
   the first file-changing turn creating it and later file-changing turns updating it by
   amending `HEAD`.
1. Update docs when lifecycle/status behavior changes:
   `docs/site/content/docs/usage/workflow.md`.

## Changing Campaign Orchestration

1. Keep shared task and lifecycle models in `ag-session`, campaign policy and prompts in
   `ag-orchestration`, and SQL in `ag-store`.
1. Route session mutations through `SessionService`. Emit campaign notifications through
   `OrchestrationEventSink`; Agentty owns translation to reducer events and injects the
   reconciliation schedule.
1. Run `test-ag-orchestration-src` and affected session/store/application checks through
   `prek`. Preserve the campaign E2E coverage in `crates/agentty/tests/e2e/`.

## Add a New Agent Backend or Model

1. Update provider model declarations in `crates/ag-agent/src/model/agent.rs`.
1. Add backend behavior in `crates/ag-agent/src/agent/` and register it in
   `crates/ag-agent/src/agent/provider.rs`.
1. If app-server-based, wire the provider client through
   `crates/ag-agent/src/agent/provider.rs` so the provider owns its runtime wiring.
1. Register any shared parsing, prompt-transport, streaming, or thought-policy changes
   in `crates/ag-agent/src/agent/provider.rs`.
1. The channel factory re-exported by the `ag-agent` crate root routes automatically
   based on the backend-owned transport mode - no change needed there unless the runtime
   contract itself changes.
1. Update `docs/site/content/docs/agents/backends.md` with backend/model documentation.

## Add or Change a Utility Agent Prompt

1. Submit an owned `OneShotRequest` through `OneShotClient`; do not select a CLI,
   app-server, backend, or protocol-repair helper from application orchestration.
1. Inject `&dyn OneShotClient` into the smallest workflow helper that needs
   deterministic coverage and test it with `MockOneShotClient`.
1. Keep provider routing, protocol repair, usage aggregation, and runtime cleanup in
   `crates/ag-agent/src/agent/submission.rs`.

## Add a Keybinding or Mode Interaction

1. For basic text editing, add or update the semantic command in
   `crates/agentty/src/domain/input.rs`, then map terminal keys once in
   `crates/agentty/src/runtime/mode/input_key.rs`.
1. Let prompt, question, branch-publish, and settings input modes intercept only their
   context-specific actions before falling back to the shared input command mapping.
1. For other interactions, update the handler in `crates/agentty/src/runtime/mode/`, or
   in `crates/agentty/src/runtime/key_handler.rs` when the interaction is a cross-mode
   overlay dispatch.
1. If a new mode/state is needed, extend `crates/agentty/src/presentation/app_mode.rs`.
1. If help content changes, update `crates/agentty/src/presentation/help_action.rs` as
   needed.
1. Update `docs/site/content/docs/usage/keybindings.md`.

## Add or Change Database Schema

1. Add a new migration file in `crates/ag-store/migrations/` (`NNN_description.sql`).
1. Never modify existing migration files.
1. Keep query changes in the matching `crates/ag-store/src/*.rs` domain module instead
   of expanding Agentty's composition facade.
1. Ensure any status/model behavior changes are reflected in docs pages affected by
   user-facing behavior.

## Add a New UI Page or Component

1. Add the page in `crates/agentty/src/ui/page/` or component in
   `crates/agentty/src/ui/component/`.
1. Wire the page into `crates/agentty/src/ui/router.rs`.
1. If a new `AppMode` is needed, extend the shared presentation contract implemented in
   `crates/agentty/src/presentation/app_mode.rs` and exported through
   `crates/agentty/src/presentation.rs`, then add a key handler in
   `crates/agentty/src/runtime/mode/`.

## Contributor Checklist for Architecture-Safe Changes

1. Keep workflow/state transitions in `app/`, not in UI rendering modules.
1. Keep external integrations in `infra/` behind traits.
1. Keep frontend-neutral session entities, enums, and policies in `ag-session`; keep
   Agentty-specific entities and interaction state in `domain/`.
1. In `app/` and `runtime/` orchestration, avoid direct `Command::new`, `Instant::now`,
   `SystemTime::now`, and direct filesystem/process calls unless they run behind trait
   boundaries.
1. For helpers that need timestamps in `app/` or `runtime/`, reuse the shared
   `app/session/core.rs` `Clock` boundary instead of adding direct `Instant::now()` or
   `SystemTime::now()` calls.
1. New external boundaries should get a trait with
   `#[cfg_attr(test, mockall::automock)]`.
1. Update docs in `docs/site/content/docs/` whenever user-facing behavior changes.
1. Update `docs/site/content/docs/architecture/module-map.md`,
   `docs/site/content/docs/architecture/runtime-flow.md`, and
   `docs/site/content/docs/architecture/testability-boundaries.md` when architecture
   responsibilities change.
1. Keep the nearest semantic `AGENTS.md` guides aligned when a major module's purpose,
   invariants, or change-routing guidance changes.
1. Treat render-time helpers as hot paths: avoid per-frame cloning of large render
   inputs, and make line-count/layout helpers reuse the same cached derived data as the
   final paint path.
1. When changing `TurnRequest`/`TurnContinuation`/`TurnEvent`/`TurnResult` shapes in
   `crates/ag-agent/src/channel/contract.rs` (re-exported by the `ag-agent` crate root),
   update the key-types table in `docs/site/content/docs/architecture/runtime-flow.md`.
1. When adding/removing `#[cfg_attr(test, mockall::automock)]` external-boundary traits,
   update `docs/site/content/docs/architecture/testability-boundaries.md`.
1. Run quality gates from `AGENTS.md` before opening a PR.
