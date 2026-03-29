use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::app::{self, App, AppEvent};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::infra::agent::protocol::QuestionItem;
use crate::infra::channel::TurnPrompt;
use crate::runtime::EventResult;
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::page::session_chat::SessionChatPage;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode, QuestionFocus};
use crate::ui::state::prompt::PromptAtMentionState;

/// Default response stored when users skip one model question.
const NO_ANSWER: &str = "no answer";

/// Applies one key event in question-answer mode.
///
/// `Tab` toggles focus between the question panel and the chat output for
/// scrolling. When chat is focused, scroll keys (`j`/`k`/`Up`/`Down`/`g`/`G`/
/// `Ctrl+d`/`Ctrl+u`) navigate the session transcript. `Enter` submits the
/// typed answer (or `no answer` when blank), and `Esc` ends the entire turn
/// without sending a reply, reverting the session to `Review`.
pub(crate) async fn handle(app: &mut App, terminal_size: Rect, key: KeyEvent) -> EventResult {
    if handle_focus_toggle(app, key) {
        return EventResult::Continue;
    }

    if handle_chat_scroll(app, terminal_size, key) {
        return EventResult::Continue;
    }

    if is_active_at_mention(app) && handle_at_mention_key(app, key) {
        return EventResult::Continue;
    }

    let Some(action) = resolve_question_action(app, key) else {
        return EventResult::Continue;
    };

    match action {
        QuestionAction::Submit(response) => submit_response(app, response).await,
        QuestionAction::EndTurn => end_turn_no_answer(app).await,
        QuestionAction::Continue => sync_question_at_mention_state(app),
    }

    EventResult::Continue
}

/// Toggles focus between the question panel and chat output on `Tab`.
///
/// Returns `true` when the key was consumed as a focus toggle.
fn handle_focus_toggle(app: &mut App, key: KeyEvent) -> bool {
    if key.code != KeyCode::Tab {
        return false;
    }

    let AppMode::Question { focus, .. } = &mut app.mode else {
        return false;
    };

    *focus = match *focus {
        QuestionFocus::Answer => QuestionFocus::Chat,
        QuestionFocus::Chat => QuestionFocus::Answer,
    };

    true
}

/// Applies scroll keys when the chat output area is focused.
///
/// Returns `true` when the key was consumed as a scroll action. `Enter`
/// switches focus back to `Answer` so it cannot accidentally submit a
/// response while scrolling. `Esc` is not intercepted — it always reaches
/// the question handler so users can end the turn regardless of focus.
fn handle_chat_scroll(app: &mut App, terminal_size: Rect, key: KeyEvent) -> bool {
    if !matches!(
        &app.mode,
        AppMode::Question {
            focus: QuestionFocus::Chat,
            ..
        }
    ) {
        return false;
    }

    let metrics = question_view_metrics(app, terminal_size);

    let AppMode::Question {
        focus,
        scroll_offset,
        ..
    } = &mut app.mode
    else {
        return false;
    };

    match key.code {
        KeyCode::Enter => {
            *focus = QuestionFocus::Answer;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            *scroll_offset = scroll_offset_down(*scroll_offset, metrics, 1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            *scroll_offset = Some(scroll_offset_up(*scroll_offset, metrics, 1));
        }
        KeyCode::Char('g') => *scroll_offset = Some(0),
        KeyCode::Char('G') => *scroll_offset = None,
        KeyCode::Char('d') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            let step = metrics.view_height / 2;
            *scroll_offset = scroll_offset_down(*scroll_offset, metrics, step);
        }
        KeyCode::Char('u') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
            let step = metrics.view_height / 2;
            *scroll_offset = Some(scroll_offset_up(*scroll_offset, metrics, step));
        }
        _ => return false,
    }

    true
}

/// Scroll metrics for the chat output area in question mode.
#[derive(Clone, Copy)]
struct QuestionViewMetrics {
    total_lines: u16,
    view_height: u16,
}

/// Computes scroll metrics for the chat output area above the question panel.
fn question_view_metrics(app: &App, terminal_size: Rect) -> QuestionViewMetrics {
    let view_height = terminal_size.height.saturating_sub(5);
    let output_width = terminal_size.width.saturating_sub(2);

    let AppMode::Question { session_id, .. } = &app.mode else {
        return QuestionViewMetrics {
            total_lines: 0,
            view_height,
        };
    };

    let session_index = app
        .sessions
        .sessions
        .iter()
        .position(|session| session.id == *session_id);

    let total_lines = session_index
        .and_then(|index| app.sessions.sessions.get(index))
        .map_or(0, |session| {
            let active_progress = app.session_progress_message(session_id);

            SessionChatPage::rendered_output_line_count(
                session,
                None,
                output_width,
                DoneSessionOutputMode::Summary,
                None,
                None,
                active_progress,
            )
        });

    QuestionViewMetrics {
        total_lines,
        view_height,
    }
}

/// Returns the next scroll offset after scrolling down by `step` lines.
fn scroll_offset_down(
    scroll_offset: Option<u16>,
    metrics: QuestionViewMetrics,
    step: u16,
) -> Option<u16> {
    let current_offset = scroll_offset?;
    let next_offset = current_offset.saturating_add(step.max(1));

    if next_offset >= metrics.total_lines.saturating_sub(metrics.view_height) {
        return None;
    }

    Some(next_offset)
}

/// Returns the next scroll offset after scrolling up by `step` lines.
fn scroll_offset_up(scroll_offset: Option<u16>, metrics: QuestionViewMetrics, step: u16) -> u16 {
    let current_offset =
        scroll_offset.unwrap_or_else(|| metrics.total_lines.saturating_sub(metrics.view_height));

    current_offset.saturating_sub(step.max(1))
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

/// Returns the default selected option index for a question at the given
/// position. Returns `Some(0)` when the question has predefined options so
/// the UI starts in option-selection mode, or `None` when there are no
/// predefined options so the input line opens immediately.
pub(crate) fn default_option_index(
    questions: &[QuestionItem],
    question_index: usize,
) -> Option<usize> {
    questions
        .get(question_index)
        .filter(|item| !item.options.is_empty())
        .map(|_| 0)
}

/// Semantic action emitted by one question-mode key event.
enum QuestionAction {
    Submit(String),
    /// Ends the entire question turn, filling all remaining questions with
    /// `no answer`.
    EndTurn,
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
            KeyCode::Esc => QuestionAction::EndTurn,
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
fn navigate_option_up(selected_option_index: &mut Option<usize>) {
    *selected_option_index = match *selected_option_index {
        Some(0) => None,
        Some(index) => Some(index.saturating_sub(1)),
        None => unreachable!(),
    };
}

/// Moves the selected option index down, entering free-text mode when
/// advancing past the last predefined option.
fn navigate_option_down(selected_option_index: &mut Option<usize>, option_count: usize) {
    *selected_option_index = match *selected_option_index {
        Some(index) if index + 1 >= option_count => None,
        Some(index) => Some(index + 1),
        None => unreachable!(),
    };
}

/// Resolves a key event in free-text input mode (no option selected).
fn resolve_free_text_key(input: &mut InputState, key: KeyEvent) -> QuestionAction {
    match key.code {
        KeyCode::Backspace if input_key::is_line_delete_backspace(key) => {
            input.delete_current_line();
        }
        KeyCode::Backspace if input_key::is_word_delete_backspace(key) => {
            input_key::delete_word_backward(input);
        }
        KeyCode::Backspace => input.delete_backward(),
        KeyCode::Delete => input.delete_forward(),
        KeyCode::Left if key.modifiers.contains(event::KeyModifiers::SUPER) => {
            input.move_line_start();
        }
        KeyCode::Left
            if key
                .modifiers
                .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT) =>
        {
            input_key::move_cursor_word_left(input);
        }
        KeyCode::Left => input.move_left(),
        KeyCode::Right if key.modifiers.contains(event::KeyModifiers::SUPER) => {
            input.move_line_end();
        }
        KeyCode::Right
            if key
                .modifiers
                .intersects(event::KeyModifiers::ALT | event::KeyModifiers::SHIFT) =>
        {
            input_key::move_cursor_word_right(input);
        }
        KeyCode::Right => input.move_right(),
        KeyCode::Up => input.move_up(),
        KeyCode::Down => input.move_down(),
        KeyCode::Home => input.move_home(),
        KeyCode::End => input.move_end(),
        // Ctrl+a / Ctrl+e: macOS terminals send these for Cmd+Left / Cmd+Right.
        KeyCode::Char('a') if input_key::is_control_key(key) => {
            input.move_line_start();
        }
        KeyCode::Char('e') if input_key::is_control_key(key) => {
            input.move_line_end();
        }
        // Ctrl+f / Ctrl+b: emacs-style forward/backward character movement.
        KeyCode::Char('f') if input_key::is_control_key(key) => {
            input.move_right();
        }
        KeyCode::Char('b') if input_key::is_control_key(key) => {
            input.move_left();
        }
        // Ctrl+p / Ctrl+n: emacs-style up/down line movement.
        KeyCode::Char('p') if input_key::is_control_key(key) => {
            input.move_up();
        }
        KeyCode::Char('n') if input_key::is_control_key(key) => {
            input.move_down();
        }
        // Ctrl+d: emacs-style forward delete.
        KeyCode::Char('d') if input_key::is_control_key(key) => {
            input.delete_forward();
        }
        // Ctrl+k: kill to end of current line.
        KeyCode::Char('k') if input_key::is_control_key(key) => {
            input.delete_to_line_end();
        }
        // Ctrl+w: delete previous word.
        KeyCode::Char('w') if input_key::is_control_key(key) => {
            input_key::delete_word_backward(input);
        }
        // Alt+b / Alt+f: macOS terminals send these for Option+Left / Option+Right.
        KeyCode::Char('b') if input_key::is_alt_key(key) => {
            input_key::move_cursor_word_left(input);
        }
        KeyCode::Char('f') if input_key::is_alt_key(key) => {
            input_key::move_cursor_word_right(input);
        }
        // KeyCode::Tab is handled by the focus toggle at the top level.
        KeyCode::Char('u') if input_key::is_control_key(key) => {
            input.delete_current_line();
        }
        // Ctrl+j / Ctrl+m: alternative newline insertion (macOS terminal compat).
        KeyCode::Char(character) if input_key::is_control_newline_key(key, character) => {
            input.insert_newline();
        }
        KeyCode::Char(character) if input_key::is_insertable_char_key(key) => {
            input.insert_char(character);
        }
        _ => {}
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
    let session_folder = app
        .sessions
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .map_or_else(
            || app.working_dir().to_path_buf(),
            |session| session.folder.clone(),
        );
    let owned_session_id = session_id.to_string();
    let event_tx = app.services.event_sender();

    at_mention::start_loading_entries(event_tx, session_folder, owned_session_id);

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
        input.replace_range(selection.at_start, selection.cursor, &selection.text);
    }

    sync_question_at_mention_state(app);
}

/// Stores one question response and runs follow-up reply when complete.
async fn submit_response(app: &mut App, response: String) {
    let Some((session_id, questions, responses)) = store_question_response(app, response) else {
        return;
    };

    let question_reply = build_question_reply_prompt(&questions, &responses);
    app.mode = AppMode::View {
        done_session_output_mode: DoneSessionOutputMode::Summary,
        review_status_message: None,
        review_text: None,
        session_id: session_id.clone(),
        scroll_offset: None,
    };
    app.reply(&session_id, TurnPrompt::from_text(question_reply))
        .await;
}

/// Ends the question turn without sending a reply to the agent.
///
/// Triggered by `Esc`. The session status is reverted to `Review` so the
/// user can inspect the current diff or start a new follow-up manually.
/// If the database write fails the mode stays on `Question` so the user
/// can retry, avoiding a split between persisted and in-memory state.
///
/// The persisted transition uses the timing-aware status update so any
/// lingering active-work interval is closed before the session returns to
/// `Review`. After the write succeeds it emits both
/// [`AppEvent::SessionUpdated`] and [`AppEvent::RefreshSessions`] so the UI
/// refreshes the focused session snapshot and any list-derived state. It also
/// updates the shared runtime handle status alongside the snapshot so the
/// periodic `sync_from_handles` cycle does not revert the status back to
/// `Question`.
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

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.clone(),
    });
    app.services.emit_app_event(AppEvent::RefreshSessions);

    if let Some(session) = app
        .sessions
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = Status::Review;
    }

    if let Some(handles) = app.sessions.handles.get(&session_id)
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::Review;
    }

    app.mode = AppMode::View {
        done_session_output_mode: DoneSessionOutputMode::Summary,
        review_status_message: None,
        review_text: None,
        session_id,
        scroll_offset: None,
    };
}

/// Writes one response into question mode and returns completion payload when
/// all questions are answered.
fn store_question_response(
    app: &mut App,
    response: String,
) -> Option<(String, Vec<QuestionItem>, Vec<String>)> {
    let AppMode::Question {
        at_mention_state,
        current_index,
        input,
        questions,
        responses,
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

    Some((
        session_id.clone(),
        std::mem::take(questions),
        std::mem::take(responses),
    ))
}

/// Returns a normalized user response, falling back to `no answer`.
fn normalize_response_text(response_text: &str) -> String {
    let trimmed = response_text.trim();
    if trimmed.is_empty() {
        return NO_ANSWER.to_string();
    }

    trimmed.to_string()
}

/// Builds the follow-up prompt sent after all clarification responses are
/// collected.
fn build_question_reply_prompt(questions: &[QuestionItem], responses: &[String]) -> String {
    let mut lines = vec!["Clarifications:".to_string()];

    for (question_index, question) in questions.iter().enumerate() {
        let response = responses
            .get(question_index)
            .map_or(NO_ANSWER, std::string::String::as_str);
        lines.push(format!("{}. Q: {}", question_index + 1, question.text));
        lines.push(format!("   A: {response}"));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::infra::db::Database;
    use crate::ui::state::app_mode::QuestionFocus;

    /// Fake terminal size used by tests that don't exercise scrolling.
    const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 24);

    /// Creates one test app with in-memory persistence.
    async fn new_test_app() -> App {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        App::new(true, base_path.clone(), base_path, None, database)
            .await
            .expect("failed to build app")
    }

    #[tokio::test]
    async fn test_handle_enter_on_type_custom_answer_with_blank_input_records_no_answer() {
        // Arrange — user navigated to "Type custom answer" and entered
        // free-text mode, then pressed Enter with empty input.
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".to_string(),
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
            focus: QuestionFocus::Answer,
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
    async fn test_handle_escape_ends_turn_and_transitions_to_view() {
        // Arrange — two unanswered questions. Esc should cancel the question
        // turn and transition to View without sending a reply.
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-esc".to_string(),
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
            focus: QuestionFocus::Answer,
            input: InputState::with_text("partial answer".to_string()),
            scroll_offset: None,
            selected_option_index: Some(0),
        };

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;

        // Assert — transitions to View mode with no reply sent.
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                done_session_output_mode: DoneSessionOutputMode::Summary,
                ..
            } if session_id == "session-esc"
        ));
    }

    #[tokio::test]
    async fn test_handle_escape_sets_in_memory_session_status_to_review() {
        // Arrange — session exists in memory with Question status. Esc should
        // revert it to Review.
        use std::path::PathBuf;

        use crate::domain::agent::AgentModel;
        use crate::domain::session::{Session, SessionSize, SessionStats};

        let mut app = new_test_app().await;
        let session_id = "session-review-check";
        app.sessions.sessions.push(Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from("/tmp/test"),
            follow_up_tasks: Vec::new(),
            id: session_id.to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: String::new(),
            prompt: String::new(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Question,
            summary: None,
            title: None,
            updated_at: 0,
        });
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
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

        // Assert — session status updated to Review in memory.
        let session = app
            .sessions
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should exist");
        assert_eq!(session.status, Status::Review);
    }

    #[tokio::test]
    async fn test_handle_escape_updates_session_handle_status_to_review() {
        // Arrange — session has a runtime handle with Question status.
        // Esc must update the handle so sync_from_handles does not revert
        // the snapshot status back to Question.
        use std::path::PathBuf;

        use crate::domain::agent::AgentModel;
        use crate::domain::session::{Session, SessionHandles, SessionSize, SessionStats};

        let mut app = new_test_app().await;
        let session_id = "session-handle-review";
        app.sessions.sessions.push(Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from("/tmp/test"),
            follow_up_tasks: Vec::new(),
            id: session_id.to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: String::new(),
            prompt: String::new(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Question,
            summary: None,
            title: None,
            updated_at: 0,
        });
        app.sessions.handles.insert(
            session_id.to_string(),
            SessionHandles::new(String::new(), Status::Question),
        );
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
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

        // Assert — handle status updated so sync_from_handles preserves Review.
        let handles = app
            .sessions
            .handles
            .get(session_id)
            .expect("handle should exist");
        let handle_status = handles.status.lock().expect("lock should succeed");
        assert_eq!(*handle_status, Status::Review);
    }

    #[tokio::test]
    async fn test_handle_escape_closes_open_in_progress_timer_before_review() {
        // Arrange — persisted state still has an open active-work interval
        // when question mode exits.
        let mut app = new_test_app().await;
        let session_id = "session-timer-close";
        let project_id = app
            .services
            .db()
            .upsert_project("/tmp/test", None)
            .await
            .expect("failed to upsert project");
        app.services
            .db()
            .insert_session(
                session_id,
                "gemini-3-flash-preview",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        app.services
            .db()
            .update_session_status_with_timing_at(session_id, "InProgress", 0)
            .await
            .expect("failed to open timing window");
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: session_id.to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Q?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::default(),
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
        let sessions = app
            .services
            .db()
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
    async fn test_handle_enter_on_last_question_transitions_to_view_mode() {
        // Arrange — free-text mode on last question.
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".to_string(),
            questions: vec![QuestionItem {
                options: vec!["Today".to_string(), "Tomorrow".to_string()],
                text: "Need exact date?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
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
            AppMode::View {
                ref session_id,
                done_session_output_mode: DoneSessionOutputMode::Summary,
                ..
            } if session_id == "missing-session"
        ));
    }

    #[tokio::test]
    async fn test_handle_paste_normalizes_line_endings_in_free_text_mode() {
        // Arrange — free-text mode (user selected "Type custom answer").
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: vec!["Default".to_string()],
                text: "Question".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
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
        let mut app = new_test_app().await;
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
            session_id: "session-id".to_string(),
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
            focus: QuestionFocus::Answer,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        }
    }

    #[tokio::test]
    async fn test_handle_down_from_first_selects_second_option() {
        // Arrange
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".to_string(),
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
            focus: QuestionFocus::Answer,
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".to_string(),
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
            focus: QuestionFocus::Answer,
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
        let mut app = new_test_app().await;
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
                focus: QuestionFocus::Chat,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_tab_toggles_focus_from_chat_to_answer() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question { focus, .. } = &mut app.mode {
            *focus = QuestionFocus::Chat;
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
                focus: QuestionFocus::Answer,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_scroll_down_in_chat_focus_updates_scroll_offset() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            *focus = QuestionFocus::Chat;
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
                focus: QuestionFocus::Chat,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_scroll_keys_ignored_in_answer_focus() {
        // Arrange
        let mut app = new_test_app().await;
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
                focus: QuestionFocus::Answer,
                selected_option_index: Some(1),
                scroll_offset: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_jump_to_top_in_chat_focus_sets_offset_zero() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            *focus = QuestionFocus::Chat;
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
                focus: QuestionFocus::Chat,
                scroll_offset: Some(0),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_jump_to_bottom_in_chat_focus_sets_offset_none() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            focus,
            scroll_offset,
            ..
        } = &mut app.mode
        {
            *focus = QuestionFocus::Chat;
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
                focus: QuestionFocus::Chat,
                scroll_offset: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_in_chat_focus_switches_to_answer_without_submitting() {
        // Arrange — chat focused, pressing Enter should return focus to Answer
        // without submitting the question response.
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question { focus, .. } = &mut app.mode {
            *focus = QuestionFocus::Chat;
        }

        // Act
        let _ = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await;

        // Assert — focus switched to Answer, no response submitted.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                focus: QuestionFocus::Answer,
                current_index: 0,
                ref responses,
                ..
            } if responses.is_empty()
        ));
    }

    #[test]
    fn test_build_question_reply_prompt_formats_all_pairs() {
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
        let message = build_question_reply_prompt(&questions, &responses);

        // Assert
        assert_eq!(
            message,
            "Clarifications:\n1. Q: Need target?\n   A: main\n2. Q: Need tests?\n   A: no answer"
        );
    }

    /// Creates a free-text question mode with the given text and cursor
    /// position for modifier key tests.
    fn free_text_question_mode(text: &str, cursor: usize) -> AppMode {
        let mut input = InputState::with_text(text.to_string());
        input.cursor = cursor;

        AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Question?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input,
            scroll_offset: None,
            selected_option_index: None,
        }
    }

    #[tokio::test]
    async fn test_resolve_free_text_super_left_moves_to_line_start() {
        // Arrange
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
    async fn test_resolve_free_text_alt_backspace_deletes_previous_word() {
        // Arrange
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
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
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".to_string(),
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
            focus: QuestionFocus::Answer,
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
}
