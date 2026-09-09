<div align="center">

<h1>Agentty ADE</h1>

![NPM Version](https://img.shields.io/npm/v/agentty)
[![codecov](https://codecov.io/gh/agentty-xyz/agentty/graph/badge.svg?token=YRGKGTM0HP)](https://codecov.io/gh/agentty-xyz/agentty)
[![Postsubmit](https://github.com/agentty-xyz/agentty/actions/workflows/postsubmit.yml/badge.svg?branch=main)](https://github.com/agentty-xyz/agentty/actions/workflows/postsubmit.yml)

[![Quality gate status](https://sonarcloud.io/api/project_badges/measure?project=agentty-xyz_agentty&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=agentty-xyz_agentty)
[![Bugs](https://sonarcloud.io/api/project_badges/measure?project=agentty-xyz_agentty&metric=bugs)](https://sonarcloud.io/summary/new_code?id=agentty-xyz_agentty)
[![Code Smells](https://sonarcloud.io/api/project_badges/measure?project=agentty-xyz_agentty&metric=code_smells)](https://sonarcloud.io/summary/new_code?id=agentty-xyz_agentty)
[![Duplicated Lines (%)](https://sonarcloud.io/api/project_badges/measure?project=agentty-xyz_agentty&metric=duplicated_lines_density)](https://sonarcloud.io/summary/new_code?id=agentty-xyz_agentty)
[![Coverage](https://sonarcloud.io/api/project_badges/measure?project=agentty-xyz_agentty&metric=coverage)](https://sonarcloud.io/summary/new_code?id=agentty-xyz_agentty)

Agentty is an **ADE (Agentic Development Environment) for structured, controllable
AI-assisted software development**. Built with Rust and [Ratatui](https://ratatui.rs),
and refined through its own day-to-day use, it brings agents, review, and iteration into
one focused terminal workflow.

[Installation](#installation) · [Supported CLI Agents](#supported-cli-agents) ·
[Usage](#usage) · [Documentation](#documentation) · [Contributing](#contributing) ·
[License](#license)

<img src="docs/site/static/demo/demo.gif" alt="Agentty demo" width="900" />

</div>

______________________________________________________________________

## Installation

### npm (recommended, supports auto-update)

```sh
npm install -g agentty
```

### Other methods

<details>
<summary>npx (run without installing)</summary>

```sh
npx agentty
```

</details>

<details>
<summary>Shell</summary>

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/agentty-xyz/agentty/releases/latest/download/agentty-installer.sh | sh
```

</details>

<details>
<summary>Cargo</summary>

macOS source builds require Xcode Command Line Tools for native process bindings.

```sh
cargo install agentty
```

</details>

## Supported CLI Agents

Agentty currently supports Codex, Claude, Antigravity, and Gemini. Install and
authenticate at least one provider CLI before starting a session:

- Codex (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex) and run `codex login`.

> [!NOTE]
>
> Codex is the recommended CLI agent for Agentty when you want subscription-backed
> usage. Codex supports signing in with ChatGPT for subscription access, including in
> the CLI, and Agentty uses the Codex `app-server` integration surface. Usage remains
> subject to the [OpenAI Terms of Use](https://openai.com/policies/terms-of-use/).
> OpenClaw documents ChatGPT/Codex subscription sign-in as the normal
> [Codex harness](https://docs.openclaw.ai/plugins/codex-harness) path, and opencode
> recommends ChatGPT Plus or Pro authentication for its
> [OpenAI provider](https://opencode.ai/docs/providers/#openai).

- Claude (`claude`): install [Claude Code](https://github.com/anthropics/claude-code)
  and run `claude auth login`.

> [!WARNING]
>
> For Agentty usage through `claude -p`, use API key authentication through Claude
> Console or a supported cloud provider instead of a Claude Free, Pro, or Max
> subscription sign-in. Anthropic's
> [Claude Code legal and compliance docs](https://code.claude.com/docs/en/legal-and-compliance)
> describe subscription OAuth as intended for ordinary use of Claude Code and native
> Anthropic applications, while developer integrations should use API keys or supported
> cloud providers. Theo's
> [explanation video](https://www.youtube.com/watch?v=RIkSlHgQYog) discusses the same
> uncertainty, but commentary does not guarantee that subscription-backed usage is safe
> for third-party tool invocation.

- Antigravity (`agy`): install version 1.1.18 or newer of the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli) and run `agy`
  to sign in when prompted.

> [!WARNING]
>
> For Agentty usage through `agy` headless mode, use authentication backed by a Google
> Cloud project or an API key when available for your Antigravity setup, rather than a
> Google Account subscription sign-in. The
> [Antigravity terms](https://antigravity.google/terms) do not yet clearly describe how
> subscription access applies when `agy` headless mode is invoked by third-party tools.

- Gemini (`gemini`): install the
  [Gemini CLI](https://github.com/google-gemini/gemini-cli) and authenticate with an API
  key or Vertex AI.

> [!WARNING]
>
> "Sign in with Google" (Google Account OAuth) no longer works for the Gemini CLI after
> Google's
> [transition of the Gemini CLI to the Antigravity CLI](https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/).
> API key (`GEMINI_API_KEY`) and Vertex AI authentication still work, so use one of
> those for the Gemini backend, or use the Antigravity (`agy`) backend instead.

Agentty uses each provider's official non-interactive CLI or app-server surface
(`claude -p`, `agy --input-format stream-json`, `codex app-server`, or `gemini --acp`)
after you authenticate with that provider's CLI. It does not implement OAuth flows, read
provider OAuth tokens directly, or call private provider APIs. You are responsible for
choosing an authentication method that is permitted for your account, plan, and usage
pattern.

## Usage

```sh
agentty              # Launch with auto-update enabled (default)
agentty --no-update  # Launch without automatic updates
agentty --help       # Show supported command-line options
agentty --version    # Show the installed Agentty version
```

Only one Agentty instance can use a given Agentty root at a time. Close the running
instance before launching another. Separate `AGENTTY_ROOT` directories can run
independently.

From the **Sessions** tab, create an `Orchestrator` session for broad goals. It can run
temporary read-only research waves to map architecture, review risks, or answer design
questions before proposing implementation workers. Research reports are verified by the
controller. Agentty selects each provider's native read-only or plan policy, denies
mutation permission requests, and archives any unexpected temporary diff for inspection
before discarding it without integration.

## Documentation

Documentation for installation and workflows is available at
[agentty.xyz/docs](https://agentty.xyz/docs/).

> [!WARNING]
>
> Agentty is in active development. While releases follow Semantic Versioning, the
> current `0.y.z` series may still introduce breaking changes between releases as
> workflows, integrations, and safeguards evolve. Always review and verify the changes
> Agentty proposes or applies in your repositories before you rely on them.

The workspace includes reusable session contracts (`ag-session`), persistence
(`ag-store`), and campaign coordination (`ag-orchestration`). Campaign hosts supply
session execution, notifications, and reconciliation scheduling independently of the
TUI.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidance.

## License

Apache-2.0
