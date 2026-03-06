# UI Beautification Plan

Status-aware UI polish plan aligned with the current Ratatui implementation in `crates/agentty/src/ui/`.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Color system | Most UI uses ANSI colors (`Color::Cyan`, `Color::DarkGray`, etc.). A few targeted RGB usages exist (for example heatmap colors in `pages/stat.rs`). | Not started |
| Spacing and padding | Most pages render with outer `margin(1)` and some overlays use inner padding (`InfoOverlay`). Table `column_spacing` remains `1` in major list views. | Partial |
| Tab bar | Active tab is yellow + bold; no active background treatment or separators. | Not started |
| Tables | Header row still uses gray background with black text; row selection still uses `>> `. | Not started |
| Status and footer bars | Status/footer bars are present and functional, but page-level keybinding hints are rendered as plain single-style strings. | Partial |
| Diff view | Strong structure exists (file explorer, line-number gutter, sign column). Added/removed lines are foreground-only (no background tinting). | Partial |
| Overlays/dialogs | Overlay components are established. `InfoOverlay` already uses rounded borders and padding; others still use square borders and no shared dimmed backdrop. | Partial |
| Chat presentation | Prompt blocks already have background treatment in markdown rendering; spinner/progress text exists. Input still uses square-border style with no focus variant. | Partial |
| Empty-state and badge UX | Session group placeholders are still generic (`No sessions...`), and session size/status display remains text-color only (no badge/pill styling). | Not started |
| Typography consistency | Bold/dim/color usage is inconsistent across pages/components. | Partial |

## Updated Priorities

## 1) Build a shared palette and migrate hard-coded colors

**Why now:** Almost every other visual change depends on consistent color tokens.

- Add a UI palette module (or constants in `style.rs`) with semantic tokens (surface, border, text, muted, accent, success, warning, danger).
- Keep heatmap colors as a separate data-visualization scale.
- Replace scattered `Color::*` usage across:
  - `components/status_bar.rs`
  - `components/footer_bar.rs`
  - `components/tab.rs`
  - `components/chat_input.rs`
  - `components/session_output.rs`
  - `components/file_explorer.rs`
  - `pages/session_list.rs`
  - `pages/project_list.rs`
  - `pages/diff.rs`
  - `pages/stat.rs`
  - `pages/setting.rs`

## 2) Fix keybinding discoverability (footer hint styling)

**Why now:** Highest UX payoff with small surface area.

- Move footer hint rendering from plain `String` output to styled spans where keys and labels are visually distinct.
- Keep `help_action` as the source of actions, but return richer render data for page/footer components.
- Apply to all page footers currently using `Paragraph::new(help_text).style(Color::Gray)`.

## 3) Refresh table visual language

**Why now:** List pages are the most frequently viewed screens.

- Replace `>> ` selection marker with either full-row highlight only or a subtle single-column marker.
- Replace heavy gray header backgrounds with lighter-weight header styling (bold + muted divider treatment).
- Increase list table column spacing from `1` to `2` where it improves readability.
- Consider rounded borders for list tables if consistent with the updated palette.

Primary files:

- `pages/session_list.rs`
- `pages/project_list.rs`
- `pages/setting.rs`
- `pages/stat.rs`

## 4) Upgrade tab bar affordance

- Add active-tab background (not only yellow text).
- Add separators or spacing treatment that clearly partitions tabs.
- Keep project-qualified Sessions label behavior already present.

Primary file:

- `components/tab.rs`

## 5) Improve diff scanability with background tints

- Keep the current useful structure (file tree + numeric gutter + sign column).
- Add subtle background colors for additions/deletions in `pages/diff.rs`.
- Preserve readable contrast for wrapped lines and hunk headers.

Primary files:

- `pages/diff.rs`
- `diff_util.rs` (only if tokenization helpers need extension)

## 6) Make overlay styling consistent

- Introduce a shared overlay frame style (border type, title style, padding defaults).
- Normalize `ConfirmationOverlay`, `HelpOverlay`, and `OpenCommandOverlay` with the same visual system used by `InfoOverlay`.
- Add background dimming behind overlays (currently background is redrawn and popup area is `Clear`ed only).

Primary files:

- `overlay.rs`
- `components/confirmation_overlay.rs`
- `components/help_overlay.rs`
- `components/info_overlay.rs`
- `components/open_command_overlay.rs`

## 7) Chat panel polish

- Keep existing prompt block background styling from `markdown.rs`.
- Add a clear focused-input border treatment in `chat_input.rs`.
- Evaluate whether output panel should keep top/bottom-only borders or move to a subtle full box after palette migration.

Primary files:

- `components/chat_input.rs`
- `components/session_output.rs`
- `markdown.rs`

## 8) Empty states and badge treatments

- Replace generic placeholders like `No sessions...` with action-oriented copy.
- Add optional badge/pill styles for session size and status where density remains acceptable.

Primary files:

- `pages/session_list.rs`
- `pages/project_list.rs`

## Suggested Execution Order

1. Shared color palette + color token migration.
1. Footer keybinding styling.
1. Table selection/header/spacing refresh.
1. Diff addition/deletion background tinting.
1. Tab bar polish.
1. Overlay consistency + background dimming.
1. Chat input focus and output border follow-up.
1. Empty states and optional badges.

## Out of Scope for This Pass

- Full syntax highlighting inside diff hunks.
- Deep animation systems beyond existing spinner/progress behavior.
