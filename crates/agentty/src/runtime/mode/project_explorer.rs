use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::infra::file_index::{self, FileEntry};
use crate::runtime::EventResult;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode, HelpContext};

const DIRECTORY_PREVIEW_PREFIX: &str = "Directory selected:";
const EMPTY_PREVIEW_MESSAGE: &str = "No files found in this worktree.";

/// Opens project explorer mode for `session_id` using gitignore-aware file
/// indexing.
pub(crate) async fn open_for_session(app: &mut App, session_id: &str, return_to_list: bool) {
    let Some(session_folder) = session_folder(app, session_id) else {
        app.mode = AppMode::List;

        return;
    };

    let entries = load_entries(session_folder.clone()).await;
    let selected_index = initial_selected_index(&entries);
    let preview = load_preview(session_folder, entries.get(selected_index).cloned()).await;

    app.mode = AppMode::ProjectExplorer {
        entries,
        preview,
        return_to_list,
        scroll_offset: 0,
        selected_index,
        session_id: session_id.to_string(),
    };
}

/// Handles key input while the app is in `AppMode::ProjectExplorer`.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    let _ = open_for_session;

    match key.code {
        KeyCode::Char('?') => {
            open_help_overlay(app);
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            close_project_explorer(app);
        }
        KeyCode::Char('j') if is_plain_char_key(key, 'j') => {
            move_selection(app, 1).await;
        }
        KeyCode::Char('k') if is_plain_char_key(key, 'k') => {
            move_selection(app, -1).await;
        }
        KeyCode::Down => {
            scroll_preview(app, 1);
        }
        KeyCode::Up => {
            scroll_preview(app, -1);
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Returns the active session folder for `session_id`.
fn session_folder(app: &App, session_id: &str) -> Option<PathBuf> {
    let session_index = app.session_index_for_id(session_id)?;

    Some(app.sessions.sessions.get(session_index)?.folder.clone())
}

/// Loads project-explorer entries from `session_folder` while keeping terminal
/// handling responsive.
async fn load_entries(session_folder: PathBuf) -> Vec<FileEntry> {
    tokio::task::spawn_blocking(move || {
        file_index::list_files_for_explorer(session_folder.as_path(), None, None)
    })
    .await
    .unwrap_or_default()
}

/// Returns the initial selected index for project explorer mode.
fn initial_selected_index(entries: &[FileEntry]) -> usize {
    entries
        .iter()
        .position(|entry| !entry.is_dir)
        .unwrap_or_default()
}

/// Loads preview text for one selected project-explorer entry.
async fn load_preview(session_folder: PathBuf, selected_entry: Option<FileEntry>) -> String {
    tokio::task::spawn_blocking(move || build_preview(session_folder.as_path(), selected_entry))
        .await
        .unwrap_or_else(|_| EMPTY_PREVIEW_MESSAGE.to_string())
}

/// Builds preview content for one selected entry.
fn build_preview(session_folder: &Path, selected_entry: Option<FileEntry>) -> String {
    let Some(entry) = selected_entry else {
        return EMPTY_PREVIEW_MESSAGE.to_string();
    };

    if entry.is_dir {
        return format!("{DIRECTORY_PREVIEW_PREFIX} {}", entry.path);
    }

    let path = session_folder.join(&entry.path);

    std::fs::read_to_string(path)
        .unwrap_or_else(|error| format!("Failed to read `{}`: {error}", entry.path))
}

/// Opens the help overlay while preserving project explorer state.
fn open_help_overlay(app: &mut App) {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::ProjectExplorer {
        entries,
        preview,
        return_to_list,
        scroll_offset,
        selected_index,
        session_id,
    } = mode
    {
        app.mode = AppMode::Help {
            context: HelpContext::ProjectExplorer {
                entries,
                preview,
                return_to_list,
                scroll_offset,
                selected_index,
                session_id,
            },
            scroll_offset: 0,
        };

        return;
    }

    app.mode = mode;
}

/// Returns to list or session view based on explorer entry source.
fn close_project_explorer(app: &mut App) {
    let Some((return_to_list, session_id)) = project_explorer_origin(app) else {
        return;
    };

    if return_to_list {
        app.mode = AppMode::List;

        return;
    }

    app.mode = AppMode::View {
        done_session_output_mode: DoneSessionOutputMode::Summary,
        focused_review_status_message: None,
        focused_review_text: None,
        session_id,
        scroll_offset: None,
    };
}

/// Returns explorer source metadata used to choose close behavior.
fn project_explorer_origin(app: &App) -> Option<(bool, String)> {
    let AppMode::ProjectExplorer {
        return_to_list,
        session_id,
        ..
    } = &app.mode
    else {
        return None;
    };

    Some((*return_to_list, session_id.clone()))
}

/// Updates selected index and refreshes preview content.
async fn move_selection(app: &mut App, offset: isize) {
    let Some((entries, return_to_list, selected_index, session_id)) = next_selection(app, offset)
    else {
        return;
    };

    let Some(session_folder) = session_folder(app, &session_id) else {
        app.mode = AppMode::List;

        return;
    };

    let preview = load_preview(session_folder, entries.get(selected_index).cloned()).await;

    app.mode = AppMode::ProjectExplorer {
        entries,
        preview,
        return_to_list,
        scroll_offset: 0,
        selected_index,
        session_id,
    };
}

/// Computes the next explorer selection target.
fn next_selection(app: &App, offset: isize) -> Option<(Vec<FileEntry>, bool, usize, String)> {
    let AppMode::ProjectExplorer {
        entries,
        return_to_list,
        selected_index,
        session_id,
        ..
    } = &app.mode
    else {
        return None;
    };

    if entries.is_empty() {
        return None;
    }

    let current_index = *selected_index;
    let next_index = if offset.is_negative() {
        current_index.saturating_sub(offset.unsigned_abs())
    } else {
        current_index
            .saturating_add(offset.unsigned_abs())
            .min(entries.len().saturating_sub(1))
    };

    if next_index == current_index {
        return None;
    }

    Some((
        entries.clone(),
        *return_to_list,
        next_index,
        session_id.clone(),
    ))
}

/// Updates preview scroll position by `offset` lines.
fn scroll_preview(app: &mut App, offset: i16) {
    let AppMode::ProjectExplorer { scroll_offset, .. } = &mut app.mode else {
        return;
    };

    if offset.is_negative() {
        *scroll_offset = scroll_offset.saturating_sub(offset.unsigned_abs());

        return;
    }

    *scroll_offset = scroll_offset.saturating_add(offset.unsigned_abs());
}

/// Returns true when `key` is the exact plain character without modifiers.
fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == KeyModifiers::NONE
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

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

    fn setup_test_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config failed");
        std::fs::write(path.join("README.md"), "test").expect("write failed");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit failed");
        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(path)
            .output()
            .expect("git branch failed");
    }

    async fn new_test_app_with_git() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        setup_test_git_repo(base_dir.path());
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let app = App::new(
            base_path.clone(),
            base_path,
            Some("main".to_string()),
            database,
            mock_app_server(),
        )
        .await;

        (app, base_dir)
    }

    #[tokio::test]
    async fn test_open_for_session_sets_project_explorer_mode() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let expected_session_id = app
            .create_session()
            .await
            .expect("failed to create session");

        // Act
        open_for_session(&mut app, &expected_session_id, true).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectExplorer {
                ref session_id,
                return_to_list: true,
                ..
            } if session_id == &expected_session_id
        ));
    }

    #[tokio::test]
    async fn test_handle_escape_returns_to_list_when_opened_from_list() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::ProjectExplorer {
            entries: vec![],
            preview: String::new(),
            return_to_list: true,
            scroll_offset: 0,
            selected_index: 0,
            session_id: "session-id".to_string(),
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_escape_returns_to_view_when_opened_from_view() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::ProjectExplorer {
            entries: vec![],
            preview: String::new(),
            return_to_list: false,
            scroll_offset: 0,
            selected_index: 0,
            session_id: "session-id".to_string(),
        };

        // Act
        let event_result = handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

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
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = AppMode::ProjectExplorer {
            entries: vec![FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            }],
            preview: "fn main() {}".to_string(),
            return_to_list: true,
            scroll_offset: 0,
            selected_index: 0,
            session_id: "session-id".to_string(),
        };

        // Act
        let event_result = handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to handle key");

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::ProjectExplorer { .. },
                scroll_offset: 0,
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_j_moves_selection_and_resets_scroll() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing session");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        std::fs::write(session_folder.join("a.rs"), "alpha").expect("failed to write file");
        std::fs::write(session_folder.join("b.rs"), "beta").expect("failed to write file");

        app.mode = AppMode::ProjectExplorer {
            entries: vec![
                FileEntry {
                    is_dir: false,
                    path: "a.rs".to_string(),
                },
                FileEntry {
                    is_dir: false,
                    path: "b.rs".to_string(),
                },
            ],
            preview: "alpha".to_string(),
            return_to_list: false,
            scroll_offset: 5,
            selected_index: 0,
            session_id,
        };

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
            AppMode::ProjectExplorer {
                selected_index: 1,
                scroll_offset: 0,
                ..
            }
        ));
    }
}
