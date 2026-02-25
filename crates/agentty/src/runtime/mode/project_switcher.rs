use std::io;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::runtime::EventResult;
use crate::ui::state::app_mode::AppMode;

/// Handles key input while the quick project switcher overlay is open.
///
/// Navigation accepts both arrow keys and vim-style `j`/`k`.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    let mut selected_index = match &app.mode {
        AppMode::ProjectSwitcher { selected_index } => *selected_index,
        _ => return Ok(EventResult::Continue),
    };

    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::List;

            return Ok(EventResult::Continue);
        }
        KeyCode::Enter => {
            let project_switcher_items = app.project_switcher_items();
            if let Some(project_item) = project_switcher_items.get(selected_index)
                && app.switch_project(project_item.id).await.is_ok()
            {
                app.mode = AppMode::List;

                return Ok(EventResult::Continue);
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let item_count = app.project_switcher_items().len();
            selected_index = increment_selected_index(selected_index, item_count);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let item_count = app.project_switcher_items().len();
            selected_index = decrement_selected_index(selected_index, item_count);
        }
        _ => {}
    }

    let item_count = app.project_switcher_items().len();
    selected_index = clamp_selected_index(selected_index, item_count);
    app.mode = AppMode::ProjectSwitcher { selected_index };

    Ok(EventResult::Continue)
}

/// Returns selected index clamped into `[0, project_count - 1]`.
fn clamp_selected_index(selected_index: usize, project_count: usize) -> usize {
    if project_count == 0 {
        return 0;
    }

    selected_index.min(project_count - 1)
}

/// Increments selected index with wrap-around.
fn increment_selected_index(selected_index: usize, project_count: usize) -> usize {
    if project_count == 0 {
        return 0;
    }

    (selected_index + 1) % project_count
}

/// Decrements selected index with wrap-around.
fn decrement_selected_index(selected_index: usize, project_count: usize) -> usize {
    if project_count == 0 {
        return 0;
    }

    if selected_index == 0 {
        return project_count - 1;
    }

    selected_index - 1
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::db::Database;

    async fn new_test_app() -> App {
        let base_path = PathBuf::from("/tmp");
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        App::new(base_path.clone(), base_path, None, database).await
    }

    /// Builds a test app seeded with a second project for navigation tests.
    async fn new_test_app_with_additional_project() -> App {
        let base_path = PathBuf::from("/tmp");
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        database
            .upsert_project("/tmp/agentty-switcher-secondary", None)
            .await
            .expect("failed to seed additional project");

        App::new(base_path.clone(), base_path, None, database).await
    }

    #[tokio::test]
    async fn test_handle_esc_closes_switcher() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::ProjectSwitcher { selected_index: 0 };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_character_input_keeps_selected_index() {
        // Arrange
        let mut app = new_test_app().await;
        app.mode = AppMode::ProjectSwitcher { selected_index: 0 };

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ProjectSwitcher { selected_index: 0 }
        ));
    }

    /// Verifies `j` advances project switcher selection.
    #[tokio::test]
    async fn test_handle_j_moves_selected_index_forward() {
        // Arrange
        let mut app = new_test_app_with_additional_project().await;
        app.mode = AppMode::ProjectSwitcher { selected_index: 0 };

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ProjectSwitcher { selected_index: 1 }
        ));
    }

    /// Verifies `k` moves selection backward and wraps at the first row.
    #[tokio::test]
    async fn test_handle_k_moves_selected_index_backward_with_wrap() {
        // Arrange
        let mut app = new_test_app_with_additional_project().await;
        app.mode = AppMode::ProjectSwitcher { selected_index: 0 };

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::ProjectSwitcher { selected_index: 1 }
        ));
    }

    #[test]
    fn test_increment_selected_index_wraps() {
        // Arrange
        let selected_index = 1;

        // Act
        let next_index = increment_selected_index(selected_index, 2);

        // Assert
        assert_eq!(next_index, 0);
    }
}
