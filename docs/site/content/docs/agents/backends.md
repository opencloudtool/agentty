+++
title = "Agents & Models"
description = "Supported agent backends, available models, and how to configure them."
weight = 1
+++

<a id="backends-introduction"></a> Agentty delegates coding work to external AI agent
CLIs. Each backend is a standalone CLI tool that Agentty launches in an isolated
worktree. This page covers the supported backends, available models, and configuration
options.

<!-- more -->

## Supported Backends

<a id="backends-supported-backends"></a> Agentty supports four agent backends. Each
requires its respective CLI to be installed and available on your `PATH`.

- Codex (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- Claude (`claude`): install [Claude Code](https://github.com/anthropics/claude-code),
  then run `claude auth login`.
- Antigravity (`agy` 1.1.18 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow. Agentty excludes older versions from provider
  selection and reports `agy update` as the recovery step if a session encounters an
  outdated executable.
- Gemini (`gemini`): install the
  [Gemini CLI](https://github.com/google-gemini/gemini-cli) and authenticate with an API
  key or Vertex AI.

All backends accept pasted local prompt images from the Agentty composer (`Ctrl+V`,
`Ctrl+Shift+V`, or `Alt+V` in prompt mode) and run their turns non-interactively inside
the session worktree.

Codex `Auto Edit` turns run with full command access so browser tests, local services,
and other tools that cannot run inside the provider sandbox remain available. This also
means Codex commands are not confined to the session worktree; review the session diff
and use `Read Only` for inspection-only work.

Claude turns also allow Claude Code's `WebSearch` and `WebFetch` tools, so prompts that
need current external information can use the web without an interactive permission
grant. Claude `Auto Edit` retains Claude Code's unsandboxed command fallback for tools
that cannot run inside its sandbox.

Treat fetched web content as untrusted context. Claude still has edit-capable tools
during the turn, so keep web-backed prompts specific and review the session diff before
merging.

Agentty requires at least one supported backend CLI on `PATH` at startup and fails with
an install hint when none is found.

Agentty uses each provider's official non-interactive CLI or app-server surface
(`claude -p`, `agy --input-format stream-json`, `codex app-server`, or `gemini --acp`)
after you authenticate with that provider's CLI. It does not implement OAuth flows, read
provider OAuth tokens directly, or call private provider APIs. You are responsible for
choosing an authentication method permitted for your account, plan, and usage pattern.

## Subagent Limits

Agentty requests a limit of two concurrent subagents per Codex or Claude session,
excluding the parent agent. It applies the limit when starting the provider process,
including when resuming a saved session. Restart Agentty after updating to apply the
limit to existing sessions.

Codex limits concurrently open child-agent threads. Claude limits ordinary subagent
spawning, but provider exceptions such as resumed subagents can exceed the limit.
Enforcement depends on the installed CLI supporting its native setting. Gemini and
Antigravity have no verified concurrency setting, so Agentty does not cap their internal
subagents.

These are per-session provider limits, not host-wide CPU or memory limits. Multiple
sessions each have their own allowance, and builds or tests can consume additional
resources.

## Authentication and Usage

### Codex

<a id="backends-codex-authentication"></a> Codex is the recommended backend when you
want subscription-backed usage. The CLI supports signing in with ChatGPT through
`codex login`, and Agentty uses the supported `codex app-server` integration surface.
Usage remains subject to the
[OpenAI Terms of Use](https://openai.com/policies/terms-of-use/).

### Claude

<a id="backends-claude-authentication"></a> For Agentty usage through `claude -p`, use
API-key authentication through Claude Console or a supported cloud provider instead of a
Claude Free, Pro, or Max subscription sign-in. Anthropic's
[Claude Code legal and compliance documentation](https://code.claude.com/docs/en/legal-and-compliance)
describes subscription OAuth as intended for Claude Code and native Anthropic
applications, while developer integrations should use API keys or supported cloud
providers.

Claude turns use the CLI's schema-validated structured result for the final chat
response. Tool-use events remain transient progress updates and are replaced by the
validated answer when the turn completes.

If Claude session turns or utility prompts fail with `authentication_error`,
`Failed to authenticate`, or `OAuth token has expired`, refresh the CLI session and
retry:

```bash
claude auth login
claude auth status
```

### Antigravity

<a id="backends-antigravity-authentication"></a> For Agentty usage through `agy`
headless mode, prefer authentication backed by a Google Cloud project or API key rather
than Google Account subscription sign-in. The
[Antigravity terms](https://antigravity.google/terms) do not currently explain how
subscription access applies when third-party tools invoke headless sessions.

Agentty starts `agy` with `--input-format stream-json` and sends each prompt as an
NDJSON user event over standard input. The process remains active between turns, so
Antigravity retains native conversation context and performs its own context compaction
instead of receiving a replayed transcript on every follow-up. Agentty persists the
native conversation ID and resumes it after a process or application restart. Prompts
are no longer constrained by an operating-system command-argument limit.

### Gemini

<a id="backends-gemini-authentication"></a> Google Account OAuth no longer works for
Gemini CLI after Google's
[transition from Gemini CLI to Antigravity CLI](https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/).
Use `GEMINI_API_KEY` or Vertex AI authentication, or choose the Antigravity backend
instead.

## Project Instruction Files

<a id="backends-project-instruction-files"></a> Agentty relies on each backend's native
project-instruction discovery instead of inlining repository guidance into prompts.

- Codex loads `AGENTS.md`.
- Claude Code loads `CLAUDE.md`.
- Gemini CLI loads `GEMINI.md`.
- Antigravity CLI loads `AGENTS.md` and `GEMINI.md` from the active workspace.

Keeping `CLAUDE.md` and `GEMINI.md` as symlinks to a canonical `AGENTS.md` gives all
backends the same repository guidance.

## Selecting a Backend

<a id="backends-selecting-a-backend"></a> Choose the backend from the `/model` picker:

```bash
# Open model selection (backend first, then model)
/model
```

The picker is filtered to the backend CLIs currently available on the machine. If only
`agy` is installed, `/model` shows only Antigravity and its selectable Gemini model
choices.

At startup, Agentty refreshes each available agent CLI in the background, then probes
`--version` and updates the Projects tab's **Agent CLIs** rows with the current version.
Antigravity, Claude, and Codex use their native `update` commands. Because current
Gemini CLI releases do not expose that command, npm-global Gemini installations are
refreshed with `npm install -g @google/gemini-cli@latest`. Rows show `updating...` until
the refresh completes. Gemini installations that Agentty cannot identify as npm-global
are version-probed without an automatic update. Antigravity setup and turns reuse the
validated discovery result instead of running `agy --version` on the async session path.
Replacing or modifying the `agy` executable invalidates that result and asks you to wait
for discovery or restart Agentty before retrying.

<a id="backends-persistent-defaults"></a> For persistent defaults, configure each Smart,
Fast, and Review role in the **Settings** tab (`Tab` to navigate, `Enter` to open the
selector). Choose the `agent/model` first and press `Enter`, then choose its reasoning
level. Claude and Codex selections continue to a response-speed picker with `Normal` and
`Fast`; Gemini and Antigravity save after reasoning. Each role's model, reasoning, and
speed defaults are stored per project. Keeping the backend in the selection ensures
shared Gemini model ids remain tied to the selected Gemini or Antigravity provider.
Stored defaults that point at an unavailable backend fall back to the first available
backend default.

The separate `Default Response Style` setting applies to every backend and initializes
new sessions as `Concise`, `Balanced`, or `Detailed`. Existing sessions retain their
stored style.

<a id="backends-reasoning-level"></a> Smart reasoning becomes the default for new
sessions, Fast reasoning is used for title and commit-message utility prompts, and
Review reasoning is used for focused review assists. A session-specific `/reasoning`
override still takes precedence for that session's turns. Antigravity receives
`--effort low`, `--effort medium`, or `--effort high`; `xhigh` and `max` map to its
highest supported value, `--effort high`. Codex receives `max` as a distinct reasoning
effort. For Claude, both `xhigh` and `max` map to `--effort max`, which is currently
only supported by `claude-opus-5`.

Smart speed becomes the default for new sessions, Fast speed is used for title and
commit-message utility prompts, and Review speed is used for focused review assists.
Fast is available only for Claude and Codex. It applies the same compatible-model
adjustment as `/speed`: Claude uses `claude-opus-5`, and Codex Spark uses `gpt-5.6-sol`.

## Available Models

<a id="backends-available-models"></a> Each backend exposes one or more selectable model
entries with different trade-offs between speed, quality, and cost.

### Antigravity and Gemini Models

Both providers share the same Gemini model ids:

- `gemini-3.1-pro-preview` (default): Higher-quality Gemini model for deeper reasoning.
- `gemini-3.8-flash`: Fast Gemini model for agentic and multimodal tasks.
- `gemini-3.5-flash-lite`: Lightweight Gemini model for fast, cost-conscious workloads.

### Claude Models

- `claude-fable-5` (default): Claude Fable model for creative, narrative-heavy tasks.
- `claude-opus-5`: Latest Claude Opus model for complex tasks.
- `claude-sonnet-5`: Balanced Claude model for quality and latency.
- `claude-haiku-4-5-20251001`: Fast Claude model for lighter tasks.

### Codex Models

- `gpt-6-astra`: Most capable Codex model for the hardest end-to-end work.
- `gpt-5.6-sol` (default): Flagship Codex model for complex professional work.
- `gpt-5.6-terra`: Current Codex model for balanced coding performance.
- `gpt-5.6-luna`: Current Codex model for lighter coding iterations.
- `gpt-5.3-codex-spark`: Codex spark model for quick coding iterations.

### Stored Model Upgrades

Model pickers show only the current models listed above. When a stored project default
or active session references a superseded model, Agentty upgrades and persists its
replacement automatically. Finished sessions preserve their historical model data.

## Switching Models

<a id="backends-switching-models"></a> You can switch the model for the current session
using the `/model` slash command in the prompt input. This opens a two-step picker:
first choose the backend, then choose one of its models. Both steps are filtered to
locally available backends.

You can also switch the reasoning level for the current session with the `/reasoning`
slash command. The picker preselects the current effective reasoning level.

Use `/style` with any backend to choose concise, balanced, or detailed answers for the
current session. Agentty persists the preference and adds provider-neutral guidance to
each interactive turn. Explicit user instructions about length or format take
precedence, and the guidance never replaces required protocol fields, safety details, or
verification. One-shot utility prompts are unchanged.

Claude and Codex sessions also expose `/speed`. Choose Normal for standard provider
routing or Fast for lower latency at higher provider cost. Agentty persists the choice
per session and displays it after reasoning in the session header and beside the prompt
title. Gemini and Antigravity sessions have no speed control, so they show neither the
command nor the speed display. Claude Fast uses the noninteractive `fastMode` setting
and requires `claude-opus-5`, so Agentty switches other Claude models to Opus 5 when
Fast is enabled. Codex Fast uses the app-server `fast` service tier; Agentty switches
`gpt-5.3-codex-spark` to `gpt-5.6-sol` first. Selecting Normal restores the provider's
standard tier without reverting that model change. These automatic compatibility
switches do not change the project's default model. Selecting a model that cannot use
Fast resets the session to Normal before switching. See the provider guides for
[Codex fast mode](https://learn.chatgpt.com/docs/agent-configuration/speed) and
[Claude Code fast mode](https://code.claude.com/docs/en/fast-mode).

<a id="backends-switching-default-model"></a> To change the **default model**
persistently, use the **Settings** tab (`Tab` to navigate to it, `Enter` to open the
selector).
