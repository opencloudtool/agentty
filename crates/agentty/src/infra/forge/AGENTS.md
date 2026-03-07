# Forge Review Requests

Forge review-request adapters, command boundaries, and remote parsing helpers.

## Directory Index

- [`AGENTS.md`](AGENTS.md) - Local forge module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
- [`client.rs`](client.rs) - Public `ReviewRequestClient` trait and production adapter dispatch.
- [`command.rs`](command.rs) - Mockable forge CLI command runner and shared command output helpers.
- [`github.rs`](github.rs) - GitHub pull-request adapter built around `gh`.
- [`gitlab.rs`](gitlab.rs) - GitLab merge-request adapter built around `glab`.
- [`model.rs`](model.rs) - Shared forge review-request types and normalized errors.
- [`remote.rs`](remote.rs) - Remote parsing and supported-forge detection helpers.
