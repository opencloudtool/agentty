+++
title = "Installation"
description = "Install Agentty, launch your first session, and review generated changes."
weight = 1
+++

<a id="installation-introduction"></a>
`agentty` is a terminal-first environment for running AI coding agents in isolated git worktrees.

<!-- more -->

## Install

<a id="installation-options"></a>
Use one of these installation options:

### Shell installer

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/agentty-xyz/agentty/releases/latest/download/agentty-installer.sh | sh
```

### Cargo

```bash
cargo install agentty
```

### npm (global)

```bash
npm install -g agentty
```

### npx (no install)

```bash
npx agentty
```

## Start a Session

1. Open a git repository in your terminal.
1. Run `agentty`.
1. Start a new session and let the agent work in its dedicated worktree branch.

## Review Changes

<a id="installation-review-changes"></a>
Inside `agentty`, open the diff view (`d`) to inspect the generated `git diff` before you keep or discard edits.
