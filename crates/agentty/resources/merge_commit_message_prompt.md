Generate a git squash commit message using only the diff below.
Return strict JSON with exactly two keys: `title` and `description`.
Use repository default commit format unless explicit user instructions in the diff request a different format.

Rules:
- `title` must be one line, concise, and in present simple tense.
- Do not use Conventional Commit prefixes like `feat:` or `fix:`.
- `description` is commit body text and may be an empty string when no body is needed.
- If `description` is not empty, write in present simple tense and use `-` bullets when listing multiple points.
- Include `Co-Authored-By: [Agentty](https://github.com/opencloudtool/agentty)` at the end of the final message.
- Use only the diff content.
- Do not wrap the JSON in markdown fences.

Diff:
{{diff}}
