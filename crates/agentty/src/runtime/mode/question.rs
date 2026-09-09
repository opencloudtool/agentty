use ag_session::{AnswerQuestionsRequest, QuestionAnswer};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tracing::warn;

use crate::app::session::SessionTaskService;
use crate::app::{self, App, AppEvent};
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, QuestionProgress, default_option_index};
use crate::domain::session::{SessionId, Status};
use crate::presentation::app_mode::{
    AppMode, ChatFocus, DiffRestoreTarget, DiffSidebarFocus, QuestionModeSnapshot,
};
use crate::presentation::prompt::PromptAtMentionState;
use crate::runtime::EventResult;
use crate::runtime::mode::chat_scroll::{self, ChatScrollMetrics};
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::RenderCacheStore;

/// Default response stored when users skip one model question.
const NO_ANSWER: &str = "no answer";

/// Applies one key event in question-answer mode.
///
/// `Tab` toggles focus between the question panel and the chat output for
/// scrolling, unless an open `@` lookup consumes it for file selection. When
/// chat is focused, scroll keys (`j`/`k`/`Up`/`Down`/`g`/`G`/
/// `Ctrl+d`/`Ctrl+u`) navigate the session transcript. `Enter` submits the
/// typed answer (or `no answer` when blank), `Ctrl+C` ends the entire turn
/// without sending a reply while the answer input is focused, `Esc` dismisses
/// an open at-mention dropdown without ending the turn, and `q` returns to the
/// sessions list while saving already-submitted answers for the next visit
/// (skipped while the user is actively typing a free-text answer so the
/// character can still be inserted into the response).
pub(crate) async fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal_size: Rect,
    mut key: KeyEvent,
) -> EventResult {
    // Canonicalize plain terminal Enter encodings before lookup and submission
    // routing. Modified character forms retain their multiline editing meaning.
    if key.modifiers.is_empty() && input_key::is_enter_key(key.code) {
        key.code = KeyCode::Enter;
    }

    if is_plain_q(key) && should_exit_to_list_on_q(app) {
        exit_to_list_saving_progress(app);

        return EventResult::Continue;
    }

    if is_answer_input_focused(app) && is_active_at_mention(app) && handle_at_mention_key(app, key)
    {
        return EventResult::Continue;
    }

    if handle_chat_focus_key(app, render_cache_store, terminal_size, key) {
        return EventResult::Continue;
    }

    if is_ctrl_c(key) {
        if is_answer_input_focused(app) {
            end_turn_no_answer(app).await;
        }

        return EventResult::Continue;
    }

    let Some(action) = resolve_question_action(app, key) else {
        return EventResult::Continue;
    };

    match action {
        QuestionAction::Submit(response) => submit_response(app, response).await,
        QuestionAction::Continue => sync_question_at_mention_state(app),
    }

    EventResult::Continue
}

#[cfg(test)]
async fn handle(app: &mut App, terminal_size: Rect, key: KeyEvent) -> EventResult {
    handle_with_cache(app, &RenderCacheStore::default(), terminal_size, key).await
}

/// Returns whether `key` is a plain `Ctrl+C` press.
fn is_ctrl_c(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Returns whether the question answer input currently holds focus.
fn is_answer_input_focused(app: &App) -> bool {
    matches!(
        &app.mode,
        AppMode::Question {
            focus: ChatFocus::Input,
            ..
        }
    )
}

/// Returns whether `key` is a plain `q` press without modifiers.
fn is_plain_q(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q')) && key.modifiers.is_empty()
}

/// Returns whether a plain `q` press should exit question mode to the sessions
/// list.
///
/// `q` exits while reading the chat transcript (`ChatFocus::Chat`) or while
/// navigating predefined options (`selected_option_index` is `Some`). It is
/// preserved as a free-text character whenever the answer input is focused and
/// the user is past the option list, so answers can still contain the letter.
fn should_exit_to_list_on_q(app: &App) -> bool {
    let AppMode::Question {
        focus,
        selected_option_index,
        ..
    } = &app.mode
    else {
        return false;
    };

    *focus == ChatFocus::Chat || selected_option_index.is_some()
}

/// Saves in-progress clarification answers and returns to the sessions list.
///
/// The saved progress is restored by `App::enter_question_mode` the next
/// time the session's question mode opens, so answers already submitted for
/// earlier questions survive leaving the view with `q`.
fn exit_to_list_saving_progress(app: &mut App) {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);

    if let AppMode::Question {
        current_index,
        input,
        responses,
        selected_option_index,
        session_id,
        ..
    } = mode
    {
        app.question_progress.insert(
            session_id,
            QuestionProgress {
                current_index,
                input,
                responses,
                selected_option_index,
            },
        );
    }
}

/// Applies shared semantic actions while the chat output area is focused.
///
/// Returns `true` when the key was consumed as a chat-focus action. Question
/// mode permits the shared diff-preview action; unsupported actions are
/// swallowed to keep the answer draft unchanged.
fn handle_chat_focus_key(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal_size: Rect,
    key: KeyEvent,
) -> bool {
    let AppMode::Question {
        focus, session_id, ..
    } = &app.mode
    else {
        return false;
    };
    let focus = *focus;

    match chat_scroll::classify_chat_focus_action(focus, key) {
        None => false,
        Some(chat_scroll::ChatFocusAction::ToggleFocus) => {
            if let AppMode::Question { focus, .. } = &mut app.mode {
                chat_scroll::toggle_chat_focus(focus);
            }

            true
        }
        Some(chat_scroll::ChatFocusAction::OpenDiff) => {
            let session_id = session_id.clone();

            show_question_diff(app, &session_id);

            true
        }
        Some(chat_scroll::ChatFocusAction::Scroll) => {
            if let Some(metrics) = question_scroll_metrics(app, render_cache_store, terminal_size)
                && let AppMode::Question { scroll_offset, .. } = &mut app.mode
            {
                chat_scroll::apply_scroll_key(scroll_offset, metrics, key);
            }

            true
        }
        Some(chat_scroll::ChatFocusAction::Swallow) => true,
    }
}

/// Returns transcript scroll metrics while question mode focuses the chat.
///
/// Returns `None` when the answer input holds focus, so scroll keys stay
/// available as answer text. Questions raised for a session that is not loaded
/// into the session list scroll over an empty transcript.
fn question_scroll_metrics(
    app: &App,
    render_cache_store: &RenderCacheStore,
    terminal_size: Rect,
) -> Option<ChatScrollMetrics> {
    let AppMode::Question {
        focus: ChatFocus::Chat,
        session_id,
        ..
    } = &app.mode
    else {
        return None;
    };

    let session_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == *session_id);

    Some(session_index.map_or_else(
        || ChatScrollMetrics::empty(terminal_size),
        |session_index| {
            ChatScrollMetrics::new(
                app,
                render_cache_store,
                session_id,
                session_index,
                terminal_size,
            )
        },
    ))
}

/// Opens the diff preview from question mode.
///
/// Snapshots the current question state so that exiting the diff view
/// restores back to question mode instead of session view.
fn show_question_diff(app: &mut App, session_id: &str) {
    let restore = take_question_snapshot(app).map(DiffRestoreTarget::Question);
    app.start_diff_view_load(
        &SessionId::from(session_id),
        restore,
        DiffSidebarFocus::Files,
        false,
    );
}

/// Snapshots the current question-mode state for later restoration.
///
/// Returns `None` if the app is not in question mode.
fn take_question_snapshot(app: &mut App) -> Option<QuestionModeSnapshot> {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);

    if let AppMode::Question {
        at_mention_state,
        current_index,
        input,
        questions,
        responses,
        scroll_offset,
        selected_option_index,
        session_id,
        ..
    } = mode
    {
        Some(QuestionModeSnapshot {
            at_mention_state,
            current_index,
            input,
            questions,
            responses,
            scroll_offset,
            selected_option_index,
            session_id,
        })
    } else {
        app.mode = mode;

        None
    }
}

/// Inserts pasted text into the active question response input.
///
/// Paste only takes effect when the user is in free-text mode
/// (`selected_option_index` is `None`). While navigating predefined options,
/// paste is ignored — the user must first navigate past the last option to
/// enter free-text mode.
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    let normalized_text = input_key::normalize_pasted_text(pasted_text);
    if normalized_text.is_empty() {
        return;
    }

    if let AppMode::Question {
        input,
        selected_option_index,
        ..
    } = &mut app.mode
    {
        if selected_option_index.is_some() {
            return;
        }

        input.insert_text(&normalized_text);
    }

    sync_question_at_mention_state(app);
}

/// Semantic action emitted by one question-mode key event.
enum QuestionAction {
    Submit(String),
    Continue,
}

/// Resolves and applies one key event against question input state.
///
/// When navigating predefined options (`selected_option_index` is `Some`),
/// `Up`/`Down`/`j`/`k` cycle through the options. Moving past the last (or
/// first) option automatically enters free-text mode where the text input is
/// visible. In free-text mode, `Up` returns to the last predefined option
/// and `Down` wraps to the first.
fn resolve_question_action(app: &mut App, key: KeyEvent) -> Option<QuestionAction> {
    let action = {
        let AppMode::Question {
            current_index,
            input,
            questions,
            selected_option_index,
            ..
        } = &mut app.mode
        else {
            return None;
        };

        let option_count = questions
            .get(*current_index)
            .map_or(0, |item| item.options.len());
        let is_navigating_options = selected_option_index.is_some();

        match key.code {
            KeyCode::Enter | KeyCode::Char('\r' | '\n')
                if !is_navigating_options && input_key::should_insert_newline(key) =>
            {
                input.insert_newline();

                QuestionAction::Continue
            }
            KeyCode::Enter => {
                resolve_enter_action(input, questions, *current_index, selected_option_index)
            }
            KeyCode::Up | KeyCode::Char('k') if is_navigating_options => {
                navigate_option_up(selected_option_index);

                QuestionAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') if is_navigating_options => {
                navigate_option_down(selected_option_index, option_count);

                QuestionAction::Continue
            }
            KeyCode::Up
                if !is_navigating_options
                    && option_count > 0
                    && input_key::is_cursor_on_first_line(input) =>
            {
                *selected_option_index = Some(option_count - 1);

                QuestionAction::Continue
            }
            KeyCode::Down
                if !is_navigating_options
                    && option_count > 0
                    && input_key::is_cursor_on_last_line(input) =>
            {
                *selected_option_index = Some(0);

                QuestionAction::Continue
            }
            _ if !is_navigating_options => resolve_free_text_key(input, key),
            _ => QuestionAction::Continue,
        }
    };

    sync_question_at_mention_state(app);

    Some(action)
}

/// Resolves an `Enter` key press in question mode.
///
/// When navigating options, submits the highlighted predefined option. In
/// free-text mode, submits the typed text.
fn resolve_enter_action(
    input: &mut InputState,
    questions: &[QuestionItem],
    current_index: usize,
    selected_option_index: &mut Option<usize>,
) -> QuestionAction {
    if let Some(option_index) = *selected_option_index {
        let selected_text = questions
            .get(current_index)
            .and_then(|item| item.options.get(option_index))
            .cloned()
            .unwrap_or_default();

        QuestionAction::Submit(normalize_response_text(&selected_text))
    } else {
        let response_text = input.take_text();

        QuestionAction::Submit(normalize_response_text(&response_text))
    }
}

/// Moves the selected option index up, entering free-text mode when wrapping
/// past the first predefined option.
///
/// # Invariant
///
/// The caller in `resolve_question_action` only routes `Up`/`k` here when
/// `is_navigating_options` (i.e. `selected_option_index.is_some()`) holds, so
/// `*selected_option_index` is always `Some(_)` on entry. Reaching the `None`
/// arm means a key handler regressed.
fn navigate_option_up(selected_option_index: &mut Option<usize>) {
    *selected_option_index = match *selected_option_index {
        Some(0) => None,
        Some(index) => Some(index.saturating_sub(1)),
        None => unreachable!("navigate_option_up requires selected_option_index = Some(_)"),
    };
}

/// Moves the selected option index down, entering free-text mode when
/// advancing past the last predefined option.
///
/// # Invariant
///
/// The caller in `resolve_question_action` only routes `Down`/`j` here when
/// `is_navigating_options` (i.e. `selected_option_index.is_some()`) holds, so
/// `*selected_option_index` is always `Some(_)` on entry. Reaching the `None`
/// arm means a key handler regressed.
fn navigate_option_down(selected_option_index: &mut Option<usize>, option_count: usize) {
    *selected_option_index = match *selected_option_index {
        Some(index) if index + 1 >= option_count => None,
        Some(index) => Some(index + 1),
        None => unreachable!("navigate_option_down requires selected_option_index = Some(_)"),
    };
}

/// Resolves a key event in free-text input mode (no option selected).
fn resolve_free_text_key(input: &mut InputState, key: KeyEvent) -> QuestionAction {
    if let Some(command) = input_key::command_for_key(key, input_key::InputCapabilities::MULTILINE)
    {
        input.apply(command);
    }

    QuestionAction::Continue
}

/// Returns whether the question-mode at-mention dropdown is currently visible.
fn is_active_at_mention(app: &App) -> bool {
    matches!(
        &app.mode,
        AppMode::Question {
            at_mention_state: Some(_),
            input,
            selected_option_index: None,
            ..
        } if input.at_mention_query().is_some()
    )
}

/// Intercepts navigation/selection keys when the at-mention dropdown is open.
///
/// Returns `true` when the key was consumed by the at-mention handler.
fn handle_at_mention_key(app: &mut App, key: KeyEvent) -> bool {
    if input_key::should_insert_newline(key) {
        return false;
    }

    match key.code {
        KeyCode::Esc => dismiss_question_at_mention(app),
        KeyCode::Enter | KeyCode::Tab => {
            handle_question_at_mention_select(app);

            return true;
        }
        KeyCode::Up => handle_question_at_mention_up(app),
        KeyCode::Down => handle_question_at_mention_down(app),
        _ => return false,
    }

    true
}

/// Keeps the at-mention dropdown aligned with the current input cursor
/// position.
///
/// Opens the dropdown when the cursor sits inside an `@` token and the
/// dropdown is not yet visible. Resets the selection index when already open.
/// Dismisses the dropdown when the cursor moves away from any `@` token.
fn sync_question_at_mention_state(app: &mut App) {
    let (session_id, sync_action) = match &app.mode {
        AppMode::Question {
            at_mention_state,
            input,
            selected_option_index: None,
            session_id,
            ..
        } => (
            session_id.clone(),
            at_mention::sync_action(input, at_mention_state.as_ref()),
        ),
        _ => return,
    };

    match sync_action {
        at_mention::AtMentionSyncAction::Activate => activate_question_at_mention(app, &session_id),
        at_mention::AtMentionSyncAction::Dismiss => dismiss_question_at_mention(app),
        at_mention::AtMentionSyncAction::KeepOpen => {
            if let AppMode::Question {
                at_mention_state: Some(state),
                ..
            } = &mut app.mode
            {
                at_mention::reset_selection(state);
            }
        }
    }
}

/// Starts asynchronous loading of file entries for the question-mode
/// at-mention dropdown.
fn activate_question_at_mention(app: &mut App, session_id: &str) {
    let lookup_root = app.at_mention_lookup_root(session_id);
    let owned_session_id = SessionId::from(session_id);
    let event_tx = app.services.event_sender();

    at_mention::start_loading_entries(event_tx, lookup_root, owned_session_id, &mut app.sessions);

    if let AppMode::Question {
        at_mention_state, ..
    } = &mut app.mode
    {
        *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    }
}

/// Clears the question-mode at-mention dropdown state.
fn dismiss_question_at_mention(app: &mut App) {
    if let AppMode::Question {
        at_mention_state, ..
    } = &mut app.mode
    {
        at_mention::dismiss(at_mention_state);
    }
}

/// Moves the at-mention selection up in question mode.
fn handle_question_at_mention_up(app: &mut App) {
    if let AppMode::Question {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_up(state);
    }
}

/// Moves the at-mention selection down in question mode.
fn handle_question_at_mention_down(app: &mut App) {
    if let AppMode::Question {
        at_mention_state: Some(state),
        input,
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_down(input, state);
    }
}

/// Selects the currently highlighted file and inserts it into the question
/// input.
fn handle_question_at_mention_select(app: &mut App) {
    let replacement = match &app.mode {
        AppMode::Question {
            at_mention_state: Some(state),
            input,
            ..
        } => at_mention::selected_replacement(input, state),
        _ => return,
    };

    if replacement.is_none() {
        dismiss_question_at_mention(app);

        return;
    }

    if let Some(selection) = replacement
        && let AppMode::Question { input, .. } = &mut app.mode
    {
        input.replace_range(selection.at_start, selection.at_end, &selection.text);
    }

    sync_question_at_mention_state(app);
}

/// Stores one question response and runs follow-up reply when complete.
async fn submit_response(app: &mut App, response: String) {
    let Some(completed_response) = store_question_response(app, response) else {
        return;
    };

    let session_id = completed_response.session_id.clone();
    let answers =
        structured_question_answers(&completed_response.questions, &completed_response.responses);
    let is_orchestration_proxy = app.has_orchestration_question_proxy(&session_id).await;
    app.mode = AppMode::View {
        session_id: session_id.clone(),
        scroll_offset: None,
    };
    let service = app.session_service();
    let request = service.answer_questions(&session_id, AnswerQuestionsRequest { answers });
    let reply_enqueued = match app.drive_session_request(request).await {
        Ok(()) => true,
        Err(error) => {
            warn!(
                session_id = %session_id,
                error = %error,
                "failed to send completed question response"
            );

            false
        }
    };

    // Optimistically advance the session out of `Question` once the reply is
    // enqueued so the open-view question reconciliation does not re-open the
    // just-answered panel before the worker transitions to `InProgress`. The
    // send eligibility check runs against the pre-send `Question` status, so
    // this must happen after `send_message()`. Skip the advance when the reply
    // never reached the worker (for example a rejected stacked reply) so the
    // pending question is not hidden behind a stalled `InProgress` state.
    if reply_enqueued {
        app.clear_diff_comment_progress(&session_id);
        mark_answered_session(app, &session_id, is_orchestration_proxy);
    } else {
        restore_completed_question_response(app, completed_response);
    }
}

/// Advances the session that owned one accepted question response.
fn mark_answered_session(app: &mut App, session_id: &str, is_orchestration_proxy: bool) {
    if is_orchestration_proxy {
        mark_orchestration_controller_review(app, session_id);
    } else {
        mark_session_in_progress(app, session_id);
    }
}

struct CompletedQuestionResponse {
    questions: Vec<QuestionItem>,
    responses: Vec<String>,
    scroll_offset: Option<u16>,
    session_id: SessionId,
}

/// Restores the final question and answer when reply enqueue fails.
///
/// `store_question_response` has already consumed the completed question-mode
/// vectors by this point. Rebuilding the panel at the final question keeps all
/// earlier answers plus the just-entered final answer visible so pressing
/// `Enter` retries the same clarification reply instead of discarding input.
fn restore_completed_question_response(
    app: &mut App,
    mut completed_response: CompletedQuestionResponse,
) {
    let Some(final_response) = completed_response.responses.pop() else {
        return;
    };
    if completed_response.questions.is_empty() {
        return;
    }

    let current_index = completed_response
        .responses
        .len()
        .min(completed_response.questions.len().saturating_sub(1));
    let selected_option_index =
        completed_response
            .questions
            .get(current_index)
            .and_then(|question| {
                question
                    .options
                    .iter()
                    .position(|option| option == &final_response)
            });
    let input = if selected_option_index.is_some() || final_response == NO_ANSWER {
        InputState::default()
    } else {
        InputState::with_text(final_response)
    };

    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index,
        focus: ChatFocus::Input,
        input,
        questions: completed_response.questions,
        responses: completed_response.responses,
        scroll_offset: completed_response.scroll_offset,
        selected_option_index,
        session_id: completed_response.session_id,
    };
}

/// Optimistically advances one session's live handle and snapshot status to
/// [`Status::InProgress`] so a submitted answer is reflected immediately.
///
/// Both the shared handle and the render snapshot are updated so the next
/// `sync_from_handles` sweep does not revert the snapshot back to `Question`.
/// The session worker persists the authoritative transition when it dequeues
/// the reply command.
fn mark_session_in_progress(app: &mut App, session_id: &str) {
    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::InProgress;
    }

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = Status::InProgress;
    }
}

/// Optimistically restores a controller after its proxied child answers were
/// accepted.
fn mark_orchestration_controller_review(app: &mut App, session_id: &str) {
    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::Review;
    }

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = Status::Review;
        session.questions.clear();
    }
}

/// Ends the question turn without sending a reply to the agent.
///
/// Triggered by `Ctrl+C` or `Esc` while the answer input is focused. The
/// session status is reverted to `Review` so the
/// user can inspect the current diff or start a new follow-up manually.
/// If the database write fails the mode stays on `Question` so the user
/// can retry, avoiding a split between persisted and in-memory state.
///
/// The persisted transition uses the timing-aware status update so any
/// lingering active-work interval is closed before the session returns to
/// `Review`. After the write succeeds it emits both
/// [`AppEvent::SessionUpdated`] plus session and project refresh events so the
/// UI refreshes the focused session snapshot and any aggregate project-list
/// state. It also updates the shared runtime handle status alongside the
/// snapshot so the periodic `sync_from_handles` cycle does not revert the
/// status back to `Question`.
async fn end_turn_no_answer(app: &mut App) {
    let AppMode::Question { session_id, .. } = &app.mode else {
        return;
    };

    let session_id = session_id.clone();
    let timestamp_seconds =
        app::session::unix_timestamp_from_system_time(app.services.clock().now_system_time());

    if app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(
            &session_id,
            &Status::Review.to_string(),
            timestamp_seconds,
        )
        .await
        .is_err()
    {
        return;
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str())
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::Review;
    }
    app.sessions.wake_session_worker(session_id.as_str());

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.clone(),
        version: SessionTaskService::next_session_update_version(
            &app.services.session_update_versions(),
            session_id.as_str(),
        ),
    });
    app.services.emit_session_and_project_refresh_events();

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = Status::Review;
    }

    app.mode = AppMode::View {
        session_id,
        scroll_offset: None,
    };
}

/// Writes one response into question mode and returns completion payload when
/// all questions are answered.
fn store_question_response(app: &mut App, response: String) -> Option<CompletedQuestionResponse> {
    let AppMode::Question {
        at_mention_state,
        current_index,
        input,
        questions,
        responses,
        scroll_offset,
        selected_option_index,
        session_id,
        ..
    } = &mut app.mode
    else {
        return None;
    };

    responses.push(response);
    *current_index += 1;
    *input = InputState::default();
    *at_mention_state = None;
    *selected_option_index = default_option_index(questions, *current_index);

    if *current_index < questions.len() {
        return None;
    }

    Some(CompletedQuestionResponse {
        questions: std::mem::take(questions),
        responses: std::mem::take(responses),
        scroll_offset: *scroll_offset,
        session_id: session_id.clone(),
    })
}

/// Returns a normalized user response, falling back to `no answer`.
fn normalize_response_text(response_text: &str) -> String {
    let trimmed = response_text.trim();
    if trimmed.is_empty() {
        return NO_ANSWER.to_string();
    }

    trimmed.to_string()
}

/// Pairs the current ordered question set with its collected responses.
fn structured_question_answers(
    questions: &[QuestionItem],
    responses: &[String],
) -> Vec<QuestionAnswer> {
    questions
        .iter()
        .zip(responses)
        .map(|(question, answer)| QuestionAnswer {
            answer: answer.clone(),
            question: question.text.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{SessionRole, Status};
    use crate::domain::transient_message::TransientMessageStore;
    use crate::presentation::app_mode::ChatFocus;
    use crate::ui::component::session_output::SessionOutputLineContext;
    use crate::ui::page::session_chat::SessionChatPage;

    /// Fake terminal size used by tests that don't exercise scrolling.
    const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 24);

    #[tokio::test]
    async fn test_question_lookup_selects_file_without_submitting_or_changing_focus() {
        for selection_key in [
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Char('\r'),
            KeyCode::Char('\n'),
        ] {
            // Arrange
            let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
            app.mode = question_mode_with_options();
            if let AppMode::Question {
                at_mention_state,
                input,
                selected_option_index,
                ..
            } = &mut app.mode
            {
                *selected_option_index = None;
                *input = InputState::with_text("Use @src/pending".to_string());
                input.cursor = "Use @src".chars().count();
                *at_mention_state = Some(PromptAtMentionState::new(vec![
                    crate::domain::file_entry::FileEntry {
                        is_dir: false,
                        path: "src/first.rs".to_string(),
                    },
                    crate::domain::file_entry::FileEntry {
                        is_dir: false,
                        path: "src/second.rs".to_string(),
                    },
                ]));
            }

            // Act
            for code in [KeyCode::Down, KeyCode::Up, KeyCode::Down, selection_key] {
                handle(
                    &mut app,
                    TEST_TERMINAL_SIZE,
                    KeyEvent::new(code, KeyModifiers::NONE),
                )
                .await;
            }

            // Assert
            assert!(matches!(
                &app.mode,
                AppMode::Question {
                    at_mention_state: None,
                    current_index: 0,
                    focus: ChatFocus::Input,
                    input,
                    responses,
                    selected_option_index: None,
                    ..
                } if input.text() == "Use @src/second.rs " && responses.is_empty()
            ));
        }
    }

    #[tokio::test]
    async fn test_question_enter_encodings_submit_without_lookup() {
        for code in [KeyCode::Enter, KeyCode::Char('\r'), KeyCode::Char('\n')] {
            for (selected_option, draft, expected) in [
                (Some(1), "ignored draft", "Option B"),
                (None, "typed answer", "typed answer"),
                (None, "", NO_ANSWER),
            ] {
                // Arrange
                let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
                app.mode = question_mode_with_options();
                if let AppMode::Question {
                    input,
                    questions,
                    selected_option_index,
                    ..
                } = &mut app.mode
                {
                    *input = InputState::with_text(draft.to_string());
                    *selected_option_index = selected_option;
                    questions.push(QuestionItem::new("Anything else?"));
                }

                // Act
                handle(
                    &mut app,
                    TEST_TERMINAL_SIZE,
                    KeyEvent::new(code, KeyModifiers::NONE),
                )
                .await;

                // Assert
                assert!(matches!(
                    &app.mode,
                    AppMode::Question {
                        current_index: 1,
                        input,
                        responses,
                        focus: ChatFocus::Input,
                        ..
                    } if responses == &vec![expected.to_string()] && input.is_empty()
                ));
            }
        }
    }

    #[tokio::test]
    async fn test_question_lookup_modified_enter_inserts_newline() {
        for (code, modifiers) in [
            (KeyCode::Enter, KeyModifiers::ALT),
            (KeyCode::Enter, KeyModifiers::SHIFT),
            (KeyCode::Char('\r'), KeyModifiers::ALT),
            (KeyCode::Char('\r'), KeyModifiers::SHIFT),
            (KeyCode::Char('\n'), KeyModifiers::ALT),
            (KeyCode::Char('\n'), KeyModifiers::SHIFT),
            (KeyCode::Char('\r'), KeyModifiers::CONTROL),
            (KeyCode::Char('\n'), KeyModifiers::CONTROL),
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
            (KeyCode::Char('m'), KeyModifiers::CONTROL),
        ] {
            // Arrange
            let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
            app.mode = question_mode_with_options();
            if let AppMode::Question {
                at_mention_state,
                input,
                selected_option_index,
                ..
            } = &mut app.mode
            {
                *selected_option_index = None;
                *input = InputState::with_text("@src".to_string());
                *at_mention_state = Some(PromptAtMentionState::new(vec![
                    crate::domain::file_entry::FileEntry {
                        is_dir: false,
                        path: "src/lib.rs".to_string(),
                    },
                ]));
            }

            // Act
            handle(&mut app, TEST_TERMINAL_SIZE, KeyEvent::new(code, modifiers)).await;

            // Assert
            assert!(matches!(
                &app.mode,
                AppMode::Question {
                    at_mention_state: None,
                    current_index: 0,
                    focus: ChatFocus::Input,
                    input,
                    responses,
                    ..
                } if input.text() == "@src\n" && responses.is_empty()
            ));
        }
    }

    #[tokio::test]
    async fn test_question_empty_lookup_selection_closes_without_submitting() {
        for code in [
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Char('\r'),
            KeyCode::Char('\n'),
        ] {
            for entries in [
                Vec::new(),
                vec![crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/lib.rs".to_string(),
                }],
            ] {
                // Arrange
                let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
                app.mode = question_mode_with_options();
                if let AppMode::Question {
                    at_mention_state,
                    input,
                    selected_option_index,
                    ..
                } = &mut app.mode
                {
                    *selected_option_index = None;
                    *input = InputState::with_text("@missing".to_string());
                    *at_mention_state = Some(PromptAtMentionState::new(entries));
                }

                // Act
                handle(
                    &mut app,
                    TEST_TERMINAL_SIZE,
                    KeyEvent::new(code, KeyModifiers::NONE),
                )
                .await;

                // Assert
                assert!(matches!(
                    &app.mode,
                    AppMode::Question {
                        at_mention_state: None,
                        current_index: 0,
                        focus: ChatFocus::Input,
                        input,
                        responses,
                        ..
                    } if input.text() == "@missing" && responses.is_empty()
                ));
            }
        }
    }

    #[tokio::test]
    async fn test_question_delayed_lookup_result_respects_dismissal() {
        for dismissal in [
            None,
            Some(KeyCode::Esc),
            Some(KeyCode::Tab),
            Some(KeyCode::Enter),
        ] {
            // Arrange
            let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
            app.mode = question_mode_with_options();
            if let AppMode::Question {
                at_mention_state,
                input,
                selected_option_index,
                ..
            } = &mut app.mode
            {
                *selected_option_index = None;
                *input = InputState::with_text("@src".to_string());
                *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
            }
            let entries = vec![crate::domain::file_entry::FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            }];
            let lookup_root = app.at_mention_lookup_root("session-id");
            let delayed_result = AppEvent::AtMentionEntriesLoaded {
                entries: entries.clone(),
                session_id: "session-id".into(),
            };

            // Act: deliver the queued load after the dismissal key was handled.
            if let Some(code) = dismissal {
                handle(
                    &mut app,
                    TEST_TERMINAL_SIZE,
                    KeyEvent::new(code, KeyModifiers::NONE),
                )
                .await;
            }
            app.apply_app_events(delayed_result).await;

            // Assert: late results may populate the cache, but cannot reopen
            // the picker.
            assert_eq!(
                app.sessions.at_mention_index_for_root(&lookup_root),
                Some(entries.clone())
            );
            let expected_entries = dismissal.is_none().then_some(entries);
            assert!(matches!(
                &app.mode,
                AppMode::Question {
                    at_mention_state,
                    input,
                    focus: ChatFocus::Input,
                    ..
                } if input.text() == "@src"
                    && at_mention_state.as_ref().map(|state| &state.all_entries)
                        == expected_entries.as_ref()
            ));
        }
    }

    #[tokio::test]
    async fn test_question_lookup_escape_preserves_draft_and_restores_tab_focus_toggle() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            at_mention_state,
            input,
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
            *input = InputState::with_text("@src".to_string());
            *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
        }

        // Act
        for code in [KeyCode::Esc, KeyCode::Tab] {
            handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await;
        }

        // Assert
        assert!(matches!(
            &app.mode,
            AppMode::Question {
                at_mention_state: None,
                focus: ChatFocus::Chat,
                input,
                ..
            } if input.text() == "@src"
        ));
    }

    #[tokio::test]
    async fn test_question_at_mention_loads_parent_worktree_entries_for_stacked_session() {
        // Arrange
        let (mut app, base_dir) = crate::test_support::new_test_app().await;
        let parent_session_id = SessionId::from("parent-session");
        let child_session_id = SessionId::from("child-session");
        let parent_folder = base_dir.path().join("parent-worktree");
        let expected_path = "parent_question_target.txt";
        std::fs::create_dir_all(&parent_folder).expect("failed to create parent worktree");
        std::fs::write(parent_folder.join(expected_path), "parent")
            .expect("failed to write parent worktree file");
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id(parent_session_id.clone())
                .folder(parent_folder)
                .build(),
        );
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id(child_session_id.clone())
                .folder(base_dir.path().join("missing-child-worktree"))
                .parent_session_id(Some(parent_session_id))
                .status(Status::Question)
                .build(),
        );
        app.mode = AppMode::Question {
            at_mention_state: None,
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("@parent".to_string()),
            questions: vec![QuestionItem::new("Which file?")],
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: child_session_id.clone(),
        };

        // Act
        sync_question_at_mention_state(&mut app);
        let next_event = loop {
            let next_event =
                tokio::time::timeout(std::time::Duration::from_secs(1), app.next_app_event())
                    .await
                    .expect("at-mention event should arrive")
                    .expect("at-mention event channel should stay open");
            if matches!(next_event, AppEvent::AtMentionEntriesLoaded { .. }) {
                break next_event;
            }
        };

        // Assert
        assert!(matches!(
            next_event,
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } if session_id == child_session_id
                && entries.contains(&crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: expected_path.to_string(),
                })
        ));
    }

    #[tokio::test]
    async fn test_question_scroll_metrics_uses_default_review_model_for_loading_fallback() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-review-model";
        app.settings.default_review_selection = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeHaiku4520251001,
        );
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id(session_id)
                .model(AgentModel::Gpt56Sol)
                .status(Status::AgentReview)
                .build(),
        );
        app.mode = AppMode::Question {
            at_mention_state: None,
            current_index: 0,
            focus: ChatFocus::Chat,
            input: InputState::default(),
            questions: vec![QuestionItem::new("Need a target branch?")],
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: session_id.into(),
        };
        let terminal_size = Rect::new(0, 0, 16, 24);
        let output_width = terminal_size.width.saturating_sub(2);
        let render_cache_store = RenderCacheStore::default();
        // Act
        let metrics = question_scroll_metrics(&app, &render_cache_store, terminal_size)
            .expect("chat-focused question mode should have scroll metrics");

        let session = &app.sessions.sessions()[0];
        let expected = SessionChatPage::rendered_output_line_count(
            session,
            output_width,
            metrics.view_height,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                session_update_version: app.session_update_version(session_id),
            },
            render_cache_store.markdown_render_cache(),
            render_cache_store.session_output_layout_cache(),
        );

        // Assert
        assert_eq!(
            metrics.total_lines, expected,
            "chat-focused question mode must report transcript line count"
        );
    }

    #[tokio::test]
    async fn test_handle_enter_on_type_custom_answer_with_blank_input_records_no_answer() {
        // Arrange — user navigated to "Type custom answer" and entered
        // free-text mode, then pressed Enter with empty input.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".into(),
            questions: vec![
                QuestionItem {
                    options: vec!["Yes".to_string(), "No".to_string()],
                    text: "Need a target branch?".to_string(),
                },
                QuestionItem {
                    options: vec!["Unit".to_string(), "Integration".to_string()],
                    text: "Need tests?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                ref responses,
                selected_option_index: Some(0),
                ..
            } if responses == &vec![NO_ANSWER.to_string()]
        ));
    }

    #[tokio::test]
    async fn test_handle_ctrl_c_ends_turn_and_transitions_to_view() {
        // Arrange — two unanswered questions. Ctrl+C should cancel the
        // question turn and transition to View without sending a reply.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.review_cache.insert(
            "session-ctrl-c".into(),
            crate::app::ReviewCacheEntry::Ready {
                text: "Focused review".to_string(),
                diff_hash: 42,
            },
        );
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-ctrl-c".into(),
            questions: vec![
                QuestionItem {
                    options: vec!["Yes".to_string(), "No".to_string()],
                    text: "First question?".to_string(),
                },
                QuestionItem {
                    options: vec!["A".to_string(), "B".to_string()],
                    text: "Second question?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("partial answer".to_string()),
            scroll_offset: None,
            selected_option_index: Some(0),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — transitions to View mode with no reply sent.
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                ..
            } if session_id == "session-ctrl-c"
        ));
    }

    #[tokio::test]
    async fn test_handle_ctrl_c_is_swallowed_while_chat_is_focused() {
        // Arrange — chat focus is read-only, so Ctrl+C must not end the turn
        // while the user scrolls the transcript.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-chat-ctrl-c".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Chat,
            input: InputState::with_text("partial answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — the question turn stays active with the draft preserved.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Chat,
                ref input,
                ..
            } if input.text() == "partial answer"
        ));
    }

    #[tokio::test]
    async fn test_handle_escape_preserves_question_turn_and_draft() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-esc".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("partial answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                ref input,
                ref responses,
                ..
            } if session_id == "session-esc"
                && input.text() == "partial answer"
                && responses.is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_q_returns_to_sessions_list_in_chat_focus() {
        // Arrange — chat focus is read-only, so plain `q` should jump to the
        // sessions list (matching session-view navigation).
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-q-chat".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Chat,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await;

        // Assert — switched to the sessions list.
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_q_returns_to_sessions_list_when_navigating_options() {
        // Arrange — option-navigation mode treats letters as navigation
        // keys, so `q` should exit to the sessions list.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-q-options".into(),
            questions: vec![QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Need a target branch?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await;

        // Assert — switched to the sessions list.
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_q_saves_partial_answers_for_reopen() {
        // Arrange — first question already answered, second in option
        // navigation. `q` must keep the submitted answer for the next visit.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-q-save".into(),
            questions: vec![
                QuestionItem::with_options("First?", vec!["Yes".to_string(), "No".to_string()]),
                QuestionItem::with_options("Second?", vec!["A".to_string(), "B".to_string()]),
            ],
            responses: vec!["Yes".to_string()],
            current_index: 1,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(1),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await;

        // Assert — list mode plus saved progress for the session.
        assert!(matches!(app.mode, AppMode::List));
        let progress = app
            .question_progress
            .get("session-q-save")
            .expect("progress should be saved");
        assert_eq!(progress.responses, vec!["Yes".to_string()]);
        assert_eq!(progress.current_index, 1);
        assert_eq!(progress.selected_option_index, Some(1));
    }

    #[tokio::test]
    async fn test_handle_q_inserts_character_in_free_text_answer() {
        // Arrange — free-text mode (no option navigation) must accept `q` as
        // a regular character so users can type answers containing it.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-q-text".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Free text?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        )
        .await;

        // Assert — character inserted into input, mode unchanged.
        assert!(matches!(
            &app.mode,
            AppMode::Question { input, .. } if input.text() == "q"
        ));
    }

    #[tokio::test]
    async fn test_handle_ctrl_c_sets_in_memory_session_status_to_review() {
        // Arrange — session exists in memory with Question status. Ctrl+C
        // should revert it to Review.
        use std::path::PathBuf;

        use crate::domain::session::{Session, SessionSize, SessionStats};

        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-review-check";
        app.sessions.push_session(Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from("/tmp/test"),
            follow_up_tasks: Vec::new(),
            id: session_id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: crate::domain::agent::AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                crate::domain::agent::AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: String::new(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: crate::domain::agent::SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::Question,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
        });
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — session status updated to Review in memory.
        let session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        assert_eq!(session.status, Status::Review);
    }

    #[tokio::test]
    async fn test_handle_ctrl_c_updates_session_handle_status_to_review() {
        // Arrange — session has a runtime handle with Question status.
        // Ctrl+C must update the handle so sync_from_handles does not revert
        // the snapshot status back to Question.
        use std::path::PathBuf;

        use crate::domain::session::{Session, SessionHandles, SessionSize, SessionStats};

        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-handle-review";
        app.sessions.push_session(Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from("/tmp/test"),
            follow_up_tasks: Vec::new(),
            id: session_id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: crate::domain::agent::AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                crate::domain::agent::AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: String::new(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: crate::domain::agent::SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::Question,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
        });
        app.sessions.session_handles_mut().insert(
            session_id.to_string().into(),
            SessionHandles::new(Status::Question),
        );
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — handle status updated so sync_from_handles preserves Review.
        let handles = app
            .sessions
            .session_handles()
            .get(session_id)
            .expect("handle should exist");
        let handle_status = handles.status.lock().expect("lock should succeed");
        assert_eq!(*handle_status, Status::Review);
    }

    #[tokio::test]
    async fn test_handle_ctrl_c_closes_open_in_progress_timer_before_review() {
        // Arrange — persisted state still has an open active-work interval
        // when question mode exits.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-timer-close";
        let project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        app.services
            .db()
            .sessions()
            .insert_session(
                session_id,
                "gemini-3.8-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        app.services
            .db()
            .sessions()
            .update_session_status_with_timing_at(session_id, "InProgress", 0)
            .await
            .expect("failed to open timing window");
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await;
        let sessions = app
            .services
            .db()
            .sessions()
            .load_sessions_for_project(project_id)
            .await
            .expect("failed to load sessions");

        // Assert
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("missing session row");
        assert_eq!(session.status, "Review");
        assert_eq!(session.in_progress_started_at, None);
        assert!(session.in_progress_total_seconds > 0);
    }

    #[tokio::test]
    async fn test_handle_enter_on_last_question_restores_answer_when_reply_is_not_enqueued() {
        // Arrange — free-text mode on last question with no matching session
        // handle.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".into(),
            questions: vec![QuestionItem {
                options: vec!["Today".to_string(), "Tomorrow".to_string()],
                text: "Need exact date?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("March 4, 2026".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 0,
                ref input,
                ref questions,
                ref responses,
                selected_option_index: None,
                ref session_id,
                ..
            } if session_id == "missing-session"
                && questions.len() == 1
                && responses.is_empty()
                && input.text() == "March 4, 2026"
        ));
    }

    #[tokio::test]
    async fn mark_session_in_progress_advances_snapshot_and_handle_status() {
        // Arrange — a session sits in `Question` with a matching runtime
        // handle.
        use crate::domain::session::SessionHandles;

        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-in-progress";
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id(session_id)
                .folder(std::path::PathBuf::from("/tmp/test"))
                .status(Status::Question)
                .build(),
        );
        app.sessions.session_handles_mut().insert(
            session_id.to_string().into(),
            SessionHandles::new(Status::Question),
        );

        // Act
        mark_session_in_progress(&mut app, session_id);

        // Assert — both the snapshot and the shared handle advance to
        // InProgress so sync_from_handles keeps the session off `Question`.
        let session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        assert_eq!(session.status, Status::InProgress);

        let handles = app
            .sessions
            .session_handles()
            .get(session_id)
            .expect("handle should exist");
        assert_eq!(
            *handles.status.lock().expect("lock should succeed"),
            Status::InProgress
        );
    }

    #[tokio::test]
    async fn mark_orchestration_controller_review_clears_proxied_questions() {
        // Arrange
        use crate::domain::session::SessionHandles;

        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "campaign-controller";
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id(session_id)
                .role(SessionRole::Orchestrator)
                .status(Status::Question)
                .questions(vec![QuestionItem::new("Choose one")])
                .build(),
        );
        app.sessions.session_handles_mut().insert(
            session_id.to_string().into(),
            SessionHandles::new(Status::Question),
        );

        // Act
        mark_answered_session(&mut app, session_id, true);

        // Assert
        let session = &app.sessions.sessions()[0];
        assert_eq!(session.status, Status::Review);
        assert_eq!(session.questions, [] as [ag_protocol::QuestionItem; 0]);
        assert_eq!(
            *app.sessions
                .session_handles()
                .get(session_id)
                .expect("handle should exist")
                .status
                .lock()
                .expect("status should remain available"),
            Status::Review
        );
    }

    #[tokio::test]
    async fn submit_response_advances_session_when_reply_is_enqueued() {
        // Arrange
        let (mut app, _base_dir) =
            crate::test_support::new_git_test_app_with_mock_tmux_client().await;
        let session_id = app
            .create_session()
            .await
            .expect("session should be created");
        crate::test_support::set_session_status_for_test(&mut app, &session_id, Status::Question);
        let questions = vec![QuestionItem::new("Which tests should be added?")];
        app.sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session should be loaded")
            .questions = questions.clone();
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                &serde_json::to_string(&questions).expect("questions should serialize"),
            )
            .await
            .expect("questions should persist");
        app.mode = AppMode::Question {
            at_mention_state: None,
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            questions,
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: session_id.clone().into(),
        };

        // Act
        submit_response(&mut app, "Unit and integration tests".to_string()).await;

        // Assert
        let session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        assert_eq!(session.status, Status::InProgress);
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
            } if session_id == &session.id
        ));
    }

    #[tokio::test]
    async fn submit_response_keeps_question_status_when_reply_is_not_enqueued() {
        // Arrange — a viewed `Question` session with no runtime handle, so the
        // reply cannot be enqueued on any worker and `App::reply` reports
        // failure.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-no-handle";
        let questions = vec![
            QuestionItem::with_options(
                "Use the default target branch?",
                vec!["Yes".to_string(), "No".to_string()],
            ),
            QuestionItem::new("Which tests should be added?"),
        ];
        app.sessions.push_session(
            crate::test_support::SessionFixtureBuilder::new()
                .id(session_id)
                .folder(std::path::PathBuf::from("/tmp/test"))
                .status(Status::Question)
                .questions(questions.clone())
                .build(),
        );
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.into(),
            questions,
            responses: vec!["Yes".to_string()],
            current_index: 1,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act — answer the final question so `submit_response` attempts the
        // reply.
        submit_response(&mut app, "Unit and integration tests".to_string()).await;

        // Assert — the failed reply leaves the session on `Question` instead of
        // stranding it behind an optimistic `InProgress` no worker will
        // advance, and restores the panel with the completed answers
        // ready to retry.
        let session = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        assert_eq!(session.status, Status::Question);
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                ref input,
                ref questions,
                ref responses,
                selected_option_index: None,
                ..
            } if questions.len() == 2
                && responses == &vec!["Yes".to_string()]
                && input.text() == "Unit and integration tests"
        ));
    }

    #[tokio::test]
    async fn test_handle_paste_normalizes_line_endings_in_free_text_mode() {
        // Arrange — free-text mode (user selected "Type custom answer").
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: vec!["Default".to_string()],
                text: "Question".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        handle_paste(&mut app, "line1\r\nline2\rline3");

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question { ref input, .. } if input.text() == "line1\nline2\nline3"
        ));
    }

    #[tokio::test]
    async fn test_handle_paste_ignored_while_navigating_options() {
        // Arrange — navigating options mode.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();

        // Act
        handle_paste(&mut app, "pasted text");

        // Assert — input unchanged, selection unchanged.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(0),
                ref input,
                ..
            } if input.text().is_empty()
        ));
    }

    /// Creates a question mode with predefined options for navigation tests.
    ///
    /// Defaults `selected_option_index` to `Some(0)` matching production
    /// behavior where the first option is pre-selected.
    fn question_mode_with_options() -> AppMode {
        AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: vec![
                    "Option A".to_string(),
                    "Option B".to_string(),
                    "Option C".to_string(),
                ],
                text: "Pick one?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        }
    }

    #[tokio::test]
    async fn test_handle_down_from_first_selects_second_option() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(1),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_from_first_enters_free_text_mode() {
        // Arrange — 3 real options, navigating up from first wraps to
        // free-text input.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_down_from_last_real_enters_free_text_mode() {
        // Arrange — 3 real options, navigating down from last enters
        // free-text input.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = Some(2);
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_from_free_text_returns_to_last_real_option() {
        // Arrange — free-text mode with 3 real options available.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(2),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_down_from_free_text_wraps_to_first_option() {
        // Arrange — free-text mode with 3 real options available.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(0),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_with_selected_option_submits_option_text() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".into(),
            questions: vec![
                QuestionItem {
                    options: vec!["Yes".to_string(), "No".to_string()],
                    text: "Continue?".to_string(),
                },
                QuestionItem {
                    options: vec!["Details".to_string(), "Skip".to_string()],
                    text: "Follow-up?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(1),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                ref responses,
                selected_option_index: Some(0),
                ..
            } if responses == &vec!["No".to_string()]
        ));
    }

    #[tokio::test]
    async fn test_handle_char_ignored_while_navigating_options() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = Some(1);
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert — selection unchanged, input still empty.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(1),
                ref input,
                ..
            } if input.text().is_empty()
        ));
    }

    #[tokio::test]
    async fn test_handle_up_from_free_text_stays_in_free_text_when_no_options() {
        // Arrange — question has no predefined options, so Up stays in
        // free-text mode (no options to navigate to).
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("some text", 4);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .await;

        // Assert — remains in free-text mode, Up moves cursor.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_from_free_text_stays_when_cursor_not_on_first_line() {
        // Arrange — multiline input with cursor on second line. Up should
        // move the cursor within the text, not exit to option navigation.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            input,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
            *input = InputState::with_text("first\nsecond".to_string());
            input.cursor = "first\nseco".chars().count();
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .await;

        // Assert — stays in free-text mode, cursor moved up within text.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_down_from_free_text_stays_when_cursor_not_on_last_line() {
        // Arrange — multiline input with cursor on first line. Down should
        // move the cursor within the text, not exit to option navigation.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            input,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
            *input = InputState::with_text("first\nsecond".to_string());
            input.cursor = 2;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .await;

        // Assert — stays in free-text mode, cursor moved down within text.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_char_inserts_in_free_text_mode() {
        // Arrange — free-text mode after selecting "Type custom answer".
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = None;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                ref input,
                ..
            } if input.text() == "x"
        ));
    }

    #[tokio::test]
    async fn test_handle_j_selects_next_option() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(1),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_selects_previous_option() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = Some(2);
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(1),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_store_question_response_defaults_to_first_option_on_next_question() {
        // Arrange — free-text mode on first question, next has options.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".into(),
            questions: vec![
                QuestionItem {
                    options: vec!["Foo".to_string()],
                    text: "First question?".to_string(),
                },
                QuestionItem {
                    options: vec!["Alpha".to_string(), "Beta".to_string()],
                    text: "Pick one?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                selected_option_index: Some(0),
                ..
            }
        ));
    }

    #[test]
    fn test_default_option_index_returns_first_when_options_exist() {
        // Arrange
        let questions = vec![QuestionItem {
            options: vec!["A".to_string(), "B".to_string()],
            text: "Pick?".to_string(),
        }];

        // Act & Assert
        assert_eq!(default_option_index(&questions, 0), Some(0));
    }

    #[test]
    fn test_default_option_index_returns_none_for_question_without_predefined_options() {
        // Arrange — without predefined options the UI starts directly in
        // free-text input mode.
        let questions = vec![QuestionItem {
            options: Vec::new(),
            text: "Type something?".to_string(),
        }];

        // Act & Assert
        assert_eq!(default_option_index(&questions, 0), None);
    }

    #[test]
    fn test_default_option_index_returns_none_for_out_of_bounds() {
        // Arrange
        let questions: Vec<QuestionItem> = Vec::new();

        // Act & Assert
        assert_eq!(default_option_index(&questions, 0), None);
    }

    #[tokio::test]
    async fn test_handle_tab_toggles_focus_from_answer_to_chat() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Chat,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_tab_toggles_focus_from_chat_to_answer() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question { focus, .. } = &mut app.mode {
            *focus = ChatFocus::Chat;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Input,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_chat_focus_key_ignores_non_question_mode() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::List;

        // Act
        let is_consumed = handle_chat_focus_key(
            &mut app,
            &RenderCacheStore::default(),
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );

        // Assert
        assert!(!is_consumed);
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_scroll_down_in_chat_focus_updates_scroll_offset() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            *focus = ChatFocus::Chat;
            *scroll_offset = Some(0);
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await;

        // Assert — offset incremented (or set to None if at bottom, since no
        // session content exists in test).
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Chat,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_scroll_keys_ignored_in_answer_focus() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();

        // Act — 'j' in answer focus navigates options, not scroll.
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await;

        // Assert — selected_option_index moved, not scroll.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Input,
                selected_option_index: Some(1),
                scroll_offset: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_jump_to_top_in_chat_focus_sets_offset_zero() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            *focus = ChatFocus::Chat;
            *scroll_offset = None;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Chat,
                scroll_offset: Some(0),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_jump_to_bottom_in_chat_focus_sets_offset_none() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            *focus = ChatFocus::Chat;
            *scroll_offset = Some(5);
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE),
        )
        .await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Chat,
                scroll_offset: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_chat_focus_preserves_question_state_for_enter_and_escape() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question { focus, .. } = &mut app.mode {
            *focus = ChatFocus::Chat;
        }

        // Act
        for key in [KeyCode::Enter, KeyCode::Esc] {
            let _ = handle(
                &mut app,
                TEST_TERMINAL_SIZE,
                KeyEvent::new(key, KeyModifiers::NONE),
            )
            .await;
        }

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: ChatFocus::Chat,
                current_index: 0,
                ref responses,
                ..
            } if responses.is_empty()
        ));
    }

    #[test]
    fn test_structured_question_answers_pairs_all_responses() {
        // Arrange
        let questions = vec![
            QuestionItem {
                options: vec!["main".to_string(), "develop".to_string()],
                text: "Need target?".to_string(),
            },
            QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Need tests?".to_string(),
            },
        ];
        let responses = vec!["main".to_string(), NO_ANSWER.to_string()];

        // Act
        let answers = structured_question_answers(&questions, &responses);

        // Assert
        assert_eq!(
            answers,
            [
                QuestionAnswer {
                    answer: "main".to_string(),
                    question: "Need target?".to_string(),
                },
                QuestionAnswer {
                    answer: NO_ANSWER.to_string(),
                    question: "Need tests?".to_string(),
                },
            ]
        );
    }

    /// Creates a free-text question mode with the given text and cursor
    /// position for modifier key tests.
    fn free_text_question_mode(text: &str, cursor: usize) -> AppMode {
        let mut input = InputState::with_text(text.to_string());
        input.cursor = cursor;

        AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Question?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input,
            scroll_offset: None,
            selected_option_index: None,
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_super_left_moves_to_line_start() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond\nthird", "first\nseco".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "first\n".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_super_right_moves_to_line_end() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond\nthird", "first\nse".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "first\nsecond".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_a_moves_to_line_start() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond\nthird", "first\nseco".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "first\n".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_e_moves_to_line_end() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond\nthird", "first\nse".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "first\nsecond".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_alt_b_moves_to_previous_word() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode =
            free_text_question_mode("hello brave world", "hello brave world".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello brave ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_alt_f_moves_to_next_word() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello brave world", 0);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_alt_left_moves_to_previous_word() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode =
            free_text_question_mode("hello brave world", "hello brave world".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello brave ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_alt_right_moves_to_next_word() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello brave world", 0);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, "hello ".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_alt_enter_inserts_newline() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", "hello".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello\n");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_shift_enter_inserts_newline() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", "hello".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello\n");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_j_inserts_newline() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", "hello".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello\n");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_m_inserts_newline() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", "hello".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello\n");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_f_moves_right() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", 2);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, 3);
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_b_moves_left() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", 3);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.cursor, 2);
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_p_moves_up() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond", "first\nseco".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — cursor moved up to the first line.
        if let AppMode::Question { input, .. } = &app.mode {
            assert!(input.cursor < "first\n".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_n_moves_down() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond", 2);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — cursor moved down to the second line.
        if let AppMode::Question { input, .. } = &app.mode {
            assert!(input.cursor >= "first\n".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_d_deletes_forward() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello", 2);

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "helo");
            assert_eq!(input.cursor, 2);
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_k_kills_to_line_end() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond\nthird", "first\nse".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — text from cursor to end of "second" line is deleted.
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "first\nse\nthird");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_w_deletes_previous_word() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode =
            free_text_question_mode("hello brave world", "hello brave world".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert — deletes "world" and the preceding whitespace.
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello brave");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_ctrl_z_undoes_previous_edit() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("hello brave", "hello brave".chars().count());
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        )
        .await;

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        )
        .await;

        // Assert
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello brave");
            assert_eq!(input.cursor, "hello brave".chars().count());
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_alt_backspace_deletes_previous_word() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode =
            free_text_question_mode("hello brave world", "hello brave world".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
        )
        .await;

        // Assert — deletes "world" and the preceding whitespace.
        if let AppMode::Question { input, .. } = &app.mode {
            assert_eq!(input.text(), "hello brave");
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_super_backspace_deletes_current_line() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = free_text_question_mode("first\nsecond\nthird", "first\nseco".chars().count());

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER),
        )
        .await;

        // Assert — current line "second" is deleted.
        if let AppMode::Question { input, .. } = &app.mode {
            assert!(!input.text().contains("second"));
        }
    }

    #[tokio::test]
    async fn test_alt_enter_ignored_while_navigating_options() {
        // Arrange — navigating options, Alt+Enter should submit (not insert
        // newline), because newline insertion only applies in free-text mode.
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".into(),
            questions: vec![
                QuestionItem {
                    options: vec!["Yes".to_string(), "No".to_string()],
                    text: "Continue?".to_string(),
                },
                QuestionItem {
                    options: vec!["A".to_string()],
                    text: "Follow-up?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        )
        .await;

        // Assert — option was submitted, advanced to next question.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                ref responses,
                ..
            } if responses == &vec!["Yes".to_string()]
        ));
    }

    #[tokio::test]
    async fn test_d_key_in_chat_focus_opens_diff_with_question_snapshot() {
        // Arrange — question mode with chat focused in a git worktree
        // session that has a non-empty diff.
        use crate::domain::session::{Session, SessionSize, SessionStats};

        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let session_id = "session-diff-question";

        // Set up a session with a real temp dir (no git repo, so diff will
        // produce an error message — which counts as non-empty content).
        let session_dir = tempdir().expect("failed to create session dir");
        app.sessions.push_session(Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: session_dir.path().to_path_buf(),
            follow_up_tasks: Vec::new(),
            id: session_id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: crate::domain::agent::AgentSelection::new(
                crate::domain::agent::AgentKind::Antigravity,
                crate::domain::agent::AgentModel::Gemini38Flash,
            ),
            parent_session_id: None,
            permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
            personality_id: None,
            project_name: String::new(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override: None,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: crate::domain::agent::SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::Question,
            title: None,
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
        });

        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.into(),
            questions: vec![QuestionItem {
                options: vec!["A".to_string()],
                text: "Pick one".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Chat,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        )
        .await;

        // Assert — transitioned to diff loading with a question snapshot.
        assert!(matches!(
            app.mode,
            AppMode::DiffLoading {
                ref session_id,
                restore: Some(_),
                ..
            } if session_id == "session-diff-question"
        ));
    }
}
