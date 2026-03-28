# Domain Layer

## Overview

Pure business logic and domain entities, decoupled from UI and infrastructure.

## Key Files

- `agent.rs` defines provider kinds, models, and model metadata.
- `session.rs` defines persisted session entities, statuses, and sizing logic.
- `project.rs`, `setting.rs`, `permission.rs`, and `input.rs` define shared application concepts.

## Docs

Changes to agent kinds, models, or session status/sizes require updating:

- `docs/site/content/docs/agents/backends.md` — agent backends and models.
- `docs/site/content/docs/usage/workflow.md` — session lifecycle and sizes.
