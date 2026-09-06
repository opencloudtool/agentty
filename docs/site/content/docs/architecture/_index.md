+++
title = "Architecture"
description = "Design and architecture references for Agentty internals."
weight = 5
sort_by = "weight"
+++

<a id="architecture-overview"></a> <a id="architecture-introduction"></a> Design and
architecture references for Agentty runtime flow, module ownership, testability
boundaries, orchestration, and change paths.

## Architecture Topics

- [Runtime Flow](@/docs/architecture/runtime-flow.md) explains runtime goals, the
  workspace map, the event loop, and agent channel routing.
- [Module Map](@/docs/architecture/module-map.md) maps source paths to responsibilities
  across `app`, `domain`, `infra`, `runtime`, and `ui`.
- [Change Recipes](@/docs/architecture/change-recipes.md) gives architecture-safe change
  paths for common contribution scenarios.
- [Testability Boundaries](@/docs/architecture/testability-boundaries.md) documents
  trait boundaries and deterministic testing guidance for external integrations.
- [`ag-harness` Design](@/docs/architecture/ag-harness-design.md) documents the model
  contract, durable session lifecycle, concurrency, and local observability workflow.
- [Orchestrator Design](@/docs/architecture/orchestrator.md) documents the current
  campaign model and the target wave, dependency-graph, and board design.
