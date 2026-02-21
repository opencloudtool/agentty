# UI Agent Instructions

When working within `crates/agentty/src/ui/`:

## Core Rules

- **Modularization**: Always respect the module boundaries. Do not put page-specific rendering logic in `mod.rs`. Use the dedicated files in `pages/` (`session_list.rs`, `session_chat.rs`, etc.).
- **Helper Functions**: If you write a helper function that calculates layout or processes text, **IMMEDIATELY** move it to `util.rs` and write a unit test for it. Do not leave complex logic inline within render functions.
- **Component Reuse**: Check the `components/` directory before building a new common widget. All components must implement the `Component` trait.
- **Symlinks**: Ensure `CLAUDE.md` and `GEMINI.md` are symlinked to this file.

## Procedures

### Adding a New Page

1. Create a new module in `pages/` (e.g., `pages/my_page.rs`)
2. Define a struct (e.g., `MyPage`) that holds necessary references
3. Implement the `Page` trait for your struct with a `render(&mut self, f: &mut Frame, area: Rect)` method
4. Expose the module in `pages/mod.rs`
5. Update the match expression in `mod.rs` to instantiate and render your page

### Adding a New Component

1. Create a new module in `components/` (e.g., `components/my_widget.rs`)
2. Define a struct that holds the rendering data needed
3. Implement the `Component` trait with a `render(&self, f: &mut Frame, area: Rect)` method
4. Add a `new()` constructor to initialize the struct
5. Expose the module in `components/mod.rs`
6. Usage pattern: `MyWidget::new(...).render(f, area)`

### Modifying Layouts

- Use `util.rs` for complex layout logic (like splitting areas or calculating heights)
- Ensure extensive unit tests in `util.rs` for any layout calculations
- Do not leave layout calculations inline within render functions

### Testing Requirements

- **Unit Tests**: Focus heavily on `util.rs`. Test layout logic, string manipulation, and input height calculations.
- **Integration**: Verifying actual `render` output is difficult. Rely on visual verification for broad changes, but ensure the underlying logic in `util` is rock-solid with comprehensive tests.

## Directory Index
- [components/](components/) - Reusable UI components.
- [pages/](pages/) - Full-screen page implementations.
- [AGENTS.md](AGENTS.md) - UI specific instructions.
- [CLAUDE.md](CLAUDE.md) - Symlink to AGENTS.md.
- [GEMINI.md](GEMINI.md) - Symlink to AGENTS.md.
- [icon.rs](icon.rs) - UI icons.
- [markdown.rs](markdown.rs) - Styled markdown renderer for session output.
- [mod.rs](mod.rs) - UI module definition and main render loop.
- [README.md](README.md) - Additional documentation.
- [state/](state/) - UI state definitions.
- [style.rs](style.rs) - UI styling constants.
- [util.rs](util.rs) - Shared UI utilities and layout logic.
