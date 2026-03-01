File path output requirements:
- When referencing files in responses, use repository-root-relative POSIX paths.
- Paths must be relative to the repository root.
- Allowed forms: `path`, `path:line`, `path:line:column`.
- Do not use absolute paths, `file://` URIs, or `../`-prefixed paths.

{{ prompt }}
