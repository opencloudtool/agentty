______________________________________________________________________

## name: git-commit description: Guide for preparing git commits in this repository, including context gathering and repository-specific commit message conventions.

# Git Commit Workflow

Use this skill when preparing or creating commits.

## Workflow

1. **Gather commit context in one tool call**

   - Run: `git status && git diff && git log -n 3 --format="---%n%B"`.

1. **Write commit title and description**

   - Use a concise summary on the first line (the title) in present simple tense (example: `Fix cursor offset`).
   - Leave one blank line between the title and the body.
   - Add a body only when it improves clarity.
   - Explain *why* and *how* in present simple tense.
   - If there are multiple points, format them as `-` bullet lines with one point per line.

1. **Avoid Conventional Commit prefixes**

   - Do not use prefixes such as `feat:` or `fix:` in commit titles.

1. **Apply repository-specific commit rules**

   - Never use `--no-verify` with git commands to bypass `pre-commit` hooks.
   - Do not add `Co-Authored-By` trailers or AI attribution to commit messages.
