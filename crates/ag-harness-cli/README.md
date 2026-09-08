# `ag-harness-cli`

Chat with models through a repository-aware, durable terminal harness.

## Get started

```sh
export MODEL_API_KEY="your-key"
cargo run --locked -p ag-harness-cli -- run muse-spark-1.3
```

This starts an interactive chat with read-only access to the current directory.

## Common commands

```sh
# Start a named session
cargo run --locked -p ag-harness-cli -- run muse-spark-1.3 --session review-42

# Resume it later
cargo run --locked -p ag-harness-cli -- resume review-42

# Allow repository writes
cargo run --locked -p ag-harness-cli -- run muse-spark-1.3 --allow-write

# Chat about another repository
cargo run --locked -p ag-harness-cli -- \
  run muse-spark-1.3 --read-dir /path/to/repository
```

## Defaults

- Repository writes are disabled unless `--allow-write` is set.
- Sessions are stored in `~/.ag-harness/db/harness.db`.
- The first valid Git in `PATH` is used; `--git-executable <FILE>` overrides it.
- Muse is the default provider. Run `cargo run --locked -p ag-harness-cli -- run --help`
  for Kimi, Qwen, model, and credential options.
