# Contributing

Thanks for contributing to Agentty.

## Quickstart

```sh
git clone <repo-url>
cd agentty
cargo run # Builds and runs the 'agentty' binary
```

## Tooling Setup

### Install `uv`

Install `uv` using the official instructions:
https://docs.astral.sh/uv/getting-started/installation/

```sh
curl -LsSf https://astral.sh/uv/install.sh | sh
```

### Install `pre-commit`

```sh
uv tool install pre-commit
```

### Install `cargo-llvm-cov`

```sh
cargo install cargo-llvm-cov
```

## Website

`agentty.xyz` is a Zola site stored in `docs/site/` and deployed through GitHub Pages.

```sh
# Preview locally
zola serve --root docs/site

# Build static output
zola build --root docs/site
```

## Development Checks

Run the following checks before opening a pull request:

```sh
pre-commit run rustfmt-fix --all-files --hook-stage manual
pre-commit run clippy-fix --all-files --hook-stage manual
pre-commit run --all-files
cargo test -q -- --test-threads=1
```

`pre-commit run --all-files` now includes the workspace coverage ratchet via
`cargo llvm-cov --workspace --summary-only --fail-under-lines 87 --fail-under-functions 85`.

## Architecture Documentation

If your PR changes module boundaries, cross-layer control flow, trait-based external boundaries, or workspace crate ownership, update:

- `docs/site/content/docs/contributing/design-architecture.md`

See the [Design & Architecture](/docs/contributing/design-architecture/) page for the full architecture map and change-path recipes.
