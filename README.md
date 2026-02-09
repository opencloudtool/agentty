# Agentty

A terminal UI tool for managing agents, built with Rust and [Ratatui](https://ratatui.rs).

## Installation

### Shell

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/opencloudtool/agentty/releases/latest/download/agentty-installer.sh | sh
```

### npm

```sh
npm install -g @opencloudtool/agentty
```

### npx

Run without installing:

```sh
npx @opencloudtool/agentty
```

## Prerequisites

- Rust nightly toolchain (configured via `rust-toolchain.toml`)
- Git (required for session worktree integration)
- One supported agent CLI installed: `gemini`, `claude`, or `codex`

## Session Agent and Model

Agent/model configuration is session-scoped.

- New sessions start with `gemini` + `gemini-3-flash-preview`.
- In prompt mode, type `/model` as the first input token to open the multistep picker:
  - choose agent (`gemini`, `codex`, `claude`)
  - choose model from that agent's curated model list
- Changes apply to that session only and are persisted.

## Features

### Git Worktree Integration

Agentty creates isolated worktrees for each session:

- **Isolated Branches:** Each session gets its own branch named `agentty/<hash>` based on your current branch
- **Separate Working Directory:** Sessions work in isolated directories under `/var/tmp/.agentty/`
- **Diff View:** Press `d` in the chat view to see real-time changes made by the agent
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
