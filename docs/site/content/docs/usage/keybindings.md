+++
title = "Keybindings"
description = "Keyboard shortcuts across lists, session view, diff mode, prompt input, and question input."
weight = 2
+++

<a id="usage-keybindings-introduction"></a> This page lists keyboard shortcuts for each
Agentty view.

For session states and transition behavior, see [Workflow](@/docs/usage/workflow.md).

<!-- more -->

## Shared Text Editing

Prompt, question, publish-branch, and launch-configuration inputs share the same basic
editing shortcuts. Context-specific actions such as prompt submission, slash commands,
`@` completion, and question-option navigation run before these common fallbacks.

| Key                                      | Action                              |
| ---------------------------------------- | ----------------------------------- |
| `Left` / `Right`                         | Move one character                  |
| `Option+Left` / `Shift+Left` / `Alt+B`   | Move to previous word               |
| `Option+Right` / `Shift+Right` / `Alt+F` | Move to next word                   |
| `Home` / `End`                           | Move to start / end of input        |
| `Ctrl+A` / `Ctrl+E`                      | Move to start / end of current line |
| `Backspace` / `Delete`                   | Delete backward / forward           |
| `Option+Backspace` / `Shift+Backspace`   | Delete previous word                |
| `Ctrl+W`                                 | Delete previous word                |
| `Cmd+Backspace` / `Ctrl+U`               | Delete current line                 |
| `Ctrl+K`                                 | Delete to end of current line       |
| `Ctrl+Z`                                 | Undo                                |
| `Ctrl+Y` / `Ctrl+Shift+Z`                | Redo                                |
| paste                                    | Insert text at the cursor           |

On macOS, undo still uses `Ctrl+Z`, not `Cmd+Z`. Terminal applications such as Ghostty
may consume `Cmd+Z` before Agentty or a surrounding `tmux` session receives it.

Multiline prompt and question inputs also share vertical cursor movement and newline
insertion. Single-line publish and launch-configuration inputs keep only the first line
of pasted text.

## Session List

| Key                 | Action                                               |
| ------------------- | ---------------------------------------------------- |
| `q`                 | Quit                                                 |
| `a`                 | Check hooks, then open the session creation selector |
| `s`                 | Sync active project branch                           |
| `c`                 | Cancel selected session after confirmation           |
| `Enter`             | Open session                                         |
| `j` / `k`           | Navigate sessions                                    |
| `p`                 | Open project switcher popup                          |
| `Tab` / `Shift+Tab` | Switch to next / previous tab                        |
| `?`                 | Help                                                 |

Project sync is non-modal. While `s` is running, navigation, project switching, and
isolated session work continue; repeated `s` presses coalesce. Creating or starting a
draft session, merging, and rebasing against the syncing project's base checkout wait
for a retry after the status bar reports completion.

If pre-commit configuration exists without an executable hook, `a` first opens a
warning. Press `Enter` to continue to the `Regular`, `Draft`, `Orchestrator`, `Stacked`,
or `Append to stack` selector, or `Esc` / `q` to cancel. `Orchestrator` is marked
`[Preview]`, as is `Append to stack`.

Choosing `Regular` or `Orchestrator` opens the composer while workspace setup runs in
background. Submit immediately to save the prompt until setup completes. Completion
preserves your input and navigation. If setup fails, press `s` from session view to
retry. `Draft` keeps `Enter` for staging and `s` for starting the staged prompt.

In the `a` selector, `Stacked` is enabled only when the selected session can parent a
new draft within the five-level stack limit. `Append to stack` is enabled only for an
independent **Review** or **AgentReview** session with an eligible idle parent. Select
the action, choose the destination parent with `j` / `k`, and press `Enter` to move and
sync the session branch. Long parent lists keep the current selection visible while you
navigate. `c` appears only for cancelable rows: running sessions, review-ready sessions,
unstarted draft sessions, and draft orchestrators. Canceling a running orchestrator
opens a confirmation that names its running-child count and cascades to those children.

<a id="usage-session-list-project-switcher"></a> The `p` popup lists registered projects
in most-recently-opened order with the active project marked by a `* ` prefix. Each row
shows `▶ N` for projects with running sessions and stays blank otherwise. Use `j` / `k`
to move, `Enter` to switch the active project without leaving the Sessions view, and
`Esc` or `q` to close.

## Project List

| Key                 | Action                        |
| ------------------- | ----------------------------- |
| `q`                 | Quit                          |
| `s`                 | Sync active project branch    |
| `Enter`             | Select active project         |
| `j` / `k`           | Navigate projects             |
| `Tab` / `Shift+Tab` | Switch to next / previous tab |
| `?`                 | Help                          |

The same non-modal project-sync behavior and base-checkout safety gates apply from the
Projects list.

<a id="usage-project-list-active-highlight"></a> The currently active project is
highlighted in the table with a `* ` prefix and accented row text.

## Settings

<table>
<thead>
<tr><th>Key</th><th>Action</th></tr>
</thead>
<tbody>
<tr><td><code>q</code></td><td>Quit; closes an open selector dropdown or <code>Launch Configurations</code> browser first</td></tr>
<tr><td><code>s</code></td><td>Sync active project branch</td></tr>
<tr><td><code>j</code> / <code>k</code></td><td>Navigate settings; move inside an open selector dropdown or <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>Enter</code></td><td>Open selector dropdown or <code>Launch Configurations</code> browser; continue from model to reasoning; save the highlighted value; edit or save a launch configuration</td></tr>
<tr><td><code>Esc</code></td><td>Close selector dropdown or <code>Launch Configurations</code> browser; cancel launch-configuration add/edit input</td></tr>
<tr><td><code>a</code></td><td>Add an entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>e</code></td><td>Edit the selected entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>d</code></td><td>Delete the selected entry in the <code>Launch Configurations</code> browser</td></tr>
<tr><td><code>J</code> / <code>K</code></td><td>Move the selected <code>Launch Configurations</code> entry down or up</td></tr>
<tr><td>shared text-editing keys</td><td>Edit, move by character or word, paste, undo, or redo while adding or editing one <code>Launch Configurations</code> entry</td></tr>
<tr><td><code>Tab</code> / <code>Shift+Tab</code></td><td>Switch to next / previous tab</td></tr>
<tr><td><code>?</code></td><td>Help</td></tr>
</tbody>
</table>

<a id="usage-settings-options"></a> The page is split into `Global settings` for the
app-wide `Theme` row (`Agentty Default`, `Agentty Green`, or `Dark Horizon`) and
`'<project>' settings` for Smart, Fast, and Review `agent/model [reasoning]` defaults,
the commit coauthor toggle, and `Launch Configurations` rows described in
[Workflow](@/docs/usage/workflow.md). Selector rows open dropdowns; use `j` / `k` to
move through the dropdown. For a role default, press `Enter` after choosing the model,
then choose and save its reasoning level with `Enter`. Other selectors save directly.
The `Launch Configurations` row opens a list browser where each command is added,
edited, deleted, or reordered as its own entry.

## Session View

<a id="usage-session-view-actions"></a> Available actions depend on the session state.
The full set in **Review** state, subject to session and forge availability:

| Key                 | Action                                              |
| ------------------- | --------------------------------------------------- |
| `q`                 | Back to list                                        |
| `Enter`             | Compose a reply                                     |
| `/`                 | Open composer with `/` prefilled                    |
| `o`                 | Run a launch configuration in the worktree (`tmux`) |
| `p`                 | Publish branch and create or refresh review request |
| `c`                 | Show linked review-request comments                 |
| `d`                 | Show diff when the session has changes              |
| `f`                 | Append or regenerate focused review output          |
| `F`                 | Fork session with copied transcript history         |
| `m`                 | Add to merge queue after confirmation               |
| `r`                 | Sync session branch                                 |
| `j` / `k`           | Scroll output                                       |
| `g` / `G`           | Scroll to top / bottom                              |
| `Ctrl+d` / `Ctrl+u` | Half page down / up                                 |
| `?`                 | Help                                                |

State-specific differences:

- Sessions with a known-empty diff hide `d`; pressing `d` keeps Session View in place.
  Before opening the writable worktree, Agentty invalidates that cached result so later
  external edits remain inspectable across restarts. If durable invalidation fails, the
  worktree stays closed.

- **AgentReview** keeps the review shortcuts, including `r`. Pressing `r` starts session
  sync immediately and cancels the pending focused review so stale review output cannot
  appear after the rebase begins.

- **InProgress** sessions use `Enter` to queue a follow-up message, keep `r` available
  to queue session sync, and keep `p` available to queue review-request creation behind
  the running turn. Each `Ctrl+c` retracts the newest queued message; when the queue is
  empty, `Ctrl+c` stops the active turn.

- **Rebasing** sessions use `Enter` to queue a follow-up message and keep `p` available
  to queue review-request creation behind the active session sync. Slash commands and
  other branch actions remain unavailable until sync finishes.

- Root **Review** and **AgentReview** sessions offer `F` to fork the current branch and
  copied transcript history into a new independent session; stacked children hide `F`.

- **Draft** sessions use `Enter` to add a staged message and `s` to start the staged
  session. They hide `o` and `r` until launch and let `Ctrl+V`, `Ctrl+Shift+V`, or
  `Alt+V` open the composer with an image paste; stacked drafts also hide `m` and show
  `s` only when the parent is review-ready and the stack is idle.

- **Question** sessions hide `r` until they return to review-ready state.

- **Orchestrator** sessions use a campaign board above chat. On a parked plan, `a`
  approves the plan; after verification, `a` opens a choice between local merges and
  review requests. Worker parallelism comes from the global **Orchestrator Parallelism**
  setting. Read-only research waves also use that cap and can start without approval
  through **Auto-approve Research**. Controllers hide branch actions: `d`, `o`, `p`,
  `F`, `m`, and `r`.

- Managed orchestration workers restrict direct Agentty actions. `d` opens their diff,
  `D` confirms a one-way detach into a regular user-owned session, and, when Agentty is
  running inside `tmux`, a worker in **Review** exposes `o` to open its materialized
  worktree. The confirmation warns that the shell has normal write access and edits can
  invalidate orchestration verification. Reply, slash-command, publish, fork, merge,
  sync, cancel, linked review-comment, and direct question-answer actions stay hidden;
  `Ctrl+c` is ignored while the worker remains managed. After a managed merge removes
  the worktree, `d` reads the immutable diff archived during integration.

- Temporary research children expose their transcript and `d` evidence while active, but
  hide `D` and `o`: their worktree is always reclaimed after report capture and cannot
  be transferred into a user-owned session. After cleanup, `d` reads the archived
  observed diff so unexpected writes remain inspectable.

- Review-ready stacked parents with a materialized child keep `Enter`, `/`, `m`, and `r`
  while the stack is idle.

- Sessions with a linked pull request or merge request use `c` to open Diff mode focused
  on its Comments section and hide `m`; merge the linked request through its forge
  instead of Agentty's local merge queue.

- **Merged** sessions remain in Active and expose only read-only navigation, linked
  review comments, and `d` for the diff until list-mode `s` successfully syncs their
  local target branch.

- **Done** and **Canceled** sessions offer `c` to start a continuation draft
  (confirmation popup). Done-session drafts use the merged commit hash when available;
  canceled-session drafts use the saved transcript or original prompt. Both terminal
  states hide linked review comments so continuation remains unambiguous.

- **Queued** and **Merging** sessions are otherwise read-only (`q`, scroll, help).
  Linked review requests remain available from other session states with `c`.

`o` is available only when Agentty runs inside `tmux`. It runs the configured
`Launch Configurations` entry, or opens a selector popup when several are configured,
because those commands are dispatched into tmux windows. Publish (`p`), sync (`r`), and
stacked behavior are described in [Workflow](@/docs/usage/workflow.md).

## Review Comments in Diff Mode

For linked review requests, Diff mode divides its left sidebar into Files and Comments.
`d` opens the workspace focused on Files, while `c` opens the same workspace focused on
Comments. The Comments section groups unresolved, outdated, and resolved threads plus
standalone review-request comments. The Outdated group contains unresolved threads with
stale anchors; resolved threads stay in the Resolved group even when their anchors are
outdated. Selecting a comment replaces the right diff pane with its author, resolution
state, outdated-anchor metadata when applicable, current diff context for its attached
line or range, and conversation. Comment bodies render Markdown and common embedded HTML
without showing HTML comments. Outdated threads explicitly report that their original
context is unavailable instead of mapping the stale anchor onto the current diff.
File-level comments similarly show that they have no attached code line instead of
highlighting an arbitrary diff row. Current inline snippets use the same gutters and
added/removed line colors as diff view.

| Key           | Action                                   |
| ------------- | ---------------------------------------- |
| `q` / `Esc`   | Return to session view                   |
| `j` / `k`     | Select previous/next comment             |
| `f`           | Focus the Files section                  |
| `Up` / `Down` | Scroll selected comment info             |
| `Space`       | Toggle the selected actionable thread    |
| `Enter`       | Submit all selected threads to the agent |

Actionable rows start with `[ ]` and show `[x]` when selected. Pressing `Space` again
clears that row. `Space` and `Enter` appear only while the session can accept a turn,
and `Enter` appears only after at least one row is selected. Unresolved outdated threads
remain actionable through their forge thread ID, although their original code context is
no longer available. Submission returns the session to **InProgress** with a count-aware
resolution loader; the generated batch prompt tells the agent to evaluate each selected
comment, address it when needed, and post a very short explanation of what was done and
why in every case. The prompt remains hidden from chat. Resolved threads and standalone
comments are read-only.

## Publish Popup

| Key                      | Action                                             |
| ------------------------ | -------------------------------------------------- |
| `Enter`                  | Publish typed or default target in the background  |
| `Esc`                    | Cancel and return to session view                  |
| shared text-editing keys | Edit, paste, move, delete, undo, or redo           |
| text keys                | Edit remote branch name, including the character q |

## Launch Configuration Selector

| Key         | Action                                 |
| ----------- | -------------------------------------- |
| `j` / `k`   | Move selection                         |
| `Enter`     | Open worktree and run selected command |
| `Esc` / `q` | Cancel and return to session view      |

## Diff Mode

Pressing `d` from session view opens Diff mode focused on Files with the right panel
showing the git diff. While Files remains focused, use `Shift+j` / `Shift+k` or `Up` /
`Down` to scroll the selected file without moving focus. Press `Enter` or `l` on a file
to focus its changes, or press `Shift+C` to open a whole-file comment above its patch.
Within the patch, `Shift+C` also opens the whole-file comment, including while a visual
row selection is active; opening it clears that row selection. Press `Enter` to edit the
selected changed line inline. File editors are titled `File comment`; inline editors
identify added ranges as `New line N` or `New lines N-M` and deleted ranges as
`Old line N` or `Old lines N-M`. A selection spanning both sides shows both ranges. Use
`Alt+Enter` or `Shift+Enter` to add lines, and finish with `Enter` or `Esc`. Then use
`j` / `k` or the arrow keys to move through changed lines and completed file or inline
comments. Press `Enter` on a selected comment to edit its text again. Press `Shift+V` on
a changed line to start a visual changed-row selection, extend it with the same
navigation keys, then press `Enter` to comment on the range or `Esc` to cancel the
selection. The range stays highlighted while its inline editor is open and after the
comment is finished, so the comment's source remains visible. `Esc`, `Left`, `h`, or `f`
returns focus to the file tree when no visual selection is active. Linked review
requests add a Comments section below Files; `c` focuses Comments while the file tree is
focused, and `f` returns to Files. The Comments section keeps its own `Enter` action for
submitting marked review threads. Press `s` to submit every file, line, and range
comment together in the next turn from any Diff pane.

| Key                         | Action                                         |
| --------------------------- | ---------------------------------------------- |
| `q`                         | Back to session                                |
| `Esc`                       | Focus Files, or leave from Files               |
| `j` / `k`                   | Select a file, changed line, or inline comment |
| `Shift+j` / `Shift+k`       | Scroll selected file, or select a diff row     |
| `Shift+C`                   | Comment on the selected whole file             |
| `Shift+V`                   | Start visual changed-row selection             |
| `Alt+Enter` / `Shift+Enter` | Insert a comment newline                       |
| `Enter`                     | Focus a file, or edit/finish a comment         |
| `l`                         | Focus the selected file's changes              |
| `Up` / `Down`               | Scroll file/preview, or select a diff row      |
| `Left` / `h` / `f`          | Return to Files                                |
| `p`                         | Toggle markdown preview                        |
| `c`                         | Focus linked review comments                   |
| `s`                         | Submit all diff comments                       |
| `?`                         | Help                                           |

<a id="usage-diff-totals"></a> The diff panel title includes aggregate `+added` and
`-removed` line totals. Every file and folder row in the left panel shows its own
right-aligned `+added/-removed` counts. Uninterrupted single-child folder chains are
shown on one compact row, such as `docs/site/content/`, to fit more changed paths in the
Files panel without flattening branches. Top-level paths omit a redundant tree connector
so labels begin at the panel's left content edge.

On a selected `.md` file, `p` replaces the raw patch with the rendered post-change
worktree file. Headings, lists, tables, code blocks, and supported Mermaid diagrams use
the same renderer as session output. Preview stays enabled while navigating: another
markdown file loads automatically, while folders and other file types continue showing
their normal diff. Deleted, binary, oversized, and unreadable markdown files show a
short availability notice. Press `p` again to return to raw diff lines.

Type `@` in a diff comment to look up repository files. `Up` / `Down` navigate matches,
and `Tab` / `Enter` insert the selected path without finishing the comment. `Esc`
dismisses the lookup while preserving the draft; press it again to finish editing. With
no matches, `Tab` / `Enter` dismiss the lookup. Modified `Enter` still inserts a
newline.

Whole-file comments stay visible above the selected file's patch, while line and range
comments stay beneath their source. Press `Enter` again on a completed comment to edit
it; clearing its text and finishing removes it. Completed comments use a distinct inset
background, while the active editor uses the stronger selection highlight. Completed
comments survive leaving Diff mode and return when the same session's diff is reopened.
`s` combines all finished comments with any draft text and image attachments that were
present before opening the diff, then submits the batch as one session turn. Submission,
or any other new turn in that session, clears the saved comments when the turn starts.
If submission is queued behind active work, `Ctrl+C` can retract that queued message
without losing the comments. The chat renders file comments by path and inline comments
by path, line or range, side, and feedback. Comments containing deleted lines also
include their captured pre-change source text so the agent retains context that is
absent from the worktree.

Read-only diffs, including `Merged` sessions, keep changed-line navigation but hide file
and inline comment shortcuts and batch submission.

## Prompt Input

| Key                                 | Action                              |
| ----------------------------------- | ----------------------------------- |
| `Enter`                             | Send prompt or stage it in a draft  |
| `Alt+Enter` / `Shift+Enter`         | Insert newline                      |
| `Ctrl+J` / `Ctrl+M`                 | Insert newline (terminal fallback)  |
| `Ctrl+V` / `Ctrl+Shift+V` / `Alt+V` | Paste image as `[Image #n]`         |
| `Cmd+Left` / `Cmd+Right`            | Move to start / end of current line |
| `Option+Left` / `Option+Right`      | Move to previous / next word        |
| `Option+Backspace`                  | Delete previous word                |
| `Cmd+Backspace`                     | Delete current line                 |
| `Ctrl+Z`                            | Undo                                |
| `Ctrl+Y` / `Ctrl+Shift+Z`           | Redo                                |
| `Esc`                               | Cancel                              |
| `Tab`                               | Focus chat output for scrolling     |
| `Shift+Tab`                         | Cycle the session permission mode   |
| `@`                                 | Open file picker                    |
| `/`                                 | Open slash commands                 |
| `j` / `k` / `Up` / `Down`           | Navigate and wrap slash menu        |

Use `/mode` to select `Auto Edit`, `Auto Edit + Auto Address Comments`, or `Read Only`.
`Shift+Tab` cycles those modes in that order.

While the chat output is focused, the `d` diff-preview hint is hidden only when the
latest successful refresh found an empty diff against the session's base branch. The
shortcut remains available for text, binary, metadata-only, and diagnostic diff output.
`j` / `k` / `Up` / `Down` scroll the transcript, `g` / `G` jump to the top or bottom,
`Ctrl+D` / `Ctrl+U` scroll by half a page, `Tab` returns focus to the composer, and `q`
returns to the sessions list. The typed draft is preserved when leaving with `q`:
reopening the session restores the composer with input focus. Other keys pressed in chat
focus never edit the draft, and `Ctrl+C` is ignored. Leaving the diff preview also
returns to the composer with the draft intact.

Prompt input keeps regular text paste on terminal `Event::Paste`. The dedicated image
paste shortcuts insert highlighted `[Image #n]` tokens directly in the composer and send
the referenced local image for Codex, Gemini, Antigravity, and Claude session models.
Clipboard image capture uses Agentty's host clipboard backend, with Wayland reads using
`wl-paste` when it is available. Missing or unsupported clipboard backends report an
inline paste error. Codex preserves the multimodal ordering at transport level, while
Antigravity and Claude rewrite the placeholders to local image paths before streaming
the prompt.

Agentty requests Kitty keyboard reporting from supporting terminals so modified keys
remain distinct without changing shifted punctuation such as `@`. Inside `tmux`, it also
requests xterm modified-key reporting so panes can translate `Shift+Enter` to CSI-u.
Image paste and `@` lookup behavior are described in
[Workflow](@/docs/usage/workflow.md).

## Question Input — Option Selection

When predefined options are shown:

| Key                       | Action                          |
| ------------------------- | ------------------------------- |
| `j` / `k` / `Up` / `Down` | Navigate options                |
| `Enter`                   | Send highlighted option         |
| `Tab`                     | Focus chat output for scrolling |
| `q`                       | Return to sessions list         |
| `Ctrl+C`                  | End turn without answering      |

## Question Input — Free Text

After moving above or below the predefined option list, or when no predefined options
exist:

| Key                              | Action                               |
| -------------------------------- | ------------------------------------ |
| `Enter`                          | Send response; blank means no answer |
| `Alt+Enter` / `Shift+Enter`      | Insert newline                       |
| `Ctrl+J` / `Ctrl+M`              | Insert newline (terminal fallback)   |
| `Ctrl+C`                         | End turn without answering           |
| `Left` / `Right` / `Up` / `Down` | Move cursor                          |
| `Backspace` / `Delete`           | Delete character                     |
| `Home` / `End`                   | Move to start / end                  |
| `Cmd+Left` / `Cmd+Right`         | Move to start / end of current line  |
| `Option+Left` / `Option+Right`   | Move to previous / next word         |
| `Option+Backspace` / `Ctrl+W`    | Delete previous word                 |
| `Cmd+Backspace`                  | Delete current line                  |
| `Ctrl+K`                         | Delete to end of current line        |
| `Ctrl+D`                         | Delete character forward             |
| `Ctrl+Z`                         | Undo                                 |
| `Ctrl+Y` / `Ctrl+Shift+Z`        | Redo                                 |
| `Tab`                            | Focus chat output for scrolling      |

Type `@` followed by a file-name fragment to look up repository files in your answer.
While the lookup has matches, `Up` / `Down` navigate them and `Tab` / `Enter` replace
the entire `@` token with the selected path, including any query text after the cursor.
With no matches, `Tab` / `Enter` close the lookup. `Esc` always dismisses the lookup,
and `Alt+Enter` / `Shift+Enter` still insert a newline. After selecting a file with
`Enter`, press `Enter` again to send the answer; `Tab` switches focus as usual. When the
terminal is too short to show a result row, the footer hides lookup selection hints;
`Esc` still dismisses the lookup.

In free-text mode every other printable character — including `q` — is inserted into the
answer. To leave without answering, press `Tab` to focus the chat output and then `q`,
or press `Ctrl+C` while the answer input is focused.

## Question Input — Chat Scroll

When chat output is focused (press `Tab` to switch):

| Key                       | Action                            |
| ------------------------- | --------------------------------- |
| `j` / `k` / `Up` / `Down` | Scroll chat output                |
| `g` / `G`                 | Scroll to top / bottom            |
| `Ctrl+d` / `Ctrl+u`       | Half page down / up               |
| `d`                       | Open available diff or diagnostic |
| `Tab`                     | Return focus to answer input      |
| `q`                       | Return to sessions list           |

<a id="usage-question-input-submit-flow"></a> After the last question is answered,
Agentty sends one follow-up message with each question and its response, then returns to
session view. Pressing `q` (outside free-text input) returns to the sessions list while
leaving the session in **Question** state; answers already submitted and the current
free-text draft are kept, so reopening the session resumes at the next unanswered
question.
