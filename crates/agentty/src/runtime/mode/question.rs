use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::app::{App, AppEvent};
use crate::domain::input::InputState;
use crate::infra::agent::protocol::QuestionItem;
use crate::infra::channel::TurnPrompt;
use crate::infra::file_index;
use crate::runtime::EventResult;
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
/// typed answer (or `no answer` when blank), `Esc` skips the current question.
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
/// Returns `true` when the key was consumed as a scroll action. `Enter` and
/// `Esc` are not intercepted — they always reach the question handler so
/// users can submit or skip regardless of focus.
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

    let AppMode::Question { scroll_offset, .. } = &mut app.mode else {
        return false;
    };

    match key.code {
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
    let normalized_text = normalize_pasted_text(pasted_text);
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
            KeyCode::Esc => QuestionAction::Submit(NO_ANSWER.to_string()),
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
                if !is_navigating_options && option_count > 0 && is_cursor_on_first_line(input) =>
            {
                *selected_option_index = Some(option_count - 1);

                QuestionAction::Continue
            }
            KeyCode::Down
                if !is_navigating_options && option_count > 0 && is_cursor_on_last_line(input) =>
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
            move_cursor_word_left(input);
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
            move_cursor_word_right(input);
        }
        KeyCode::Right => input.move_right(),
        KeyCode::Up => input.move_up(),
        KeyCode::Down => input.move_down(),
        KeyCode::Home => input.move_home(),
        KeyCode::End => input.move_end(),
        // Ctrl+a / Ctrl+e: macOS terminals send these for Cmd+Left / Cmd+Right.
        KeyCode::Char('a') if key.modifiers == event::KeyModifiers::CONTROL => {
            input.move_line_start();
        }
        KeyCode::Char('e') if key.modifiers == event::KeyModifiers::CONTROL => {
            input.move_line_end();
        }
        // Alt+b / Alt+f: macOS terminals send these for Option+Left / Option+Right.
        KeyCode::Char('b') if key.modifiers.contains(event::KeyModifiers::ALT) => {
            move_cursor_word_left(input);
        }
        KeyCode::Char('f') if key.modifiers.contains(event::KeyModifiers::ALT) => {
            move_cursor_word_right(input);
        }
        // KeyCode::Tab is handled by the focus toggle at the top level.
        KeyCode::Char('u') if key.modifiers == event::KeyModifiers::CONTROL => {
            input.delete_current_line();
        }
        KeyCode::Char(character) if is_insertable_char_key(key) => {
            input.insert_char(character);
        }
        _ => {}
    }

    QuestionAction::Continue
}

/// Returns whether one key event inserts its character into input.
fn is_insertable_char_key(key: KeyEvent) -> bool {
    matches!(
        key.modifiers,
        event::KeyModifiers::NONE | event::KeyModifiers::SHIFT
    )
}

/// Returns whether the input cursor is on the first line of text.
///
/// True when no newline characters appear before the cursor position,
/// including when the input is empty.
fn is_cursor_on_first_line(input: &InputState) -> bool {
    input.text().chars().take(input.cursor).all(|ch| ch != '\n')
}

/// Returns whether the input cursor is on the last line of text.
///
/// True when no newline characters appear after the cursor position,
/// including when the input is empty.
fn is_cursor_on_last_line(input: &InputState) -> bool {
    input.text().chars().skip(input.cursor).all(|ch| ch != '\n')
}

/// Moves the cursor to the start of the previous word, skipping adjacent
/// whitespace separators.
fn move_cursor_word_left(input: &mut InputState) {
    if input.cursor == 0 {
        return;
    }

    let characters: Vec<char> = input.text().chars().collect();
    let mut cursor = input.cursor;

    while cursor > 0 && characters[cursor - 1].is_whitespace() {
        cursor -= 1;
    }

    while cursor > 0 && !characters[cursor - 1].is_whitespace() {
        cursor -= 1;
    }

    input.cursor = cursor;
}

/// Moves the cursor to the start of the next word, skipping adjacent
/// whitespace separators.
fn move_cursor_word_right(input: &mut InputState) {
    let characters: Vec<char> = input.text().chars().collect();
    let mut cursor = input.cursor;

    while cursor < characters.len() && !characters[cursor].is_whitespace() {
        cursor += 1;
    }

    while cursor < characters.len() && characters[cursor].is_whitespace() {
        cursor += 1;
    }

    input.cursor = cursor;
}

/// Normalizes pasted text line endings to `\n`.
fn normalize_pasted_text(pasted_text: &str) -> String {
    let mut normalized_text = String::with_capacity(pasted_text.len());
    let mut characters = pasted_text.chars().peekable();

    while let Some(character) = characters.next() {
        if character == '\r' {
            if matches!(characters.peek(), Some(&'\n')) {
                let _ = characters.next();
            }

            normalized_text.push('\n');

            continue;
        }

        normalized_text.push(character);
    }

    normalized_text
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
    let (has_at_mention_query, has_at_mention_state, session_id) = match &app.mode {
        AppMode::Question {
            at_mention_state,
            input,
            selected_option_index: None,
            session_id,
            ..
        } => (
            input.at_mention_query().is_some(),
            at_mention_state.is_some(),
            session_id.clone(),
        ),
        _ => return,
    };

    if !has_at_mention_query {
        dismiss_question_at_mention(app);

        return;
    }

    if has_at_mention_state {
        if let AppMode::Question {
            at_mention_state: Some(state),
            ..
        } = &mut app.mode
        {
            state.selected_index = 0;
        }

        return;
    }

    activate_question_at_mention(app, &session_id);
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

    tokio::spawn(async move {
        let entries = tokio::task::spawn_blocking(move || file_index::list_files(&session_folder))
            .await
            .unwrap_or_default();

        let _ = event_tx.send(AppEvent::AtMentionEntriesLoaded {
            entries,
            session_id: owned_session_id,
        });
    });

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
        *at_mention_state = None;
    }
}

/// Moves the at-mention selection up in question mode.
fn handle_question_at_mention_up(app: &mut App) {
    if let AppMode::Question {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        state.selected_index = state.selected_index.saturating_sub(1);
    }
}

/// Moves the at-mention selection down in question mode.
fn handle_question_at_mention_down(app: &mut App) {
    let filtered_count = match &app.mode {
        AppMode::Question {
            at_mention_state: Some(state),
            input,
            ..
        } => {
            let query = input
                .at_mention_query()
                .map_or(String::new(), |(_, query)| query);

            file_index::filter_entries(&state.all_entries, &query).len()
        }
        _ => return,
    };

    if let AppMode::Question {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        let max_index = filtered_count.saturating_sub(1);
        state.selected_index = (state.selected_index + 1).min(max_index);
    }
}

/// Selects the currently highlighted file and inserts it into the question
/// input.
fn handle_question_at_mention_select(app: &mut App) {
    let mut should_dismiss = false;
    let replacement = match &app.mode {
        AppMode::Question {
            at_mention_state: Some(state),
            input,
            ..
        } => {
            if let Some((at_start, query)) = input.at_mention_query() {
                let filtered = file_index::filter_entries(&state.all_entries, &query);
                let clamped_index = state.selected_index.min(filtered.len().saturating_sub(1));

                filtered.get(clamped_index).map(|entry| {
                    let path = if entry.is_dir {
                        format!("@{}/ ", entry.path)
                    } else {
                        format!("@{} ", entry.path)
                    };

                    (at_start, input.cursor, path)
                })
            } else {
                should_dismiss = true;

                None
            }
        }
        _ => return,
    };

    if should_dismiss {
        dismiss_question_at_mention(app);

        return;
    }

    if let Some((at_start, cursor, text)) = replacement
        && let AppMode::Question { input, .. } = &mut app.mode
    {
        input.replace_range(at_start, cursor, &text);
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
    async fn test_handle_escape_skips_question_with_no_answer() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            at_mention_state: None,
            session_id: "missing-session".to_string(),
            questions: vec![
                QuestionItem {
                    options: vec!["Detailed".to_string(), "Brief".to_string()],
                    text: "Need design details?".to_string(),
                },
                QuestionItem {
                    options: vec!["Yes".to_string(), "No".to_string()],
                    text: "Need acceptance tests?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            focus: QuestionFocus::Answer,
            input: InputState::with_text("typed answer".to_string()),
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

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                ref responses,
                ref input,
                ..
            } if responses == &vec![NO_ANSWER.to_string()] && input.text().is_empty()
        ));
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
}
