+++
title = "Agents & Models"
description = "Supported agent backends, available models, and how to configure them."
weight = 1
+++

<a id="backends-introduction"></a>
Agentty delegates coding work to external AI agent CLIs. Each backend is a
standalone CLI tool that Agentty launches in an isolated worktree. This page
covers the supported backends, available models, and configuration options.

<!-- more -->

## Supported Backends

<a id="backends-supported-backends"></a>
Agentty supports three agent backends. Each requires its respective CLI to be
installed and available on your `PATH`.

| Backend | CLI command | Description |
|---------|-------------|-------------|
| Gemini | `gemini` | Google Gemini CLI agent. |
| Claude | `claude` | Anthropic Claude Code agent. |
| Codex | `codex` | OpenAI Codex CLI agent. |

## Claude Authentication

<a id="backends-claude-authentication"></a>
If Claude session turns or utility prompts fail with `authentication_error`,
`Failed to authenticate`, or `OAuth token has expired`, refresh the Claude CLI
session and retry:

```bash
claude auth login
claude auth status
```

For SSO-backed accounts, use `claude auth login --sso`.

## File Path Output Format

<a id="backends-path-output-format"></a>
Agentty prompts all backends to reference files using repository-root-relative
POSIX paths. This keeps file references consistent in session output and reviews.

- Allowed forms: `path`, `path:line`, `path:line:column`
- Example: `crates/agentty/src/infra/agent/backend.rs:151`
- Not allowed: absolute paths, `file://` URIs, or `../`-prefixed paths

## Structured Response Protocol

<a id="backends-structured-response-protocol"></a>
Agent responses should be a single JSON object with a `messages` array, where
each entry has:

- `type`: `answer` or `question`
- `text`: markdown text payload
- `options` (optional): array of predefined answer strings (only for `question` type)

Session discussion turns add one extra prompt contract on top of the JSON
envelope:

- every discussion turn must include at least one `answer` message that ends
  with a `## Change Summary` section
- `## Change Summary` must contain `### Current Turn` (what changed in this
  turn only) and `### Session Changes` (the cumulative session-branch diff)

One-shot utility prompts, such as title generation, session commit-message
generation, focused review preparation, auto-commit assistance, and rebase
conflict assistance, still return the same protocol JSON shape but skip the
change-summary footer so the answer text stays usable as a machine-readable
artifact.

Example payload:

```json
{
  "messages": [
    {
      "type": "answer",
      "text": "Implemented the change.\n\n## Change Summary\n### Current Turn\n- Updated the protocol prompt templates.\n### Session Changes\n- Added mandatory per-turn change summaries to the structured response contract."
    },
    { "type": "question", "text": "Should I run the full test suite?", "options": ["Yes", "No", "Only changed files"] }
  ]
}
```

<a id="backends-structured-response-routing"></a>
`answer` messages are appended to the normal session transcript. `question`
messages are persisted separately and move the session to **Question** status
so Agentty can collect clarifications in question input mode.

## Protocol Validation and Repair

<a id="backends-protocol-validation-repair"></a>
Agentty validates final agent output against the structured response protocol.

- Claude and Gemini integrations use strict parsing and run one automatic
  repair retry loop (up to three repair turns) when output does not match the
  protocol schema.
- One-shot utility prompts use the same repair loop for all backends so
  internal callers always receive valid protocol JSON before parsing titles,
  commit messages, or review text.
- Claude turns use native schema validation via `claude --json-schema` and
  `--output-format json` (no Claude `stream-json` mode).
- Claude turns pass `--strict-mcp-config`, so only MCP servers explicitly
  provided by Agentty are allowed (none by default).
- Claude turns allow file-modifying tools (`Edit`, `MultiEdit`, `Write`) plus
  `Bash`, `EnterPlanMode`, and `ExitPlanMode` for unattended worktree edits.
- Codex app-server turns enforce structured output through transport
  `outputSchema`; Codex prompts do not prepend schema text.
- Claude always uses structured protocol output, including isolated one-shot
  utility prompts, through native schema enforcement plus prompt instructions.
- Codex app-server turns include `outputSchema` at transport level and then use
  permissive final parsing fallback so non-schema text is still visible if
  needed.
- Partial protocol JSON fragments are suppressed during streaming so raw JSON
  wrappers do not leak into live transcript output.

## Session Resume Behavior

<a id="backends-session-resume"></a>
Agentty persists provider-native conversation identifiers for app-server
backends and uses them to restore context after runtime restarts.

- Codex app-server: resumes by stored `threadId` via `thread/resume`.
- Gemini ACP: currently creates a fresh ACP `session/new` on runtime restart,
  so Agentty falls back to transcript replay when needed.

## Codex Turn Timeout

<a id="backends-codex-turn-timeout"></a>
Codex app-server turns can run for a long time. Agentty waits up to 4 hours
for Codex `turn/completed` by default.

## Selecting a Backend

<a id="backends-selecting-a-backend"></a>
Choose the backend from the `/model` picker:

```bash
# Open model selection (backend first, then model)
/model
```

<a id="backends-persistent-defaults"></a>
For persistent defaults, choose a default model in the **Settings** tab
(`Tab` to navigate, `Enter` to edit). The selected model determines which
backend is used for new sessions.

<a id="backends-codex-reasoning-level"></a>
For Codex sessions, the **Settings** tab also exposes `Reasoning Level`
(`low`, `medium`, `high`, `xhigh`). The selected level is persisted and sent
with Codex turns.

## Available Models

<a id="backends-available-models"></a>
Each backend offers multiple models with different trade-offs between speed,
quality, and cost.

### Gemini Models

| Model ID | Description | Default |
|----------|-------------|---------|
| `gemini-3-flash-preview` | Fast Gemini model for quick iterations. | |
| `gemini-3.1-pro-preview` | Higher-quality Gemini model for deeper reasoning. | Yes |

### Claude Models

| Model ID | Description | Default |
|----------|-------------|---------|
| `claude-opus-4-6` | Top-tier Claude model for complex tasks. | Yes |
| `claude-sonnet-4-6` | Balanced Claude model for quality and latency. | |
| `claude-haiku-4-5-20251001` | Fast Claude model for lighter tasks. | |

### Codex Models

| Model ID | Description | Default |
|----------|-------------|---------|
| `gpt-5.4` | Latest Codex model for coding quality. | Yes |
| `gpt-5.3-codex` | Previous Codex model for coding quality. | |
| `gpt-5.3-codex-spark` | Codex spark model for quick coding iterations. | |

## Switching Models

<a id="backends-switching-models"></a>
You can switch the model for the current session using the `/model` slash
command in the prompt input. This opens a two-step picker: first choose the
backend, then choose one of its models.

<a id="backends-switching-default-model"></a>
To change the **default model** persistently, use the **Settings** tab
(`Tab` to navigate to it, `Enter` to edit).
