+++
+++
title = "Overview"
description = "Understand how Agentty organizes AI agent sessions and workflows."
weight = 0
+++

`agentty` is a terminal-first tool that runs AI coding agents in dedicated, isolated git worktrees.

<!-- more -->

## What Agentty Provides

When you start a session, Agentty does the operational heavy lifting for workflow safety:

- Spawns a clean worktree branch for every session.
- Runs agent-driven edits in isolation from your base branch.
- Keeps terminal output, diffs, and generated changes in one reviewable stream.

## Typical Flow

1. Open a repository and start `agentty`.
1. Start a new session from the UI and ask for changes.
1. Let the agent modify files in its worktree.
1. Review the diff (`d`) and decide to apply or discard.

## Next Steps

Continue with the `installation` guide for exact commands and the first-run setup.

[Go to Installation](./installation)
