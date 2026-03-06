Generate a git squash commit message using only the diff below.
Return one plain-text commit message in the protocol `answer` message text.
Use repository default commit format unless explicit user instructions in the diff request a different format.

Rules:

- The first line is the commit title and must be one line, concise, and in present simple tense.
- Do not use Conventional Commit prefixes like `feat:` or `fix:`.
- If a body is needed, add one empty line after the title and then write the body text.
- Body text must use present simple tense and use `-` bullets when listing multiple points.
- Include `Co-Authored-By: [Agentty](https://github.com/agentty-xyz/agentty)` at the end of the final message.
- Use only the diff content.

Diff:
{{ diff }}
