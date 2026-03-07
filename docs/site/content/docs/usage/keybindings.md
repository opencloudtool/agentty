+++
title = "Keybindings"
description = "Keyboard shortcuts across lists, session view, diff mode, prompt input, and question input."
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
| `Enter` | Select active project |
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
| `Enter` | Edit setting / finish text edit |
| `Alt+Enter` or `Shift+Enter` | Add newline while editing `Open Commands` |
| `Up` / `Down` / `Left` / `Right` | Move cursor while editing `Open Commands` |
| `Tab` | Switch tab |
| `?` | Help |

<a id="usage-settings-options"></a>
The Settings tab includes:

- `Reasoning Level` (`low`, `medium`, `high`, `xhigh`) for Codex turns in the active project.
- `Default Smart Model`, `Default Fast Model`, and `Default Review Model` for the active project.
- `Open Commands` for launching session worktrees in the active project (one command per line).

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
| `m` | Add to merge queue (confirmation popup) |
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

- **Open command behavior**: `o` always opens the session worktree in tmux.
  If one `Open Commands` entry is configured for the active project, it runs immediately.
  If multiple entries are configured (one command per line), Agentty opens a selector popup.
- **Question**: opening the session enters Question Input mode until all prompts are answered or skipped.
- **Done**: `t` toggles between summary and full output.
- **Review**: Runs in read-only review mode. It can use internet lookup
  and non-editing verification commands, but it should not edit files or
  mutate git/workspace state.

## Open Command Selector

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection |
| `Enter` | Open worktree and run selected command |
| `Esc` / `q` | Cancel and return to session view |

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
| `Alt+Enter` or `Shift+Enter` | Insert newline |
| `Option+Backspace` | Delete previous word |
| `Cmd+Backspace` | Delete current line |
| `Esc` | Cancel |
| `@` | Open file picker |
| `/` | Open slash commands |

## Question Input

| Key | Action |
|-----|--------|
| `Enter` | Submit response for current question |
| `Esc` | Skip current question (`no answer`) |
| `Left` / `Right` | Move cursor |
| `Up` / `Down` | Move cursor across wrapped lines |
| `Backspace` / `Delete` | Delete character |
| `Home` / `End` | Move cursor to start/end |
| `Ctrl+u` | Delete current line |
| `Tab` | Insert tab |

<a id="usage-question-input-submit-flow"></a>
After the last question is answered (or skipped), Agentty sends one follow-up
message to the session with each question and its response, then returns to
session view.
