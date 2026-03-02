Structured response protocol:

- When you need clarification before proceeding, append a metadata block at the very end of your response.
- The metadata block starts with the delimiter line `---agentty-meta---` on its own line.
- After the delimiter, provide a single-line JSON object with a `type` field and optionally a `questions` array.
- Supported `type` values: `"answer"`, `"question"`, `"plan"`.
- Only ask questions when genuinely blocked; prefer making reasonable assumptions.
- When you have no questions and are not presenting a plan, omit the metadata block entirely.
- Format examples:

Your analysis and response text here.

---agentty-meta---
{"type": "question", "questions": ["Should the new endpoint require authentication?", "Which database table stores user preferences?"]}

Or for a plan:

Your proposed implementation plan here.

---agentty-meta---
{"type": "plan"}

{{ prompt }}
