+++
title = "Overview"
description = "Understand how Agentty organizes AI agent sessions and workflows."
weight = 0
+++

<a id="overview-introduction"></a>
`agentty` is a terminal-first Agentic Development Environment (ADE), designed for building software alongside AI agents.
This project is built using `agentty` itself, including the docs and docs-site you are reading.

<a id="overview-ai-sessions"></a>
It runs AI coding agents in dedicated AI sessions.

<!-- more -->

## What Agentty Provides

<a id="overview-operational-lift"></a>
When you start a session, Agentty does the operational heavy lifting for workflow safety:

- Spawns a clean worktree branch for every session.
- Runs agent-driven edits in isolation from your base branch.
- Keeps terminal output, diffs, and generated changes in one reviewable stream.

## Typical Flow

1. Open a repository and start `agentty`.
1. Start a new session from the UI and ask for changes.
1. Let the agent modify files in its worktree.
1. Review the diff (`d`) and decide to apply or discard.

## Worktree Isolation

<a id="overview-worktree-isolation"></a>
Every session runs in its own [git worktree](https://git-scm.com/docs/git-worktree),
created automatically when the session starts:

- The worktree branch is named `agentty/<hash>`, where `<hash>` is derived from
  the session ID.
- The branch is based on whichever branch was active when you launched `agentty`.
- All agent edits happen inside the worktree, keeping your base branch untouched
  until you explicitly merge.
- If worktree creation fails (e.g., git is not installed or permissions are
  insufficient), session creation fails atomically and displays an error.

<a id="overview-worktree-cleanup"></a>
Worktrees are stored under `~/.agentty/wt/` and are cleaned up automatically
when you delete a session.

## Key Concepts

| Concept | Description |
|---------|-------------|
| **Agent** | An external AI CLI backend (Gemini, Claude, or Codex) that performs coding work. See [Agents & Models](@/docs/agents/backends.md). |
| **Session** | An isolated unit of work: one prompt, one worktree branch, one reviewable diff. See [Workflow & Keybindings](@/docs/usage/usage.md). |
| **Project** | A git repository registered in Agentty. Switch between projects with the Projects tab. |
| **Diff view** | Press `d` in a review-state session to see exactly what the agent changed. |

## Next Steps

- [Installation](./installation) — install Agentty and run it for the first time.
- [Agents & Models](@/docs/agents/backends.md) — configure backends and choose models.
- [Workflow & Keybindings](@/docs/usage/usage.md) — learn the interface and keyboard shortcuts.
