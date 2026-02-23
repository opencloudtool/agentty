# Agentty

![NPM Version](https://img.shields.io/npm/v/agentty)

A terminal UI tool for managing agents, built with Rust and [Ratatui](https://ratatui.rs).

![Demo](/docs/assets/demo/demo.gif)

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
  - Use `Up` / `Down` arrows to choose and `Enter` to confirm.
- The active mode is shown in the session chat title.
- Changes are persisted for that session and used as defaults for future sessions.

## Codex Sessions

- Codex sessions run through a persistent per-session `codex app-server` connection.
- Each Agentty session keeps one Codex thread open across replies.
- If the app-server connection fails, Agentty retries by restarting Codex app-server with a new thread.
- When a Codex thread is reset, Agentty replays the session transcript in the next turn prompt so implementation can continue with prior context.
- Codex session modes map to app-server permissions:
  - `plan`: `approvalPolicy=on-request` and read-only sandbox. Pre-action file/command requests are declined.
  - `auto_edit`: `approvalPolicy=on-request` and workspace-write sandbox. Pre-action file/command requests are accepted.
  - `autonomous`: `approvalPolicy=never` and danger-full-access sandbox. If a pre-action request still appears, it is accepted for the session.

## Features

### Git Worktree Integration

Agentty creates isolated worktrees for each session:

- **Isolated Branches:** Each session gets its own branch named `agentty/<hash>` based on your current branch
- **Separate Working Directory:** Sessions work in isolated directories under `~/.agentty/wt/`
- **Diff View:** Press `d` in the chat view to see real-time changes made by the agent
- **Rebase Action:** Press `r` in the chat view to rebase the session branch onto its base branch
- **Sync Action:** Press `s` in the session list to run session sync (`pull --rebase` + `push`) for review sessions
  - Sync is available only when the active project branch is `main`
  - Sync is blocked until `main` has no uncommitted changes
- **Automatic Cleanup:** Worktrees and branches are automatically removed when sessions are deleted

This allows agents to work on code changes without affecting your main working directory.

### Styled Session Output

- Agent responses in chat view now render a markdown subset with terminal styling.
- Supported formatting includes headings, bold/italic text, inline code, fenced code blocks, lists, blockquotes, and horizontal rules.
- User prompt lines (` › ...`) remain visually distinct in cyan bold styling.

### Stats Activity Heatmap

- The `Stats` tab now includes a GitHub-style activity heatmap for session creation activity over the last 53 weeks.
- Heatmap intensity scales with daily session counts and now includes month labels for easier timeline scanning.
- A right-side summary panel highlights favorite model usage, the longest session duration, and per-model input/output token totals.
- The per-session token table and overall totals remain available below the heatmap.

### Version Update Notice

- On startup, Agentty checks npmjs for the latest `agentty` version.
- If a newer version exists, an inline `vX.Y.Z version available update with npm i -g agentty@latest` notice is shown:
  - in the top status bar next to the current version
  - on the onboarding screen when there are no sessions

## Quickstart

```sh
git clone <repo-url>
cd agentty
cargo run # Builds and runs the 'agentty' binary
```

## Website

`agentty.xyz` is a Zola site stored in `docs/site/` and deployed through GitHub Pages.

```sh
# Preview locally
zola serve --root docs/site

# Build static output
zola build --root docs/site
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
