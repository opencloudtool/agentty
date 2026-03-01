# Channel Module

Provider-agnostic agent channel abstraction for session turn execution.

## Overview

The `channel` module defines the \[`AgentChannel`\] trait and all supporting
types used to drive a single agent session turn without coupling callers to a
specific transport.

- \[`CliAgentChannel`\] spawns a CLI subprocess per turn and streams its stdout
  as \[`TurnEvent`\]s.
- \[`AppServerAgentChannel`\] delegates to \[`AppServerClient`\] and bridges
  \[`AppServerStreamEvent`\]s to \[`TurnEvent`\]s.
- \[`create_agent_channel`\] selects the right implementation for a given
  \[`AgentKind`\].

## Directory Index

- [app_server.rs](app_server.rs) - App-server RPC `AgentChannel` adapter (`AppServerAgentChannel`).
- [cli.rs](cli.rs) - CLI subprocess `AgentChannel` adapter (`CliAgentChannel`).
- [AGENTS.md](AGENTS.md) - Local module guidance and directory index.
- [CLAUDE.md](CLAUDE.md) - Symlink to AGENTS.md.
- [GEMINI.md](GEMINI.md) - Symlink to AGENTS.md.
