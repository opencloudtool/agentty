# `ag-harness`

`ag-harness` runs structured LLM turns with explicit repository permissions and durable
SQLite sessions.

## Durable sessions

```rust
use ag_harness::{Harness, Muse, MUSE_SPARK_1_3, Repository, Tool};

let repository = Repository::new(".", git_executable)?;
let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?)
    .database("harness.db")
    .repository(repository)
    .allow(Tool::Read)
    .allow(Tool::Write);

let mut session = harness
    .session("review-42", output_schema)
    .system_prompt("Keep the review concise.")
    .create()
    .await?;

let result = session.send("Review the current changes").await?;
println!("{}", result.output());
```

Resume the same session after restarting the application:

```rust
let mut session = harness.resume("review-42").await?;
let result = session.send("Now focus on error handling").await?;
```

SQLite is the source of truth. A completed turn retains the user prompt, assistant
messages, tool calls, and tool results. Failed and interrupted turns remain visible in
the database but are not replayed. Different sessions can run concurrently; one session
accepts only one active turn at a time.

The library does not choose a database location. Configure it once with
`Harness::database()`. The companion CLI defaults to `~/.ag-harness/db/harness.db`;
override that with `AG_HARNESS_ROOT` or `--database`.

## One turn

Use `run_once` when no resumable history is needed:

```rust
let result = harness.run_once("Summarize Cargo.toml", output_schema).await?;
```

## Permissions and models

Tools are denied by default. `Tool::Read` provides bounded file, list, search, diff, and
show operations. `Tool::Write` applies one bounded unified diff. Enabling either tool
requires a `Repository` built from the repository root and an absolute, host-controlled
Git executable. Construction canonicalizes both paths and rejects an executable inside
the containing worktree before any model request.

External providers implement the single `Model` trait and return `ModelCompletion`. They
receive the complete ordered history in `ModelRequest::messages()`. A provider may also
return an opaque continuation identifier; if native resume is unavailable, the harness
retries once using the SQLite history and retains any replacement continuation returned
by that replay.

Attach `Harness::with_lifecycle_observer()` for content-free turn, model, and tool
events. The rejected resume and replay are separate model attempts in lifecycle events
and `TurnOutcome::report()`.
