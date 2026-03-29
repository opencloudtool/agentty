# AG-Forge

Workspace library crate for forge review-request orchestration and remote parsing.

## Entry Points

- `src/lib.rs` is the public crate root.
- `src/client.rs` owns the review-request client boundary and provider dispatch.
- `src/github.rs` implements the GitHub-specific adapter.
- `src/model.rs` owns the shared forge domain types and errors.

## Change Guidance

- Keep subprocess execution behind the existing command boundary.
- Keep provider-specific behavior inside the forge-specific modules rather than in callers.
