# Agentty

![NPM Version](https://img.shields.io/npm/v/agentty) [![codecov](https://codecov.io/gh/agentty-xyz/agentty/graph/badge.svg?token=YRGKGTM0HP)](https://codecov.io/gh/agentty-xyz/agentty) [![Postsubmit](https://github.com/agentty-xyz/agentty/actions/workflows/postsubmit.yml/badge.svg?branch=main)](https://github.com/agentty-xyz/agentty/actions/workflows/postsubmit.yml)

Agentty is an **ADE (Agentic Development Environment) for structured, controllable AI-assisted software development**. Built with Rust and [Ratatui](https://ratatui.rs), and refined through its own day-to-day use, it brings agents, review, and iteration into one focused terminal workflow.

<p align="center">
  <img src="docs/site/static/demo/demo.gif" alt="Agentty demo" width="900" />
</p>

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

```sh
cargo install agentty
```

</details>

## Usage

```sh
agentty              # Launch with auto-update enabled (default)
agentty --no-update  # Launch without automatic updates
```

## Documentation

Documentation for installation and workflows is available at [agentty.xyz/docs](https://agentty.xyz/docs/).

> [!WARNING]
> Agentty is in active development. While releases follow Semantic Versioning, the
> current `0.y.z` series may still introduce breaking changes between releases as
> workflows, integrations, and safeguards evolve. Always review and verify the
> changes Agentty proposes or applies in your repositories before you rely on
> them.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for `prek`-based development checks and contribution guidance.

## License

Apache-2.0
