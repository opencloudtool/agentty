use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::{SettingsManager, Tab};
use crate::domain::project::ProjectListItem;
use crate::domain::session::{DailyActivity, Session};
use crate::ui::overlay::{SyncBlockedPopupRenderContext, ViewInfoPopupRenderContext};
use crate::ui::state::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::ui::{Component, Page, RenderContext, component, overlay, page};

/// Shared borrowed data required to render list-page backgrounds.
pub(crate) struct ListBackgroundRenderContext<'a> {
    /// Identifier for the currently active project in the project list tab.
    pub(crate) active_project_id: i64,
    pub(crate) current_tab: Tab,
    pub(crate) project_table_state: &'a mut TableState,
    pub(crate) projects: &'a [ProjectListItem],
    pub(crate) sessions: &'a [Session],
    pub(crate) settings: &'a mut SettingsManager,
    pub(crate) stats_activity: &'a [DailyActivity],
    pub(crate) table_state: &'a mut TableState,
}

/// Shared mutable routing data reused across app modes in `route_frame`.
struct RouteSharedContext<'a> {
    /// Identifier for the active project shared across list-mode renders.
    active_project_id: i64,
    current_tab: Tab,
    project_table_state: &'a mut TableState,
    projects: &'a [ProjectListItem],
    sessions: &'a [Session],
    settings: &'a mut SettingsManager,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl RouteSharedContext<'_> {
    /// Creates a list-background context for overlays/pages that render on top
    /// of the tabbed list content.
    fn list_background(&mut self) -> ListBackgroundRenderContext<'_> {
        ListBackgroundRenderContext {
            active_project_id: self.active_project_id,
            current_tab: self.current_tab,
            project_table_state: self.project_table_state,
            projects: self.projects,
            sessions: self.sessions,
            settings: self.settings,
            stats_activity: self.stats_activity,
            table_state: self.table_state,
        }
    }
}

/// Borrowed inputs for rendering a session chat page.
#[derive(Clone, Copy)]
struct SessionChatRenderContext<'a> {
    mode: &'a AppMode,
    session_id: &'a str,
    session_progress_messages: &'a HashMap<String, String>,
    sessions: &'a [Session],
    scroll_offset: Option<u16>,
}

/// Shared immutable routing inputs that are not part of list-background state.
#[derive(Clone, Copy)]
struct RouteAuxContext<'a> {
    session_progress_messages: &'a HashMap<String, String>,
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        current_tab,
        mode,
        project_table_state,
        projects,
        session_progress_messages,
        settings,
        stats_activity,
        sessions,
        table_state,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        active_project_id,
        current_tab,
        project_table_state,
        projects,
        sessions,
        settings,
        stats_activity,
        table_state,
    };

    let aux = RouteAuxContext {
        session_progress_messages,
    };

    if render_list_or_overlay_mode(f, area, mode, &mut shared, aux) {
        return;
    }

    render_session_or_diff_mode(f, area, mode, shared.sessions, aux);
}

/// Renders all list/overlay-driven modes and returns whether it handled `mode`.
fn render_list_or_overlay_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &mut RouteSharedContext<'_>,
    aux: RouteAuxContext<'_>,
) -> bool {
    match mode {
        AppMode::List => render_list_background(f, area, shared.list_background()),
        AppMode::Confirmation {
            confirmation_intent,
            confirmation_message,
            confirmation_title,
            restore_view,
            selected_confirmation_index,
            ..
        } => {
            if matches!(
                confirmation_intent,
                ConfirmationIntent::MergeSession | ConfirmationIntent::RegenerateFocusedReview
            ) && let Some(view_mode) = restore_view
            {
                render_session_confirmation_overlay(
                    f,
                    area,
                    &SessionConfirmationContext {
                        confirmation_message,
                        confirmation_title,
                        selected_confirmation_index: *selected_confirmation_index,
                    },
                    view_mode,
                    shared.sessions,
                    aux.session_progress_messages,
                );
            } else {
                overlay::render_confirmation_overlay(f, area, mode, shared.list_background());
            }
        }

        AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } => overlay::render_sync_blocked_popup(
            f,
            area,
            shared.list_background(),
            SyncBlockedPopupRenderContext {
                default_branch: default_branch.as_deref(),
                is_loading: *is_loading,
                message,
                project_name: project_name.as_deref(),
                title,
            },
        ),
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            restore_view,
            title,
        } => overlay::render_view_info_popup(
            f,
            area,
            restore_view,
            shared.sessions,
            aux.session_progress_messages,
            ViewInfoPopupRenderContext {
                is_loading: *is_loading,
                loading_label,
                message,
                title,
            },
        ),
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => overlay::render_help(
            f,
            area,
            help_context,
            *scroll_offset,
            shared.list_background(),
            aux.session_progress_messages,
        ),
        AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::OpenCommandSelector { .. }
        | AppMode::Diff { .. } => {
            return false;
        }
    }

    true
}

/// Borrowed context for the confirmation overlay portion of a session-scoped
/// confirmation render (merge, regenerate focused review).
struct SessionConfirmationContext<'a> {
    /// The body text displayed inside the confirmation dialog.
    confirmation_message: &'a str,
    /// The header title of the confirmation dialog.
    confirmation_title: &'a str,
    /// Index of the currently highlighted confirmation option.
    selected_confirmation_index: usize,
}

/// Renders a session-scoped confirmation above the originating session chat
/// page.
fn render_session_confirmation_overlay(
    f: &mut Frame,
    area: Rect,
    context: &SessionConfirmationContext<'_>,
    view_mode: &ConfirmationViewMode,
    sessions: &[Session],
    session_progress_messages: &HashMap<String, String>,
) {
    let background_mode = view_mode.clone().into_view_mode();

    render_session_chat(
        f,
        area,
        SessionChatRenderContext {
            mode: &background_mode,
            session_id: &view_mode.session_id,
            session_progress_messages,
            sessions,
            scroll_offset: view_mode.scroll_offset,
        },
    );
    overlay::render_overlay_backdrop(f, area);

    component::confirmation_overlay::ConfirmationOverlay::new(
        context.confirmation_title,
        context.confirmation_message,
    )
    .selected_yes(context.selected_confirmation_index == 0)
    .render(f, area);
}

/// Renders session-scoped modes tied to one selected session.
fn render_session_or_diff_mode(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    sessions: &[Session],
    aux: RouteAuxContext<'_>,
) {
    match mode {
        AppMode::View {
            session_id,
            scroll_offset,
            ..
        }
        | AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        } => render_session_chat(
            f,
            area,
            SessionChatRenderContext {
                mode,
                session_id,
                session_progress_messages: aux.session_progress_messages,
                sessions,
                scroll_offset: *scroll_offset,
            },
        ),
        AppMode::Question { session_id, .. } => render_session_chat(
            f,
            area,
            SessionChatRenderContext {
                mode,
                session_id,
                session_progress_messages: aux.session_progress_messages,
                sessions,
                scroll_offset: None,
            },
        ),
        AppMode::OpenCommandSelector {
            commands,
            restore_view,
            selected_command_index,
        } => render_open_command_selector_overlay(
            f,
            area,
            sessions,
            aux.session_progress_messages,
            restore_view,
            commands,
            *selected_command_index,
        ),
        AppMode::Diff {
            diff,
            file_explorer_selected_index,
            scroll_offset,
            session_id,
        } => render_diff_mode(
            f,
            area,
            sessions,
            session_id,
            diff,
            *scroll_offset,
            *file_explorer_selected_index,
        ),
        AppMode::List
        | AppMode::Confirmation { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::ViewInfoPopup { .. }
        | AppMode::Help { .. } => {}
    }
}

/// Renders open-command selection overlay above the originating session chat.
fn render_open_command_selector_overlay(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    session_progress_messages: &HashMap<String, String>,
    restore_view: &ConfirmationViewMode,
    commands: &[String],
    selected_command_index: usize,
) {
    let background_mode = restore_view.clone().into_view_mode();

    render_session_chat(
        f,
        area,
        SessionChatRenderContext {
            mode: &background_mode,
            session_id: &restore_view.session_id,
            session_progress_messages,
            sessions,
            scroll_offset: restore_view.scroll_offset,
        },
    );
    overlay::render_overlay_backdrop(f, area);

    component::open_command_overlay::OpenCommandOverlay::new(commands)
        .selected_command_index(selected_command_index)
        .render(f, area);
}

/// Renders the session chat page for all session-chat modes.
fn render_session_chat(f: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        mode,
        session_id,
        session_progress_messages,
        sessions,
        scroll_offset,
    } = context;

    let Some(session_index) = sessions.iter().position(|session| session.id == session_id) else {
        return;
    };

    let active_progress = session_progress_messages
        .get(session_id)
        .map(std::string::String::as_str);

    page::session_chat::SessionChatPage::new(
        sessions,
        session_index,
        scroll_offset,
        mode,
        active_progress,
    )
    .render(f, area);
}

/// Renders the diff page for a specific session when present.
fn render_diff_mode(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    session_id: &str,
    diff: &str,
    scroll_offset: u16,
    file_explorer_selected_index: usize,
) {
    if let Some(session) = sessions.iter().find(|session| session.id == session_id) {
        page::diff::DiffPage::new(
            session,
            diff.to_string(),
            scroll_offset,
            file_explorer_selected_index,
        )
        .render(f, area);
    }
}

/// Renders base list tabs and the currently selected list tab content.
pub(crate) fn render_list_background(
    f: &mut Frame,
    content_area: Rect,
    context: ListBackgroundRenderContext<'_>,
) {
    let ListBackgroundRenderContext {
        active_project_id,
        current_tab,
        project_table_state,
        projects,
        sessions,
        settings,
        stats_activity,
        table_state,
    } = context;

    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    component::tab::Tabs::new(current_tab, active_project_id, projects).render(f, chunks[0]);

    match current_tab {
        Tab::Projects => {
            page::project_list::ProjectListPage::new(
                projects,
                project_table_state,
                active_project_id,
            )
            .render(f, chunks[1]);
        }
        Tab::Sessions => {
            page::session_list::SessionListPage::new(sessions, table_state).render(f, chunks[1]);
        }
        Tab::Stats => {
            page::stat::StatsPage::new(sessions, stats_activity).render(f, chunks[1]);
        }
        Tab::Settings => {
            page::setting::SettingsPage::new(settings).render(f, chunks[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::widgets::Paragraph;

    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::session::{SessionSize, SessionStats, Status};
    use crate::ui::state::app_mode::DoneSessionOutputMode;

    /// Builds one deterministic session fixture for router render tests.
    fn session_fixture(session_id: &str) -> Session {
        Session {
            base_branch: "main".to_string(),
            created_at: 0,
            folder: PathBuf::from(format!("/tmp/{session_id}")),
            id: session_id.to_string(),
            model: AgentModel::Gemini3FlashPreview,
            output: "Captured output".to_string(),
            project_name: "project".to_string(),
            prompt: "Prompt".to_string(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            stats: SessionStats::default(),
            status: Status::Review,
            summary: Some("Summary line for router test".to_string()),
            title: Some("Router Session".to_string()),
            updated_at: 0,
        }
    }

    /// Flattens a rendered test buffer into a plain string for text assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn render_session_or_diff_mode_renders_view_session_content() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-1234";
        let sessions = vec![session_fixture(session_id)];
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let progress_messages = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_or_diff_mode(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        session_progress_messages: &progress_messages,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Router Session"));
        assert!(text.contains("Captured output"));
    }

    #[test]
    fn render_session_or_diff_mode_keeps_background_when_session_is_missing() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            session_id: "missing-session".to_string(),
            scroll_offset: None,
        };
        let progress_messages = HashMap::new();
        let sessions = Vec::new();

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(Paragraph::new("sentinel"), area);
                render_session_or_diff_mode(
                    frame,
                    area,
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        session_progress_messages: &progress_messages,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("sentinel"));
    }

    #[test]
    fn render_session_or_diff_mode_renders_diff_page_for_matching_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-diff";
        let mut session = session_fixture(session_id);
        session.title = Some("Diff Session".to_string());
        let sessions = vec![session];
        let mode = AppMode::Diff {
            session_id: session_id.to_string(),
            diff: String::new(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
        };
        let progress_messages = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_or_diff_mode(
                    frame,
                    frame.area(),
                    &mode,
                    &sessions,
                    RouteAuxContext {
                        session_progress_messages: &progress_messages,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Diff Session"));
        assert!(text.contains("No changes found."));
    }

    #[test]
    fn render_session_confirmation_overlay_renders_confirmation_text() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-merge";
        let sessions = vec![session_fixture(session_id)];
        let progress_messages = HashMap::new();
        let confirmation_context = SessionConfirmationContext {
            confirmation_message: "Queue merge now?",
            confirmation_title: "Confirm Merge",
            selected_confirmation_index: 0,
        };
        let view_mode = ConfirmationViewMode {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            focused_review_status_message: None,
            focused_review_text: None,
            scroll_offset: None,
            session_id: session_id.to_string(),
        };

        // Act
        terminal
            .draw(|frame| {
                render_session_confirmation_overlay(
                    frame,
                    frame.area(),
                    &confirmation_context,
                    &view_mode,
                    &sessions,
                    &progress_messages,
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Confirm Merge"));
        assert!(text.contains("Queue merge now?"));
    }
}
