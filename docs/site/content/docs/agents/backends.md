+++
title = "Agents & Models"
description = "Supported agent backends, available models, and how to configure them."
weight = 1
+++

Agentty delegates coding work to external AI agent CLIs. Each backend is a
standalone CLI tool that Agentty launches in an isolated worktree. This page
covers the supported backends, available models, and configuration options.

<!-- more -->

## Supported Backends

Agentty supports three agent backends. Each requires its respective CLI to be
installed and available on your `PATH`.

| Backend | CLI command | Description |
|---------|-------------|-------------|
| Gemini | `gemini` | Google Gemini CLI agent. |
| Claude | `claude` | Anthropic Claude Code agent. |
| Codex | `codex` | OpenAI Codex CLI agent. |

## File Path Output Format

Agentty prompts all backends to reference files using repository-root-relative
POSIX paths. This keeps file references consistent in session output and reviews.

- Allowed forms: `path`, `path:line`, `path:line:column`
- Example: `crates/agentty/src/infra/agent/backend.rs:151`
- Not allowed: absolute paths, `file://` URIs, or `../`-prefixed paths

## Session Resume Behavior

Agentty persists provider-native conversation identifiers for app-server
backends and uses them to restore context after runtime restarts.

- Codex app-server: resumes by stored `threadId` via `thread/resume`.
- Gemini ACP: currently creates a fresh ACP `session/new` on runtime restart,
  so Agentty falls back to transcript replay when needed.

## Selecting a Backend

Choose the backend from the `/model` picker:

```bash
# Open model selection (backend first, then model)
/model
```

For persistent defaults, choose a default model in the **Settings** tab
(`Tab` to navigate, `Enter` to edit). The selected model determines which
backend is used for new sessions.

## Available Models

Each backend offers multiple models with different trade-offs between speed,
quality, and cost.

### Gemini Models

| Model ID | Description | Default |
|----------|-------------|---------|
| `gemini-3-flash-preview` | Fast Gemini model for quick iterations. | Yes |
| `gemini-3.1-pro-preview` | Higher-quality Gemini model for deeper reasoning. | |

### Claude Models

| Model ID | Description | Default |
|----------|-------------|---------|
| `claude-opus-4-6` | Top-tier Claude model for complex tasks. | Yes |
| `claude-sonnet-4-6` | Balanced Claude model for quality and latency. | |
| `claude-haiku-4-5-20251001` | Fast Claude model for lighter tasks. | |

### Codex Models

| Model ID | Description | Default |
|----------|-------------|---------|
| `gpt-5.3-codex` | Latest Codex model for coding quality. | Yes |
| `gpt-5.3-codex-spark` | Latest Codex spark model for coding quality. | |
| `gpt-5.2-codex` | Faster Codex model with lower cost. | |

## Switching Models

You can switch the model for the current session using the `/model` slash
command in the prompt input. This opens a two-step picker: first choose the
backend, then choose one of its models.

To change the **default model** persistently, use the **Settings** tab
(`Tab` to navigate to it, `Enter` to edit).
