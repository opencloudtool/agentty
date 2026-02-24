# Contributing

Thanks for contributing to Agentty.

## Quickstart

```sh
git clone <repo-url>
cd agentty
cargo run # Builds and runs the 'agentty' binary
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
cargo test
cargo clippy -- -D warnings
cargo fmt --all -- --check
cargo shear
```
