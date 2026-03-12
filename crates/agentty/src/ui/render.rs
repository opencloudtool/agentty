use std::collections::HashMap;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::session::session_branch;
use crate::app::{SettingsManager, Tab, UpdateStatus};
use crate::domain::project::ProjectListItem;
use crate::domain::session::{DailyActivity, Session};
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, HelpContext};
use crate::ui::{component, router};

/// A trait for UI pages that enforces a standard rendering interface.
pub trait Page {
    /// Renders a page in the provided frame and area.
    fn render(&mut self, f: &mut Frame, area: Rect);
}

/// A trait for UI components that enforces a standard rendering interface.
pub trait Component {
    /// Renders a component in the provided frame and area.
    fn render(&self, f: &mut Frame, area: Rect);
}

/// Immutable data required to draw a single UI frame.
pub struct RenderContext<'a> {
    /// Identifier of the currently active project.
    pub active_project_id: i64,
    pub current_tab: Tab,
    pub git_branch: Option<&'a str>,
    pub git_status: Option<(u32, u32)>,
    pub latest_available_version: Option<&'a str>,
    pub mode: &'a AppMode,
    pub project_table_state: &'a mut TableState,
    pub projects: &'a [ProjectListItem],
    pub session_progress_messages: &'a HashMap<String, String>,
    pub settings: &'a mut SettingsManager,
    pub stats_activity: &'a [DailyActivity],
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
    /// Background auto-update progress state for the status bar.
    pub update_status: Option<&'a UpdateStatus>,
    pub working_dir: &'a Path,
}

/// Renders a complete frame including status bar, content area, and footer.
pub fn render(f: &mut Frame, context: RenderContext<'_>) {
    let area = f.area();
    let outer_chunks = Layout::default()
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let status_bar_area = outer_chunks[0];
    let content_area = outer_chunks[1];
    let footer_bar_area = outer_chunks[2];

    component::status_bar::StatusBar::new(current_version_display_text())
        .latest_available_version(
            context
                .latest_available_version
                .map(std::string::ToString::to_string),
        )
        .update_status(context.update_status.cloned())
        .render(f, status_bar_area);
    render_footer_bar(
        f,
        footer_bar_area,
        context.mode,
        context.sessions,
        context.working_dir,
        context.git_branch,
        context.git_status,
    );

    router::route_frame(f, content_area, context);
}

/// Returns the current app version as displayed in the status bar.
fn current_version_display_text() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Renders the footer bar with directory, branch, and git status info.
fn render_footer_bar(
    f: &mut Frame,
    footer_bar_area: Rect,
    mode: &AppMode,
    sessions: &[Session],
    working_dir: &Path,
    git_branch: Option<&str>,
    git_status: Option<(u32, u32)>,
) {
    let session_id = match mode {
        AppMode::Confirmation {
            session_id: Some(session_id),
            ..
        }
        | AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::OpenCommandSelector {
            restore_view: ConfirmationViewMode { session_id, .. },
            ..
        }
        | AppMode::PublishBranchInput {
            restore_view: ConfirmationViewMode { session_id, .. },
            ..
        }
        | AppMode::Help {
            context: HelpContext::View { session_id, .. } | HelpContext::Diff { session_id, .. },
            ..
        } => Some(session_id.as_str()),
        _ => None,
    };
    let session_for_footer = session_id.and_then(|session_identifier| {
        sessions
            .iter()
            .find(|session| session.id == session_identifier)
    });

    let (footer_dir, footer_branch, footer_status) = match session_for_footer {
        Some(session) => (
            session.folder.to_string_lossy().to_string(),
            Some(session_branch(&session.id)),
            None,
        ),
        None => (
            working_dir.to_string_lossy().to_string(),
            git_branch.map(std::string::ToString::to_string),
            git_status,
        ),
    };

    component::footer_bar::FooterBar::new(footer_dir)
        .git_branch(footer_branch)
        .git_status(footer_status)
        .render(f, footer_bar_area);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{SessionSize, SessionStats, Status};
    use crate::ui::state::app_mode::DoneSessionOutputMode;

    /// Builds one deterministic session fixture for footer render tests.
    fn session_fixture(session_id: &str, folder: &str) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::from(folder),
            id: session_id.to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: String::new(),
            project_name: "project".to_string(),
            prompt: "prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: Some("summary".to_string()),
            title: Some("title".to_string()),
            updated_at: 0,
        }
    }

    /// Flattens one test backend buffer into plain text for assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn render_footer_bar_prefers_session_folder_and_branch_for_view_mode() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-view-mode";
        let session = session_fixture(session_id, "/tmp/session-view-folder");
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let sessions = vec![session];

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    Path::new("/tmp/workspace-root"),
                    Some("main"),
                    Some((2, 1)),
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("/tmp/session-view-folder"));
        assert!(text.contains(&session_branch(session_id)));
    }

    #[test]
    fn render_footer_bar_uses_working_directory_when_mode_has_no_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mode = AppMode::List;
        let sessions = Vec::new();
        let working_dir = Path::new("/tmp/current-workspace");
        let git_branch = Some("feature/test-render");
        let git_status = Some((0, 0));

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    working_dir,
                    git_branch,
                    git_status,
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("/tmp/current-workspace"));
        assert!(text.contains("feature/test-render"));
    }

    #[test]
    fn current_version_display_text_includes_v_prefix() {
        // Arrange

        // Act
        let version = current_version_display_text();

        // Assert
        assert!(version.starts_with('v'));
        assert!(version.len() > 1);
    }
}
