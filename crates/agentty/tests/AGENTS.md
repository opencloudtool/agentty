# Integration Tests

Integration tests for Agentty crate behavior that runs against the compiled
public API.

## Directory Index

- [`e2e.rs`](e2e.rs) - VHS-based E2E tests that launch the real `agentty` binary, capture PNG screenshots, and compare against stored references.
- [`e2e_support/`](e2e_support/) - Support module for VHS E2E tests: harness, tape generation, and screenshot comparison.
- [`e2e_screenshots/`](e2e_screenshots/) - Reference PNG screenshots for VHS E2E comparison tests.
- [`protocol_compliance_e2e.rs`](protocol_compliance_e2e.rs) - Live provider protocol-compliance tests for Codex, Gemini Flash, and Claude Sonnet.
- [`AGENTS.md`](AGENTS.md) - Local test directory guidance and index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
