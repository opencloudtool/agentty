Generate the canonical session commit message using the cumulative session diff below.
Return one plain-text commit message in the protocol `answer` message text.
Use repository default commit format unless explicit user instructions in the diff request a different format.

Rules:

- The first line is the commit title and must be one line, concise, and in present simple tense.
- Do not use Conventional Commit prefixes like `feat:` or `fix:`.
- If a body is needed, add one empty line after the title and then write the body text.
- Body text must use present simple tense and use `-` bullets when listing multiple points.
- Include `Co-Authored-By: [Agentty](https://github.com/agentty-xyz/agentty)` at the end of the final message.
- If an existing session commit message is provided, refine that same message to fit the new diff instead of restarting from scratch.
- Use only the diff content and the existing session commit message.

Existing session commit message (may be empty):
{{ current_commit_message }}

Diff:
{{ diff }}
