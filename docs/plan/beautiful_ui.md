# UI Beautification Plan

Status-aware UI polish plan aligned with the current Ratatui implementation in `crates/agentty/src/ui/`.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its status in this document (for example add `- Done`, update checkboxes, and adjust related snapshot rows).

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Color system | Shared semantic palette tokens are defined in `style.rs` and migrated across target UI components/pages. Heatmap colors in `pages/stat.rs` remain a separate RGB data-visualization scale. | Done |
| Spacing and padding | Most pages render with outer `margin(1)` and some overlays use inner padding (`InfoOverlay`). Major table/list views now use `column_spacing(2)` for improved scanability. | Partial |
| Tab bar | Active tab now uses warning-colored bold text (without background fill), and tabs are partitioned with styled separators and a muted bottom border. | Done |
| Tables | Table headers now use bold muted text (without heavy gray fill), row selection uses full-row highlight (no `>> ` marker), and primary list tables have wider spacing. | Done |
| Status and footer bars | Status/footer bars are present and functional, and page-level keybinding hints now render as styled key/label spans. | Partial |
| Diff view | Strong structure exists (file explorer, line-number gutter, sign column). Added/removed lines now include subtle background tinting for faster scanning. | Done |
| Overlays/dialogs | Overlay components now share a rounded frame system (title style + padding defaults) and all modal overlays render with a dimmed backdrop over their source page. | Done |
| Chat presentation | Prompt blocks already have background treatment in markdown rendering; spinner/progress text exists. Input still uses square-border style with no focus variant. | Partial |
| Empty-state and badge UX | Session group placeholders are still generic (`No sessions...`), and session size/status display remains text-color only (no badge/pill styling). | Not started |
| Typography consistency | Bold/dim/color usage is inconsistent across pages/components. | Partial |

## Updated Priorities

## 1) Build a shared palette and migrate hard-coded colors - Done

**Why now:** Almost every other visual change depends on consistent color tokens.

- [x] Add a UI palette module (or constants in `style.rs`) with semantic tokens (surface, border, text, muted, accent, success, warning, danger).
- [x] Keep heatmap colors as a separate data-visualization scale.
- [x] Replace scattered `Color::*` usage across:
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

## 2) Fix keybinding discoverability (footer hint styling) - Done

**Why now:** Highest UX payoff with small surface area.

- [x] Move footer hint rendering from plain `String` output to styled spans where keys and labels are visually distinct.
- [x] Keep `help_action` as the source of actions, but return richer render data for page/footer components.
- [x] Apply to all page footers currently using `Paragraph::new(help_text).style(Color::Gray)`.

## 3) Refresh table visual language - Done

**Why now:** List pages are the most frequently viewed screens.

- [x] Replace `>> ` selection marker with full-row highlight only.
- [x] Replace heavy gray header backgrounds with lighter-weight header styling (bold + muted divider treatment).
- [x] Increase list table column spacing from `1` to `2` where it improves readability.
- [ ] Consider rounded borders for list tables if consistent with the updated palette.

Primary files:

- `pages/session_list.rs`
- `pages/project_list.rs`
- `pages/setting.rs`
- `pages/stat.rs`

## 4) Upgrade tab bar affordance - Done

- [x] Keep selected-tab emphasis text-only (warning color + bold) for cleaner visual weight.
- [x] Add separators or spacing treatment that clearly partitions tabs.
- [x] Keep project-qualified Sessions label behavior already present.

Primary file:

- `component/tab.rs`

## 5) Improve diff scanability with background tints - Done

- [x] Keep the current useful structure (file tree + numeric gutter + sign column).
- [x] Add subtle background colors for additions/deletions in `pages/diff.rs`.
- [x] Preserve readable contrast for wrapped lines and hunk headers.

Primary files:

- `pages/diff.rs`
- `diff_util.rs` (only if tokenization helpers need extension)

## 6) Make overlay styling consistent - Done

- [x] Introduce a shared overlay frame style (border type, title style, padding defaults).
- [x] Normalize `ConfirmationOverlay`, `HelpOverlay`, and `OpenCommandOverlay` with the same visual system used by `InfoOverlay`.
- [x] Add background dimming behind overlays (currently background is redrawn and popup area is `Clear`ed only).

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

1. Shared color palette + color token migration. (Done)
1. Footer keybinding styling. (Done)
1. Table selection/header/spacing refresh. (Done)
1. Diff addition/deletion background tinting. (Done)
1. Tab bar polish. (Done)
1. Overlay consistency + background dimming. (Done)
1. Chat input focus and output border follow-up.
1. Empty states and optional badges.

## Out of Scope for This Pass

- Full syntax highlighting inside diff hunks.
- Deep animation systems beyond existing spinner/progress behavior.
