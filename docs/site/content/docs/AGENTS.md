# Docs Content

Documentation pages for `/docs/` in the Zola site.

## Paragraph Anchor IDs

- Treat paragraph-level deep links as a maintained contract for docs pages.
- Add explicit HTML anchors before paragraphs that should be directly linkable: `<a id="..."></a>`.
- Use lowercase kebab-case IDs and keep them stable over time.
- Prefer page-scoped prefixes in IDs (for example, `usage-...`, `backends-...`) to avoid collisions and keep intent clear.
- When editing or rewriting anchored paragraphs, preserve existing IDs when possible to avoid breaking inbound links.
- If an anchor must be renamed or removed, update all known internal references in `content/docs/` in the same change.

## Directory Index

- [\_index.md](_index.md) - Docs section configuration and landing content.
- [getting-started/](getting-started/) - Getting started section and installation guide.
- [agents/](agents/) - Agent backends, models, and configuration.
- [usage/](usage/) - Workflow, keybindings, and session lifecycle.
- [mcp/](mcp/) - Model Context Protocol setup guides.
- [contributing/](contributing/) - Contribution and documentation guides.
- [AGENTS.md](AGENTS.md) - Context and instructions for AI agents.
- [CLAUDE.md](CLAUDE.md) - Symlink to AGENTS.md.
- [GEMINI.md](GEMINI.md) - Symlink to AGENTS.md.
