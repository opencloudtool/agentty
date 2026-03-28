# UI Agent Instructions

When working within `crates/agentty/src/ui/`:

## Core Rules

- **Modularization**: Always respect the module boundaries. Do not put page-specific rendering logic in the UI module root file. Use the dedicated files in `page/` (`session_list.rs`, `session_chat.rs`, etc.).
- **Helper Functions**: If you write a helper function that calculates layout or processes text, **IMMEDIATELY** move it to `util.rs` and write a unit test for it. Do not leave complex logic inline within render functions.
- **Component Reuse**: Check the `component/` directory before building a new common widget. All components must implement the `Component` trait.
- **Palette Usage**: Use semantic color tokens from `style.rs` (`palette::*`) for UI colors. Avoid direct `Color::*` usage in UI components/pages, except approved data-visualization scales (for example heatmap intensity colors).
- **Symlinks**: Ensure `CLAUDE.md` and `GEMINI.md` are symlinked to this file.

## Procedures

### Adding a New Page

1. Create a new module in `page/` (e.g., `page/my_page.rs`)
1. Define a struct (e.g., `MyPage`) that holds necessary references
1. Implement the `Page` trait for your struct with a `render(&mut self, f: &mut Frame, area: Rect)` method
1. Expose the module in `page.rs`
1. Update the match expression in the UI module root file to instantiate and render your page

### Adding a New Component

1. Create a new module in `component/` (e.g., `component/my_widget.rs`)
1. Define a struct that holds the rendering data needed
1. Implement the `Component` trait with a `render(&self, f: &mut Frame, area: Rect)` method
1. Add a `new()` constructor to initialize the struct
1. Expose the module in `component.rs`
1. Usage pattern: `MyWidget::new(...).render(f, area)`

### Modifying Layouts

- Use `util.rs` for complex layout logic (like splitting areas or calculating heights)
- Ensure extensive unit tests in `util.rs` for any layout calculations
- Do not leave layout calculations inline within render functions

### Testing Requirements

- **Unit Tests**: Focus heavily on `util.rs`. Test layout logic, string manipulation, and input height calculations.
- **Integration**: Verifying actual `render` output is difficult. Rely on visual verification for broad changes, but ensure the underlying logic in `util` is rock-solid with comprehensive tests.

## Entry Points

- `render.rs` and `router.rs` own frame composition and page dispatch.
- `page.rs` and `page/` own full-screen pages.
- `component.rs` and `component/` own reusable widgets and overlays.
- `state.rs` and `state/` own UI mode and prompt state.
- `style.rs`, `layout.rs`, `markdown.rs`, and `util.rs` own shared presentation helpers.
