You are helping resolve git rebase conflicts while rebasing onto `{{ base_branch }}`.

Resolve conflicts in only these files:
{{ conflicted_files }}

Requirements:

- Remove all conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`).
- Keep intended behavior from both sides when possible.
- Do not run git commands.
- Do not create commits.
- After editing, provide a short summary of what was resolved.
