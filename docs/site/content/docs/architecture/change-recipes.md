+++
title = "Change Recipes"
description = "Concrete change paths for common contribution scenarios, plus a contributor checklist."
weight = 4
+++

<a id="architecture-change-recipes-introduction"></a>
Use these recipes to route changes through the correct modules without crossing
layer boundaries.

<!-- more -->

## Add or Modify a Session Workflow

1. Update orchestration in `crates/agentty/src/app/session/` (`lifecycle.rs`, `worker.rs`, `task.rs`, etc.).
1. Keep persistence in `crates/agentty/src/infra/db.rs`.
1. Keep git operations behind `GitClient` in `crates/agentty/src/infra/git/client.rs` (re-exported from `crates/agentty/src/infra/git.rs`).
1. Update docs when lifecycle/status behavior changes: `docs/site/content/docs/usage/workflow.md`.

## Add a New Agent Backend or Model

1. Update domain model declarations in `crates/agentty/src/domain/agent.rs`.
1. Add backend behavior in `crates/agentty/src/infra/agent/` and wiring in `backend.rs`.
1. If app-server-based, extend routing in `crates/agentty/src/infra/app_server_router.rs`.
1. Register transport mode in `crates/agentty/src/infra/agent/backend.rs` (`transport_mode()`).
1. The channel layer (`infra/channel.rs`) routes automatically based on transport mode - no change needed there.
1. Update `docs/site/content/docs/agents/backends.md` with backend/model documentation.

## Add a Keybinding or Mode Interaction

1. Update the handler in `crates/agentty/src/runtime/mode/`.
1. If a new mode/state is needed, extend `crates/agentty/src/ui/state/app_mode.rs`.
1. If help content changes, update `crates/agentty/src/ui/state/help_action.rs` as needed.
1. Update `docs/site/content/docs/usage/keybindings.md`.

## Add or Change Database Schema

1. Add a new migration file in `crates/agentty/migrations/` (`NNN_description.sql`).
1. Never modify existing migration files.
1. Keep query changes in `crates/agentty/src/infra/db.rs`.
1. Ensure any status/model behavior changes are reflected in docs pages affected by user-facing behavior.

## Add a New UI Page or Component

1. Add the page in `crates/agentty/src/ui/page/` or component in `crates/agentty/src/ui/component/`.
1. Wire the page into `crates/agentty/src/ui/router.rs`.
1. If a new `AppMode` is needed, extend `crates/agentty/src/ui/state/app_mode.rs` and add a key handler in `crates/agentty/src/runtime/mode/`.

## Contributor Checklist for Architecture-Safe Changes

1. Keep workflow/state transitions in `app/`, not in UI rendering modules.
1. Keep external integrations in `infra/` behind traits.
1. Keep business entities and enums in `domain/`.
1. In `app/` and `runtime/` orchestration, avoid direct `Command::new`, `Instant::now`, `SystemTime::now`, and direct filesystem/process calls unless they run behind trait boundaries.
1. New external boundaries should get a trait with `#[cfg_attr(test, mockall::automock)]`.
1. Update docs in `docs/site/content/docs/` whenever user-facing behavior changes.
1. Update `docs/site/content/docs/architecture/module-map.md`, `docs/site/content/docs/architecture/runtime-flow.md`, and `docs/site/content/docs/architecture/testability-boundaries.md` when architecture responsibilities change.
1. When adding/removing files in `runtime/mode/`, update the runtime-mode file list in `docs/site/content/docs/architecture/module-map.md`.
1. When changing `TurnRequest`/`TurnEvent`/`TurnResult` shapes in `infra/channel.rs`, update the key-types table in `docs/site/content/docs/architecture/runtime-flow.md`.
1. When adding/removing `#[cfg_attr(test, mockall::automock)]` external-boundary traits, update `docs/site/content/docs/architecture/testability-boundaries.md`.
1. Run quality gates from `AGENTS.md` before opening a PR.
