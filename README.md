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
- Git (optional, for automatic worktree integration)

## Features

### Git Worktree Integration

When launched from within a git repository, Agentty automatically creates isolated worktrees for each session:

- **Isolated Branches:** Each session gets its own branch named `agentty/<hash>` based on your current branch
- **Separate Working Directory:** Sessions work in isolated directories under `/var/tmp/.agentty/`
- **Diff View:** Press `d` in the chat view to see real-time changes made by the agent
- **Automatic Cleanup:** Worktrees and branches are automatically removed when sessions are deleted
- **No Git Interference:** Works seamlessly in non-git directories without any special configuration

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

hello world
