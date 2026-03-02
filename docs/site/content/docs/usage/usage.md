+++
title = "Workflow & Keybindings"
description = "Session lifecycle, keybinding reference, and slash commands."
weight = 1
+++

<a id="usage-introduction"></a>
This page covers the Agentty interface layout, session lifecycle, keybinding
reference, and slash commands.

<!-- more -->

## Interface Layout

<a id="usage-interface-layout"></a>
Agentty organizes its interface into four tabs, accessible with `Tab`:

| Tab | Purpose |
|-----|---------|
| **Sessions** | List, create, and manage agent sessions. |
| **Projects** | Switch between projects (git repositories). |
| **Stats** | View usage statistics. |
| **Settings** | Configure default smart/fast/review models and other preferences. |

## Session Lifecycle

<a id="usage-session-lifecycle"></a>
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

<a id="usage-title-refinement"></a>
When the first prompt is submitted for a new session, Agentty stores that
prompt as the initial title, then runs a background title refinement task using
the configured **Default Fast Model**.

## Session Sizes

<a id="usage-session-size"></a>
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
| `e` | Open `nvim` (active project root) |
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

<a id="usage-project-list-active-highlight"></a>
The currently active project is highlighted in the table with a `* ` prefix and
accented row text, even while cursor selection moves to other rows.

### Settings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `k` | Navigate settings |
| `Enter` | Edit setting |
| `Tab` | Switch tab |
| `?` | Help |

### Session View

<a id="usage-session-view-actions"></a>
Available actions depend on the session state. The full set in **Review**
state:

| Key | Action |
|-----|--------|
| `q` | Back to list |
| `Enter` | Reply to agent |
| `o` | Open worktree in tmux |
| `e` | Open `nvim` (session worktree) |
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

<a id="usage-additional-keys"></a>
Additional state-specific keys:

- **Open command behavior**: `o` runs the configured `Open Command` as
  `exec <command>` unless it already starts with `exec`, so the tmux window
  exits when that command ends.
- **InProgress**: `Ctrl+c` stops the agent.
- **Done**: `t` toggles between summary and full output.
- **Focused review**: Runs in read-only review mode. It can use internet lookup
  and non-editing verification commands, but it should not edit files or
  mutate git/workspace state.

### Diff Mode

| Key | Action |
|-----|--------|
| `q` / `Esc` | Back to session |
| `j` / `k` | Select file |
| `Up` / `Down` | Scroll selected file |
| `?` | Help |

<a id="usage-diff-totals"></a>
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

<a id="usage-slash-commands"></a>
Type these in the prompt input to access special actions:

| Command | Description |
|---------|-------------|
| `/model` | Switch the model for the current session. |
| `/stats` | Show token usage statistics for the session. |

## Data Location

<a id="usage-data-location"></a>
Agentty stores its data in `~/.agentty/`. This includes the SQLite database,
session logs, and worktree checkouts (under `~/.agentty/wt/`).
