# Agent Infrastructure Module

Provider-specific command builders and response parsing helpers for agent CLI integrations.

## Directory Index

- [backend.rs](backend.rs) - Shared backend trait, backend factory, and resume prompt construction.
- [claude.rs](claude.rs) - Claude CLI backend command construction.
- [codex.rs](codex.rs) - Codex CLI backend command construction.
- [gemini.rs](gemini.rs) - Gemini CLI backend command construction.
- [response_parser.rs](response_parser.rs) - Provider-specific parsing for final and streaming output.
- [AGENTS.md](AGENTS.md) - Local module guidance and directory index.
- [CLAUDE.md](CLAUDE.md) - Symlink to AGENTS.md.
- [GEMINI.md](GEMINI.md) - Symlink to AGENTS.md.
