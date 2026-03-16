---
name: security-audit
description: Audit subprocess execution, path handling, SQL queries, panic conditions, and dependency risks in this Rust TUI project.
---

# Security Audit Skill

Use this skill when asked to audit agentty for security vulnerabilities or assess its attack surface.

Agentty is a local TUI tool that spawns agent backends (Claude CLI, Codex, Gemini) as subprocesses, manages git worktrees, stores session data in SQLite, and streams agent output to the terminal. It has no web server, no authentication layer, and no network-facing API.

## Workflow

1. **Read Project Context**
   - Read the root `AGENTS.md` for project conventions, architecture, and directory index.
   - Identify the target directories or modules to audit (use the user's scope if provided, otherwise audit the full project).

2. **Traverse Target Directories**
   - For each target directory, read its local `AGENTS.md` to understand module purpose and file layout before inspecting source files.
   - Follow directory indexes to navigate into sub-modules.

3. **Analyze Security Concerns**

   Focus on the threat categories that apply to a local CLI/TUI tool with subprocess management:

   - **Command Injection:** Verify all subprocess spawning (`Command::new`, `tokio::process::Command`) uses argument vectors (`.arg()` / `.args()`), never string interpolation or shell invocation. Check that user-supplied data (prompts, branch names, file paths) never flows into shell-interpreted strings.
   - **Path Traversal:** Review file path construction from user input or database values. Ensure paths use safe APIs (`PathBuf::join`, `Path::strip_prefix`) and reject `..` traversal or symlink escapes. Check `AGENTTY_ROOT` override handling.
   - **SQL Safety:** Confirm all `sqlx` queries use parameterized bindings (`query!`, `query_as!`, `$1`/`$2` placeholders). Flag any string-formatted SQL.
   - **Panic Conditions:** Identify bare `.unwrap()` / `.expect()` on data from untrusted sources (user input, file reads, subprocess output, database results, environment variables). These can crash the TUI and lose unsaved state.
   - **Sensitive Data Exposure:** Check for API keys, tokens, or credentials hardcoded in source or logged to session output files. Verify that agent stdout/stderr capture does not persist secrets to disk unintentionally.
   - **Subprocess Lifecycle:** Look for missing timeouts on spawned processes, unhandled zombie processes, or missing signal forwarding that could cause resource leaks or hangs.
   - **Concurrency & Race Conditions:** Check for TOCTOU races in worktree creation/cleanup, unguarded shared state across `tokio` tasks, and unsafe `Send`/`Sync` implementations.
   - **Dependency Risks:** Flag known-vulnerable dependencies, `unsafe` blocks, unsafe FFI calls, and overly broad feature flags (e.g., `tokio/full`).
   - **Git Worktree Safety:** Verify that worktree branch names are sanitized, `--force` cleanup cannot delete unrelated worktrees, and concurrent session creation/deletion does not race.

4. **Return Findings**
   - Format findings as a prioritized markdown task list in your answer.
   - Each item must include:
     - **Title:** Short description of the finding.
     - **Priority:** `Critical`, `High`, `Medium`, or `Low`.
     - **Scope:** File path or module where the issue was found.
     - **Description:** What the vulnerability is, why it matters, and a recommended mitigation.
   - Order findings by priority (Critical first, Low last).
   - Omit empty priority sections.

### Output Format

```markdown
# Security Audit Report

## Critical
- [ ] **[Title]** — `path/to/file.rs`
  [Description of the vulnerability and recommended mitigation.]

## High
- [ ] **[Title]** — `path/to/file.rs`
  [Description and mitigation.]

## Medium
- [ ] **[Title]** — `path/to/file.rs`
  [Description and mitigation.]

## Low
- [ ] **[Title]** — `path/to/file.rs`
  [Description and mitigation.]
```
