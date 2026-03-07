use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
use crate::domain::input::InputState;
use crate::infra::agent::protocol::QuestionItem;
use crate::runtime::EventResult;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode};

/// Default response stored when users skip one model question.
const NO_ANSWER: &str = "no answer";

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
/// Deselects any highlighted option so the pasted text is treated as a custom
/// free-text response.
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
        *selected_option_index = None;
        input.insert_text(&normalized_text);
    }
}

/// Semantic action emitted by one question-mode key event.
enum QuestionAction {
    Submit(String),
    Continue,
}

/// Resolves and applies one key event against question input state.
///
/// When the active question has predefined options, `Up`/`Down` navigate the
/// option list instead of moving the text cursor. Typing any character
/// deselects the highlighted option so the input acts as free-text.
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
    let has_options = option_count > 0;

    let action = match key.code {
        KeyCode::Esc => QuestionAction::Submit(NO_ANSWER.to_string()),
        KeyCode::Enter => {
            if let Some(option_index) = *selected_option_index {
                let selected_text = questions
                    .get(*current_index)
                    .and_then(|item| item.options.get(option_index))
                    .cloned()
                    .unwrap_or_default();

                QuestionAction::Submit(normalize_response_text(&selected_text))
            } else {
                let response_text = input.take_text();

                QuestionAction::Submit(normalize_response_text(&response_text))
            }
        }
        KeyCode::Up if has_options => {
            *selected_option_index = match *selected_option_index {
                None => Some(option_count.saturating_sub(1)),
                Some(0) => None,
                Some(index) => Some(index.saturating_sub(1)),
            };

            QuestionAction::Continue
        }
        KeyCode::Down if has_options => {
            *selected_option_index = match *selected_option_index {
                None => Some(0),
                Some(index) if index + 1 >= option_count => None,
                Some(index) => Some(index + 1),
            };

            QuestionAction::Continue
        }
        KeyCode::Backspace => {
            input.delete_backward();

            QuestionAction::Continue
        }
        KeyCode::Delete => {
            input.delete_forward();

            QuestionAction::Continue
        }
        KeyCode::Left => {
            input.move_left();

            QuestionAction::Continue
        }
        KeyCode::Right => {
            input.move_right();

            QuestionAction::Continue
        }
        KeyCode::Up => {
            input.move_up();

            QuestionAction::Continue
        }
        KeyCode::Down => {
            input.move_down();

            QuestionAction::Continue
        }
        KeyCode::Home => {
            input.move_home();

            QuestionAction::Continue
        }
        KeyCode::End => {
            input.move_end();

            QuestionAction::Continue
        }
        KeyCode::Tab => {
            input.insert_char('\t');

            QuestionAction::Continue
        }
        KeyCode::Char('u') if key.modifiers == event::KeyModifiers::CONTROL => {
            input.delete_current_line();

            QuestionAction::Continue
        }
        KeyCode::Char(character) if is_insertable_char_key(key) => {
            *selected_option_index = None;
            input.insert_char(character);

            QuestionAction::Continue
        }
        _ => QuestionAction::Continue,
    };

    Some(action)
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
    app.reply(&session_id, &question_reply).await;
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
    *selected_option_index = None;

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
    async fn test_handle_enter_with_blank_response_records_no_answer() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            session_id: "missing-session".to_string(),
            questions: vec![
                QuestionItem {
                    options: Vec::new(),
                    text: "Need a target branch?".to_string(),
                },
                QuestionItem {
                    options: Vec::new(),
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
                selected_option_index: None,
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
                    options: Vec::new(),
                    text: "Need design details?".to_string(),
                },
                QuestionItem {
                    options: Vec::new(),
                    text: "Need acceptance tests?".to_string(),
                },
            ],
            responses: Vec::new(),
            current_index: 0,
            input: InputState::with_text("typed answer".to_string()),
            selected_option_index: None,
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
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            session_id: "missing-session".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
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
    async fn test_handle_paste_normalizes_line_endings() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::Question {
            session_id: "session-id".to_string(),
            questions: vec![QuestionItem {
                options: Vec::new(),
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

    /// Creates a question mode with predefined options for navigation tests.
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
            selected_option_index: None,
        }
    }

    #[tokio::test]
    async fn test_handle_down_from_none_selects_first_option() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();

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
    async fn test_handle_up_from_none_selects_last_option() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).await;

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
    async fn test_handle_down_wraps_past_last_to_none() {
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
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;

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
    async fn test_handle_up_wraps_past_first_to_none() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = question_mode_with_options();
        if let AppMode::Question {
            selected_option_index,
            ..
        } = &mut app.mode
        {
            *selected_option_index = Some(0);
        }

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).await;

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
                    options: Vec::new(),
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
                selected_option_index: None,
                ..
            } if responses == &vec!["No".to_string()]
        ));
    }

    #[tokio::test]
    async fn test_handle_char_resets_selected_option_to_none() {
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

    #[test]
    fn test_build_question_reply_prompt_formats_all_pairs() {
        // Arrange
        let questions = vec![
            QuestionItem {
                options: Vec::new(),
                text: "Need target?".to_string(),
            },
            QuestionItem {
                options: Vec::new(),
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
