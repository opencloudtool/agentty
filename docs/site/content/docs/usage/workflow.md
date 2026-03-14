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
| **Projects** | Select between projects (git repositories) in a split view: Agentty info (ASCII art, version, short description) on top, project table below. Agentty skips stale entries whose project directories no longer exist. |
| **Stats** | View usage statistics. |
| **Settings** | Configure reasoning level, default models, the session commit coauthor trailer, and `Open Commands` for the active project. |

In session chat view, the status and session title render in a dedicated
header row above the output panel.

## Session Lifecycle

<a id="usage-session-lifecycle"></a>
Session statuses and what you can do in each state:

| Status | Description | Available actions |
|--------|-------------|-------------------|
| **New** | Session created, prompt not yet sent. | `Enter` reply, `m` add to merge queue, `r` rebase, `o` open worktree, scroll, help |
| **InProgress** | Agent is actively working. | `o` open worktree, scroll, help |
| **Review** | Agent finished; changes are ready for review. | `Enter` reply, `m` add to merge queue, `r` rebase, `o` open worktree, `p` publish branch, `d` diff, `f` focused review, scroll, help |
| **Question** | Agent requested clarification before continuing. | question input mode (`Enter` submit, `Esc` skip, text editing keys) |
| **Queued** | Session is waiting in the merge queue. | read-only view (`q`, scroll, help) |
| **Rebasing** | Worktree branch is rebasing onto the base branch. | `o` open worktree, scroll, help |
| **Merging** | Changes are being merged into the base branch. | read-only view (`q`, scroll, help) |
| **Done** | Session completed, merged, and its worktree checkout was removed. | `t` toggle summary/output, scroll, help |
| **Canceled** | Session was canceled by the user and its worktree checkout was removed. | read-only view (`q`, scroll, help) |

Settings values are stored per active project. Switching projects reloads that
project's `Reasoning Level`, default models, `Coauthored by Agentty` toggle,
and `Open Commands`.

When a session enters **Review**, Agentty starts generating the focused review
in the background. Pressing `f` opens the cached review immediately when it is
ready, or shows a loading message while generation is still running.

After each successful turn with file changes, Agentty keeps the session branch
at one evolving commit. It regenerates that commit message from the cumulative
session diff, applies the active project's `Coauthored by Agentty` setting to
the final commit trailer, amends `HEAD`, and refreshes the session title from
the same commit text before merge begins. The session summary panel continues
to show the structured turn/session summary returned by the agent.

When a session enters **Merging**, Agentty reuses the session branch `HEAD`
commit message for the final squash commit on the base branch. Merge still
stops and returns the session to **Review** if rebase or squash-merge git steps
fail, but it no longer runs a separate merge-only commit-message prompt.

When `Open Commands` in Settings contains multiple entries (one command per
line), pressing `o` opens a selector popup (`j`/`k` to move, `Enter` to open,
`Esc` to cancel).

In prompt input, `Ctrl+V` and `Alt+V` paste one clipboard image into the
current new-session prompt or reply. Agentty stores the image under
`AGENTTY_ROOT/tmp/<session-id>/images/`, inserts a highlighted inline token
such as `[Image #1]`, and submits the ordered local attachments with the
prompt. Text paste remains unchanged on the normal terminal paste event path.
Codex turns serialize the local image items directly through the app-server,
Gemini turns send ordered ACP text-plus-image content blocks, and Claude turns
rewrite the inline placeholders to local image paths before the prompt is
streamed to `claude`. Draft image files are removed when you cancel the
composer, after a submitted turn finishes using them, and when a session is
deleted or canceled.

## Branch Publish Flow

<a id="usage-review-request-flow"></a>
Session view exposes one manual branch-publish action on `p`:

- In **Review**, `p` opens a publish popup for the session branch.
- Leave the field empty to keep the default branch target for that session, or type a custom remote branch name before pressing `Enter`.
- After the session branch already tracks a remote branch, Agentty locks the popup to that same remote branch instead of allowing renames.
- After the push succeeds, Agentty shows the branch name and, for GitHub or GitLab remotes, a forge-native link you can open to start the pull request or merge request.

Branch-publish actions stay inside session view by using a publish input popup
followed by informational popups for loading, success, and blocked states.

<a id="usage-review-request-prerequisites"></a>
Branch publishing on `p` uses regular Git authentication only:

- HTTPS remotes need a working credential helper or PAT.
- SSH remotes need a working SSH key.

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
Questions always include predefined answer options. Agentty displays them as
a numbered list under an "Options:" header with the first option
pre-selected. A "Type custom answer" entry always appears at the end of the
list. Use `j`/`k` or `Up`/`Down` to navigate options and `Enter` to submit
the highlighted choice. If you select "Type custom answer" and press
`Enter`, the option list is replaced by a free-text input where you can type
any response. Press `Esc` at any point to skip the question with
`no answer`.

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

## Auto-Update

<a id="usage-auto-update"></a>
When Agentty launches, it checks npmjs for a newer version in the background.
If a newer version is detected, it automatically runs `npm i -g agentty@latest`
without blocking the UI:

| Status bar text | Meaning |
|-----------------|---------|
| **Updating to vX.Y.Z...** | Background npm install is running. |
| **Updated to vX.Y.Z — restart to use new version** | Install succeeded; relaunch to use the new version. |
| **vX.Y.Z version available update with npm i -g agentty@latest** | Install failed; manual update hint shown as fallback. |

To disable automatic updates, launch with `--no-update`:

```bash
agentty --no-update
```

When `--no-update` is set, Agentty still checks for newer versions and shows the
manual update hint, but does not run `npm i -g agentty@latest` automatically.

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
