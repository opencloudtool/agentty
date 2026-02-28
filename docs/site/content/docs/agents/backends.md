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
| Gemini  | `gemini`    | Google Gemini CLI agent. |
| Claude  | `claude`    | Anthropic Claude Code agent. |
| Codex   | `codex`     | OpenAI Codex CLI agent. |

## Selecting a Backend

Set the `AGENTTY_AGENT` environment variable to choose which backend to use:

```bash
# Use Claude as the default agent
export AGENTTY_AGENT=claude

# Use Codex as the default agent
export AGENTTY_AGENT=codex

# Use Gemini (the default if unset)
export AGENTTY_AGENT=gemini
```

If `AGENTTY_AGENT` is not set or contains an unrecognized value, Agentty
defaults to **Gemini**.

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
command in the prompt input. This opens a picker showing all models available
for the active backend.

To change the **default model** persistently, use the **Settings** tab
(`Tab` to navigate to it, `Enter` to edit).
