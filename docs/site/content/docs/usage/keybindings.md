+++
title = "Keybindings"
description = "Keyboard shortcuts across lists, session view, diff mode, and prompt input."
weight = 2
+++

<a id="usage-keybindings-introduction"></a>
This page lists keyboard shortcuts for each Agentty view.

For session states and transition behavior, see [Workflow](@/docs/usage/workflow.md).

<!-- more -->

## Session List

| Key | Action |
|-----|--------|
| `q` | Quit |
| `a` | Start new session |
| `s` | Sync |
| `d` | Delete session |
| `c` | Cancel session |
| `Enter` | Open session |
| `e` | Open editor (active project root) |
| `j` / `k` | Navigate sessions |
| `Tab` | Switch tab |
| `?` | Help |

## Project List

| Key | Action |
|-----|--------|
| `q` | Quit |
| `Enter` | Switch active project |
| `b` | Switch to previous project |
| `j` / `k` | Navigate projects |
| `Tab` | Switch tab |
| `?` | Help |

<a id="usage-project-list-active-highlight"></a>
The currently active project is highlighted in the table with a `* ` prefix and
accented row text, even while cursor selection moves to other rows.

## Settings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `k` | Navigate settings |
| `Enter` | Edit setting |
| `Tab` | Switch tab |
| `?` | Help |

<a id="usage-settings-options"></a>
The Settings tab includes:

- `Reasoning Level` (`low`, `medium`, `high`, `xhigh`) for Codex turns.
- `Default Smart Model`, `Default Fast Model`, and `Default Review Model`.
- `Open Command` for launching session worktrees.

## Session View

<a id="usage-session-view-actions"></a>
Available actions depend on the session state. The full set in **Review**
state:

| Key | Action |
|-----|--------|
| `q` | Back to list |
| `Enter` | Reply to agent |
| `o` | Open worktree in tmux |
| `e` | Open editor (session worktree) |
| `d` | Show diff |
| `f` | Show review (read-only) |
| `m` | Queue merge (confirmation popup) |
| `r` | Rebase |
| `Shift+Tab` | Toggle permission mode |
| `j` / `k` | Scroll output |
| `g` | Scroll to top |
| `G` | Scroll to bottom |
| `Ctrl+d` | Half page down |
| `Ctrl+u` | Half page up |
| `?` | Help |

<a id="usage-additional-keys"></a>
Additional notes:

- **Open command behavior**: `o` runs the configured `Open Command` as
  `exec <command>` unless it already starts with `exec`, so the tmux window
  exits when that command ends.
- **InProgress**: `Ctrl+c` stops the agent.
- **Done**: `t` toggles between summary and full output.
- **Review**: Runs in read-only review mode. It can use internet lookup
  and non-editing verification commands, but it should not edit files or
  mutate git/workspace state.

## Diff Mode

| Key | Action |
|-----|--------|
| `q` / `Esc` | Back to session |
| `j` / `k` | Select file |
| `Up` / `Down` | Scroll selected file |
| `?` | Help |

<a id="usage-diff-totals"></a>
The diff panel title shows aggregate line-change totals as `+added` and
`-removed` counts for the current session diff.

## Prompt Input

| Key | Action |
|-----|--------|
| `Enter` | Submit prompt |
| `Shift+Enter` | Insert newline |
| `Option+Backspace` | Delete previous word |
| `Cmd+Backspace` | Delete current line |
| `Esc` | Cancel |
| `@` | Open file picker |
| `/` | Open slash commands |
