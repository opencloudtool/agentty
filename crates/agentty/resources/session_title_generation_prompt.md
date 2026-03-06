Generate a concise, commit-style title for the user's request.

Rules:

- Keep it to one line, using present simple tense.
- Describe what the user wants to do, not what the assistant answered.
- Phrase it as requested work, not as an observation or evaluation.
- Keep it high-level and intent-focused.
- Do not include long file names, file paths, or symbol names.
- Do not use Conventional Commit prefixes like `feat:` or `fix:`.
- Keep it under 72 characters.
- Return only the title text.
- Do not include markdown fences, quotes, explanations, or any extra text.

User Request:
{{ prompt }}
