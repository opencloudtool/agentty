# Runtime Module

Terminal runtime loop and mode dispatch.

## Directory Index

- [`AGENTS.md`](AGENTS.md) - Context and index for the runtime module.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
- [`clipboard_image.rs`](clipboard_image.rs) - Clipboard image capture and temp PNG persistence helpers for prompt-mode attachments.
- [`core.rs`](core.rs) - Runtime loop implementation (`run`, `EventResult`, `TuiTerminal`).
- [`event.rs`](event.rs) - Event reader and event processing loop.
- [`key_handler.rs`](key_handler.rs) - App mode dispatch for key events.
- [`mode/`](mode/) - Per-mode key handlers.
- [`mode.rs`](mode.rs) - Runtime mode module root and handler exports.
- [`terminal.rs`](terminal.rs) - Terminal setup and cleanup guard.
- [`timing.rs`](timing.rs) - Shared runtime redraw and input polling cadence constants.
