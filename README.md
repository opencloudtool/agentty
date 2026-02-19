# Agentty

![NPM Version](https://img.shields.io/npm/v/%40opencloudtool%2Fagentty)

A terminal UI tool for managing agents, built with Rust and [Ratatui](https://ratatui.rs).

![Demo](/docs/demo/demo.gif)

## Installation

### Shell

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/opencloudtool/agentty/releases/latest/download/agentty-installer.sh | sh
```

### Cargo

```sh
cargo install agentty
```

### npm

```sh
npm install -g agentty
```

### npx

Run without installing:

```sh
npx agentty
```

## Prerequisites

- Rust nightly toolchain (configured via `rust-toolchain.toml`)
- Git (required for session worktree integration)
- One supported agent CLI installed: `gemini`, `claude`, or `codex`

## Session Agent, Model, and Permission Mode

Agent, model, and permission mode are configured per session, and the latest
selection becomes the default for newly created sessions.

- New sessions start with the most recently used `agent` + `model` +
  `permission_mode`.
- On a fresh setup (no previous session changes), defaults are `gemini` +
  `gemini-3-flash-preview` + `auto_edit`.
- In prompt mode, type `/model` as the first input token to open the multistep picker:
  - choose agent (`gemini`, `codex`, `claude`)
  - choose model from that agent's curated model list
- In prompt mode, press `Up` / `Down` to iterate previously sent messages for the active session and quickly resend/edit them.
- Press `Shift+Tab` in chat view or prompt mode to toggle permission mode for the current session.
- Permission modes:
  - `auto_edit` (default): runs with standard edit permissions.
  - `autonomous`: runs with elevated autonomy (backend-specific flags such as `--yolo` or skipping permission prompts).
  - `plan`: asks the agent for a detailed plan instead of implementation.
- After a plan response in chat view, an inline action bar appears:
  - `Implement the plan`: switches that session to `auto_edit` and sends an implementation prompt.
  - `Type feedback`: opens prompt input so you can send feedback while keeping `plan` mode.
  - Use `Left` / `Right` arrows to choose and `Enter` to confirm.
- The active mode is shown in the session chat title.
- Changes are persisted for that session and used as defaults for future sessions.

## Features

### Git Worktree Integration

Agentty creates isolated worktrees for each session:

- **Isolated Branches:** Each session gets its own branch named `agentty/<hash>` based on your current branch
- **Separate Working Directory:** Sessions work in isolated directories under `~/.agentty/wt/`
- **Diff View:** Press `d` in the chat view to see real-time changes made by the agent
- **Rebase Action:** Press `r` in the chat view to rebase the session branch onto its base branch
- **Automatic Cleanup:** Worktrees and branches are automatically removed when sessions are deleted

This allows agents to work on code changes without affecting your main working directory.

## Quickstart

```sh
git clone <repo-url>
cd agentty
cargo run # Builds and runs the 'agentty' binary
```

## Development

```sh
cargo test
cargo clippy -- -D warnings
cargo fmt --all -- --check
cargo shear
```

## License

Apache-2.0
