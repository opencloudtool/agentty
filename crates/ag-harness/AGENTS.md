# ag-harness

Provider-neutral LLM model calls, independent of Agentty UI and orchestration.

## Boundaries

- `Model` is the object-safe application boundary; `ModelClient` owns the common request
  lifecycle, telemetry, and structured-output validation.
- Provider modules own configuration and capability policy. API-family modules own
  shared authentication, translation, and wire handling; keep their runtime types
  private.
- Keep network access behind the injectable client boundary.

## Invariants

- Require an output schema for every request and validate every response locally.
- Return explicit errors for unsupported capabilities; never weaken the shared contract.
- Validate and retain provider metadata at construction.
- Keep response bodies and diagnostics bounded, and apply duration telemetry uniformly.

## SQLite Invariants

- Prefer checked SQLx query macros. Use `sqlx::query_as!` with named row structs for
  row-mapped reads, and keep `crates/ag-harness/.sqlx/` metadata current for offline
  builds. Regenerate it with the live-database workflow in `CONTRIBUTING.md`.

Update `docs/site/content/docs/architecture/ag-harness-design.md` when these boundaries
or the runtime role changes.
