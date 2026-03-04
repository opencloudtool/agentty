use crossterm::event::{self, KeyCode, KeyEvent};

use crate::app::App;
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
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    let normalized_text = normalize_pasted_text(pasted_text);
    if normalized_text.is_empty() {
        return;
    }

    if let AppMode::Question { input, .. } = &mut app.mode {
        input.insert_text(&normalized_text);
    }
}

/// Semantic action emitted by one question-mode key event.
enum QuestionAction {
    Submit(String),
    Continue,
}

/// Resolves and applies one key event against question input state.
fn resolve_question_action(app: &mut App, key: KeyEvent) -> Option<QuestionAction> {
    let AppMode::Question { input, .. } = &mut app.mode else {
        return None;
    };

    let action = match key.code {
        KeyCode::Esc => QuestionAction::Submit(NO_ANSWER.to_string()),
        KeyCode::Enter => {
            let response_text = input.take_text();

            QuestionAction::Submit(normalize_response_text(&response_text))
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
) -> Option<(String, Vec<String>, Vec<String>)> {
    let AppMode::Question {
        current_index,
        input,
        questions,
        responses,
        session_id,
    } = &mut app.mode
    else {
        return None;
    };

    responses.push(response);
    *current_index += 1;
    *input = crate::domain::input::InputState::default();

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
fn build_question_reply_prompt(questions: &[String], responses: &[String]) -> String {
    let mut lines = vec!["Clarifications:".to_string()];

    for (question_index, question) in questions.iter().enumerate() {
        let response = responses
            .get(question_index)
            .map_or(NO_ANSWER, std::string::String::as_str);
        lines.push(format!("{}. Q: {}", question_index + 1, question));
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
    use crate::infra::db::Database;

    /// Builds one mock app-server client wrapped in `Arc`.
    fn mock_app_server() -> Arc<dyn crate::infra::app_server::AppServerClient> {
        Arc::new(crate::infra::app_server::MockAppServerClient::new())
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
                "Need a target branch?".to_string(),
                "Need tests?".to_string(),
            ],
            responses: Vec::new(),
            current_index: 0,
            input: crate::domain::input::InputState::default(),
        };

        // Act
        let _ = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question {
                current_index: 1,
                ref responses,
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
                "Need design details?".to_string(),
                "Need acceptance tests?".to_string(),
            ],
            responses: Vec::new(),
            current_index: 0,
            input: crate::domain::input::InputState::with_text("typed answer".to_string()),
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
            questions: vec!["Need exact date?".to_string()],
            responses: Vec::new(),
            current_index: 0,
            input: crate::domain::input::InputState::with_text("March 4, 2026".to_string()),
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
            questions: vec!["Question".to_string()],
            responses: Vec::new(),
            current_index: 0,
            input: crate::domain::input::InputState::default(),
        };

        // Act
        handle_paste(&mut app, "line1\r\nline2\rline3");

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Question { ref input, .. } if input.text() == "line1\nline2\nline3"
        ));
    }

    #[test]
    fn test_build_question_reply_prompt_formats_all_pairs() {
        // Arrange
        let questions = vec!["Need target?".to_string(), "Need tests?".to_string()];
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
