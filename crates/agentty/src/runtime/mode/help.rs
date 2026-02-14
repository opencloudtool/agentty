use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::model::AppMode;
use crate::runtime::EventResult;

/// Handles key input while the app is showing the help overlay.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    if let AppMode::Help {
        scroll_offset,
        context: _,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('?' | 'q') | KeyCode::Esc => {
                let mode = std::mem::replace(&mut app.mode, AppMode::List);
                if let AppMode::Help { context, .. } = mode {
                    app.mode = context.restore_mode();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                *scroll_offset = scroll_offset.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll_offset = scroll_offset.saturating_sub(1);
            }
            _ => {}
        }
    }

    EventResult::Continue
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;
    use crate::model::HelpContext;

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(base_path.clone(), base_path, None, database).await;

        (app, base_dir)
    }

    #[tokio::test]
    async fn test_handle_question_mark_restores_list_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::List,
            scroll_offset: 0,
        };

        // Act
        let result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_quit_key_restores_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::View {
                session_id: "s1".to_string(),
                scroll_offset: Some(5),
            },
            scroll_offset: 0,
        };

        // Act
        let result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(5),
            } if session_id == "s1"
        ));
    }

    #[tokio::test]
    async fn test_handle_escape_restores_health_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::Health,
            scroll_offset: 0,
        };

        // Act
        let result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::Health));
    }

    #[tokio::test]
    async fn test_handle_down_key_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::List,
            scroll_offset: 0,
        };

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_saturates_at_zero() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::List,
            scroll_offset: 0,
        };

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Help {
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_non_help_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_restores_diff_mode_with_content() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                session_id: "s1".to_string(),
                diff: "diff content".to_string(),
                scroll_offset: 7,
            },
            scroll_offset: 3,
        };

        // Act
        handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                ref session_id,
                ref diff,
                scroll_offset: 7,
            } if session_id == "s1" && diff == "diff content"
        ));
    }
}
