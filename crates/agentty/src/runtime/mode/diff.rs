use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::{App, AppEvent};
use crate::domain::input::InputState;
use crate::domain::session::SessionId;
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineCommentTarget, DiffLineComments, DiffPreview,
    DiffPreviewUnavailableReason, DiffRestoreTarget, DiffReviewComments, DiffScrollCache,
    DiffSidebarFocus, HelpContext, PromptModeSnapshot, ViewportRect,
    allows_diff_line_comment_reply,
};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState,
};
use crate::runtime::EventResult;
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::{RenderCacheStore, diff_util, page};

/// Handles key input while the app is in `AppMode::Diff`.
///
/// File selection via `j`/`k` wraps around between the first and last file
/// explorer entries. Leaving diff mode restores the prior composer or question
/// snapshot when present; otherwise it rebuilds session view with any cached
/// focused review output for the same session.
pub(crate) fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    if handle_line_comment_edit_key(app, key) {
        return EventResult::Continue;
    }

    if handle_help_key(app, key) {
        return EventResult::Continue;
    }

    if handle_exit_key(app, key) {
        return EventResult::Continue;
    }

    handle_navigation_key(app, render_cache_store, content_area, key);

    EventResult::Continue
}

#[cfg(test)]
fn handle(app: &mut App, content_area: Rect, key: KeyEvent) -> EventResult {
    handle_with_cache(app, &RenderCacheStore::default(), content_area, key)
}

/// Handles the only interactive action available while a full diff loads.
pub(crate) fn handle_loading(app: &mut App, key: KeyEvent) -> EventResult {
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        app.cancel_diff_view_load();
    }

    EventResult::Continue
}

/// Enters `AppMode::Diff` for `session_id` with a preloaded `diff`.
///
/// `restore` records the originating page so leaving the diff returns there;
/// `None` falls back to session view.
#[cfg(test)]
pub(crate) fn enter_diff_mode(
    app: &mut App,
    session_id: &str,
    diff: String,
    restore: Option<DiffRestoreTarget>,
    sidebar_focus: DiffSidebarFocus,
) {
    let session_id = session_id.into();
    let mut review_comments = app.start_session_review_comment_load(&session_id);
    if let Some(review_comments) = &mut review_comments {
        review_comments.sidebar_focus = sidebar_focus;
    }
    let line_comments = app
        .diff_comment_progress
        .remove(&session_id)
        .unwrap_or_default();

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments,
        preview: DiffPreview::default(),
        review_comments,
        restore: restore.map(Box::new),
        scroll_cache: None,
        selected_diff_line_index: 0,
        session_id,
        scroll_offset: 0,
    };
}

/// Opens diff help while preserving the current diff-mode snapshot.
fn handle_help_key(app: &mut App, key: KeyEvent) -> bool {
    if key.code != KeyCode::Char('?') {
        return false;
    }

    let can_comment = can_reply_with_line_comments(app);
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments,
        restore,
        session_id,
        selected_diff_line_index,
        scroll_offset,
        ..
    } = mode
    {
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                can_comment,
                diff,
                file_explorer_selected_index,
                focus,
                line_comments,
                preview,
                review_comments: review_comments.map(Box::new),
                restore,
                selected_diff_line_index,
                session_id,
                scroll_offset,
            },
            scroll_offset: 0,
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Leaves diff mode and restores the originating view or question state.
fn handle_exit_key(app: &mut App, key: KeyEvent) -> bool {
    let should_exit = match key.code {
        KeyCode::Char('q') => true,
        KeyCode::Esc => !matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                ..
            }
        ),
        _ => false,
    };
    if !should_exit {
        return false;
    }

    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::Diff {
        line_comments,
        restore,
        session_id,
        ..
    } = mode
    {
        app.save_diff_comment_progress(session_id.clone(), line_comments);
        app.mode = if let Some(restore) = restore {
            restore.into_mode()
        } else {
            AppMode::View {
                session_id,
                scroll_offset: None,
            }
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Applies file-selection and scroll navigation keys in diff mode.
fn handle_navigation_key(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) {
    let can_reply_with_line_comments = can_reply_with_line_comments(app);
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::Diff {
        diff,
        mut file_explorer_selected_index,
        mut focus,
        mut line_comments,
        mut preview,
        mut review_comments,
        restore,
        mut scroll_cache,
        mut scroll_offset,
        mut selected_diff_line_index,
        session_id,
    } = mode
    else {
        app.mode = mode;

        return;
    };

    let mut navigation = DiffKeyNavigation {
        diff: &diff,
        file_explorer_selected_index: &mut file_explorer_selected_index,
        focus: &mut focus,
        line_comments: &mut line_comments,
        preview: &mut preview,
        review_comments: &mut review_comments,
        scroll_cache: &mut scroll_cache,
        scroll_offset: &mut scroll_offset,
        selected_diff_line_index: &mut selected_diff_line_index,
        session_id: &session_id,
    };
    let row_selection_key_handled = handle_row_selection_key(
        render_cache_store,
        key,
        &mut navigation,
        can_reply_with_line_comments,
    );
    let line_comment_target = (!row_selection_key_handled)
        .then(|| {
            selected_line_comment_target(
                render_cache_store,
                key,
                &navigation,
                can_reply_with_line_comments,
            )
        })
        .flatten();
    let selection_changed = !row_selection_key_handled
        && apply_navigation_key(app, render_cache_store, content_area, key, &mut navigation);

    if selection_changed && preview.is_enabled() {
        refresh_selected_preview(
            app,
            render_cache_store,
            &diff,
            file_explorer_selected_index,
            &mut preview,
            &session_id,
        );
    }

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments,
        restore,
        scroll_cache,
        scroll_offset,
        selected_diff_line_index,
        session_id,
    };
    if let Some(target) = line_comment_target {
        start_line_comment_edit(app, render_cache_store, content_area, target);
    } else if should_submit_line_comments(app, key) {
        open_line_comment_prompt(app);
    }
}

/// Returns whether `s` requests submission of all completed diff comments.
pub(crate) fn should_submit_line_comments(app: &App, key: KeyEvent) -> bool {
    let AppMode::Diff { line_comments, .. } = &app.mode else {
        return false;
    };

    can_reply_with_line_comments(app)
        && is_plain_char_key(key, 's')
        && !line_comments.is_editing()
        && !line_comments.is_selecting()
        && !line_comments.comments.is_empty()
}

/// Handles `Shift+V` row-selection entry and `Esc` cancellation.
fn handle_row_selection_key(
    render_cache_store: &RenderCacheStore,
    key: KeyEvent,
    navigation: &mut DiffKeyNavigation<'_>,
    can_reply_with_line_comments: bool,
) -> bool {
    if navigation.line_comments.is_selecting() && key.code == KeyCode::Esc {
        navigation.line_comments.cancel_selection();

        return true;
    }
    let review_comments_are_focused = navigation
        .review_comments
        .as_ref()
        .is_some_and(|review_comments| review_comments.sidebar_focus == DiffSidebarFocus::Comments);
    if !can_reply_with_line_comments
        || !is_shift_char_key(key, 'v')
        || *navigation.focus != DiffFocus::Content
        || review_comments_are_focused
        || selected_preview_is_visible(
            navigation.diff,
            *navigation.file_explorer_selected_index,
            render_cache_store.diff_layout_cache(),
            navigation.preview,
        )
    {
        return false;
    }

    navigation
        .line_comments
        .start_selection(*navigation.selected_diff_line_index);

    true
}

/// Returns the selected file or changed rows requested for comment editing.
fn selected_line_comment_target(
    render_cache_store: &RenderCacheStore,
    key: KeyEvent,
    navigation: &DiffKeyNavigation<'_>,
    can_reply_with_line_comments: bool,
) -> Option<DiffCommentTarget> {
    let review_comments_are_focused = navigation
        .review_comments
        .as_ref()
        .is_some_and(|review_comments| review_comments.sidebar_focus == DiffSidebarFocus::Comments);
    if can_reply_with_line_comments && is_shift_char_key(key, 'c') && !review_comments_are_focused {
        return render_cache_store
            .diff_layout_cache()
            .content(navigation.diff)
            .selected_file_path(*navigation.file_explorer_selected_index)
            .map(DiffCommentTarget::file);
    }
    if !can_reply_with_line_comments
        || key.code != KeyCode::Enter
        || key.modifiers != KeyModifiers::NONE
        || *navigation.focus != DiffFocus::Content
        || review_comments_are_focused
        || selected_preview_is_visible(
            navigation.diff,
            *navigation.file_explorer_selected_index,
            render_cache_store.diff_layout_cache(),
            navigation.preview,
        )
    {
        return None;
    }

    if let Some(target) = navigation.line_comments.selected_comment_target() {
        return Some(target.clone());
    }

    let content = render_cache_store
        .diff_layout_cache()
        .content(navigation.diff);
    let (start_changed_line_index, end_changed_line_index) = navigation
        .line_comments
        .selected_row_bounds(*navigation.selected_diff_line_index);
    if start_changed_line_index == end_changed_line_index {
        return content
            .selected_changed_line(
                *navigation.file_explorer_selected_index,
                start_changed_line_index,
            )
            .map(DiffLineCommentTarget::single)
            .map(Into::into);
    }
    let anchors = content.selected_changed_lines(
        *navigation.file_explorer_selected_index,
        start_changed_line_index,
        end_changed_line_index,
    );

    DiffLineCommentTarget::from_anchors(anchors).map(Into::into)
}

/// Starts comment editing and keeps its file or changed rows visible.
fn start_line_comment_edit(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    target: impl Into<DiffCommentTarget>,
) {
    let target = target.into();
    let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        scroll_cache,
        scroll_offset,
        selected_diff_line_index,
        ..
    } = &mut app.mode
    else {
        return;
    };

    let content = render_cache_store.diff_layout_cache().content(diff);
    let (comment_changed_line_index, select_changed_line) = match &target {
        DiffCommentTarget::File { path }
            if content.selected_file_path(*file_explorer_selected_index) == Some(path.as_str()) =>
        {
            line_comments.cancel_selection();

            (0, true)
        }
        DiffCommentTarget::File { .. } => return,
        DiffCommentTarget::Lines(target) => {
            let Some(comment_changed_line_index) = content
                .changed_line_index_for_anchor(*file_explorer_selected_index, target.last_anchor())
            else {
                return;
            };

            (comment_changed_line_index, false)
        }
    };
    let editing_index = line_comments.start_editing_target(target);
    *focus = DiffFocus::Content;
    *preview = preview.disabled();
    *scroll_cache = None;
    if select_changed_line {
        *selected_diff_line_index = comment_changed_line_index;
    }
    let layout = page::diff::diff_changed_line_layout(
        diff,
        line_comments,
        *file_explorer_selected_index,
        content_area,
        render_cache_store.diff_layout_cache(),
    );
    *scroll_offset = layout
        .content_selection_scroll_offset(
            comment_changed_line_index,
            Some(editing_index),
            *scroll_offset,
        )
        .unwrap_or(*scroll_offset);
}

/// Applies one key to the active diff comment editor before shortcuts run.
fn handle_line_comment_edit_key(app: &mut App, key: KeyEvent) -> bool {
    let AppMode::Diff {
        line_comments,
        scroll_cache,
        ..
    } = &mut app.mode
    else {
        return false;
    };
    if !line_comments.is_editing() {
        return false;
    }

    if !input_key::should_insert_newline(key)
        && let Some(state) = &mut line_comments.at_mention_state
        && let Some(input) = line_comments
            .editing_index
            .and_then(|index| line_comments.comments.get_mut(index))
            .map(|comment| &mut comment.input)
    {
        match key.code {
            KeyCode::Esc => line_comments.at_mention_state = None,
            KeyCode::Up => at_mention::move_selection_up(state),
            KeyCode::Down => at_mention::move_selection_down(input, state),
            KeyCode::Tab | KeyCode::Enter | KeyCode::Char('\r' | '\n') => {
                if let Some(selection) = at_mention::selected_replacement(input, state) {
                    input.replace_range(selection.at_start, selection.at_end, &selection.text);
                }
                line_comments.at_mention_state = None;
            }
            _ => return apply_comment_input_key(app, key),
        }

        return true;
    }

    if key.code == KeyCode::Esc
        || (input_key::is_enter_key(key.code) && !input_key::should_insert_newline(key))
    {
        let previous_count = line_comments.comments.len();
        line_comments.finish_editing();
        if line_comments.comments.len() != previous_count {
            *scroll_cache = None;
        }

        return true;
    }
    apply_comment_input_key(app, key)
}

/// Applies text editing before refreshing the repository lookup.
fn apply_comment_input_key(app: &mut App, key: KeyEvent) -> bool {
    if let AppMode::Diff { line_comments, .. } = &mut app.mode
        && let Some(command) =
            input_key::command_for_key(key, input_key::InputCapabilities::MULTILINE)
        && let Some(input) = line_comments.editing_input_mut()
    {
        input.apply(command);
        sync_comment_at_mention(app);
    }

    true
}

/// Refreshes lookup state after typing, cursor movement, or paste.
fn sync_comment_at_mention(app: &mut App) {
    let AppMode::Diff {
        line_comments,
        session_id,
        ..
    } = &mut app.mode
    else {
        return;
    };
    let Some(index) = line_comments.editing_index else {
        return;
    };
    match at_mention::sync_action(
        &line_comments.comments[index].input,
        line_comments.at_mention_state.as_deref(),
    ) {
        at_mention::AtMentionSyncAction::Dismiss => line_comments.at_mention_state = None,
        at_mention::AtMentionSyncAction::KeepOpen => {
            if let Some(state) = &mut line_comments.at_mention_state {
                at_mention::reset_selection(state);
            }
        }
        at_mention::AtMentionSyncAction::Activate => {
            line_comments.at_mention_state = Some(Box::new(PromptAtMentionState::new(Vec::new())));
            let session_id = session_id.clone();
            let lookup_root = app.at_mention_lookup_root(&session_id);
            at_mention::start_loading_entries(
                app.services.event_sender(),
                lookup_root,
                session_id,
                &mut app.sessions,
            );
        }
    }
}

/// Inserts normalized pasted text into the active multiline diff comment
/// editor.
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    let AppMode::Diff { line_comments, .. } = &mut app.mode else {
        return;
    };
    let Some(input) = line_comments.editing_input_mut() else {
        return;
    };

    input.insert_text(&input_key::normalize_pasted_text(pasted_text));
    sync_comment_at_mention(app);
}

/// Replaces Diff mode with one next-turn prompt containing every comment.
fn open_line_comment_prompt(app: &mut App) {
    let (line_comments, restored_prompt, session_id) = match &app.mode {
        AppMode::Diff {
            line_comments,
            restore,
            session_id,
            ..
        } => {
            let restored_prompt = match restore.as_deref() {
                Some(DiffRestoreTarget::Prompt(snapshot)) => Some(snapshot.clone()),
                Some(DiffRestoreTarget::Question(_)) => return,
                None => None,
            };

            (line_comments.clone(), restored_prompt, session_id.clone())
        }
        _ => return,
    };
    let Some(session) = app.sessions.session_for_id(session_id.as_str()) else {
        return;
    };
    if !allows_diff_line_comment_reply(session, app.sessions.sessions(), None) {
        return;
    }
    let history_entries = super::session_view::session_prompt_history_entries(session);
    app.save_diff_comment_progress(session_id.clone(), line_comments.clone());

    let slash_state = app.prompt_slash_state();
    let mut snapshot = restored_prompt.unwrap_or_else(|| PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(history_entries),
        input: InputState::default(),
        scroll_offset: None,
        session_id,
        slash_state,
    });
    append_line_comments(&mut snapshot, &line_comments);
    app.mode = snapshot.into_prompt_mode();
}

/// Returns whether the active diff can collect comments for one agent reply.
fn can_reply_with_line_comments(app: &App) -> bool {
    let AppMode::Diff {
        restore,
        session_id,
        ..
    } = &app.mode
    else {
        return false;
    };
    let Some(session) = app.sessions.session_for_id(session_id.as_str()) else {
        return false;
    };

    allows_diff_line_comment_reply(session, app.sessions.sessions(), restore.as_deref())
}

/// Appends every structured line-comment block without losing image positions.
fn append_line_comments(snapshot: &mut PromptModeSnapshot, line_comments: &DiffLineComments) {
    let comment_blocks = line_comments.prompt_text();
    if comment_blocks.is_empty() {
        return;
    }
    let separator = if snapshot.input.is_empty() {
        ""
    } else {
        "\n\n"
    };
    let insertion = format!("{separator}{comment_blocks}");
    snapshot.input.move_end();
    let insertion_start = snapshot.input.cursor;
    snapshot
        .attachment_state
        .remember_current_revision(&snapshot.input);
    snapshot.input.insert_text(&insertion);
    snapshot.attachment_state.sync_after_edit(
        &snapshot.input,
        insertion_start,
        insertion_start,
        snapshot.input.cursor,
    );
    snapshot.at_mention_state = None;
    snapshot.history_state.reset_navigation();
    snapshot.slash_state.reset();
}

/// Mutable diff-mode values affected by one navigation key.
struct DiffKeyNavigation<'a> {
    diff: &'a str,
    file_explorer_selected_index: &'a mut usize,
    focus: &'a mut DiffFocus,
    line_comments: &'a mut DiffLineComments,
    preview: &'a mut DiffPreview,
    review_comments: &'a mut Option<DiffReviewComments>,
    scroll_cache: &'a mut Option<DiffScrollCache>,
    scroll_offset: &'a mut u16,
    selected_diff_line_index: &'a mut usize,
    session_id: &'a SessionId,
}

impl DiffKeyNavigation<'_> {
    /// Reborrows the right-pane subset used by focus and cursor helpers.
    fn content_navigation(&mut self) -> DiffContentNavigation<'_> {
        DiffContentNavigation {
            diff: self.diff,
            file_explorer_selected_index: *self.file_explorer_selected_index,
            focus: self.focus,
            line_comments: self.line_comments,
            preview: self.preview,
            review_comments_are_visible: self.review_comments.is_some(),
            scroll_cache: self.scroll_cache,
            scroll_offset: self.scroll_offset,
            selected_diff_line_index: self.selected_diff_line_index,
        }
    }
}

/// Applies one file-tree or right-pane navigation key.
fn apply_navigation_key(
    app: &App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
    navigation: &mut DiffKeyNavigation<'_>,
) -> bool {
    if apply_unfocused_scroll_key(render_cache_store, content_area, key, navigation) {
        return false;
    }

    match key.code {
        KeyCode::Char(character @ ('j' | 'k'))
            if *navigation.focus == DiffFocus::Files && is_plain_char_key(key, character) =>
        {
            let content = render_cache_store
                .diff_layout_cache()
                .content(navigation.diff);
            let new_index = selected_index_after_key(
                character,
                *navigation.file_explorer_selected_index,
                content.item_count(),
            );
            if *navigation.file_explorer_selected_index != new_index {
                *navigation.file_explorer_selected_index = new_index;
                navigation.line_comments.clear_comment_selection();
                *navigation.scroll_cache = None;
                *navigation.scroll_offset = 0;
                *navigation.selected_diff_line_index = 0;

                return true;
            }
        }
        KeyCode::Enter | KeyCode::Char('l')
            if *navigation.focus == DiffFocus::Files
                && (key.code == KeyCode::Enter || is_plain_char_key(key, 'l')) =>
        {
            let mut content_navigation = navigation.content_navigation();
            focus_selected_file_changes(&mut content_navigation, content_area, render_cache_store);
        }
        KeyCode::Down | KeyCode::Char('J' | 'j')
            if *navigation.focus == DiffFocus::Content
                && is_content_navigation_key(key, KeyCode::Down, 'j') =>
        {
            let mut content_navigation = navigation.content_navigation();
            move_content_selection(
                &mut content_navigation,
                content_area,
                render_cache_store,
                DiffContentDirection::Next,
            );
        }
        KeyCode::Up | KeyCode::Char('K' | 'k')
            if *navigation.focus == DiffFocus::Content
                && is_content_navigation_key(key, KeyCode::Up, 'k') =>
        {
            let mut content_navigation = navigation.content_navigation();
            move_content_selection(
                &mut content_navigation,
                content_area,
                render_cache_store,
                DiffContentDirection::Previous,
            );
        }
        KeyCode::Esc | KeyCode::Left if *navigation.focus == DiffFocus::Content => {
            navigation.line_comments.cancel_selection();
            *navigation.focus = DiffFocus::Files;
        }
        KeyCode::Char(character @ ('f' | 'h'))
            if *navigation.focus == DiffFocus::Content && is_plain_char_key(key, character) =>
        {
            navigation.line_comments.cancel_selection();
            *navigation.focus = DiffFocus::Files;
        }
        KeyCode::Char('p')
            if *navigation.focus == DiffFocus::Files && is_plain_char_key(key, 'p') =>
        {
            if let Some(updated_preview) = toggle_selected_preview(
                app,
                render_cache_store.diff_layout_cache(),
                navigation.diff,
                *navigation.file_explorer_selected_index,
                navigation.preview,
                navigation.session_id,
            ) {
                *navigation.preview = updated_preview;
                *navigation.scroll_cache = None;
                *navigation.scroll_offset = 0;
            }
        }
        KeyCode::Char('c')
            if *navigation.focus == DiffFocus::Files
                && is_plain_char_key(key, 'c')
                && navigation.review_comments.is_some() =>
        {
            *navigation.focus = DiffFocus::Files;
            focus_review_comments(
                navigation.review_comments,
                navigation.scroll_cache,
                navigation.scroll_offset,
            );
        }
        _ => {}
    }

    false
}

/// Applies a row-scroll key without moving focus out of the Files pane.
fn apply_unfocused_scroll_key(
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
    navigation: &mut DiffKeyNavigation<'_>,
) -> bool {
    if *navigation.focus != DiffFocus::Files {
        return false;
    }
    let direction = match key.code {
        KeyCode::Down | KeyCode::Char('J' | 'j')
            if is_unfocused_scroll_key(key, KeyCode::Down, 'j') =>
        {
            DiffContentDirection::Next
        }
        KeyCode::Up | KeyCode::Char('K' | 'k')
            if is_unfocused_scroll_key(key, KeyCode::Up, 'k') =>
        {
            DiffContentDirection::Previous
        }
        _ => return false,
    };
    let mut content_navigation = navigation.content_navigation();
    scroll_content_by_row(
        &mut content_navigation,
        content_area,
        render_cache_store,
        direction,
    );

    true
}

/// Returns whether a key scrolls the right pane while Files stays focused.
fn is_unfocused_scroll_key(key: KeyEvent, arrow_key: KeyCode, character: char) -> bool {
    key.code == arrow_key || is_shift_char_key(key, character)
}

/// Returns whether a key moves through the active right-hand content pane.
fn is_content_navigation_key(key: KeyEvent, arrow_key: KeyCode, character: char) -> bool {
    if key.code == arrow_key {
        return true;
    }
    if is_plain_char_key(key, character) {
        return true;
    }

    is_shift_char_key(key, character)
}

/// Mutable diff-pane navigation values shared by focus and cursor movement.
struct DiffContentNavigation<'a> {
    diff: &'a str,
    file_explorer_selected_index: usize,
    focus: &'a mut DiffFocus,
    line_comments: &'a mut DiffLineComments,
    preview: &'a DiffPreview,
    review_comments_are_visible: bool,
    scroll_cache: &'a mut Option<DiffScrollCache>,
    scroll_offset: &'a mut u16,
    selected_diff_line_index: &'a mut usize,
}

/// Direction in which the right-hand diff cursor moves.
#[derive(Clone, Copy)]
enum DiffContentDirection {
    Next,
    Previous,
}

/// Moves focus into a file's preview or first visible added/removed line.
fn focus_selected_file_changes(
    navigation: &mut DiffContentNavigation<'_>,
    content_area: Rect,
    render_cache_store: &RenderCacheStore,
) {
    let content = render_cache_store
        .diff_layout_cache()
        .content(navigation.diff);
    let preview_is_visible = selected_preview_is_visible(
        navigation.diff,
        navigation.file_explorer_selected_index,
        render_cache_store.diff_layout_cache(),
        navigation.preview,
    );
    if !content.selected_item_is_file(navigation.file_explorer_selected_index) {
        return;
    }

    if preview_is_visible {
        *navigation.focus = DiffFocus::Content;
        *navigation.selected_diff_line_index = 0;
        navigation.line_comments.clear_comment_selection();

        return;
    }
    let changed_line_layout = page::diff::diff_changed_line_layout(
        navigation.diff,
        navigation.line_comments,
        navigation.file_explorer_selected_index,
        content_area,
        render_cache_store.diff_layout_cache(),
    );
    let page_areas = diff_util::diff_page_areas(content_area);
    let sidebar_areas = diff_util::diff_sidebar_areas(
        page_areas.file_list_area,
        navigation.review_comments_are_visible,
    );
    let selected_visual_row = FileExplorer::selected_visual_row(
        navigation.file_explorer_selected_index,
        content.item_count(),
        sidebar_areas.file_list_area,
    )
    .unwrap_or_default();
    let Some(selected_diff_line_index) = changed_line_layout
        .changed_line_index_at_visual_row(*navigation.scroll_offset, selected_visual_row)
    else {
        return;
    };

    *navigation.focus = DiffFocus::Content;
    *navigation.selected_diff_line_index = selected_diff_line_index;
    navigation.line_comments.clear_comment_selection();
    *navigation.scroll_offset = changed_line_layout
        .changed_line_scroll_offset(
            *navigation.selected_diff_line_index,
            *navigation.scroll_offset,
        )
        .unwrap_or(*navigation.scroll_offset);
}

/// Moves through preview rows or added/removed lines in the active file.
fn move_content_selection(
    navigation: &mut DiffContentNavigation<'_>,
    content_area: Rect,
    render_cache_store: &RenderCacheStore,
    direction: DiffContentDirection,
) {
    if selected_preview_is_visible(
        navigation.diff,
        navigation.file_explorer_selected_index,
        render_cache_store.diff_layout_cache(),
        navigation.preview,
    ) {
        scroll_content_by_row(navigation, content_area, render_cache_store, direction);

        return;
    }

    let changed_line_layout = page::diff::diff_changed_line_layout(
        navigation.diff,
        navigation.line_comments,
        navigation.file_explorer_selected_index,
        content_area,
        render_cache_store.diff_layout_cache(),
    );
    let selected_comment_index = navigation.line_comments.selected_comment_index();
    let (selected_diff_line_index, selected_comment_index) =
        if navigation.line_comments.is_selecting() {
            let line_count = changed_line_layout.changed_line_count();
            let selected_diff_line_index = match direction {
                DiffContentDirection::Next => (*navigation.selected_diff_line_index)
                    .saturating_add(1)
                    .min(line_count.saturating_sub(1)),
                DiffContentDirection::Previous => {
                    (*navigation.selected_diff_line_index).saturating_sub(1)
                }
            };

            (selected_diff_line_index, None)
        } else {
            match direction {
                DiffContentDirection::Next => changed_line_layout.next_content_selection(
                    *navigation.selected_diff_line_index,
                    selected_comment_index,
                ),
                DiffContentDirection::Previous => changed_line_layout.previous_content_selection(
                    *navigation.selected_diff_line_index,
                    selected_comment_index,
                ),
            }
        };
    *navigation.selected_diff_line_index = selected_diff_line_index;
    if let Some(comment_index) = selected_comment_index {
        navigation.line_comments.select_comment(comment_index);
    } else {
        navigation.line_comments.clear_comment_selection();
    }
    *navigation.scroll_offset = changed_line_layout
        .content_selection_scroll_offset(
            *navigation.selected_diff_line_index,
            selected_comment_index,
            *navigation.scroll_offset,
        )
        .unwrap_or(*navigation.scroll_offset);
}

/// Scrolls the selected file or preview by one rendered row without changing
/// pane focus or the selected changed-line cursor.
fn scroll_content_by_row(
    navigation: &mut DiffContentNavigation<'_>,
    content_area: Rect,
    render_cache_store: &RenderCacheStore,
    direction: DiffContentDirection,
) {
    let max_scroll_offset = diff_max_scroll_offset(
        &DiffScrollLimitInput {
            content_area,
            diff: navigation.diff,
            diff_layout_cache: render_cache_store.diff_layout_cache(),
            line_comments: navigation.line_comments,
            markdown_render_cache: render_cache_store.markdown_render_cache(),
            preview: navigation.preview,
            selected_index: navigation.file_explorer_selected_index,
        },
        navigation.scroll_cache,
    );
    *navigation.scroll_offset = match direction {
        DiffContentDirection::Next => (*navigation.scroll_offset)
            .min(max_scroll_offset)
            .saturating_add(1)
            .min(max_scroll_offset),
        DiffContentDirection::Previous => (*navigation.scroll_offset)
            .min(max_scroll_offset)
            .saturating_sub(1),
    };
}

/// Returns whether the active selection is currently showing markdown preview
/// content or an availability notice instead of raw diff lines.
fn selected_preview_is_visible(
    diff: &str,
    selected_file_index: usize,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    preview: &DiffPreview,
) -> bool {
    let Some(preview_path) = preview.path() else {
        return false;
    };
    let content = diff_layout_cache.content(diff);

    content.selected_markdown_path(selected_file_index) == Some(preview_path)
}

fn focus_review_comments(
    review_comments: &mut Option<DiffReviewComments>,
    scroll_cache: &mut Option<DiffScrollCache>,
    scroll_offset: &mut u16,
) {
    if let Some(review_comments) = review_comments {
        review_comments.sidebar_focus = DiffSidebarFocus::Comments;
    }
    *scroll_cache = None;
    *scroll_offset = 0;
}

fn refresh_selected_preview(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    diff: &str,
    file_explorer_selected_index: usize,
    preview: &mut DiffPreview,
    session_id: &SessionId,
) {
    *preview = start_selected_preview_load(
        app,
        render_cache_store.diff_layout_cache(),
        diff,
        file_explorer_selected_index,
        preview,
        session_id,
    );
}

/// Returns the wrapped explorer selection for a plain `j` or `k` key.
fn selected_index_after_key(character: char, current_index: usize, item_count: usize) -> usize {
    if character == 'j' {
        return FileExplorer::next_selected_index(current_index, item_count);
    }

    FileExplorer::previous_selected_index(current_index, item_count)
}

/// Toggles preview for the selected row, ignoring unsupported toggle-on keys.
fn toggle_selected_preview(
    app: &App,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    diff: &str,
    selected_index: usize,
    preview: &DiffPreview,
    session_id: &str,
) -> Option<DiffPreview> {
    if preview.is_enabled() {
        return Some(preview.disabled());
    }
    selected_markdown_path(diff, selected_index, diff_layout_cache)?;

    Some(start_selected_preview_load(
        app,
        diff_layout_cache,
        diff,
        selected_index,
        preview,
        session_id,
    ))
}

/// Returns the selected markdown path from the cached diff tree.
fn selected_markdown_path(
    diff: &str,
    selected_index: usize,
    diff_layout_cache: &page::diff::DiffLayoutCache,
) -> Option<String> {
    diff_layout_cache
        .content(diff)
        .selected_markdown_path(selected_index)
        .map(str::to_string)
}

/// Starts a bounded background read for the active markdown selection.
fn start_selected_preview_load(
    app: &App,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    diff: &str,
    selected_index: usize,
    preview: &DiffPreview,
    session_id: &str,
) -> DiffPreview {
    let request_id = preview.next_request_id();
    let Some(path) = selected_markdown_path(diff, selected_index, diff_layout_cache) else {
        return DiffPreview::Unsupported { request_id };
    };
    let Some(session_folder) = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map(|session| session.folder.clone())
    else {
        return DiffPreview::Unavailable {
            path,
            reason: DiffPreviewUnavailableReason::LoadFailed(
                "Session worktree is unavailable".to_string(),
            ),
            request_id,
        };
    };

    let event_sender = app.services.event_sender();
    let git_client = app.services.git_client();
    let loaded_path = path.clone();
    let loaded_session_id = session_id.into();
    tokio::spawn(async move {
        let result = git_client
            .read_worktree_file(session_folder, loaded_path.clone())
            .await
            .map_err(|error| error.to_string());
        let _ = event_sender.send(AppEvent::DiffPreviewLoaded {
            path: loaded_path,
            request_id,
            result,
            session_id: loaded_session_id,
        });
    });

    DiffPreview::Loading { path, request_id }
}

/// Returns true when the key event is a plain character key with no
/// modifiers.
fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == KeyModifiers::NONE
}

/// Returns true when the key event is a shifted character key, accepting both
/// uppercase and lowercase char payloads emitted by terminals.
fn is_shift_char_key(key: KeyEvent, character: char) -> bool {
    let lowercase_character = character.to_ascii_lowercase();
    let uppercase_character = character.to_ascii_uppercase();

    key.modifiers == KeyModifiers::SHIFT
        && matches!(
            key.code,
            KeyCode::Char(pressed)
                if pressed == lowercase_character || pressed == uppercase_character
        )
}

/// Inputs used to resolve and cache the active diff scroll limit.
struct DiffScrollLimitInput<'a> {
    content_area: Rect,
    diff: &'a str,
    diff_layout_cache: &'a page::diff::DiffLayoutCache,
    line_comments: &'a DiffLineComments,
    markdown_render_cache: &'a crate::ui::markdown::MarkdownRenderCache,
    preview: &'a DiffPreview,
    selected_index: usize,
}

/// Returns the max valid scroll offset for the active diff selection.
fn diff_max_scroll_offset(
    input: &DiffScrollLimitInput<'_>,
    scroll_cache: &mut Option<DiffScrollCache>,
) -> u16 {
    if let Some(cached_scroll_limit) = scroll_cache
        && cached_scroll_limit.content_area == viewport_rect(input.content_area)
        && cached_scroll_limit.file_explorer_selected_index == input.selected_index
    {
        return cached_scroll_limit.max_scroll_offset;
    }

    let max_scroll_offset = page::diff::diff_view_max_scroll_offset(
        input.diff,
        input.line_comments,
        input.selected_index,
        input.content_area,
        input.diff_layout_cache,
        input.markdown_render_cache,
        input.preview,
    );

    *scroll_cache = Some(DiffScrollCache {
        content_area: viewport_rect(input.content_area),
        file_explorer_selected_index: input.selected_index,
        max_scroll_offset,
    });

    max_scroll_offset
}

/// Converts terminal geometry into the frontend-neutral cached viewport key.
fn viewport_rect(content_area: Rect) -> ViewportRect {
    ViewportRect {
        height: content_area.height,
        width: content_area.width,
        x: content_area.x,
        y: content_area.y,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;

    use super::*;
    use crate::presentation::app_mode::{DiffLineCommentAnchor, DiffReviewComments};

    const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 12);

    /// Builds an app with one previewable session and injected git boundary.
    async fn preview_test_app(mock_git_client: ag_git::MockGitClient) -> (App, tempfile::TempDir) {
        let clients =
            crate::test_support::test_app_clients().with_git_client(Arc::new(mock_git_client));
        let (mut app, base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
        let session = crate::test_support::SessionFixtureBuilder::new()
            .id("session-id")
            .folder(base_dir.path().to_path_buf())
            .build();
        app.sessions =
            crate::test_support::session_manager_with_handles(vec![session], HashMap::new()).into();

        (app, base_dir)
    }

    /// Waits through unrelated startup events for one diff-preview result.
    async fn next_diff_preview_event(app: &mut App) -> AppEvent {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = app
                    .next_app_event()
                    .await
                    .expect("preview event channel should remain open");
                if matches!(event, AppEvent::DiffPreviewLoaded { .. }) {
                    return event;
                }
            }
        })
        .await
        .expect("diff preview event should arrive")
    }

    /// Returns a diff long enough to keep the diff pane scrollable in tests.
    fn scrollable_diff_fixture() -> String {
        format!(
            "diff --git a/src/main.rs b/src/main.rs\n@@ -0,0 +1,40 @@\n{}",
            (0..40)
                .map(|index| format!("+line {index}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    /// Returns eight file rows whose last file starts changing at line 33.
    fn aligned_file_diff_fixture() -> String {
        let preceding_files = ('a'..='g')
            .map(|file_name| {
                format!("diff --git a/{file_name}.rs b/{file_name}.rs\n@@ -0,0 +1 @@\n+seed")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let selected_file_lines = (33..=42)
            .map(|line_number| format!("+line {line_number}"))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "{preceding_files}\ndiff --git a/h.rs b/h.rs\n@@ -0,0 +33,10 @@\n{selected_file_lines}"
        )
    }

    /// Builds a diff-mode snapshot for focused navigation tests.
    fn diff_mode_fixture(
        diff: &str,
        file_explorer_selected_index: usize,
        focus: DiffFocus,
        preview: DiffPreview,
    ) -> AppMode {
        AppMode::Diff {
            diff: diff.to_string(),
            file_explorer_selected_index,
            focus,
            line_comments: DiffLineComments::default(),
            preview,
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session_id: "session-id".into(),
        }
    }

    /// Draft text carried by [`non_default_prompt_snapshot`].
    const RESTORE_DRAFT_TEXT: &str = "draft body";

    /// The single at-mention entry carried by [`non_default_prompt_snapshot`].
    fn prompt_mention_entry() -> crate::domain::file_entry::FileEntry {
        crate::domain::file_entry::FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }
    }

    /// Builds a prompt snapshot with non-default attachment, history, slash,
    /// and at-mention state so restore tests prove no composer field is
    /// dropped when leaving diff.
    fn non_default_prompt_snapshot() -> crate::presentation::app_mode::PromptModeSnapshot {
        use crate::domain::agent::AgentKind;
        use crate::domain::input::InputState;
        use crate::presentation::app_mode::PromptModeSnapshot;
        use crate::presentation::prompt::{
            PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashStage,
            PromptSlashState,
        };

        let mut attachment_state = PromptAttachmentState::default();
        attachment_state.register_local_image(std::path::PathBuf::from("/tmp/pic.png"), 0);

        let mut history_state =
            PromptHistoryState::new(vec!["prev one".to_string(), "prev two".to_string()]);
        history_state.draft_text = Some("saved draft".to_string());
        history_state.selected_index = Some(1);

        let mut slash_state = PromptSlashState::with_available_agent_kinds(vec![AgentKind::Codex]);
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_index = 2;

        PromptModeSnapshot {
            at_mention_state: Some(PromptAtMentionState {
                all_entries: vec![prompt_mention_entry()],
                selected_index: 1,
            }),
            attachment_state,
            history_state,
            input: InputState::with_text(RESTORE_DRAFT_TEXT.to_string()),
            scroll_offset: Some(4),
            session_id: "session-p".into(),
            slash_state,
        }
    }

    /// Asserts `mode` is a prompt composer restored losslessly from
    /// [`non_default_prompt_snapshot`], with input focus.
    fn assert_restored_prompt_composer(mode: &AppMode) {
        use crate::domain::agent::AgentKind;
        use crate::presentation::prompt::PromptSlashStage;

        let AppMode::Prompt {
            at_mention_state,
            attachment_state,
            focus,
            history_state,
            input,
            scroll_offset,
            slash_state,
            ..
        } = mode
        else {
            unreachable!("expected AppMode::Prompt after leaving diff");
        };

        assert_eq!(*focus, crate::presentation::app_mode::ChatFocus::Input);
        assert_eq!(input.text(), RESTORE_DRAFT_TEXT);
        assert_eq!(*scroll_offset, Some(4));

        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(attachment_state.next_attachment_number, 2);

        assert_eq!(
            history_state.entries,
            vec!["prev one".to_string(), "prev two".to_string()]
        );
        assert_eq!(history_state.draft_text, Some("saved draft".to_string()));
        assert_eq!(history_state.selected_index, Some(1));

        assert_eq!(slash_state.available_agent_kinds, vec![AgentKind::Codex]);
        assert_eq!(slash_state.stage, PromptSlashStage::Model);
        assert_eq!(slash_state.selected_index, 2);

        let at_mention_state = at_mention_state
            .as_ref()
            .expect("at-mention state must survive leaving diff");
        assert_eq!(at_mention_state.selected_index, 1);
        assert_eq!(at_mention_state.all_entries, vec![prompt_mention_entry()]);
    }

    #[tokio::test]
    async fn test_comment_lookup_selection_and_dismissal() {
        for code in [
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Char('\r'),
            KeyCode::Char('\n'),
            KeyCode::Esc,
        ] {
            for has_match in [true, false] {
                // Arrange
                let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
                app.mode = diff_mode_fixture(
                    "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
                    1,
                    DiffFocus::Content,
                    DiffPreview::default(),
                );
                if let AppMode::Diff { line_comments, .. } = &mut app.mode {
                    line_comments.start_editing_target(DiffCommentTarget::File {
                        path: "src/main.rs".into(),
                    });
                    let input = line_comments
                        .editing_input_mut()
                        .expect("comment is editable");
                    *input = InputState::with_text("Use @src/pending after".into());
                    input.cursor = "Use @src".len();
                }
                sync_comment_at_mention(&mut app);
                let entries = if has_match {
                    vec![crate::domain::file_entry::FileEntry {
                        is_dir: false,
                        path: "src/lib.rs".into(),
                    }]
                } else {
                    Vec::new()
                };
                app.apply_app_events(AppEvent::AtMentionEntriesLoaded {
                    entries: entries.clone(),
                    session_id: "session-id".into(),
                })
                .await;

                // Act
                for key in [KeyCode::Down, KeyCode::Up, code] {
                    handle_line_comment_edit_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE));
                }
                app.apply_app_events(AppEvent::AtMentionEntriesLoaded {
                    entries,
                    session_id: "session-id".into(),
                })
                .await;

                // Assert
                let AppMode::Diff { line_comments, .. } = &app.mode else {
                    unreachable!("diff mode expected")
                };
                assert!(line_comments.is_editing());
                assert!(line_comments.at_mention_state.is_none());
                assert_eq!(
                    line_comments.comments[0].input.text(),
                    if has_match && code != KeyCode::Esc {
                        "Use @src/lib.rs  after"
                    } else {
                        "Use @src/pending after"
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn test_comment_lookup_edits_paste_and_newline() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        sync_comment_at_mention(&mut app);
        apply_comment_input_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.mode = diff_mode_fixture(
            "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
            1,
            DiffFocus::Content,
            DiffPreview::default(),
        );
        sync_comment_at_mention(&mut app);
        if let AppMode::Diff { line_comments, .. } = &mut app.mode {
            line_comments.start_editing_target(DiffCommentTarget::File {
                path: "src/main.rs".into(),
            });
        }

        // Act
        handle_paste(&mut app, "@sr");
        handle_line_comment_edit_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        );

        // Assert
        assert!(
            matches!(&app.mode, AppMode::Diff { line_comments, .. } if line_comments.at_mention_state.is_some())
        );

        // Act
        handle_line_comment_edit_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        // Assert
        assert!(
            matches!(&app.mode, AppMode::Diff { line_comments, .. } if line_comments.at_mention_state.is_none() && line_comments.comments[0].input.text() == "@src\n" && line_comments.is_editing())
        );
    }

    #[tokio::test]
    async fn test_handle_quit_key_returns_to_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_key_restores_cached_review_output() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.review_cache.insert(
            "session-id".into(),
            crate::app::ReviewCacheEntry::Ready {
                text: "Focused review".to_string(),
                diff_hash: 7,
            },
        );
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_selects_next_changed_line() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                selected_diff_line_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_plain_j_k_and_f_navigate_changed_lines_and_focus_files() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = diff_mode_fixture(
            &scrollable_diff_fixture(),
            0,
            DiffFocus::Content,
            DiffPreview::default(),
        );

        // Act
        for key_code in [KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Char('f')] {
            handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(key_code, KeyModifiers::NONE),
            );
        }

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Files,
                selected_diff_line_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_and_l_from_files_focus_first_changed_line() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;

        // Act, Assert
        for key_code in [KeyCode::Enter, KeyCode::Char('l')] {
            app.mode = AppMode::Diff {
                diff: scrollable_diff_fixture(),
                file_explorer_selected_index: 1,
                focus: DiffFocus::Files,
                line_comments: DiffLineComments::default(),
                preview: DiffPreview::default(),
                review_comments: None,
                restore: None,
                scroll_cache: None,
                scroll_offset: 0,
                selected_diff_line_index: 7,
                session_id: "session-id".into(),
            };
            let event_result = handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(key_code, KeyModifiers::NONE),
            );

            assert!(matches!(event_result, EventResult::Continue));
            assert!(matches!(
                app.mode,
                AppMode::Diff {
                    focus: DiffFocus::Content,
                    line_comments: DiffLineComments {
                        editing_index: None,
                        ref comments,
                        ..
                    },
                    selected_diff_line_index: 0,
                    ..
                } if comments.is_empty()
            ));
        }
    }

    #[tokio::test]
    async fn test_handle_l_focuses_changed_line_aligned_with_selected_file_row() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let diff = aligned_file_diff_fixture();
        app.mode = AppMode::Diff {
            diff: diff.clone(),
            file_explorer_selected_index: 7,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 2,
            selected_diff_line_index: 0,
            session_id: "session-id".into(),
        };
        let content_area = Rect::new(0, 0, 80, 20);

        // Act
        handle(
            &mut app,
            content_area,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                selected_diff_line_index: 7,
                ..
            }
        ));
        let selected_anchor = page::diff::DiffLayoutCache::default()
            .content(&diff)
            .selected_changed_line(7, 7)
            .expect("aligned changed line should resolve");
        assert_eq!(selected_anchor.line, 40);
    }

    #[tokio::test]
    async fn test_handle_l_from_files_focus_preserves_scrolled_position() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: scrollable_diff_fixture(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 20,
            selected_diff_line_index: 0,
            session_id: "session-id".into(),
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                scroll_offset: 20,
                selected_diff_line_index,
                ..
            } if selected_diff_line_index > 0
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_ignores_folders_and_files_without_changed_lines() {
        // Arrange
        let (mut folder_app, _folder_base_dir) = crate::test_support::new_test_app().await;
        let (mut unchanged_app, _unchanged_base_dir) = crate::test_support::new_test_app().await;
        folder_app.mode = diff_mode_fixture(
            "diff --git a/src/main.rs b/src/main.rs\n+added",
            0,
            DiffFocus::Files,
            DiffPreview::default(),
        );
        unchanged_app.mode = diff_mode_fixture(
            "diff --git a/README.md b/README.md\n@@ -1 +1 @@\n unchanged",
            0,
            DiffFocus::Files,
            DiffPreview::default(),
        );

        // Act
        for app in [&mut folder_app, &mut unchanged_app] {
            handle(
                app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        }

        // Assert
        assert!(matches!(
            folder_app.mode,
            AppMode::Diff {
                focus: DiffFocus::Files,
                ..
            }
        ));
        assert!(matches!(
            unchanged_app.mode,
            AppMode::Diff {
                focus: DiffFocus::Files,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_l_focuses_visible_markdown_preview() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = diff_mode_fixture(
            "diff --git a/README.md b/README.md\n+changed",
            0,
            DiffFocus::Files,
            DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "README.md".to_string(),
                request_id: 1,
            },
        );

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                selected_diff_line_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_does_not_edit_line_when_review_comments_are_focused() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let mut review_comments = DiffReviewComments::loading(1);
        review_comments.sidebar_focus = DiffSidebarFocus::Comments;
        app.mode = AppMode::Diff {
            diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(review_comments),
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                line_comments: DiffLineComments { ref comments, .. },
                review_comments: Some(DiffReviewComments {
                    sidebar_focus: DiffSidebarFocus::Comments,
                    ..
                }),
                ..
            } if comments.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_preview_arrow_keys_scroll_both_directions() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let preview_content = (0..40)
            .map(|index| format!("preview line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.mode = diff_mode_fixture(
            "diff --git a/README.md b/README.md\n+changed",
            0,
            DiffFocus::Content,
            DiffPreview::Ready {
                content: preview_content,
                path: "README.md".to_string(),
                request_id: 1,
            },
        );

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                scroll_offset: 0,
                scroll_cache: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_escape_returns_changed_line_focus_to_files() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: scrollable_diff_fixture(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 4,
            selected_diff_line_index: 8,
            session_id: "session-id".into(),
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Files,
                selected_diff_line_index: 8,
                ..
            }
        ));
    }

    #[test]
    fn test_selected_preview_is_visible_only_for_matching_markdown_selection() {
        // Arrange
        let diff = "diff --git a/README.md b/README.md\n+changed";
        let cache = page::diff::DiffLayoutCache::default();
        let matching_preview = DiffPreview::Ready {
            content: "# Changed".to_string(),
            path: "README.md".to_string(),
            request_id: 1,
        };
        let stale_preview = DiffPreview::Ready {
            content: "# Stale".to_string(),
            path: "OTHER.md".to_string(),
            request_id: 2,
        };

        // Act
        let matching_is_visible = selected_preview_is_visible(diff, 0, &cache, &matching_preview);
        let stale_is_visible = selected_preview_is_visible(diff, 0, &cache, &stale_preview);
        let unsupported_is_visible = selected_preview_is_visible(
            diff,
            0,
            &cache,
            &DiffPreview::Unsupported { request_id: 3 },
        );

        // Assert
        assert!(matching_is_visible);
        assert!(!stale_is_visible);
        assert!(!unsupported_is_visible);
    }

    #[tokio::test]
    async fn test_handle_shift_j_selects_next_changed_line() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 3,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 2,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                file_explorer_selected_index: 0,
                selected_diff_line_index: 3,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_file_focus_scrolls_with_arrows_and_shift_j_k() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 7,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };
        let navigation = [
            (KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 1),
            (KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT), 2),
            (KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 1),
            (KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT), 0),
        ];

        // Act & Assert
        for (key, expected_scroll_offset) in navigation {
            let event_result = handle(&mut app, TEST_TERMINAL_SIZE, key);

            assert!(matches!(event_result, EventResult::Continue));
            assert!(matches!(
                &app.mode,
                AppMode::Diff {
                    focus: DiffFocus::Files,
                    scroll_offset,
                    selected_diff_line_index: 7,
                    ..
                } if *scroll_offset == expected_scroll_offset
            ));
        }
    }

    #[tokio::test]
    async fn test_handle_up_key_saturates_changed_line_at_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                selected_diff_line_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_shift_k_saturates_changed_line_at_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                selected_diff_line_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_j_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_j_wraps_file_selection_from_last_to_first() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_wraps_file_selection_from_first_to_last() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 5,
            file_explorer_selected_index: 3,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "README.md".to_string(),
                request_id: 6,
            },
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    ref session_id,
                    ref diff,
                    scroll_offset: 5,
                    file_explorer_selected_index: 3,
                    focus: DiffFocus::Files,
                    selected_diff_line_index: 0,
                    preview: DiffPreview::Ready { request_id: 6, .. },
                    ..
                },
                scroll_offset: 0,
            } if session_id == "session-id" && diff == "diff output"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_clamps_changed_line_at_bottom() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let diff = scrollable_diff_fixture();
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff,
            scroll_offset: u16::MAX,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 39,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset,
                selected_diff_line_index: 39,
                ..
            } if scroll_offset < u16::MAX
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_recovers_overscrolled_changed_line() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let diff = scrollable_diff_fixture();
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff,
            scroll_offset: u16::MAX,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Content,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 39,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset,
                selected_diff_line_index: 38,
                ..
            } if scroll_offset < u16::MAX
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_question_snapshot_restores_question_mode() {
        // Arrange — diff opened from question mode carries a snapshot.
        use crate::domain::input::InputState;
        use crate::domain::question::QuestionItem;
        use crate::presentation::app_mode::QuestionModeSnapshot;

        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-q".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: Some(Box::new(DiffRestoreTarget::Question(
                QuestionModeSnapshot {
                    at_mention_state: None,
                    current_index: 0,
                    input: InputState::default(),
                    questions: vec![QuestionItem {
                        options: Vec::new(),
                        text: "Q?".to_string(),
                    }],
                    responses: Vec::new(),
                    scroll_offset: None,
                    selected_option_index: None,
                    session_id: "session-q".into(),
                },
            ))),
            scroll_cache: None,
        };

        // Act — question-origin diffs cannot become text prompt composers.
        open_line_comment_prompt(&mut app);

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                restore: Some(restore),
                ..
            } if matches!(restore.as_ref(), DiffRestoreTarget::Question(_))
        ));

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — restored to Question mode, not View.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                focus: crate::presentation::app_mode::ChatFocus::Input,
                ..
            } if session_id == "session-q"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_saves_and_reopens_diff_comments() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "review();".to_string(),
            line: 1,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        }));
        line_comments
            .editing_input_mut()
            .expect("comment should be editable")
            .insert_text("Keep this comment");
        line_comments.finish_editing();
        line_comments.start_selection(0);
        app.mode = AppMode::Diff {
            diff: "diff output".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            selected_diff_line_index: 0,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        let saved_comments = app
            .diff_comment_progress
            .get("session-id")
            .expect("comments should be saved after leaving Diff mode");
        assert_eq!(saved_comments.comments[0].input.text(), "Keep this comment");
        assert_eq!(saved_comments.editing_index, None);
        assert_eq!(saved_comments.selection_anchor_index, None);
        assert_eq!(saved_comments.selected_comment_index, None);

        // Act
        enter_diff_mode(
            &mut app,
            "session-id",
            "diff output".to_string(),
            None,
            DiffSidebarFocus::Files,
        );

        // Assert
        assert!(app.diff_comment_progress.is_empty());
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.comments[0].input.text() == "Keep this comment"
        ));

        // Act
        app.clear_diff_comment_progress("session-id");

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                line_comments,
                scroll_cache: None,
                ..
            } if line_comments.comments.is_empty()
        ));

        // Arrange
        if let AppMode::Diff { line_comments, .. } = &mut app.mode {
            line_comments.start_editing_target(DiffCommentTarget::file("src/main.rs"));
            line_comments
                .editing_input_mut()
                .expect("file comment should be editable")
                .insert_text("Clear from help too");
            line_comments.finish_editing();
        }
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Act
        app.clear_diff_comment_progress("session-id");

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Help {
                context: HelpContext::Diff { line_comments, .. },
                ..
            } if line_comments.comments.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_prompt_snapshot_restores_prompt_mode() {
        // Arrange — diff opened from prompt mode carries a composer snapshot.
        use crate::domain::input::InputState;
        use crate::presentation::app_mode::PromptModeSnapshot;
        use crate::presentation::prompt::{
            PromptAttachmentState, PromptHistoryState, PromptSlashState,
        };

        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-p".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: Some(Box::new(DiffRestoreTarget::Prompt(PromptModeSnapshot {
                at_mention_state: None,
                attachment_state: PromptAttachmentState::default(),
                history_state: PromptHistoryState::new(Vec::new()),
                input: InputState::with_text("draft text".to_string()),
                scroll_offset: None,
                session_id: "session-p".into(),
                slash_state: PromptSlashState::default(),
            }))),
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — restored to prompt mode with the draft intact and input
        // focus.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            &app.mode,
            AppMode::Prompt {
                focus: crate::presentation::app_mode::ChatFocus::Input,
                input,
                session_id,
                ..
            } if input.text() == "draft text" && session_id == "session-p"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_prompt_snapshot_preserves_composer_context() {
        // Arrange — a prompt snapshot carrying non-default attachment, history,
        // slash, and at-mention state so leaving diff cannot silently drop it.
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-p".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: Some(Box::new(DiffRestoreTarget::Prompt(
                non_default_prompt_snapshot(),
            ))),
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — every composer field survives the diff round-trip.
        assert_restored_prompt_composer(&app.mode);
    }

    #[tokio::test]
    async fn test_handle_prompt_then_help_then_exit_preserves_composer_context() {
        // Arrange — diff opened from prompt mode, then the user opens help with
        // `?`.
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-p".into(),
            diff: "diff output".to_string(),
            scroll_offset: 3,
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: Some(Box::new(DiffRestoreTarget::Prompt(
                non_default_prompt_snapshot(),
            ))),
            scroll_cache: None,
        };

        // Act — open help overlay.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Intermediate assert — help carries the prompt restore target.
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    restore: Some(_),
                    ..
                },
                ..
            }
        ));

        // Act — close help overlay, returning to diff.
        crate::runtime::mode::help::handle(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Intermediate assert — diff still carries the prompt restore target.
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                restore: Some(_),
                ..
            }
        ));

        // Act — exit diff.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert — restored to the prompt composer with all context intact.
        assert_restored_prompt_composer(&app.mode);
    }

    #[tokio::test]
    async fn test_handle_question_then_help_then_exit_preserves_restore_question() {
        // Arrange — diff opened from question mode, then user opens help with
        // `?`.
        use crate::domain::input::InputState;
        use crate::domain::question::QuestionItem;
        use crate::presentation::app_mode::QuestionModeSnapshot;

        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let snapshot = QuestionModeSnapshot {
            at_mention_state: None,
            current_index: 1,
            input: InputState::default(),
            questions: vec![
                QuestionItem {
                    options: Vec::new(),
                    text: "Q1?".to_string(),
                },
                QuestionItem {
                    options: Vec::new(),
                    text: "Q2?".to_string(),
                },
            ],
            responses: vec!["answer-1".to_string()],
            scroll_offset: None,
            selected_option_index: None,
            session_id: "session-q".into(),
        };

        app.mode = AppMode::Diff {
            session_id: "session-q".into(),
            diff: "diff output".to_string(),
            scroll_offset: 3,
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: Some(Box::new(DiffRestoreTarget::Question(snapshot))),
            scroll_cache: None,
        };

        // Act — open help overlay.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Intermediate assert — help carries the snapshot.
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    restore: Some(_),
                    ..
                },
                ..
            }
        ));

        // Act — close help overlay, returning to diff.
        crate::runtime::mode::help::handle(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Intermediate assert — diff still carries the snapshot.
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                restore: Some(_),
                ..
            }
        ));

        // Act — exit diff.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert — restored to Question mode, not View.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                current_index: 1,
                focus: crate::presentation::app_mode::ChatFocus::Input,
                ..
            } if session_id == "session-q"
        ));
    }

    #[tokio::test]
    async fn test_handle_question_mark_in_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert — the help key is a no-op outside diff mode.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_navigation_key_in_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Assert — navigation keys are a no-op outside diff mode.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_unhandled_key_keeps_diff_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 4,
            file_explorer_selected_index: 2,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        // Assert — an unhandled key leaves the diff selection and scroll
        // intact.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 4,
                file_explorer_selected_index: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_c_focuses_linked_review_comments() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff output".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(DiffReviewComments::loading(1)),
            restore: None,
            scroll_cache: Some(DiffScrollCache {
                content_area: viewport_rect(TEST_TERMINAL_SIZE),
                file_explorer_selected_index: 0,
                max_scroll_offset: 3,
            }),
            scroll_offset: 2,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                review_comments: Some(DiffReviewComments {
                    sidebar_focus: DiffSidebarFocus::Comments,
                    ..
                }),
                scroll_cache: None,
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_collects_inline_comments_before_building_next_turn() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -1 +1,2 @@\n",
            " fn main() {}\n",
            "+println!(\"review\");\n",
            "+review();\n",
        );
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert — both comments stay inside Diff mode until explicit submit.
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.comments.len() == 2
                    && line_comments.comments[0].input.text() == "A"
                    && line_comments.comments[1].input.text() == "B"
        ));

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Prompt {
                focus: crate::presentation::app_mode::ChatFocus::Input,
                input,
                session_id,
                ..
            } if session_id == "session-id"
                && input.text() == concat!(
                    "Line comments:\n",
                    "- src/main.rs:2 [new]: A\n",
                    "- src/main.rs:3 [new]: B",
                )
        ));
    }

    #[tokio::test]
    async fn test_handle_comments_on_selected_file_from_active_row_selection() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -0,0 +1 @@\n",
            "+fn main() {}\n",
        );
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());

        // Act — start a row selection from inside the file, then replace it
        // with a whole-file comment.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        );
        let selection_cleared_while_editing = matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.is_editing() && !line_comments.is_selecting()
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(selection_cleared_while_editing);
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                line_comments,
                ..
            } if line_comments.comments.len() == 1
                && line_comments.comments[0].target
                    == DiffCommentTarget::file("src/main.rs")
                && line_comments.comments[0].input.text() == "R"
                && !line_comments.is_selecting()
        ));

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Prompt { input, .. }
                if input.text() == "File comments:\n- src/main.rs: R"
        ));
    }

    #[tokio::test]
    async fn test_handle_selects_inline_comment_and_reopens_editor_on_enter() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -0,0 +1,2 @@\n",
            "+first();\n",
            "+second();\n",
        );
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
        if let AppMode::Diff { line_comments, .. } = &mut app.mode {
            line_comments.start_editing_target(DiffLineCommentTarget::single(
                DiffLineCommentAnchor {
                    content: "first();".to_string(),
                    line: 1,
                    path: "src/main.rs".to_string(),
                    side: crate::presentation::app_mode::DiffLineSide::New,
                },
            ));
            line_comments
                .editing_input_mut()
                .expect("seeded inline comment should be editable")
                .insert_text("Explain this");
            line_comments.finish_editing();
            line_comments.clear_comment_selection();
        }

        // Act — move from the source row to its comment, back, and onto the
        // comment again.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        let selected_comment = matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.selected_comment_index() == Some(0)
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );
        let selected_source = matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.selected_comment_index().is_none()
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(selected_comment);
        assert!(selected_source);
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                line_comments,
                selected_diff_line_index: 0,
                ..
            } if line_comments.is_editing()
                && line_comments.selected_comment_index() == Some(0)
                && line_comments.comments[0].input.text() == "Explain this"
        ));
    }

    #[tokio::test]
    async fn test_handle_shift_v_selects_changed_rows_for_one_comment() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "@@ -0,0 +1,3 @@\n",
            "+first();\n",
            "+second();\n",
            "+third();\n",
        );
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
        let expected_target = DiffLineCommentTarget::from_anchors(vec![
            DiffLineCommentAnchor {
                content: "first();".to_string(),
                line: 1,
                path: "src/main.rs".to_string(),
                side: crate::presentation::app_mode::DiffLineSide::New,
            },
            DiffLineCommentAnchor {
                content: "second();".to_string(),
                line: 2,
                path: "src/main.rs".to_string(),
                side: crate::presentation::app_mode::DiffLineSide::New,
            },
        ])
        .expect("first two changed rows should create a range target");

        // Act — start downward selection, then cancel without leaving content
        // focus.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                line_comments,
                selected_diff_line_index: 1,
                ..
            } if !line_comments.is_selecting()
        ));

        // Act — select upward from the second row and open one range editor.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                line_comments,
                selected_diff_line_index: 0,
                ..
            } if line_comments.is_editing()
                && line_comments.is_selecting()
                && line_comments.comments[0].target
                    == DiffCommentTarget::from(expected_target)
        ));

        // Act — completed comments cannot submit while a new range is active.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if !line_comments.is_editing() && !line_comments.is_selecting()
        ));

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
        );
        let should_submit = should_submit_line_comments(
            &app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );

        // Assert
        assert!(!should_submit);
    }

    #[tokio::test]
    async fn test_merged_diff_rejects_inline_comment_creation_and_submission() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Merged;
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());

        // Act — whole-file comments are unavailable in read-only diffs.
        if let AppMode::Diff { focus, .. } = &mut app.mode {
            *focus = DiffFocus::Files;
        }
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        );
        if let AppMode::Diff { focus, .. } = &mut app.mode {
            *focus = DiffFocus::Content;
        }

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.comments.is_empty() && !line_comments.is_selecting()
        ));

        // Arrange
        if let AppMode::Diff { line_comments, .. } = &mut app.mode {
            line_comments.start_editing_target(DiffLineCommentTarget::single(
                DiffLineCommentAnchor {
                    content: "review();".to_string(),
                    line: 1,
                    path: "src/main.rs".to_string(),
                    side: crate::presentation::app_mode::DiffLineSide::New,
                },
            ));
            line_comments
                .editing_input_mut()
                .expect("seeded inline comment should be editable")
                .insert_text("read-only comment");
            line_comments.finish_editing();
        }

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );
        open_line_comment_prompt(&mut app);
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    can_comment: false,
                    ..
                },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_start_line_comment_edit_handles_layout_edges() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = scrollable_diff_fixture();
        let render_cache_store = RenderCacheStore::default();
        let target = DiffLineCommentTarget::single(
            render_cache_store
                .diff_layout_cache()
                .content(&diff)
                .selected_changed_line(1, 39)
                .expect("last changed line should resolve from the diff"),
        );

        // Act
        start_line_comment_edit(
            &mut app,
            &render_cache_store,
            TEST_TERMINAL_SIZE,
            target.clone(),
        );

        // Assert
        assert!(matches!(app.mode, AppMode::List));

        // Arrange
        app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());
        let missing_target = DiffLineCommentTarget::single(DiffLineCommentAnchor {
            content: "missing line".to_string(),
            line: 999,
            path: "src/main.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::New,
        });

        // Act
        start_line_comment_edit(
            &mut app,
            &render_cache_store,
            TEST_TERMINAL_SIZE,
            missing_target,
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if !line_comments.is_editing() && line_comments.comments.is_empty()
        ));

        // Act — a stale whole-file target from another selection is ignored.
        start_line_comment_edit(
            &mut app,
            &render_cache_store,
            TEST_TERMINAL_SIZE,
            DiffCommentTarget::file("src/other.rs"),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if !line_comments.is_editing() && line_comments.comments.is_empty()
        ));

        // Arrange
        app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());
        if let AppMode::Diff {
            selected_diff_line_index,
            ..
        } = &mut app.mode
        {
            *selected_diff_line_index = usize::MAX;
        }

        // Act
        start_line_comment_edit(
            &mut app,
            &render_cache_store,
            TEST_TERMINAL_SIZE,
            target.clone(),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                line_comments,
                scroll_offset,
                selected_diff_line_index: usize::MAX,
                ..
            } if line_comments.is_editing() && *scroll_offset > 0
        ));

        // Arrange
        app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());

        // Act
        start_line_comment_edit(
            &mut app,
            &render_cache_store,
            Rect::new(0, 0, 80, 0),
            target.clone(),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                ..
            }
        ));

        // Arrange
        app.mode = diff_mode_fixture(&diff, 1, DiffFocus::Content, DiffPreview::default());
        if let AppMode::Diff {
            selected_diff_line_index,
            ..
        } = &mut app.mode
        {
            *selected_diff_line_index = 39;
        }

        // Act
        start_line_comment_edit(&mut app, &render_cache_store, TEST_TERMINAL_SIZE, target);

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff { scroll_offset, .. } if scroll_offset > 0
        ));
    }

    #[tokio::test]
    async fn test_blank_line_comment_clears_cached_layout() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        if let AppMode::Diff { scroll_cache, .. } = &mut app.mode {
            *scroll_cache = Some(DiffScrollCache {
                content_area: viewport_rect(TEST_TERMINAL_SIZE),
                file_explorer_selected_index: 1,
                max_scroll_offset: 0,
            });
        }

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff {
                line_comments,
                scroll_cache: None,
                ..
            } if line_comments.comments.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_line_comment_paste_ignores_non_editing_modes() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

        // Act
        handle_paste(&mut app, "ignored");
        let should_submit = should_submit_line_comments(
            &app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(!should_submit);

        // Arrange
        app.mode = diff_mode_fixture(
            "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
            1,
            DiffFocus::Content,
            DiffPreview::default(),
        );

        // Act
        handle_paste(&mut app, "ignored");

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. } if line_comments.comments.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_open_line_comment_prompt_handles_mode_and_prompt_restore() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        app.mode = AppMode::List;

        // Act
        open_line_comment_prompt(&mut app);

        // Assert
        assert!(matches!(app.mode, AppMode::List));

        // Arrange
        app.mode = diff_mode_fixture(
            "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
            1,
            DiffFocus::Content,
            DiffPreview::default(),
        );
        if let AppMode::Diff {
            line_comments,
            restore,
            ..
        } = &mut app.mode
        {
            line_comments.start_editing_target(DiffLineCommentTarget::single(
                DiffLineCommentAnchor {
                    content: "review();".to_string(),
                    line: 1,
                    path: "src/main.rs".to_string(),
                    side: crate::presentation::app_mode::DiffLineSide::New,
                },
            ));
            line_comments
                .editing_input_mut()
                .expect("comment should be editable")
                .insert_text("Explain this call");
            line_comments.finish_editing();
            *restore = Some(Box::new(DiffRestoreTarget::Prompt(
                non_default_prompt_snapshot(),
            )));
        }

        // Act
        open_line_comment_prompt(&mut app);

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Prompt { input, .. }
                if input.text().starts_with(RESTORE_DRAFT_TEXT)
                    && input.text().ends_with("Explain this call")
        ));

        // Arrange
        app.mode = diff_mode_fixture(
            "diff --git a/src/main.rs b/src/main.rs\n+review();\n",
            1,
            DiffFocus::Content,
            DiffPreview::default(),
        );
        if let AppMode::Diff { session_id, .. } = &mut app.mode {
            *session_id = "missing-session".into();
        }

        // Act
        open_line_comment_prompt(&mut app);

        // Assert
        assert!(matches!(app.mode, AppMode::Diff { .. }));
    }

    #[tokio::test]
    async fn test_handle_paste_preserves_multiline_diff_comment() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Act
        handle_paste(&mut app, "first line\r\nsecond line");

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.comments[0].input.text() == "first line\nsecond line"
        ));
    }

    #[tokio::test]
    async fn test_handle_modified_enter_inserts_diff_comment_newline() {
        // Arrange
        let (mut app, _base_dir) = preview_test_app(ag_git::MockGitClient::new()).await;
        let diff = "diff --git a/src/main.rs b/src/main.rs\n+review();\n";
        app.mode = diff_mode_fixture(diff, 1, DiffFocus::Content, DiffPreview::default());
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. }
                if line_comments.is_editing()
                    && line_comments.comments[0].input.text() == "A\nB"
        ));

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Diff { line_comments, .. } if !line_comments.is_editing()
        ));
    }

    #[test]
    fn test_append_line_comment_preserves_draft_and_attachment() {
        // Arrange
        let mut snapshot = non_default_prompt_snapshot();
        let mut line_comments = DiffLineComments::default();
        let anchor = DiffLineCommentAnchor {
            content: "updated()".to_string(),
            line: 9,
            path: "src/lib.rs".to_string(),
            side: crate::presentation::app_mode::DiffLineSide::Old,
        };
        line_comments.start_editing_target(DiffLineCommentTarget::single(anchor));
        line_comments
            .editing_input_mut()
            .expect("comment should be editable")
            .insert_text("Update this call");
        line_comments.finish_editing();

        // Act
        append_line_comments(&mut snapshot, &line_comments);

        // Assert
        assert!(snapshot.input.text().starts_with(RESTORE_DRAFT_TEXT));
        assert!(snapshot.input.text().ends_with(
            "Line comments:\n- src/lib.rs:9 [old, source=\"updated()\"]: Update this call"
        ));
        assert_eq!(snapshot.attachment_state.attachments.len(), 1);
        assert_eq!(snapshot.history_state.selected_index, None);
        assert!(snapshot.at_mention_state.is_none());

        // Arrange
        let empty_comments = DiffLineComments::default();
        let unchanged_text = snapshot.input.text().to_string();

        // Act
        append_line_comments(&mut snapshot, &empty_comments);

        // Assert
        assert_eq!(snapshot.input.text(), unchanged_text);
    }

    #[tokio::test]
    async fn test_handle_preview_key_loads_renders_event_and_toggles_off() {
        // Arrange
        let mut mock_git_client = ag_git::MockGitClient::new();
        mock_git_client
            .expect_read_worktree_file()
            .withf(|_, path| path == "README.md")
            .times(1)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(ag_git::WorktreeFileContent::Text(
                        "# Rendered preview".to_string(),
                    ))
                })
            });
        let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 7,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );
        let event = next_diff_preview_event(&mut app).await;
        app.apply_app_events(event).await;
        let ready = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Ready {
                    ref content,
                    request_id: 1,
                    ..
                },
                scroll_offset: 0,
                ..
            } if content == "# Rendered preview"
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(ready);
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Off { request_id: 2 },
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_preview_key_ignores_non_markdown_selection() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 3,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Off { request_id: 0 },
                scroll_offset: 3,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_selection_change_keeps_preview_sticky_for_unsupported_row() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
            file_explorer_selected_index: 1,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "docs/README.md".to_string(),
                request_id: 4,
            },
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 6,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                selected_diff_line_index: 0,
                preview: DiffPreview::Unsupported { request_id: 5 },
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_selection_change_reloads_next_markdown_file() {
        // Arrange
        let mut mock_git_client = ag_git::MockGitClient::new();
        mock_git_client
            .expect_read_worktree_file()
            .withf(|_, path| path == "SECOND.MD")
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Ok(ag_git::WorktreeFileContent::Text("# Second".to_string())) })
            });
        let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
        app.mode = AppMode::Diff {
            diff: concat!(
                "diff --git a/README.md b/README.md\n+first\n",
                "diff --git a/SECOND.MD b/SECOND.MD\n+second\n",
            )
            .to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::Ready {
                content: "# First".to_string(),
                path: "README.md".to_string(),
                request_id: 2,
            },
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 4,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        let preview_event = next_diff_preview_event(&mut app).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                file_explorer_selected_index: 1,
                focus: DiffFocus::Files,
                selected_diff_line_index: 0,
                preview: DiffPreview::Loading {
                    ref path,
                    request_id: 3,
                },
                scroll_offset: 0,
                ..
            } if path == "SECOND.MD"
        ));
        assert!(matches!(
            preview_event,
            AppEvent::DiffPreviewLoaded { ref path, .. } if path == "SECOND.MD"
        ));
    }

    #[tokio::test]
    async fn test_handle_preview_key_reports_missing_session_worktree() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            session_id: "missing-session".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Unavailable {
                    reason: DiffPreviewUnavailableReason::LoadFailed(ref error),
                    ..
                },
                ..
            } if error == "Session worktree is unavailable"
        ));
    }

    #[test]
    fn test_diff_max_scroll_offset_returns_cached_value_on_matching_key() {
        // Arrange — a cache entry whose key matches the requested viewport and
        // selection.
        let diff = scrollable_diff_fixture();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = crate::ui::markdown::MarkdownRenderCache::default();
        let preview = DiffPreview::default();
        let line_comments = DiffLineComments::default();
        let mut scroll_cache = Some(DiffScrollCache {
            content_area: viewport_rect(TEST_TERMINAL_SIZE),
            file_explorer_selected_index: 0,
            max_scroll_offset: 4242,
        });

        // Act
        let max_scroll_offset = diff_max_scroll_offset(
            &DiffScrollLimitInput {
                content_area: TEST_TERMINAL_SIZE,
                diff: &diff,
                diff_layout_cache: &diff_layout_cache,
                line_comments: &line_comments,
                markdown_render_cache: &markdown_render_cache,
                preview: &preview,
                selected_index: 0,
            },
            &mut scroll_cache,
        );

        // Assert — the cached limit is returned verbatim without recomputing.
        assert_eq!(max_scroll_offset, 4242);
    }

    #[test]
    #[should_panic(expected = "expected AppMode::Prompt after leaving diff")]
    fn test_assert_restored_prompt_composer_rejects_non_prompt_mode() {
        // Arrange, Act & Assert — the helper rejects modes that are not a
        // restored composer.
        assert_restored_prompt_composer(&AppMode::List);
    }
}
