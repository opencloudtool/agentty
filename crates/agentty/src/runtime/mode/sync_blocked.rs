use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::runtime::EventResult;
use crate::ui::state::app_mode::AppMode;

/// Handles key input while the sync informational popup is visible.
pub(crate) fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::SyncBlockedPopup { is_loading, .. } = &app.mode else {
        return EventResult::Continue;
    };

    if *is_loading {
        return EventResult::Continue;
    }

    if should_close_sync_blocked_popup(key) {
        app.mode = AppMode::List;
    }

    EventResult::Continue
}

/// Returns whether the pressed key should close the sync informational popup.
fn should_close_sync_blocked_popup(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => true,
        KeyCode::Char(character) => character.eq_ignore_ascii_case(&'q'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use tempfile::tempdir;

    use super::*;
    use crate::db::Database;

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
    async fn test_handle_esc_closes_sync_blocked_popup() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: "Main is dirty".to_string(),
            project_name: None,
            title: "Sync blocked".to_string(),
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_enter_closes_sync_blocked_popup() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: "Main is dirty".to_string(),
            project_name: None,
            title: "Sync blocked".to_string(),
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_other_key_keeps_sync_blocked_popup_open() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: false,
            message: "Main is dirty".to_string(),
            project_name: None,
            title: "Sync blocked".to_string(),
        };

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::SyncBlockedPopup { .. }));
    }

    #[tokio::test]
    async fn test_handle_enter_does_not_close_loading_sync_popup() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::SyncBlockedPopup {
            default_branch: None,
            is_loading: true,
            message: "Synchronizing...".to_string(),
            project_name: None,
            title: "Sync in progress".to_string(),
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::SyncBlockedPopup {
                is_loading: true,
                ..
            }
        ));
    }
}
