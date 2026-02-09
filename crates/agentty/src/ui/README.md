# UI Module

This directory contains the modular user interface components for the Agentty CLI.

## Architecture

The UI is built using [Ratatui](https://ratatui.rs/) and follows a **separation of concerns** design pattern, splitting rendering logic by page and component type.

### Design Principles

- **Pages** represent complete UI views (session list, chat view, new session prompt)
- **Components** are reusable widgets shared across pages (status bar, input box)
- **Utilities** contain pure layout and text processing functions with extensive unit tests
- All pages implement the `Page` trait
- All components implement the `Component` trait

### Module Structure

- **`mod.rs`**: Main entry point containing the top-level `render` function that dispatches to pages based on `AppMode`. Defines the `Page` and `Component` traits.
- **`pages/`**: Each page is a separate module implementing the `Page` trait.
    - `session_list.rs` - Session list view (`AppMode::List`)
    - `session_chat.rs` - Session chat interface (`AppMode::View`, `AppMode::Prompt`)
- **`components/`**: Reusable widgets implementing the `Component` trait.
    - `status_bar.rs` - Top status bar with version and runtime scope hint
    - `chat_input.rs` - Chat input box with cursor positioning
- **`util.rs`**: Pure helper functions for layout calculations, text wrapping, and input handling. All complex logic that can be unit-tested goes here.

For maintenance procedures and development guidelines, see `AGENTS.md`.
