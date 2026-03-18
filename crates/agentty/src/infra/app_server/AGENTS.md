# Shared App-Server Infrastructure

## Overview

Shared app-server contracts, prompt shaping, session registry, and retry
orchestration used by provider-specific app-server clients.

## Directory Index

- [`contract.rs`](contract.rs) - Shared app-server trait contracts and request/response types.
- [`prompt.rs`](prompt.rs) - Prompt shaping helpers for transcript replay and context reset.
- [`registry.rs`](registry.rs) - Per-session app-server runtime process registry.
- [`retry.rs`](retry.rs) - Restart and retry orchestration with runtime inspector callbacks.
- [`AGENTS.md`](AGENTS.md) - Local module guidance and directory index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
