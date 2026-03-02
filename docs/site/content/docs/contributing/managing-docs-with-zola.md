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

## Authoring Workflow

1. Create a new Markdown page under `content/docs/`.
1. Add `title`, `description`, and `weight` front matter.
1. Add a `<!-- more -->` break so docs listings show concise summaries.
1. Run `zola check` before publishing.
