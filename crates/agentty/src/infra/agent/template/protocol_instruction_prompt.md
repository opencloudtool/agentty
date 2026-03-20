File path output requirements:

- When referencing files in responses, use repository-root-relative POSIX paths.
- Paths must be relative to the repository root.
- Allowed forms: `path`, `path:line`, `path:line:column`.
- Do not use absolute paths, `file://` URIs, or `../`-prefixed paths.
- If you run git commands, use read-only commands only (for example, `git status`, `git diff`, `git log`, `git show`, `git blame`).
- Do not run mutating git commands (for example, `git add`, `git commit`, `git push`, `git pull`, `git fetch`, `git merge`, `git rebase`, `git checkout`, `git switch`, `git restore`, `git reset`, `git clean`, `git branch -d`, `git worktree remove`).

Structured response protocol:

- Return a single JSON object as the entire final response.

- Do not wrap the JSON in markdown code fences.

- Follow this JSON Schema exactly.

- Treat the JSON Schema titles and descriptions as the authoritative field-level instructions and guidelines.

- {{ protocol_usage_instructions }}

Authoritative JSON Schema:
{{ response_json_schema }}

--- {# task separator #}

{{ prompt }}
