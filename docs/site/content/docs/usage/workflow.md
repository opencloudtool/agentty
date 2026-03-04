+++
title = "Workflow"
description = "Interface layout, session lifecycle, slash commands, and data location."
weight = 1
+++

<a id="usage-workflow-introduction"></a>
This page covers the Agentty interface layout, session lifecycle, session sizes,
slash commands, and data location.

For keyboard shortcuts by view, see [Keybindings](@/docs/usage/keybindings.md).

<!-- more -->

## Interface Layout

<a id="usage-interface-layout"></a>
Agentty organizes its interface into four tabs, accessible with `Tab`:

| Tab | Purpose |
|-----|---------|
| **Sessions** | List, create, and manage agent sessions. When a project is active, this tab appears as `Sessions (<project-name>)`. |
| **Projects** | Select between projects (git repositories). |
| **Stats** | View usage statistics. |
| **Settings** | Configure Codex reasoning level, default models, and other preferences. |

## Session Lifecycle

<a id="usage-session-lifecycle"></a>
Session statuses and what you can do in each state:

| Status | Description | Available actions |
|--------|-------------|-------------------|
| **New** | Session created, prompt not yet sent. | `Enter` reply, `m` add to merge queue, `r` rebase, `o` open worktree, `e` open `nvim`, scroll, help |
| **InProgress** | Agent is actively working. | `Ctrl+c` stop, `o` open worktree, `e` open `nvim`, scroll, help |
| **Review** | Agent finished; changes are ready for review. | `Enter` reply, `m` add to merge queue, `r` rebase, `o` open worktree, `e` open `nvim`, `d` diff, `f` focused review, `Shift+Tab` permission mode, scroll, help |
| **Queued** | Session is waiting in the merge queue. | read-only view (`q`, scroll, help) |
| **Rebasing** | Worktree branch is rebasing onto the base branch. | `o` open worktree, `e` open `nvim`, scroll, help |
| **Merging** | Changes are being merged into the base branch. | read-only view (`q`, scroll, help) |
| **Done** | Session completed and merged. | `t` toggle summary/output, scroll, help |
| **Canceled** | Session was canceled by the user. | read-only view (`q`, scroll, help) |

### Typical Transitions

```text
New → InProgress → Review → Done
                         ↘ Canceled
                         ↘ Queued → Merging → Done
                         ↘ Rebasing → Review
```

<a id="usage-title-refinement"></a>
When the first prompt is submitted for a new session, Agentty stores that
prompt as the initial title and starts one background title-generation task
using the configured **Default Fast Model**. That generation runs only once
for session initiation; Agentty does not continuously refresh titles.

## Session Sizes

<a id="usage-session-size"></a>
Agentty classifies sessions by the number of changed lines in their diff:

| Size | Changed Lines |
|------|---------------|
| **XS** | 0-10 |
| **S** | 11-30 |
| **M** | 31-80 |
| **L** | 81-200 |
| **XL** | 201-500 |
| **XXL** | 501+ |

## Slash Commands

<a id="usage-slash-commands"></a>
Type these in the prompt input to access special actions:

| Command | Description |
|---------|-------------|
| `/model` | Switch the model for the current session. |
| `/stats` | Show token usage statistics for the session. |

## Data Location

<a id="usage-data-location"></a>
Agentty stores its data in `~/.agentty/` by default. This includes the
SQLite database, session logs, and worktree checkouts (under `~/.agentty/wt/`).

You can override this location by setting the `AGENTTY_ROOT` environment
variable:

```bash
# Run agentty with a custom root directory
AGENTTY_ROOT=/tmp/agentty-test agentty
```
