use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::domain::input::InputState;
use crate::infra::agent::protocol::QuestionItem;
use crate::infra::channel::TurnPrompt;
use crate::runtime::EventResult;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode};

/// Default response stored when users skip one model question.
const NO_ANSWER: &str = "no answer";

/// Label for the virtual "type custom answer" option appended after predefined
/// options.
pub(crate) const TYPE_CUSTOM_ANSWER: &str = "Type custom answer";

/// Applies one key event in question-answer mode.
///
/// `Enter` submits the typed answer (or `no answer` when blank), `Esc` skips
/// the current question, and text-editing keys update the active input.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let Some(action) = resolve_question_action(app, key) else {
        return EventResult::Continue;
    };

    if let QuestionAction::Submit(response) = action {
        submit_response(app, response).await;
    }

    EventResult::Continue
}

/// Inserts pasted text into the active question response input.
///
/// Paste only takes effect when the user is in free-text mode
/// (`selected_option_index` is `None`). While navigating predefined options,
/// paste is ignored — the user must first select "Type custom answer".
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
}

/// Returns the default selected option index for a question at the given
/// position. Always returns `Some(0)` for valid questions so the UI starts
/// in option-selection mode. The virtual "Type custom answer" entry is
/// always available as the last selectable choice.
pub(crate) fn default_option_index(
    questions: &[QuestionItem],
    question_index: usize,
) -> Option<usize> {
    questions.get(question_index).map(|_| 0)
}

/// Semantic action emitted by one question-mode key event.
enum QuestionAction {
    Submit(String),
    Continue,
}

/// Resolves and applies one key event against question input state.
///
/// When navigating predefined options (`selected_option_index` is `Some`),
/// `Up`/`Down`/`j`/`k` cycle through the options plus a virtual "Type custom
/// answer" entry. Selecting that entry and pressing `Enter` transitions to
/// free-text mode where the text input is visible and all keys type normally.
fn resolve_question_action(app: &mut App, key: KeyEvent) -> Option<QuestionAction> {
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

    let action = match key.code {
        KeyCode::Esc => QuestionAction::Submit(NO_ANSWER.to_string()),
        KeyCode::Enter => resolve_enter_action(
            input,
            questions,
            *current_index,
            selected_option_index,
            option_count,
        ),
        KeyCode::Up | KeyCode::Char('k') if is_navigating_options => {
            navigate_option_up(selected_option_index, option_count);

            QuestionAction::Continue
        }
        KeyCode::Down | KeyCode::Char('j') if is_navigating_options => {
            navigate_option_down(selected_option_index, option_count);

            QuestionAction::Continue
        }
        _ if !is_navigating_options => resolve_free_text_key(input, key),
        _ => QuestionAction::Continue,
    };

    Some(action)
}

/// Resolves an `Enter` key press in question mode.
///
/// When navigating options, submits the highlighted option or enters free-text
/// mode for the virtual "Type custom answer" entry. In free-text mode, submits
/// the typed text.
fn resolve_enter_action(
    input: &mut InputState,
    questions: &[QuestionItem],
    current_index: usize,
    selected_option_index: &mut Option<usize>,
    option_count: usize,
) -> QuestionAction {
    match *selected_option_index {
        Some(option_index) if option_index >= option_count => {
            *selected_option_index = None;

            QuestionAction::Continue
        }
        Some(option_index) => {
            let selected_text = questions
                .get(current_index)
                .and_then(|item| item.options.get(option_index))
                .cloned()
                .unwrap_or_default();

            QuestionAction::Submit(normalize_response_text(&selected_text))
        }
        None => {
            let response_text = input.take_text();

            QuestionAction::Submit(normalize_response_text(&response_text))
        }
    }
}

/// Moves the selected option index up (wrapping to the virtual "Type custom
/// answer" entry).
fn navigate_option_up(selected_option_index: &mut Option<usize>, option_count: usize) {
    let total_count = option_count + 1;
    *selected_option_index = match *selected_option_index {
        Some(0) => Some(total_count - 1),
        Some(index) => Some(index.saturating_sub(1)),
        None => unreachable!(),
    };
}

/// Moves the selected option index down (wrapping from the virtual "Type
/// custom answer" entry back to the first option).
fn navigate_option_down(selected_option_index: &mut Option<usize>, option_count: usize) {
    let total_count = option_count + 1;
    *selected_option_index = match *selected_option_index {
        Some(index) if index + 1 >= total_count => Some(0),
        Some(index) => Some(index + 1),
        None => unreachable!(),
    };
}

/// Resolves a key event in free-text input mode (no option selected).
fn resolve_free_text_key(input: &mut InputState, key: KeyEvent) -> QuestionAction {
    match key.code {
        KeyCode::Backspace => input.delete_backward(),
        KeyCode::Delete => input.delete_forward(),
        KeyCode::Left => input.move_left(),
        KeyCode::Right => input.move_right(),
        KeyCode::Up => input.move_up(),
        KeyCode::Down => input.move_down(),
        KeyCode::Home => input.move_home(),
        KeyCode::End => input.move_end(),
        KeyCode::Tab => input.insert_char('\t'),
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

/// Stores one question response and runs follow-up reply when complete.
async fn submit_response(app: &mut App, response: String) {
    let Some((session_id, questions, responses)) = store_question_response(app, response) else {
        return;
    };

    let question_reply = build_question_reply_prompt(&questions, &responses);
    app.mode = AppMode::View {
        done_session_output_mode: DoneSessionOutputMode::Summary,
        focused_review_status_message: None,
        focused_review_text: None,
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
        current_index,
        input,
        questions,
        responses,
        selected_option_index,
        session_id,
    } = &mut app.mode
    else {
        return None;
    };

    responses.push(response);
    *current_index += 1;
    *input = InputState::default();
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
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::infra::app_server;
    use crate::infra::db::Database;

    /// Builds one mock app-server client wrapped in `Arc`.
    fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
        Arc::new(app_server::MockAppServerClient::new())
    }

    /// Creates one test app with in-memory persistence.
    async fn new_test_app() -> App {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        App::new(
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await
    }

    #[tokio::test]
    async fn test_handle_enter_on_type_custom_answer_with_blank_input_records_no_answer() {
        // Arrange — user navigated to "Type custom answer" and entered
        // free-text mode, then pressed Enter with empty input.
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
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
            input: InputState::default(),
            selected_option_index: None,
        };

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

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
            input: InputState::with_text("typed answer".to_string()),
            selected_option_index: Some(0),
        };

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;

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
            session_id: "missing-session".to_string(),
            questions: vec![QuestionItem {
                options: vec!["Today".to_string(), "Tomorrow".to_string()],
                text: "Need exact date?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::with_text("March 4, 2026".to_string()),
            selected_option_index: None,
        };

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

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
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: vec!["Default".to_string()],
                text: "Question".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::default(),
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
            input: InputState::default(),
            selected_option_index: Some(0),
        }
    }

    #[tokio::test]
    async fn test_handle_down_from_first_selects_second_option() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;

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
    async fn test_handle_up_from_first_wraps_to_type_custom_answer() {
        // Arrange — 3 real options → virtual "Type custom answer" at index 3.
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(3),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_down_from_last_real_selects_type_custom_answer() {
        // Arrange — 3 real options → virtual at index 3.
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
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: Some(3),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_down_from_type_custom_answer_wraps_to_first() {
        // Arrange — virtual option is at index 3.
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = Some(3);
        }

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;

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
            input: InputState::default(),
            selected_option_index: Some(1),
        };

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

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
    async fn test_handle_enter_on_type_custom_answer_enters_free_text_mode() {
        // Arrange — virtual "Type custom answer" is at index 3 (3 real options).
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = Some(3);
        }

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

        // Assert — transitions to free-text mode.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                selected_option_index: None,
                current_index: 0,
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
            input: InputState::with_text("answer".to_string()),
            selected_option_index: None,
        };

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

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
    fn test_default_option_index_returns_first_for_question_without_predefined_options() {
        // Arrange — even without predefined options, the virtual "Type custom
        // answer" entry is always available so the UI starts in selection mode.
        let questions = vec![QuestionItem {
            options: Vec::new(),
            text: "Type something?".to_string(),
        }];

        // Act & Assert
        assert_eq!(default_option_index(&questions, 0), Some(0));
    }

    #[test]
    fn test_default_option_index_returns_none_for_out_of_bounds() {
        // Arrange
        let questions: Vec<QuestionItem> = Vec::new();

        // Act & Assert
        assert_eq!(default_option_index(&questions, 0), None);
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
}
