use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::infra::file_index::{self, FileEntry};
use crate::runtime::EventResult;
use crate::ui::state::app_mode::{AppMode, DoneSessionOutputMode, HelpContext};

const DIRECTORY_PREVIEW_PREFIX: &str = "Directory selected:";
const EMPTY_PREVIEW_MESSAGE: &str = "No files found in this location.";
const PATH_SEPARATOR: char = '/';
const ROOT_DIRECTORY_KEY: &str = "";

/// Opens project explorer mode for `session_id` using gitignore-aware file
/// indexing from the source-specific root directory.
///
/// When `return_to_list` is `true`, explorer rows come from the active project
/// working directory. Otherwise rows come from the session worktree folder.
pub(crate) async fn open_for_session(app: &mut App, session_id: &str, return_to_list: bool) {
    let Some(explorer_root) = explorer_root_for_source(app, session_id, return_to_list) else {
        app.mode = AppMode::List;

        return;
    };

    let all_entries = load_entries(explorer_root.clone()).await;
    let expanded_directories = BTreeSet::new();
    let entries = build_visible_entries(&all_entries, &expanded_directories);
    let selected_index = initial_selected_index(&entries);
    let preview = load_preview(explorer_root, entries.get(selected_index).cloned()).await;

    app.mode = AppMode::ProjectExplorer {
        all_entries,
        entries,
        expanded_directories,
        preview,
        return_to_list,
        scroll_offset: 0,
        selected_index,
        session_id: session_id.to_string(),
    };
}

/// Handles key input while the app is in `AppMode::ProjectExplorer`.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
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
        KeyCode::Enter => {
            activate_selected_entry(app).await;
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

/// Returns the explorer root directory from mode source metadata.
///
/// List-origin explorers (`return_to_list = true`) use the active project's
/// working directory. View-origin explorers use the selected session worktree.
fn explorer_root_for_source(app: &App, session_id: &str, return_to_list: bool) -> Option<PathBuf> {
    if return_to_list {
        return Some(app.working_dir().to_path_buf());
    }

    session_folder(app, session_id)
}

/// Returns the active root directory for the current project explorer mode.
fn project_explorer_root(app: &App) -> Option<PathBuf> {
    let AppMode::ProjectExplorer {
        return_to_list,
        session_id,
        ..
    } = &app.mode
    else {
        return None;
    };

    explorer_root_for_source(app, session_id, *return_to_list)
}

/// Loads project-explorer entries from `explorer_root` while keeping terminal
/// handling responsive.
async fn load_entries(explorer_root: PathBuf) -> Vec<FileEntry> {
    tokio::task::spawn_blocking(move || {
        file_index::list_files_for_explorer(explorer_root.as_path(), None, None)
    })
    .await
    .unwrap_or_default()
}

/// Returns visible tree rows from `all_entries` based on expanded directories.
fn build_visible_entries(
    all_entries: &[FileEntry],
    expanded_directories: &BTreeSet<String>,
) -> Vec<FileEntry> {
    let children_by_parent = build_children_by_parent(all_entries);
    let mut visible_entries = Vec::new();
    append_visible_entries(
        &children_by_parent,
        ROOT_DIRECTORY_KEY,
        expanded_directories,
        &mut visible_entries,
    );

    visible_entries
}

/// Groups entries by their direct parent directory and sorts each sibling
/// group in tree order.
fn build_children_by_parent(all_entries: &[FileEntry]) -> BTreeMap<String, Vec<FileEntry>> {
    let mut children_by_parent: BTreeMap<String, Vec<FileEntry>> = BTreeMap::new();

    for entry in all_entries {
        let parent_key = parent_directory(entry.path.as_str())
            .unwrap_or(ROOT_DIRECTORY_KEY)
            .to_string();
        children_by_parent
            .entry(parent_key)
            .or_default()
            .push(entry.clone());
    }

    for siblings in children_by_parent.values_mut() {
        sort_sibling_entries(siblings);
    }

    children_by_parent
}

/// Sorts sibling entries with folders first, then alphabetically by displayed
/// name.
fn sort_sibling_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| entry_name(left.path.as_str()).cmp(entry_name(right.path.as_str())))
            .then_with(|| left.path.cmp(&right.path))
    });
}

/// Returns the display name used for one tree row.
fn entry_name(path: &str) -> &str {
    path.rsplit(PATH_SEPARATOR).next().unwrap_or(path)
}

/// Appends visible rows in depth-first order so expanded directory contents
/// remain adjacent to their parent directory.
fn append_visible_entries(
    children_by_parent: &BTreeMap<String, Vec<FileEntry>>,
    parent_key: &str,
    expanded_directories: &BTreeSet<String>,
    visible_entries: &mut Vec<FileEntry>,
) {
    let Some(children) = children_by_parent.get(parent_key) else {
        return;
    };

    for child in children {
        visible_entries.push(child.clone());

        if child.is_dir && expanded_directories.contains(child.path.as_str()) {
            append_visible_entries(
                children_by_parent,
                child.path.as_str(),
                expanded_directories,
                visible_entries,
            );
        }
    }
}

/// Returns the directory part of `path`, if any.
fn parent_directory(path: &str) -> Option<&str> {
    path.rsplit_once(PATH_SEPARATOR).map(|(parent, _)| parent)
}

/// Returns the initial selected index for project explorer mode.
fn initial_selected_index(entries: &[FileEntry]) -> usize {
    if entries.is_empty() {
        return 0;
    }

    entries
        .iter()
        .position(|entry| !entry.is_dir)
        .unwrap_or_default()
}

/// Loads preview text for one selected project-explorer entry.
async fn load_preview(explorer_root: PathBuf, selected_entry: Option<FileEntry>) -> String {
    tokio::task::spawn_blocking(move || build_preview(explorer_root.as_path(), selected_entry))
        .await
        .unwrap_or_else(|_| EMPTY_PREVIEW_MESSAGE.to_string())
}

/// Builds preview content for one selected entry.
fn build_preview(explorer_root: &Path, selected_entry: Option<FileEntry>) -> String {
    let Some(entry) = selected_entry else {
        return EMPTY_PREVIEW_MESSAGE.to_string();
    };

    if entry.is_dir {
        return format!("{DIRECTORY_PREVIEW_PREFIX} {}", entry.path);
    }

    let path = explorer_root.join(&entry.path);

    std::fs::read_to_string(path)
        .unwrap_or_else(|error| format!("Failed to read `{}`: {error}", entry.path))
}

/// Opens the help overlay while preserving project explorer state.
fn open_help_overlay(app: &mut App) {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::ProjectExplorer {
        all_entries,
        entries,
        expanded_directories,
        preview,
        return_to_list,
        scroll_offset,
        selected_index,
        session_id,
    } = mode
    {
        app.mode = AppMode::Help {
            context: HelpContext::ProjectExplorer {
                all_entries,
                entries,
                expanded_directories,
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
    let Some((session_id, next_index)) = next_selection_context(app, offset) else {
        return;
    };

    let Some(explorer_root) = project_explorer_root(app) else {
        app.mode = AppMode::List;

        return;
    };

    let selected_entry = project_explorer_entry_by_index(app, next_index);
    let preview = load_preview(explorer_root, selected_entry).await;
    apply_preview_and_selection(app, &session_id, next_index, preview);
}

/// Returns the next selection target for explorer navigation.
fn next_selection_context(app: &App, offset: isize) -> Option<(String, usize)> {
    let AppMode::ProjectExplorer {
        entries,
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

    Some((session_id.clone(), next_index))
}

/// Activates the selected row: toggles directory expansion or refreshes file
/// preview.
async fn activate_selected_entry(app: &mut App) {
    let Some((session_id, selected_entry)) = selected_entry_context(app) else {
        return;
    };

    if selected_entry.is_dir {
        toggle_directory_expansion(app, selected_entry.path.as_str());
    }

    let Some(explorer_root) = project_explorer_root(app) else {
        app.mode = AppMode::List;

        return;
    };

    let selected_entry = project_explorer_selected_entry(app);
    let selected_index = project_explorer_selected_index(app).unwrap_or_default();
    let preview = load_preview(explorer_root, selected_entry).await;
    apply_preview_and_selection(app, &session_id, selected_index, preview);
}

/// Returns selected-row context required before async preview loading.
fn selected_entry_context(app: &App) -> Option<(String, FileEntry)> {
    let AppMode::ProjectExplorer {
        entries,
        selected_index,
        session_id,
        ..
    } = &app.mode
    else {
        return None;
    };

    Some((session_id.clone(), entries.get(*selected_index)?.clone()))
}

/// Returns the next expanded-directory set after toggling `directory_path`.
fn toggled_directory_paths(
    expanded_directories: &BTreeSet<String>,
    directory_path: &str,
) -> BTreeSet<String> {
    let mut next_expanded_directories = expanded_directories.clone();

    if next_expanded_directories.remove(directory_path) {
        remove_collapsed_descendants(&mut next_expanded_directories, directory_path);

        return next_expanded_directories;
    }

    next_expanded_directories.insert(directory_path.to_string());

    next_expanded_directories
}

/// Removes expanded descendants when one directory gets collapsed.
fn remove_collapsed_descendants(expanded_directories: &mut BTreeSet<String>, directory_path: &str) {
    let descendant_prefix = format!("{directory_path}{PATH_SEPARATOR}");
    expanded_directories.retain(|path| !path.starts_with(descendant_prefix.as_str()));
}

/// Finds `target_path` in visible entries and falls back to index `0`.
fn selected_index_for_entry_path(entries: &[FileEntry], target_path: &str) -> usize {
    if entries.is_empty() {
        return 0;
    }

    entries
        .iter()
        .position(|entry| entry.path == target_path)
        .unwrap_or_default()
}

/// Toggles one directory expansion state and rebuilds visible tree rows.
fn toggle_directory_expansion(app: &mut App, directory_path: &str) {
    let AppMode::ProjectExplorer {
        all_entries,
        entries,
        expanded_directories,
        selected_index,
        ..
    } = &mut app.mode
    else {
        return;
    };

    *expanded_directories = toggled_directory_paths(expanded_directories, directory_path);
    *entries = build_visible_entries(all_entries, expanded_directories);
    *selected_index = selected_index_for_entry_path(entries, directory_path);
}

/// Returns the selected entry index for project explorer mode.
fn project_explorer_selected_index(app: &App) -> Option<usize> {
    let AppMode::ProjectExplorer { selected_index, .. } = &app.mode else {
        return None;
    };

    Some(*selected_index)
}

/// Returns one visible entry by row index from project explorer mode.
fn project_explorer_entry_by_index(app: &App, index: usize) -> Option<FileEntry> {
    let AppMode::ProjectExplorer { entries, .. } = &app.mode else {
        return None;
    };

    entries.get(index).cloned()
}

/// Returns the currently selected entry from project explorer mode.
fn project_explorer_selected_entry(app: &App) -> Option<FileEntry> {
    let selected_index = project_explorer_selected_index(app)?;

    project_explorer_entry_by_index(app, selected_index)
}

/// Applies preview text and selection updates if explorer still targets
/// `session_id`.
fn apply_preview_and_selection(
    app: &mut App,
    session_id: &str,
    selected_index: usize,
    preview: String,
) {
    let AppMode::ProjectExplorer {
        preview: current_preview,
        scroll_offset,
        selected_index: current_selected_index,
        session_id: current_session_id,
        ..
    } = &mut app.mode
    else {
        return;
    };

    if current_session_id != session_id {
        return;
    }

    *current_preview = preview;
    *current_selected_index = selected_index;
    *scroll_offset = 0;
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

    /// Creates a project-explorer mode fixture with one selectable row.
    fn project_explorer_mode_fixture(
        all_entries: Vec<FileEntry>,
        entries: Vec<FileEntry>,
        selected_index: usize,
        session_id: &str,
        return_to_list: bool,
    ) -> AppMode {
        AppMode::ProjectExplorer {
            all_entries,
            entries,
            expanded_directories: BTreeSet::new(),
            preview: String::new(),
            return_to_list,
            scroll_offset: 0,
            selected_index,
            session_id: session_id.to_string(),
        }
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
    async fn test_open_for_session_from_list_indexes_active_project_working_dir() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        std::fs::write(app.working_dir().join("main-root-only.txt"), "main")
            .expect("failed to write active-project file");

        // Act
        open_for_session(&mut app, &session_id, true).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectExplorer { ref all_entries, .. }
                if all_entries
                    .iter()
                    .any(|entry| entry.path == "main-root-only.txt")
        ));
    }

    #[tokio::test]
    async fn test_open_for_session_from_view_indexes_session_worktree() {
        // Arrange
        let (mut app, _base_dir) = new_test_app_with_git().await;
        let session_id = app
            .create_session()
            .await
            .expect("failed to create session");
        std::fs::write(app.working_dir().join("main-root-only.txt"), "main")
            .expect("failed to write active-project file");
        let session_index = app
            .session_index_for_id(&session_id)
            .expect("missing session");
        let session_folder = app.sessions.sessions[session_index].folder.clone();
        std::fs::write(session_folder.join("worktree-only.txt"), "worktree")
            .expect("failed to write worktree file");

        // Act
        open_for_session(&mut app, &session_id, false).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectExplorer { ref all_entries, .. }
                if all_entries
                    .iter()
                    .any(|entry| entry.path == "worktree-only.txt")
                && !all_entries
                    .iter()
                    .any(|entry| entry.path == "main-root-only.txt")
        ));
    }

    #[tokio::test]
    async fn test_handle_escape_returns_to_list_when_opened_from_list() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        app.mode = project_explorer_mode_fixture(vec![], vec![], 0, "session-id", true);

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
        app.mode = project_explorer_mode_fixture(vec![], vec![], 0, "session-id", false);

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
        let entry = FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        };
        app.mode =
            project_explorer_mode_fixture(vec![entry.clone()], vec![entry], 0, "session-id", true);

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
        let all_entries = vec![
            FileEntry {
                is_dir: false,
                path: "a.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "b.rs".to_string(),
            },
        ];
        app.mode = AppMode::ProjectExplorer {
            all_entries: all_entries.clone(),
            entries: all_entries,
            expanded_directories: BTreeSet::new(),
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

    #[test]
    fn test_build_visible_entries_shows_only_root_with_no_expanded_directory() {
        // Arrange
        let all_entries = vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
        ];
        let expanded_directories = BTreeSet::new();

        // Act
        let entries = build_visible_entries(&all_entries, &expanded_directories);

        // Assert
        assert_eq!(
            entries,
            vec![
                FileEntry {
                    is_dir: true,
                    path: "src".to_string(),
                },
                FileEntry {
                    is_dir: false,
                    path: "README.md".to_string(),
                },
            ]
        );
    }

    #[test]
    fn test_build_visible_entries_keeps_expanded_children_under_directory() {
        // Arrange
        let all_entries = vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: true,
                path: "tests".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "tests/project_explorer.rs".to_string(),
            },
        ];
        let expanded_directories = BTreeSet::from(["src".to_string()]);

        // Act
        let entries = build_visible_entries(&all_entries, &expanded_directories);

        // Assert
        assert_eq!(
            entries,
            vec![
                FileEntry {
                    is_dir: true,
                    path: "src".to_string(),
                },
                FileEntry {
                    is_dir: false,
                    path: "src/lib.rs".to_string(),
                },
                FileEntry {
                    is_dir: false,
                    path: "src/main.rs".to_string(),
                },
                FileEntry {
                    is_dir: true,
                    path: "tests".to_string(),
                },
                FileEntry {
                    is_dir: false,
                    path: "README.md".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_handle_enter_on_directory_toggles_expansion_and_reveals_children() {
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
        std::fs::create_dir_all(session_folder.join("src")).expect("failed to create src folder");
        std::fs::write(session_folder.join("src/main.rs"), "fn main() {}\n")
            .expect("failed to write file");
        let all_entries = vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
        ];
        let visible_entries = vec![FileEntry {
            is_dir: true,
            path: "src".to_string(),
        }];
        app.mode = AppMode::ProjectExplorer {
            all_entries,
            entries: visible_entries,
            expanded_directories: BTreeSet::new(),
            preview: String::new(),
            return_to_list: false,
            scroll_offset: 0,
            selected_index: 0,
            session_id,
        };

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectExplorer {
                ref entries,
                ref expanded_directories,
                ..
            } if expanded_directories.contains("src")
                && entries
                    .iter()
                    .any(|entry| entry.path == "src/main.rs" && !entry.is_dir)
        ));
    }

    #[tokio::test]
    async fn test_handle_enter_on_file_resets_preview_scroll() {
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
        std::fs::write(session_folder.join("README.md"), "updated preview")
            .expect("failed to write readme");
        let file_entry = FileEntry {
            is_dir: false,
            path: "README.md".to_string(),
        };
        app.mode = AppMode::ProjectExplorer {
            all_entries: vec![file_entry.clone()],
            entries: vec![file_entry],
            expanded_directories: BTreeSet::new(),
            preview: String::new(),
            return_to_list: false,
            scroll_offset: 8,
            selected_index: 0,
            session_id,
        };

        // Act
        handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("failed to handle key");

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectExplorer {
                scroll_offset: 0,
                ref preview,
                ..
            } if preview == "updated preview"
        ));
    }
}
