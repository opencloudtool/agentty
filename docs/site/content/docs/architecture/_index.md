+++
title = "Architecture"
description = "Design and architecture references for Agentty internals."
weight = 5
sort_by = "weight"
+++

<a id="architecture-overview"></a>
<a id="architecture-introduction"></a>
Design and architecture references for Agentty runtime flow, module ownership,
testability boundaries, and change paths.

## Architecture Topics

- [Runtime Flow](runtime-flow.md) explains runtime goals, the workspace map, the
  event loop, and agent channel routing.
- [Module Map](module-map.md) maps source paths to responsibilities across
  `app`, `domain`, `infra`, `runtime`, and `ui`.
- [Change Recipes](change-recipes.md) gives architecture-safe change paths for
  common contribution scenarios.
- [Testability Boundaries](testability-boundaries.md) documents trait
  boundaries and deterministic testing guidance for external integrations.
- [Architecture Diagrams](diagrams.md) provides comprehensive Mermaid diagrams
  for workspace crates, module layers, pipelines, state machines, and data flows.
