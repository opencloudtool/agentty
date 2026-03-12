# Agentty

![NPM Version](https://img.shields.io/npm/v/agentty)

Agentty is an **ADE (Agentic Development Environment) for structured, controllable AI-assisted software development**. It is itself developed with Agentty, built with Rust and [Ratatui](https://ratatui.rs), and designed around a deeply integrated workflow.

Session view includes a manual branch-publish workflow so reviewed session
branches can be pushed from the TUI before you open a pull request or merge
request yourself.

Prompt mode also supports pasted clipboard images for Codex, Gemini, and Claude
sessions: press `Ctrl+V` or `Alt+V` while composing the first prompt or a
reply to insert an inline `[Image #n]` placeholder backed by a local temp PNG
upload.

## Installation

### Shell

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/agentty-xyz/agentty/releases/latest/download/agentty-installer.sh | sh
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

## Documentation

Documentation for installation and workflows is available at [agentty.xyz/docs](https://agentty.xyz/docs/).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development checks and contribution guidance.

## License

Apache-2.0
