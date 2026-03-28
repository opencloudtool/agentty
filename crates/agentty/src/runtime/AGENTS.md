# Runtime Module

Terminal runtime loop and mode dispatch.

## Entry Points

- `core.rs` owns the foreground terminal lifecycle and main event loop.
- `event.rs` owns input polling and app-event integration.
- `key_handler.rs` and `mode.rs` route key handling by `AppMode`.
- `clipboard_image.rs` owns pasted-image capture for prompt attachments.
