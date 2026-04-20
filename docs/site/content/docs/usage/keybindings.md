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
| `Shift+A` | Start draft session |
| `s` | Sync |
| `c` | Cancel the selected review session or unstarted draft session (confirmation popup) |
| `Enter` | Open session |
| `j` / `k` | Navigate sessions |
| `Tab` | Switch tab |
| `?` | Help |

The rotating status-bar `FYI:` messages on the **Sessions** list currently
surface `Enter` for opening the selected session and remind you that Agentty
refreshes PR statuses every minute.

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

On the **Sessions** list, `c` appears only when the selected row is
cancelable: review-ready sessions and draft sessions that are still in
`New` before their first staged bundle starts.

## Settings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `k` | Navigate settings |
| `Enter` | Edit setting / finish text edit |
| `Esc` | Finish text edit |
| `Alt+Enter` or `Shift+Enter` | Add newline while editing `Open Commands` |
| `Up` / `Down` / `Left` / `Right` | Move cursor while editing `Open Commands` |
| `Tab` | Switch tab |
| `?` | Help |

<a id="usage-settings-options"></a>
The Settings tab includes:

- `Reasoning Level` (`low`, `medium`, `high`, `xhigh`) for Codex and Claude turns in the active project.
- `Default Smart Model`, `Default Fast Model`, and `Default Review Model` for the active project. `Default Smart Model` can also cycle to `Last used model as default`.
- `Coauthored by Agentty` to enable or disable the `Co-Authored-By` trailer on generated session commit messages for the active project. New projects start with this disabled.
- `Open Commands` for launching session worktrees in the active project (one command per line).

## Tasks

| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `k` | Scroll roadmap |
| `g` | Scroll to top |
| `Tab` | Switch tab |
| `?` | Help |

The Tasks tab appears only when the active project contains
`docs/plan/roadmap.md`, and it renders the roadmap's `Ready Now`, `Queued Next`,
and `Parked` cards.

## Session View

<a id="usage-session-view-actions"></a>
Available actions depend on the session state. The full set in **Review**
state:

| Key | Action |
|-----|--------|
| `q` | Back to list |
| `Enter` | Compose the first prompt or reply; in draft sessions it adds a draft |
| `/` | Open the composer with `/` prefilled for slash commands |
| `s` | Start a staged draft session |
| `o` | Open worktree in tmux when the session worktree exists |
| `p` | Publish session branch |
| `Shift+P` | Create or refresh forge review request |
| `d` | Show diff |
| `f` | Append focused review output (regenerate if already present) |
| `m` | Add to merge queue (confirmation popup) |
| `r` | Rebase |
| `j` / `k` | Scroll output |
| `g` | Scroll to top |
| `G` | Scroll to bottom |
| `Ctrl+d` | Half page down |
| `Ctrl+u` | Half page up |
| `Ctrl+c` | Stop running session (InProgress only) |
| `?` | Help |

During **AgentReview**, Agentty keeps the same review-oriented shortcuts but
hides `r` until the background focused-review generation finishes and the
session returns to **Review**.

<a id="usage-additional-keys"></a>
Additional notes:

- **Open command behavior**: `o` opens the session worktree in tmux when a local worktree is available.
  If one `Open Commands` entry is configured for the active project, it runs immediately.
  If multiple entries are configured (one command per line), Agentty opens a selector popup.
- **Draft sessions**: sessions created with `Shift+A` do not create a worktree until you press `s` to start the staged bundle, so `o` stays hidden before the first live turn.
- **Branch publish**: `p` is available in **Review** and **AgentReview** and opens a publish popup. Press `Enter` with an empty field to keep the default session branch target, or type a custom remote branch name first.
- **Forge review-request publish**: `Shift+P` is available in **Review** and
  **AgentReview**. It opens the publish popup, accepts the same optional
  custom branch name as `p`, then creates or refreshes the linked forge review
  request after the branch push succeeds. GitHub projects publish pull
  requests, while GitLab projects publish merge requests.
- **Focused review persistence**: when a focused review has already been generated, it stays visible after opening `d` diff mode and returning to the session view.
- **Branch publish lock**: once a session branch already tracks a remote branch, Agentty locks the popup field and re-publishes to that same remote branch only.
- **Branch publish auth**: `p` and `Shift+P` always run `git push` first. HTTPS remotes therefore need Git credentials even when the forge CLI is already logged in. `Shift+P` also needs authenticated `gh` access for GitHub repositories and authenticated `glab` access for GitLab repositories. See [Forge Authentication](@/docs/usage/forge-authentication.md) for the GitHub and GitLab CLI setup steps.
- **Question**: opening the session enters Question Input mode until all prompts are answered and submitted, or the clarification turn is ended with `Esc`.
- **Done**: `t` toggles between summary and full output.
- **Review**: Runs in read-only review mode. It can use internet lookup
  and non-editing verification commands, but it should not edit files or
  mutate git/workspace state.

## Publish Popup

| Key | Action |
|-----|--------|
| `Enter` | Publish using the typed branch name, or the default session branch target when left blank |
| `Esc` / `q` | Cancel and return to session view |
| `Left` / `Right` / `Home` / `End` | Move cursor |
| `Up` / `Down` | Move cursor across wrapped lines |
| `Backspace` / `Delete` | Delete character |
| text keys | Edit remote branch name |

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
| `Enter` | Submit the first prompt in regular `New`, stage one draft in draft `New`, or submit reply/question text elsewhere |
| `Alt+Enter` or `Shift+Enter` | Insert newline |
| `Ctrl+V` or `Alt+V` | Paste one clipboard image as an inline `[Image #n]` placeholder |
| `Cmd+Left` | Move to start of current line |
| `Cmd+Right` | Move to end of current line |
| `Option+Left` | Move to previous word |
| `Option+Right` | Move to next word |
| `Option+Backspace` | Delete previous word |
| `Cmd+Backspace` | Delete current line |
| `Esc` | Cancel |
| `@` | Open file picker |
| `/` | Open slash commands |

Prompt input keeps regular text paste on terminal `Event::Paste`. The dedicated
image paste shortcuts insert highlighted `[Image #n]` tokens directly in the
composer and send the referenced local image for Codex, Gemini, and Claude
session models. Codex and Gemini preserve the multimodal ordering at transport
level, while Claude rewrites the placeholders to local image paths before
streaming the prompt.

When the current session was created with `Shift+A`, pressing `Enter` stages
the current composer contents into the draft bundle and returns to session
view. Use `s` from session view to launch the staged bundle as the first live
turn. Sessions created with `a` start immediately on the first `Enter`.

## Question Input — Option Selection

When predefined options are shown:

| Key | Action |
|-----|--------|
| `j` / `k` / `Up` / `Down` | Navigate options |
| `Enter` | Choose highlighted option |
| `Tab` | Switch focus to chat output for scrolling |
| `Esc` | End turn — return to review without answering |

## Question Input — Free Text

After moving above or below the predefined option list, or when no predefined options exist:

| Key | Action |
|-----|--------|
| `Enter` | Submit typed response |
| `Alt+Enter` or `Shift+Enter` | Insert newline |
| `Ctrl+J` / `Ctrl+M` | Insert newline (macOS terminal compat) |
| `Esc` | End turn — return to review without answering |
| `Left` / `Right` | Move cursor |
| `Up` / `Down` | Move cursor across wrapped lines |
| `Backspace` / `Delete` | Delete character |
| `Home` / `End` | Move cursor to start/end |
| `Cmd+Left` | Move to start of current line |
| `Cmd+Right` | Move to end of current line |
| `Option+Left` / `Option+Right` | Move to previous / next word |
| `Option+Backspace` | Delete previous word |
| `Cmd+Backspace` | Delete current line |
| `Ctrl+K` | Kill to end of current line |
| `Ctrl+W` | Delete previous word |
| `Ctrl+D` | Delete character forward |
| `Tab` | Switch focus to chat output for scrolling |

## Question Input — Chat Scroll

When chat output is focused (press `Tab` to switch):

| Key | Action |
|-----|--------|
| `j` / `k` / `Up` / `Down` | Scroll chat output |
| `g` | Scroll to top |
| `G` | Scroll to bottom |
| `Ctrl+d` | Half page down |
| `Ctrl+u` | Half page up |
| `d` | Open diff preview for the current session |
| `Enter` / `Esc` | Return focus to answer input |
| `Tab` | Switch focus back to answer input |

<a id="usage-question-input-submit-flow"></a>
After the last question is answered, Agentty sends one follow-up message to the
session with each question and its response, then returns to session view.
Pressing `Esc` at any point ends the turn immediately without sending a reply
and returns the session to review.
