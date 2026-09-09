+++
title = "Installation"
description = "Install Agentty, launch your first session, and review generated changes."
weight = 1
+++

<a id="installation-introduction"></a> `agentty` is an ADE (Agentic Development
Environment) for structured, controllable AI-assisted software development.

<!-- more -->

## Prerequisites

Agentty runs inside Git repositories and uses linked worktrees for session isolation.
Install Git using the official [Git downloads](https://git-scm.com/downloads) for your
operating system, then verify the installation:

```bash
git --version
```

## Install

<a id="installation-options"></a> npm is recommended because it supports Agentty's
automatic update flow. Other installation methods remain available when npm is not the
right fit.

### npm (recommended, supports auto-update)

```bash
npm install -g agentty
```

### npx (no install)

```bash
npx agentty
```

### Shell installer

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/agentty-xyz/agentty/releases/latest/download/agentty-installer.sh | sh
```

### Cargo

macOS source builds require Xcode Command Line Tools for native process bindings.

```bash
cargo install agentty
```

## Verify a GitHub Release

Install the [GitHub CLI](https://cli.github.com/), then download a GitHub release
artifact. Each artifact has keyless Sigstore build provenance that identifies Agentty's
release workflow:

```bash
gh attestation verify PATH_TO_ARTIFACT --repo agentty-xyz/agentty
```

Release immutability also protects the published tag and complete asset set. Verify a
specific release and a downloaded asset with:

```bash
gh release verify vX.Y.Z --repo agentty-xyz/agentty
gh release verify-asset vX.Y.Z PATH_TO_ARTIFACT --repo agentty-xyz/agentty
```

## Prepare an Agent Backend

Agentty also requires at least one supported agent CLI on your `PATH`. Install and
authenticate one backend before launching Agentty:

- **Codex** (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- **Claude** (`claude`): install
  [Claude Code](https://github.com/anthropics/claude-code), then run
  `claude auth login`.
- **Antigravity** (`agy` 1.1.18 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow.
- **Gemini** (`gemini`): install the
  [Gemini CLI](https://github.com/google-gemini/gemini-cli), then configure an API key
  or Vertex AI authentication.

See [Agents & Models](@/docs/agents/backends.md) before choosing credentials. Provider
subscription and OAuth rules differ, and not every interactive CLI sign-in is suitable
for third-party invocation through Agentty.

## Start a Session

1. Open a git repository in your terminal.
1. Run `agentty`.
1. Start a new session and let the agent work in its dedicated worktree branch.

Only one Agentty instance can use a given Agentty root at a time. If startup reports
that another instance is running, close that instance first. A crash releases ownership
automatically; do not delete the lock file. Separate `AGENTTY_ROOT` directories can run
independently.

## Review Changes

<a id="installation-review-changes"></a> Inside `agentty`, open the diff view (`d`) to
inspect the generated `git diff` before you keep or discard edits.
