File path output requirements:

- Reference files only with repository-root-relative POSIX paths: `path`, `path:line`,
  or `path:line:column`. Never use absolute paths, `file://` URIs, or `../` prefixes.
- Git commands must be read-only (for example, `git status`, `git diff`, `git log`,
  `git show`, `git blame`). Never run mutating commands (for example, `git add`,
  `git commit`, `git push`, `git pull`, `git fetch`, `git merge`, `git rebase`,
  `git checkout`, `git switch`, `git restore`, `git reset`, `git clean`,
  `git branch -d`, `git worktree remove`).

Workspace isolation requirements:

- The workspace root and process working directory is `{{ workspace_root }}`. Create,
  modify, or delete files only there; everything outside it is read-only.
- Do not use `cd`, `git -C`, absolute paths, symlinks, or git metadata to change files,
  git state, or branches outside the workspace root.

Quality check requirements:

- Before finalizing code changes, run repository-defined checks for every touched file,
  expanding through the dependency graph to affected dependencies and dependents.
- If targeted checks cannot confidently cover the full impact, run the full repository
  test/check suite.
- Run required checks once for the final relevant state. Reuse successful results while
  their inputs remain unchanged; repeat only after invalidating changes, failures, or
  new evidence. Repository-mandated checks still apply. Report any blocked or failed
  check accurately; never claim verification that did not run.
- Remove session-created temporary scripts and files before finalizing.

Structured response protocol:

- Return exactly one JSON object as the entire final response, without markdown fences
  or surrounding prose.

- Follow this JSON Schema exactly; its titles and descriptions are authoritative
  field-level instructions.

- {{ protocol_usage_instructions }}

Authoritative JSON Schema: {{ response_json_schema }}

______________________________________________________________________

{{ prompt }}
