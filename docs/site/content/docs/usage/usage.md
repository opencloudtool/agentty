+++
title = "Workflow & Keybindings"
description = "Session lifecycle, keybinding reference, and slash commands."
weight = 1
+++

This page covers the Agentty interface layout, session lifecycle, keybinding
reference, and slash commands.

<!-- more -->

## Interface Layout

Agentty organizes its interface into four tabs, accessible with `Tab`:

| Tab | Purpose |
|-----|---------|
| **Sessions** | List, create, and manage agent sessions. |
| **Projects** | Switch between projects (git repositories). |
| **Stats** | View usage statistics. |
| **Settings** | Configure default smart/fast/review models and other preferences. |

## Session Lifecycle

Each session moves through a series of states:

| Status | Description |
|--------|-------------|
| **New** | Session created, prompt not yet sent. |
| **InProgress** | Agent is actively working. |
| **Review** | Agent finished; changes ready for review. |
| **Queued** | Session is waiting in the merge queue. |
| **Rebasing** | Worktree branch is being rebased onto the base branch. |
| **Merging** | Changes are being merged into the base branch. |
| **Done** | Session completed and merged. |
| **Canceled** | Session was canceled by the user. |

### Typical Transitions

```text
New → InProgress → Review → Done
                         ↘ Canceled
                         ↘ Queued → Merging → Done
                         ↘ Rebasing → Review
```

## Session Sizes

Agentty classifies sessions by the number of changed lines in their diff:

| Size | Changed Lines |
|------|---------------|
| **XS** | 0–10 |
| **S** | 11–30 |
| **M** | 31–80 |
| **L** | 81–200 |
| **XL** | 201–500 |
| **XXL** | 501+ |

## Keybindings

### Session List

| Key | Action |
|-----|--------|
| `q` | Quit |
| `a` | Start new session |
| `s` | Sync |
| `d` | Delete session |
| `c` | Cancel session |
| `Enter` | Open session |
| `e` | Open project explorer (active project root) |
| `j` / `k` | Navigate sessions |
| `Tab` | Switch tab |
| `?` | Help |

### Project List

| Key | Action |
|-----|--------|
| `q` | Quit |
| `Enter` | Switch active project |
| `b` | Switch to previous project |
| `j` / `k` | Navigate projects |
| `Tab` | Switch tab |
| `?` | Help |

### Settings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `k` | Navigate settings |
| `Enter` | Edit setting |
| `Tab` | Switch tab |
| `?` | Help |

### Session View

Available actions depend on the session state. The full set in **Review**
state:

| Key | Action |
|-----|--------|
| `q` | Back to list |
| `Enter` | Reply to agent |
| `o` | Open worktree in tmux |
| `e` | Open project explorer (session worktree) |
| `d` | Show diff |
| `f` | Show focused review (read-only) |
| `m` | Queue merge |
| `r` | Rebase |
| `Shift+Tab` | Toggle permission mode |
| `j` / `k` | Scroll output |
| `g` | Scroll to top |
| `G` | Scroll to bottom |
| `Ctrl+d` | Half page down |
| `Ctrl+u` | Half page up |
| `?` | Help |

Additional state-specific keys:

- **InProgress**: `Ctrl+c` stops the agent.
- **Done**: `t` toggles between summary and full output.
- **Focused review**: Runs in read-only review mode. It can use internet lookup
  and non-editing verification commands, but it should not edit files or
  mutate git/workspace state.

### Project Explorer

| Key | Action |
|-----|--------|
| `q` / `Esc` | Back |
| `Enter` | Open file or toggle directory |
| `j` / `k` | Select entry |
| `Up` / `Down` | Scroll preview |
| `?` | Help |

When opened from the **Sessions** list, explorer rows come from the active
project root. When opened from **Session View**, rows come from that session's
worktree.

### Diff Mode

| Key | Action |
|-----|--------|
| `q` / `Esc` | Back to session |
| `j` / `k` | Select file |
| `Up` / `Down` | Scroll selected file |
| `?` | Help |

The diff panel title shows aggregate line-change totals as `+added` and
`-removed` counts for the current session diff.

### Prompt Input

| Key | Action |
|-----|--------|
| `Enter` | Submit prompt |
| `Shift+Enter` | Insert newline |
| `Option+Backspace` | Delete previous word |
| `Cmd+Backspace` | Delete current line |
| `Esc` | Cancel |
| `@` | Open file picker |
| `/` | Open slash commands |

## Slash Commands

Type these in the prompt input to access special actions:

| Command | Description |
|---------|-------------|
| `/model` | Switch the model for the current session. |
| `/stats` | Show token usage statistics for the session. |

## Data Location

Agentty stores its data in `~/.agentty/`. This includes the SQLite database,
session logs, and worktree checkouts (under `~/.agentty/wt/`).
