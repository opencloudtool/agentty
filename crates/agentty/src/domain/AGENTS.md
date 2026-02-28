# Domain Layer

## Overview

Pure business logic and domain entities, decoupled from UI and infrastructure.

## Directory Index

- [agent.rs](agent.rs) - Agent backend traits and model definitions.
- [input.rs](input.rs) - Input state management.
- [permission.rs](permission.rs) - Permission modes and logic.
- [project.rs](project.rs) - Project entity definitions.
- [session.rs](session.rs) - Session entity definitions.

## Docs

Changes to agent kinds, models, or session status/sizes require updating:

- `docs/site/content/docs/agents/backends.md` — agent backends and models.
- `docs/site/content/docs/usage/usage.md` — session lifecycle and sizes.
