# UI Beautification Plan

Recommendations for improving the Ratatui-based TUI, organized from highest-impact to most subtle.

## 1. Adopt a True Color Palette Instead of ANSI-16

**Current:** The entire UI uses the 16 basic ANSI colors (`Color::Cyan`, `Color::DarkGray`, etc.), which look different on every terminal theme and can clash badly.

**Proposal:** Define a custom palette using `Color::Rgb(r, g, b)` with a cohesive, muted color scheme (think Catppuccin, Tokyo Night, or custom). This is the single biggest visual upgrade possible.

```rust
// Example palette module
pub const SURFACE_0: Color = Color::Rgb(30, 30, 46);    // dark bg
pub const SURFACE_1: Color = Color::Rgb(49, 50, 68);    // slightly lighter (selected row, bars)
pub const SURFACE_2: Color = Color::Rgb(88, 91, 112);   // borders, muted elements
pub const TEXT: Color = Color::Rgb(205, 214, 244);       // primary text
pub const SUBTEXT: Color = Color::Rgb(147, 153, 178);    // secondary/dim text
pub const BLUE: Color = Color::Rgb(137, 180, 250);       // primary accent
pub const GREEN: Color = Color::Rgb(166, 227, 161);      // success
pub const YELLOW: Color = Color::Rgb(249, 226, 175);     // warnings, active tabs
pub const RED: Color = Color::Rgb(243, 139, 168);        // errors, destructive
pub const MAUVE: Color = Color::Rgb(203, 166, 247);      // special states
```

Gives a visually harmonious look regardless of the user's terminal theme. Replace the scattered `Color::*` references in `style.rs`, page files, and components with palette constants.

**Files affected:** `style.rs`, all page and component files that reference `Color::*`.

______________________________________________________________________

## 2. Add Breathing Room with Padding and Spacing

**Current:** Content is packed tight — tables start at the very edge, the session list has a 1-cell margin but no inner padding, and the chat view has no visual breathing room.

**Proposals:**

- **Status bar / footer bar**: Add 1 cell of left padding consistently using `Padding`.
- **Tables**: Increase `column_spacing` from `1` to `2`. Add 1 line of `bottom_margin` between group headers and their rows.
- **Session chat**: Add a thin left gutter (1-2 cells) for the output area so text doesn't hug the border.
- **Overlays**: Consider `Padding::uniform(1)` inside overlay blocks.

**Files affected:** `session_list.rs`, `session_chat.rs`, `project_list.rs`, overlay components.

______________________________________________________________________

## 3. Improve the Tab Bar

**Current:** Plain text labels with bottom border, active tab is just yellow+bold.

**Proposals:**

- Use a subtle background highlight for the active tab (e.g., `bg(SURFACE_1)`) instead of just color change, making it look like a real tab.
- Add a separator character between tabs (e.g., `│` or a thin space) for visual clarity.
- Consider an underline (`Modifier::UNDERLINED`) on the active tab instead of/in addition to color — a common modern TUI pattern.

**Files affected:** `components/tab.rs`.

______________________________________________________________________

## 4. Soften Table Appearance

**Current:** Gray background header row with black text, `>> ` selection indicator, plain borders.

**Proposals:**

- **Header**: Use bold text with an underline instead of a solid background color. Less visually heavy.
- **Selection**: Replace `>> ` with a subtle vertical bar `▎` or highlight the entire row with a slightly different background — less jarring than the arrow prefix which shifts content.
- **Row separator**: Add alternating row tinting (subtle, like every other row gets +5 brightness) for readability in long lists.
- **Borders**: Use `BorderType::Rounded` for the session list block — softer and more modern.

**Files affected:** `session_list.rs`, `project_list.rs`, `settings.rs`.

______________________________________________________________________

## 5. Enrich the Status/Footer Bars

**Current:** Single-line `DarkGray` background bars with minimal content.

**Proposals:**

- **Status bar**: Add a subtle separator between app name and the rest (e.g., `│`). Consider right-aligning a clock or session count.
- **Footer bar**: Style keybinding hints differently from descriptions — keys in accent color, descriptions in subtext color. This is the **#1 discoverability improvement**.

Example rendering:

```
 n new  Enter view  d diff  ? help
```

Where `n`, `Enter`, `d`, `?` are rendered in the accent color and the rest is dimmed.

**Files affected:** `components/status_bar.rs`, `components/footer_bar.rs`, `session_list.rs` (help text), `state/help_action.rs`.

______________________________________________________________________

## 6. Improve the Diff View

**Current:** Functional unified diff with line numbers and color-coded +/- signs.

**Proposals:**

- Use subtle background tints for added/removed lines instead of just foreground color — `bg(Rgb(30, 50, 30))` for additions, `bg(Rgb(50, 30, 30))` for deletions. This is what every modern diff tool does and massively improves scanability.
- Add a thin vertical separator between the line number gutter and content.
- Consider syntax highlighting for code within diff hunks (higher effort, likely via `syntect` or `tree-sitter`).

**Files affected:** `pages/diff.rs`, `diff_util.rs`.

______________________________________________________________________

## 7. Polish Overlays and Dialogs

**Current:** Centered boxes with colored borders, `Clear` widget for background.

**Proposals:**

- **Dim the background** behind overlays instead of just clearing. Render the background frame normally, then draw a semi-transparent dim layer (fill area with `Style::default().bg(Color::Rgb(0,0,0)).add_modifier(Modifier::DIM)`) before rendering the overlay. Creates a modal "focus" effect.
- **Consistent rounded borders** on all overlays (the info overlay uses rounded, but confirmation doesn't).
- **Button styling**: Instead of just Cyan background on selected yes/no, use rounded brackets and a filled indicator: `[ Yes ]  No` or `Yes   No`.

**Files affected:** `overlays.rs`, `components/confirmation_overlay.rs`, `components/help_overlay.rs`, `components/info_overlay.rs`.

______________________________________________________________________

## 8. Improve the Chat Experience

**Current:** Prompt prefix `›` with markdown-rendered output above.

**Proposals:**

- **Message bubbles**: Add a subtle left-border accent (like a `▎` character or colored border) to distinguish user prompts from agent output, similar to how Slack/Discord show messages.
- **Typing indicator**: Already have the braille spinner — consider adding a subtle pulsing effect or a "thinking..." text next to it.
- **Input box**: Use `BorderType::Rounded` and an accent-colored border when focused to clearly show the input area.

**Files affected:** `pages/session_chat.rs`, `components/chat_input.rs`, `components/session_output.rs`, `markdown.rs`.

______________________________________________________________________

## 9. Add Visual Micro-Interactions

- **Empty states**: Replace `"No sessions..."` with a friendlier message and a hint: `No sessions yet — press n to start one`.
- **Session size pills**: Instead of just colored text, render size labels with a subtle background tint (like badges/pills).
- **Status labels**: Same treatment — render as colored badges: `InProgress` with a background color.

**Files affected:** `session_list.rs`, `project_list.rs`.

______________________________________________________________________

## 10. Typography Improvements

- Use **bold** sparingly and consistently — only for headings and the most important interactive element on screen.
- Use **dim** for truly secondary information (timestamps, paths, IDs) rather than for primary content.
- Ensure consistent capitalization in headers and labels.

**Files affected:** All rendering files (audit pass).

______________________________________________________________________

## Priority Matrix

| Priority | Item | Impact | Effort |
|----------|------|--------|--------|
| 1 | RGB color palette | Transformative | Medium |
| 2 | Styled keybinding hints in footer | High | Low |
| 3 | Diff line background tinting | High | Low |
| 4 | Table selection style (bar vs `>>`) | Medium | Low |
| 5 | Tab bar active highlight | Medium | Low |
| 6 | Overlay background dimming | Medium | Medium |
| 7 | Spacing/padding pass | Medium | Low |
| 8 | Chat message left-border accents | Medium | Low |
| 9 | Status badges/pills | Medium | Medium |
| 10 | Rounded borders everywhere | Low | Low |

**Recommended first pass:** Items 1-5 as a cohesive batch — they transform the look with relatively contained changes.
