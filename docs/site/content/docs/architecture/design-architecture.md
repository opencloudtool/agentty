+++
title = "Design & Architecture"
description = "Architecture guide index and stable deep-link entry points for contributors."
weight = 0
+++

<a id="architecture-introduction"></a>
This landing page maps architecture topics to focused guides so contributors can
change the right module on the first pass.

<!-- more -->

## Architecture Guide Map

- [Architecture Runtime Flow](architecture-runtime-flow.md) covers goals, workspace map, event loop flow, and `AgentChannel` turn routing.
- [Architecture Module Map](architecture-module-map.md) maps major source paths to responsibilities across `app`, `domain`, `infra`, `runtime`, and `ui`.
- [Architecture Change Recipes](architecture-change-recipes.md) provides concrete change paths and an architecture-safe contributor checklist.
- [Architecture Testability Boundaries](architecture-testability-boundaries.md) documents trait boundaries and testing guidance for external integrations.

## Legacy Deep-Link Entry Points

<a id="architecture-goals"></a>
Architecture goals moved to [Architecture Runtime Flow](architecture-runtime-flow.md#architecture-runtime-flow-goals).

<a id="architecture-runtime-flow"></a>
The runtime flow diagram moved to [Architecture Runtime Flow](architecture-runtime-flow.md#architecture-runtime-flow-main).

<a id="architecture-runtime-flow-notes"></a>
Runtime flow notes moved to [Architecture Runtime Flow](architecture-runtime-flow.md#architecture-runtime-flow-notes).

<a id="architecture-agent-channel"></a>
The `AgentChannel` overview moved to [Architecture Runtime Flow](architecture-runtime-flow.md#architecture-agent-channel).

<a id="architecture-key-types"></a>
Channel key types moved to [Architecture Runtime Flow](architecture-runtime-flow.md#architecture-key-types).

<a id="architecture-provider-conversation-id-flow"></a>
Provider conversation ID flow moved to [Architecture Runtime Flow](architecture-runtime-flow.md#architecture-provider-conversation-id-flow).

<a id="architecture-testability-boundaries"></a>
Trait boundaries moved to [Architecture Testability Boundaries](architecture-testability-boundaries.md#architecture-testability-boundaries).

<a id="architecture-boundary-testing-guidance"></a>
Boundary testing guidance moved to [Architecture Testability Boundaries](architecture-testability-boundaries.md#architecture-boundary-testing-guidance).
