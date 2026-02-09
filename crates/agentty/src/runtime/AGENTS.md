# Runtime Module

Terminal runtime loop and mode dispatch.

## Directory Index
- [event.rs](event.rs) - Event reader and event processing loop.
- [key_handler.rs](key_handler.rs) - App mode dispatch for key events.
- [mod.rs](mod.rs) - Runtime entry point and render loop.
- [mode/](mode/) - Per-mode key handlers.
- [terminal.rs](terminal.rs) - Terminal setup and cleanup guard.
