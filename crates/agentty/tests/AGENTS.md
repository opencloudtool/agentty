# Integration Tests

Integration tests for Agentty crate behavior that runs against the compiled
public API.

## Directory Index

- [`e2e.rs`](e2e.rs) - TUI E2E tests using `testty` framework with PTY-driven semantic assertions and VHS screenshot capture.
- [`protocol_compliance_e2e.rs`](protocol_compliance_e2e.rs) - Live provider protocol-compliance tests for Codex, Gemini Flash, and Claude Sonnet.
- [`AGENTS.md`](AGENTS.md) - Local test directory guidance and index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
