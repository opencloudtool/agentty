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
- Sessions are stored in `~/.ag-harness/db/harness.db`. Set `AG_HARNESS_ROOT`, or pass
  `--database <FILE>`, to choose another location. If `HOME` is unavailable, one of
  those explicit locations is required.
- The first valid Git in `PATH` is used; `--git-executable <FILE>` overrides it.
- Muse is the default provider. Run `cargo run --locked -p ag-harness-cli -- run --help`
  for Kimi, Qwen, model, and credential options.
- CLI chats default to low model reasoning to reduce latency; pass
  `--reasoning-effort <LEVEL>` to select deeper reasoning. Direct `ag-harness` library
  users retain provider defaults unless they configure `Harness::model_reasoning_effort`
  or `ModelRequest::with_model_reasoning_effort`.
