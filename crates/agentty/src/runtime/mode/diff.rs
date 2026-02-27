use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::runtime::EventResult;
use crate::ui::components::file_explorer::FileExplorer;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode, HelpContext};
use crate::ui::util::parse_diff_lines;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    if key.code == KeyCode::Char('?') {
        let mode = std::mem::replace(&mut app.mode, AppMode::List);
        if let AppMode::Diff {
            session_id,
            diff,
            scroll_offset,
            file_explorer_selected_index,
        } = mode
        {
            app.mode = AppMode::Help {
                context: HelpContext::Diff {
                    session_id,
                    diff,
                    scroll_offset,
                    file_explorer_selected_index,
                },
                scroll_offset: 0,
            };
        } else {
            app.mode = mode;
        }

        return EventResult::Continue;
    }

    if let AppMode::Diff {
        session_id,
        diff,
        scroll_offset,
        file_explorer_selected_index,
    } = &mut app.mode
    {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                app.mode = AppMode::View {
                    done_session_output_mode: DoneSessionOutputMode::Summary,
                    session_id: session_id.clone(),
                    scroll_offset: None,
                };
            }
            KeyCode::Char('j') => {
                let parsed = parse_diff_lines(diff);
                let count = FileExplorer::count_items(&parsed);
                let new_index = file_explorer_selected_index
                    .saturating_add(1)
                    .min(count.saturating_sub(1));

                if *file_explorer_selected_index != new_index {
                    *file_explorer_selected_index = new_index;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Char('k') => {
                let new_index = file_explorer_selected_index.saturating_sub(1);

                if *file_explorer_selected_index != new_index {
                    *file_explorer_selected_index = new_index;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down => {
                *scroll_offset = scroll_offset.saturating_add(1);
            }
            KeyCode::Up => {
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

    /// Returns a mock app-server client wrapped in `Arc` for test injection.
    fn mock_app_server() -> std::sync::Arc<dyn crate::infra::app_server::AppServerClient> {
        std::sync::Arc::new(crate::infra::app_server::MockAppServerClient::new())
    }

    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            base_path.clone(),
            base_path,
            None,
            database,
            mock_app_server(),
        )
        .await;

        (app, base_dir)
    }

    #[tokio::test]
    async fn test_handle_quit_key_returns_to_view_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
        };

        // Act
        let event_result = handle(
            &mut app,
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
    async fn test_handle_down_key_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_saturates_scroll_offset_at_zero() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_j_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
        };

        // Act
        handle(
            &mut app,
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
    async fn test_handle_k_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
        };

        // Act
        handle(
            &mut app,
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
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".to_string(),
            diff: "diff output".to_string(),
            scroll_offset: 5,
            file_explorer_selected_index: 3,
        };

        // Act
        let event_result = handle(
            &mut app,
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
                },
                scroll_offset: 0,
            } if session_id == "session-id" && diff == "diff output"
        ));
    }
}
