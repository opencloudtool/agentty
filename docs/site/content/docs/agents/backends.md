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

All three session backends accept pasted local prompt images from the Agentty
composer (`Ctrl+V` or `Alt+V` in prompt mode). Transport details differ by
backend:

- Codex app-server turns send `localImage` input items in placeholder order.
- Gemini ACP turns send ordered `text` and `image` ACP content blocks.
- Claude Code turns receive the prompt over stdin with `[Image #n]`
  placeholders rewritten to local image paths that Claude can inspect.

Codex now always runs through `codex app-server`, including isolated utility
prompts such as title generation, review assist, commit-message generation,
auto-commit recovery, and rebase-conflict assistance. Agentty no longer uses a
direct `codex exec` path.

## Project Instruction Files

<a id="backends-project-instruction-files"></a>
Agentty relies on each backend's native project-instruction discovery instead
of inlining repository guidance into prompts.

- Codex loads `AGENTS.md`.
- Claude Code loads `CLAUDE.md`.
- Gemini CLI loads `GEMINI.md`.

This repository keeps `CLAUDE.md` and `GEMINI.md` as symlinks to the canonical
root `AGENTS.md`, so all three backends see the same project instructions when
they run in the session worktree.

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
The rule is carried by the shared prompt preamble in
`crates/agentty/src/infra/agent/template/protocol_instruction_prompt.md`.

- Allowed forms: `path`, `path:line`, `path:line:column`
- Example: `crates/agentty/src/infra/agent/prompt.rs:48`
- Not allowed: absolute paths, `file://` URIs, or `../`-prefixed paths

## Structured Response Protocol

<a id="backends-structured-response-protocol"></a>
Agentty prepends one shared protocol preamble from
`crates/agentty/src/infra/agent/template/protocol_instruction_prompt.md`. That template
contains the repository-root-relative file path rules, the structured response
instructions, the explicit `---` separator that separates the task body, and
the full self-descriptive JSON Schema generated from the protocol subsystem in
`crates/agentty/src/infra/agent/protocol.rs`. The router delegates to
`protocol/model.rs`, `protocol/schema.rs`, and `protocol/parse.rs`, while
`crates/agentty/src/infra/agent/prompt.rs` owns the shared prompt-preparation
path used by CLI and app-server turns.

Each request path now selects one canonical `AgentRequestKind` before the
backend sees the prompt, and the backend derives the protocol-owned
`ProtocolRequestProfile` from that request kind:

- Session turns use `AgentRequestKind::SessionStart` or
  `AgentRequestKind::SessionResume`, which both derive the `SessionTurn`
  profile.
- One-shot utility prompts use `AgentRequestKind::UtilityPrompt`, which
  derives the `UtilityPrompt` profile.
- Strict and permissive request paths still share the same protocol contract
  after that derivation step.

The shared schema defines a top-level `answer` markdown string, a `questions`
array, and the optional top-level `summary` object. Session turns typically
populate:

- `summary.turn` describes only the work completed in the current turn
- `summary.session` describes the cumulative session-branch diff that still
  applies

One-shot utility prompts, such as title generation, session commit-message
generation, focused review preparation, auto-commit assistance, and rebase
conflict assistance, still return the same protocol JSON shape. They may leave
`summary` unused, while session discussion turns typically populate it.
Final parsing accepts any payload that deserializes to the shared protocol wire
type, so session-turn responses can carry meaning in `summary` even when
`answer` is blank and `questions` is empty.

Example payload:

```json
{
  "answer": "Implemented the change.",
  "questions": [
    {
      "text": "Should I run the full test suite?",
      "options": ["Yes", "No", "Only changed files"]
    }
  ],
  "summary": {
    "turn": "- Updated the protocol prompt templates.",
    "session": "- Added mandatory structured summaries to the response contract."
  }
}
```

<a id="backends-structured-response-routing"></a>
Top-level `answer` text is appended to the normal session transcript.
Structured `questions` are persisted separately and move the session to
**Question** status so Agentty can collect clarifications in question input
mode. The top-level `summary` object is persisted separately and rendered in
the session summary panel instead of being parsed back out of answer markdown.

## Protocol Validation

<a id="backends-protocol-validation-repair"></a>
Agentty validates final agent output against the structured response protocol.

- Claude, Gemini, and Codex session turns use strict parsing and fail closed
  when output does not match the protocol schema.
- Strict parsing accepts summary-only protocol payloads because the parser now
  relies on the shared protocol wire type instead of extra top-level field
  checks.
- One-shot utility prompts use the same strict final validation across both
  CLI and app-server transports. Plain text, blank responses, trailing junk
  after a schema object, and other non-schema output are rejected instead of
  being coerced into `answer`. Provider prose that appears before one final
  protocol JSON object is now tolerated so Claude-style wrapped completions
  still recover the authoritative payload.
- When strict validation fails, the surfaced error now includes parse-oriented
  debug details such as response sizing, JSON parser location/category, and
  visible top-level keys from any parsed JSON object so malformed provider
  output is easier to diagnose.
- Provider-specific transport, stdin-vs-argv prompt delivery, strict final
  parsing, and app-server thought-phase handling are centralized in the
  shared provider descriptor in `crates/agentty/src/infra/agent/backend.rs`.
- Concrete backends in `crates/agentty/src/infra/agent/` now also own app-server
  client selection and runtime command construction, so Codex and Gemini
  transport wiring stays with their provider-specific implementations instead
  of top-level `infra/` modules.
- Claude turns use native schema validation via `claude --json-schema` and
  `--output-format stream-json`, so tool/progress events can stream live while
  the final response remains schema-validated.
- Prompt-side protocol instructions rely on the raw self-descriptive
  `schemars` metadata (`title`, `description`, and related annotations),
  while transport `outputSchema` payloads are normalized separately for
  provider compatibility. The same prompt instructions also restrict any git
  usage during session turns to read-only commands such as `git diff` and
  `git show`, and explicitly forbid mutating operations such as `git commit`
  or `git push`.
- Claude and Gemini stream the rendered prompt body through stdin for CLI
  one-shot flows so large diffs and review prompts do not hit OS argv length
  limits.
- Claude turns pass `--strict-mcp-config`, so only MCP servers explicitly
  provided by Agentty are allowed (none by default).
- Claude turns allow file-modifying tools (`Edit`, `MultiEdit`, `Write`) plus
  `Bash`, `EnterPlanMode`, and `ExitPlanMode` for unattended worktree edits.
- Codex app-server turns enforce structured output through transport
  `outputSchema`; the same transport is also used for one-shot Codex utility
  prompts, and prompt instructions embed the same full self-descriptive schema
  for consistency across providers.
- Claude always uses structured protocol output, including isolated one-shot
  utility prompts, through native schema enforcement plus prompt instructions.
- Codex app-server turns include `outputSchema` at transport level and still
  require the final assistant payload itself to parse as the shared protocol
  JSON object.
- Partial protocol JSON fragments are suppressed during streaming so raw JSON
  wrappers do not leak into live transcript output.
- Wrapped stream chunks that end in one valid protocol JSON object are reduced
  to that payload's `answer`, so prefatory provider prose is not persisted
  when recovery succeeds.
- Gemini ACP final turn assembly now prefers the completed `session/prompt`
  payload when it contains valid protocol JSON and the earlier streamed chunk
  accumulation does not, so strict validation sees the authoritative
  structured response.

## Session Resume Behavior

<a id="backends-session-resume"></a>
Agentty persists provider-native conversation identifiers for app-server
backends and uses them to restore context after runtime restarts.

- Codex app-server: resumes by stored `threadId` via `thread/resume`.
- Gemini ACP: currently creates a fresh ACP `session/new` on runtime restart,
  so Agentty falls back to transcript replay when needed.

## App-Server Turn Timeout

<a id="backends-app-server-turn-timeout"></a>
App-server-backed turns can run for a long time. Agentty waits up to 4 hours
for turn completion by default for both Codex app-server and Gemini ACP.

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

<a id="backends-reasoning-level"></a>
For Codex and Claude sessions, the **Settings** tab also exposes `Reasoning Level`
(`low`, `medium`, `high`, `xhigh`). The selected level is persisted and sent
with Codex and Claude turns.

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
