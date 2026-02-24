# Agentty

![NPM Version](https://img.shields.io/npm/v/agentty)

An **Agentic Development Environment (ADE) in your terminal**, built with Rust and [Ratatui](https://ratatui.rs). Agentty provides a deeply integrated, git-native workflow for building software alongside AI agents.

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

- Git (required for session worktree integration)
- One supported agent CLI installed: `gemini`, `claude`, or `codex`

## Session Agent, Model, and Permission Mode

Agent, model, and permission mode are configured per session.

- New sessions start with the configured `Default Model` value from `Settings`
  plus the most recently used `permission_mode`.
- On a fresh setup (no previous session changes), defaults are `gemini` +
  `gemini-3-flash-preview` + `auto_edit`.
- In prompt mode, type `/model` as the first input token to open the multistep picker:
  - choose agent (`gemini`, `codex`, `claude`)
  - choose model from that agent's curated model list
- In `Settings` -> `Default Model`, select `Last used model as default` to
  persist model switches as the next-session default across app restarts.
- In prompt mode, press `Up` / `Down` to iterate previously sent messages for the active session and quickly resend/edit them.
- Permission modes:
  - `auto_edit` (default): runs with standard edit permissions.
- After a plan response in chat view, an inline action bar appears:
  - `Implement the plan`: switches that session to `auto_edit` and sends an implementation prompt.
  - `Type feedback`: opens prompt input so you can send feedback without changing permission mode.
  - Use `Up` / `Down` arrows to choose and `Enter` to confirm.
- The active mode is shown in the session chat title.
- Changes are persisted for that session and used as defaults for future sessions.

## Codex Sessions

- Codex sessions run through a persistent per-session `codex app-server` connection.
- Each Agentty session keeps one Codex thread open across replies.
- If the app-server connection fails, Agentty retries by restarting Codex app-server with a new thread.
- When a Codex thread is reset, Agentty replays the session transcript in the next turn prompt so implementation can continue with prior context.
- Codex session modes map to app-server permissions:
  - `auto_edit`: `approvalPolicy=on-request` and workspace-write sandbox. Pre-action file/command requests are accepted.

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

- The `Stats` tab now includes a GitHub-style activity heatmap for persisted session-creation activity over the last 53 weeks.
- Heatmap history is retained even after deleting old sessions.
- Heatmap intensity scales with daily session counts and now includes month labels for easier timeline scanning.
- A right-side summary panel highlights favorite model usage, the longest session duration, and per-model input/output token totals.
- The per-session token table and overall totals remain available below the heatmap.

### Version Update Notice

- On startup, Agentty checks npmjs for the latest `agentty` version.
- If a newer version exists, an inline `vX.Y.Z version available update with npm i -g agentty@latest` notice is shown:
  - in the top status bar next to the current version
  - on the onboarding screen when there are no sessions

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development checks and contribution guidance.

## License

Apache-2.0
