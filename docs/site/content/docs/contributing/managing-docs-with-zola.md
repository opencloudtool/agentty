+++
title = "Managing Docs with Zola"
description = "Recommended structure and front-matter conventions for maintaining docs in Zola."
weight = 1
+++

<a id="managing-docs-introduction"></a>
Use these conventions to keep Agentty documentation maintainable as it grows.

<!-- more -->

## Keep URLs Stable

- Keep documentation under the `content/docs/` section.
- Keep the section directory named `docs` so its canonical route remains `/docs/`.
- When moving or renaming pages, add `aliases` in page front matter to preserve old links.
- For paragraph-level deep links, add explicit HTML anchors in content (for example, `<a id="some-paragraph-id"></a>` before the paragraph).
- Paragraph anchors automatically render a `#` affordance next to the paragraph so users can copy deep links directly.

## Use Section Metadata Deliberately

- Set `sort_by = "weight"` and define page `weight` values for intentional ordering.
- Keep `page_template` on the docs section so all guides share a consistent layout.
- Use `in_search_index = true` in docs sections, and ensure site-level search indexing is enabled when search is needed.

## Scale with Nested Sections

- Group larger topics into nested sections (`content/docs/<topic>/_index.md`).
- Render navigation from `get_section(...).subsections` so new sections appear automatically.
- Use `transparent = true` only when subsection pages should be merged into the parent listing.

## Prefer Mermaid for Diagrams

- Use fenced `mermaid` code blocks for flow, lifecycle, and architecture diagrams instead of ASCII trees.
- Keep node labels concise and let the docs-page template handle theme-aware Mermaid rendering.
- Mermaid diagrams in docs pages now ship with built-in `fit`, zoom, and drag-to-pan controls automatically, so authors do not need to add extra wrapper markup.

## Add a Feature Entry

The `/features/` page auto-discovers entries from individual `.md` files in `content/features/`. To add a new feature:

1. Place the GIF in `static/features/` (E2E tests do this automatically via `save_feature_gif()`).
1. Create `content/features/<name>.md` with the following front matter:
   ```toml
   +++
   title = "Feature title"
   description = "One-line description shown on the card."
   weight = <ordering number>

   [extra]
   gif = "<name>.gif"
   +++
   ```
1. Choose a `weight` that slots the entry into the desired display position (lower weights appear first).
1. Run `zola build` to verify the features page renders the new entry.

The `features.html` template uses `get_section(path="features/_index.md")` and iterates `section.pages` ordered by `weight`. The homepage feature card in `index.html` is hardcoded and curated separately.

## Authoring Workflow

1. Create a new Markdown page under `content/docs/`.
1. Add `title`, `description`, and `weight` front matter.
1. Add a `<!-- more -->` break so docs listings show concise summaries.
1. Run `zola check` before publishing.
