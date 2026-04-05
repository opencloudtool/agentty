# Docs Site Content

Scope: `docs/site/content/docs/` and its child documentation pages.

## Purpose

- Keep user-facing documentation readable in the static docs site on both
  desktop and narrow viewports.
- Preserve stable page structure, headings, and anchors for cross-page links.

## Organization Rules

- Use markdown tables only for compact comparison data where most cells stay to
  a short phrase.
- When a table cell needs a sentence, replace the table with stacked sections
  or short titled blocks instead of relying on horizontal scrolling.
- Prefer one subsection per concept with consistent labels such as `Comes from`, `Prints`, and `Hidden or removed` when documenting lifecycle-style
  behavior.

## Change Routing

- Update `architecture/runtime-flow.md` when render order, session-output
  sources, or status-driven visibility rules change.
- Update `usage/workflow.md` and `usage/keybindings.md` when UI behavior or
  controls change.

## Docs Sync Notes

- Keep this directory guide aligned with the docs site's presentation patterns
  when a new formatting convention becomes the default for multiple pages.
