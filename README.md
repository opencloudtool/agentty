<h1 align="center">Agentty</h1>

<p align="center">
  <img alt="NPM Version" src="https://img.shields.io/npm/v/agentty" />
  <a href="https://codecov.io/gh/agentty-xyz/agentty"><img alt="codecov" src="https://codecov.io/gh/agentty-xyz/agentty/graph/badge.svg?token=YRGKGTM0HP" /></a>
  <a href="https://github.com/agentty-xyz/agentty/actions/workflows/postsubmit.yml"><img alt="Postsubmit" src="https://github.com/agentty-xyz/agentty/actions/workflows/postsubmit.yml/badge.svg?branch=main" /></a>
</p>

<p align="center"><em>Agentic Development Environment for structured, controllable AI-assisted software development.</em></p>

<p align="center">
  <img src="docs/site/static/demo/demo.gif" alt="Agentty demo" width="900" />
</p>

Built with Rust and [Ratatui](https://ratatui.rs), and refined through its own day-to-day use, Agentty brings agents, review, and iteration into one focused terminal workflow.

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

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for `prek`-based development checks and contribution guidance.

## License

Apache-2.0
