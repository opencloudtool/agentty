# Runtime Module

Terminal runtime loop and mode dispatch.

## Directory Index

- [`core.rs`](core.rs) - Runtime loop implementation (`run`, `EventResult`, `TuiTerminal`).
- [`event.rs`](event.rs) - Event reader and event processing loop.
- [`key_handler.rs`](key_handler.rs) - App mode dispatch for key events.
- [`mode/`](mode/) - Per-mode key handlers.
- [`mode.rs`](mode.rs) - Runtime mode module root and handler exports.
- [`terminal.rs`](terminal.rs) - Terminal setup and cleanup guard.
