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
| **Projects** | Select between projects (git repositories) in a split view: Agentty info (ASCII art, version, short description) on top, project table below. |
| **Stats** | View usage statistics. |
| **Settings** | Configure reasoning level, default models, and `Open Commands` for the active project. |

In session chat view, the status and session title render in a dedicated
header row above the output panel. When a session is linked to a forge review
request, the same row also shows concise PR/MR metadata such as `GitHub #42 Open`.

## Session Lifecycle

<a id="usage-session-lifecycle"></a>
Session statuses and what you can do in each state:

| Status | Description | Available actions |
|--------|-------------|-------------------|
| **New** | Session created, prompt not yet sent. | `Enter` reply, `m` add to merge queue, `r` rebase, `o` open worktree, scroll, help |
| **InProgress** | Agent is actively working. | `o` open worktree, `p` open linked PR/MR, scroll, help |
| **Review** | Agent finished; changes are ready for review. | `Enter` reply, `m` add to merge queue, `r` rebase, `o` open worktree, `p` create/open PR/MR, `d` diff, `f` toggle focused review, scroll, help |
| **Question** | Agent requested clarification before continuing. | question input mode (`Enter` submit, `Esc` skip, text editing keys) |
| **Queued** | Session is waiting in the merge queue. | read-only view (`q`, `p` open linked PR/MR, scroll, help) |
| **Rebasing** | Worktree branch is rebasing onto the base branch. | `o` open worktree, `p` open linked PR/MR, scroll, help |
| **Merging** | Changes are being merged into the base branch. | read-only view (`q`, scroll, help) |
| **Done** | Session completed, merged, and its worktree checkout was removed. | `t` toggle summary/output, `p` refresh linked PR/MR, scroll, help |
| **Canceled** | Session was canceled by the user and its worktree checkout was removed. | read-only view (`q`, `p` refresh linked PR/MR, scroll, help) |

Settings values are stored per active project. Switching projects reloads that
project's `Reasoning Level`, default models, and `Open Commands`.

When a session enters **Review**, Agentty starts generating the focused review
in the background. Pressing `f` opens the cached review immediately when it is
ready, or shows a loading message while generation is still running.

When `Open Commands` in Settings contains multiple entries (one command per
line), pressing `o` opens a selector popup (`j`/`k` to move, `Enter` to open,
`Esc` to cancel).

## Review Request Flow

<a id="usage-review-request-flow"></a>
Session view exposes one forge-generic review-request action on `p`:

- In **Review** without a linked PR/MR, `p` publishes the session branch, then creates or links a GitHub pull request or GitLab merge request.
- While a linked PR/MR is still open, `p` shows the canonical forge URL so you can open it in your browser.
- In **Done**, **Canceled**, or when the stored remote state is no longer open, `p` refreshes the linked PR/MR metadata from the forge CLI and keeps the stored link up to date.

Review-request actions stay inside session view by using an informational popup
for loading, success, and blocked states. Blocked popups include the exact
local CLI fix when `gh` or `glab` is missing, unauthenticated, or pointed at an
unsupported remote.

<a id="usage-review-request-prerequisites"></a>
Review-request actions require the forge CLI that matches the repository remote:

- GitHub remotes require `gh` plus local authentication via `gh auth login`.
- GitLab remotes require `glab` plus local authentication via `glab auth login`.

### Typical Transitions

```text
New → InProgress → Review → Done
                         ↘ Canceled
                         ↘ Question → InProgress
                         ↘ Queued → Merging → Done
                         ↘ Rebasing → Review
```

While a session is **InProgress**, Agentty keeps the `Thinking...` status badge
for streamed activity. Assistant content streams live when available; if
assistant content already streamed, Agentty skips duplicate final-answer append
at turn completion.

<a id="usage-title-refinement"></a>
When the first prompt is submitted for a new session, Agentty stores that
prompt as the initial title and starts one background title-generation task
using the configured **Default Fast Model**. That generation runs only once
for session initiation; Agentty does not continuously refresh titles.

## Clarification Interaction Loop

<a id="usage-clarification-loop"></a>
If an agent emits structured `question` messages, the session moves to
**Question** status. You answer each question in sequence (or press `Esc` to
skip with `no answer`), and Agentty sends one consolidated follow-up message
back to the same session before returning it to normal execution.

<a id="usage-question-options"></a>
Questions may include predefined answer options. When options are present,
Agentty displays them as a numbered list under an "Options:" header with the
first option pre-selected. A "Type custom answer" entry always appears at
the end of the list. Use `j`/`k` or `Up`/`Down` to navigate options and
`Enter` to submit the highlighted choice. If you select "Type custom
answer" and press `Enter`, the option list is replaced by a free-text input
where you can type any response. Press `Esc` at any point to skip the
question with `no answer`.

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

Session size is recalculated after each completed agent turn and persisted to
the session record.

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

Per-session worktree folders are removed automatically after a session reaches
`Done` or `Canceled`, and when a session record is deleted.

You can override this location by setting the `AGENTTY_ROOT` environment
variable:

```bash
# Run agentty with a custom root directory
AGENTTY_ROOT=/tmp/agentty-test agentty
```
