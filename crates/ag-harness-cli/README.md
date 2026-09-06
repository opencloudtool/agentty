# `ag-harness-cli`

Start a durable session:

```sh
MODEL_API_KEY=your-key cargo run -p ag-harness-cli -- \
  run muse-spark-1.3 --session review-42
```

Resume it later:

```sh
MODEL_API_KEY=your-key cargo run -p ag-harness-cli -- \
  resume review-42
```

The current directory is readable by default. Add `--allow-write` to permit patches or
`--read-dir <DIR>` to choose another repository root. Select Kimi or Qwen with
`--provider`; `--base-url` overrides the provider endpoint.

Session history defaults to `~/.ag-harness/db/harness.db`. Set `AG_HARNESS_ROOT`, or
pass `--database <FILE>`, to choose another location. If `HOME` is unavailable, one of
those explicit locations is required. Run `ag-harness --help` for the complete provider
and credential list.
