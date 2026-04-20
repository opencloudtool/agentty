use std::collections::HashMap;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::session::session_branch;
use crate::app::session_state::SessionGitStatus;
use crate::app::{SettingsManager, Tab, UpdateStatus};
use crate::domain::project::ProjectListItem;
use crate::domain::session::{DailyActivity, Session};
use crate::ui::state::app_mode::{AppMode, ConfirmationViewMode, HelpContext};
use crate::ui::{component, markdown, page, router};

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
    /// Exact prompt transcript blocks keyed by session id for active turns.
    pub active_prompt_outputs: &'a HashMap<String, String>,
    /// Identifier of the currently active project.
    pub active_project_id: i64,
    /// Active top-level tab selection.
    pub current_tab: Tab,
    /// Whether the active project exposes the roadmap-backed `Tasks` tab.
    pub has_tasks_tab: bool,
    /// Current local branch name for the active project.
    pub git_branch: Option<&'a str>,
    /// Current upstream reference tracked by the active project branch.
    pub git_upstream_ref: Option<&'a str>,
    /// Latest ahead/behind counts for the active project branch.
    pub git_status: Option<(u32, u32)>,
    /// Newer stable version when one is available.
    pub latest_available_version: Option<&'a str>,
    /// Shared render cache for session transcript markdown output.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current app mode and its transient state.
    pub mode: &'a AppMode,
    /// Table selection state for the projects list.
    pub project_table_state: &'a mut TableState,
    /// Project rows available for rendering.
    pub projects: &'a [ProjectListItem],
    /// Detected session worktree branch names keyed by session id.
    pub session_branch_names: &'a HashMap<String, String>,
    /// Latest session-branch ahead/behind snapshots keyed by session id,
    /// including both base-branch and tracked-remote comparisons.
    pub session_git_statuses: &'a HashMap<String, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    pub session_index_by_id: &'a HashMap<String, usize>,
    /// Background thinking messages keyed by session id.
    pub session_progress_messages: &'a HashMap<String, String>,
    /// Whether each rendered session currently has a materialized worktree on
    /// disk, keyed by session id.
    pub session_worktree_availability: &'a HashMap<String, bool>,
    /// Mutable project-scoped settings snapshot.
    pub settings: &'a mut SettingsManager,
    /// Daily session activity series used by the stats view.
    pub stats_activity: &'a [DailyActivity],
    /// Loaded roadmap markdown for the active project, when available.
    pub task_roadmap: Option<&'a str>,
    /// User-visible roadmap load failure for the active project, when present.
    pub task_roadmap_error: Option<&'a str>,
    /// Current vertical scroll offset for the roadmap-backed `Tasks` page.
    pub task_roadmap_scroll_offset: u16,
    /// Session rows available for rendering.
    pub sessions: &'a [Session],
    /// Table selection state for the session list.
    pub table_state: &'a mut TableState,
    /// Background auto-update progress state for the status bar.
    pub update_status: Option<&'a UpdateStatus>,
    /// Absolute one-minute rotation slot used for page-scoped status-bar FYIs.
    pub status_bar_fyi_rotation_index: u64,
    /// Current wall-clock time expressed as Unix seconds for deterministic
    /// render-time timers.
    pub wall_clock_unix_seconds: i64,
    /// Working directory for the active project.
    pub working_dir: &'a Path,
}

/// Project-scoped footer inputs used when no session-specific footer override
/// is active.
#[derive(Clone, Copy)]
struct ProjectFooterContext<'a> {
    /// Current local branch name for the active project.
    git_branch: Option<&'a str>,
    /// Latest ahead/behind counts for the active project branch.
    git_status: Option<(u32, u32)>,
    /// Current upstream reference tracked by the active project branch.
    git_upstream_ref: Option<&'a str>,
    /// Working directory displayed in the footer.
    working_dir: &'a Path,
}

/// Borrowed data required to render the footer bar for one frame.
#[derive(Clone, Copy)]
struct FooterBarRenderContext<'a> {
    /// Active app mode used to resolve session-scoped footer overrides.
    mode: &'a AppMode,
    /// Project footer values used when the active mode is not session-scoped.
    project: ProjectFooterContext<'a>,
    /// Detected session worktree branch names keyed by session id.
    session_branch_names: &'a HashMap<String, String>,
    /// Latest session-branch ahead/behind snapshots keyed by session id.
    session_git_statuses: &'a HashMap<String, SessionGitStatus>,
    /// Cached session list positions keyed by stable session id.
    session_index_by_id: &'a HashMap<String, usize>,
    /// Session rows available for resolving the active footer session.
    sessions: &'a [Session],
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
        .page_fyis(page::fyi::current_page_messages(
            context.current_tab,
            context.mode,
        ))
        .fyi_rotation_index(context.status_bar_fyi_rotation_index)
        .update_status(context.update_status.cloned())
        .render(f, status_bar_area);
    render_footer_bar(
        f,
        footer_bar_area,
        FooterBarRenderContext {
            mode: context.mode,
            project: ProjectFooterContext {
                git_branch: context.git_branch,
                git_status: context.git_status,
                git_upstream_ref: context.git_upstream_ref,
                working_dir: context.working_dir,
            },
            session_branch_names: context.session_branch_names,
            session_git_statuses: context.session_git_statuses,
            session_index_by_id: context.session_index_by_id,
            sessions: context.sessions,
        },
    );

    router::route_frame(f, content_area, context);
}

/// Returns the current app version as displayed in the status bar.
fn current_version_display_text() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Renders the footer bar with directory, branch, and project- or
/// session-scoped git status info.
///
/// Project branches show upstream-tracking counts. Session branches reuse the
/// same footer widget but inject counts relative to each session's base
/// branch and, when available, its tracked remote branch.
fn render_footer_bar(f: &mut Frame, footer_bar_area: Rect, context: FooterBarRenderContext<'_>) {
    let FooterBarRenderContext {
        mode,
        project,
        session_branch_names,
        session_git_statuses,
        session_index_by_id,
        sessions,
    } = context;
    let session_id = match mode {
        AppMode::Confirmation {
            session_id: Some(session_id),
            ..
        }
        | AppMode::View { session_id, .. }
        | AppMode::Prompt { session_id, .. }
        | AppMode::Question { session_id, .. }
        | AppMode::Diff { session_id, .. }
        | AppMode::ViewInfoPopup {
            restore_view: ConfirmationViewMode { session_id, .. },
            ..
        }
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
    let session_for_footer = session_id
        .and_then(|session_identifier| session_index_by_id.get(session_identifier).copied())
        .and_then(|session_index| sessions.get(session_index));

    let (
        footer_dir,
        footer_branch,
        footer_base_ref,
        footer_upstream_ref,
        footer_base_status,
        footer_status,
    ) = match session_for_footer {
        Some(session) => {
            let session_status = session_git_statuses
                .get(&session.id)
                .copied()
                .unwrap_or_default();

            (
                session.folder.to_string_lossy().to_string(),
                Some(
                    session_branch_names
                        .get(&session.id)
                        .cloned()
                        .unwrap_or_else(|| session_branch(&session.id)),
                ),
                Some(session.base_branch.clone()),
                session.published_upstream_ref.clone(),
                session_status.base_status,
                session_status.remote_status,
            )
        }
        None => (
            project.working_dir.to_string_lossy().to_string(),
            project.git_branch.map(std::string::ToString::to_string),
            None,
            project
                .git_upstream_ref
                .map(std::string::ToString::to_string),
            None,
            project.git_status,
        ),
    };

    component::footer_bar::FooterBar::new(footer_dir)
        .git_branch(footer_branch)
        .git_base_ref(footer_base_ref)
        .git_base_status(footer_base_status)
        .git_upstream_ref(footer_upstream_ref)
        .git_status(footer_status)
        .render(f, footer_bar_area);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::domain::session::tests::SessionFixtureBuilder;
    use crate::ui::state::app_mode::DoneSessionOutputMode;

    /// Builds one deterministic session fixture for footer render tests.
    fn session_fixture(session_id: &str, folder: &str) -> Session {
        SessionFixtureBuilder::new()
            .id(session_id)
            .folder(PathBuf::from(folder))
            .prompt("prompt")
            .summary(Some("summary".to_string()))
            .title(Some("title".to_string()))
            .build()
    }

    /// Flattens one test backend buffer into plain text for assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Builds a deterministic session-id lookup map for footer render tests.
    fn session_index_by_id(sessions: &[Session]) -> HashMap<String, usize> {
        sessions
            .iter()
            .enumerate()
            .map(|(session_index, session)| (session.id.clone(), session_index))
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
            review_status_message: None,
            review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let sessions = vec![session];
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch: Some("main"),
                            git_status: Some((2, 1)),
                            git_upstream_ref: Some("origin/main"),
                            working_dir: Path::new("/tmp/workspace-root"),
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &HashMap::new(),
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("/tmp/session-view-folder"));
        assert!(text.contains(&session_branch(session_id)));
    }

    #[test]
    fn render_footer_bar_prefers_session_upstream_reference_for_view_mode() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "upstream";
        let mut session = session_fixture(session_id, "/tmp/session-view-folder");
        session.published_upstream_ref = Some("origin/wt/upstream".to_string());
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let sessions = vec![session];
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch: Some("main"),
                            git_status: Some((2, 1)),
                            git_upstream_ref: Some("origin/main"),
                            working_dir: Path::new("/tmp/workspace-root"),
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &HashMap::new(),
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("wt/upstream -> origin/wt/upstream"));
    }

    #[test]
    fn render_footer_bar_prefers_session_branch_for_view_info_popup() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "popup";
        let mut session = session_fixture(session_id, "/tmp/session-popup-folder");
        session.published_upstream_ref = Some("origin/wt/popup".to_string());
        let mode = AppMode::ViewInfoPopup {
            is_loading: false,
            loading_label: "Publishing branch".to_string(),
            message: "Published".to_string(),
            restore_view: ConfirmationViewMode {
                done_session_output_mode: DoneSessionOutputMode::Summary,
                review_status_message: None,
                review_text: None,
                scroll_offset: None,
                session_id: session_id.to_string(),
            },
            title: "Branch pushed".to_string(),
        };
        let sessions = vec![session];
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch: Some("main"),
                            git_status: Some((2, 1)),
                            git_upstream_ref: Some("origin/main"),
                            working_dir: Path::new("/tmp/workspace-root"),
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &HashMap::new(),
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("wt/popup -> origin/wt/popup"));
        assert!(!text.contains("main -> origin/main"));
    }

    #[test]
    fn render_footer_bar_uses_working_directory_when_mode_has_no_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mode = AppMode::List;
        let sessions = Vec::new();
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names = HashMap::new();
        let working_dir = Path::new("/tmp/current-workspace");
        let git_branch = Some("feature/test-render");
        let git_status = Some((0, 0));

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch,
                            git_status,
                            git_upstream_ref: Some("origin/feature/test-render"),
                            working_dir,
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &HashMap::new(),
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("/tmp/current-workspace"));
        assert!(text.contains("feature/test-render -> origin/feature/test-render"));
    }

    #[test]
    fn render_footer_bar_uses_session_git_status_when_available() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-status";
        let mut session = session_fixture(session_id, "/tmp/session-status-folder");
        session.published_upstream_ref = Some("origin/wt/session-status".to_string());
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let sessions = vec![session];
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names = HashMap::new();
        let session_git_statuses = HashMap::from([(
            session_id.to_string(),
            SessionGitStatus {
                base_status: Some((3, 2)),
                remote_status: Some((1, 4)),
            },
        )]);

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch: Some("main"),
                            git_status: Some((0, 0)),
                            git_upstream_ref: Some("origin/main"),
                            working_dir: Path::new("/tmp/workspace-root"),
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &session_git_statuses,
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("↓2 ↑3 main"));
        assert!(text.contains("↓4 ↑1 wt/session- -> origin/wt/session-status"));
        assert!(!text.contains("↓0"));
    }

    #[test]
    fn render_footer_bar_uses_session_git_status_without_published_upstream() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-unpublished-status";
        let session = session_fixture(session_id, "/tmp/session-unpublished-status-folder");
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let sessions = vec![session];
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names = HashMap::new();
        let session_git_statuses = HashMap::from([(
            session_id.to_string(),
            SessionGitStatus {
                base_status: Some((5, 1)),
                remote_status: None,
            },
        )]);

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch: Some("main"),
                            git_status: Some((0, 0)),
                            git_upstream_ref: Some("origin/main"),
                            working_dir: Path::new("/tmp/workspace-root"),
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &session_git_statuses,
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("↓1 ↑5 main"));
        assert!(text.contains("| ✓ wt/session-"));
        assert!(!text.contains("origin/wt/session-unpublished-status"));
    }

    #[test]
    fn render_footer_bar_uses_detected_session_branch_name_for_legacy_worktrees() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "legacy";
        let session = session_fixture(session_id, "/tmp/session-legacy-folder");
        let mode = AppMode::View {
            done_session_output_mode: DoneSessionOutputMode::Summary,
            review_status_message: None,
            review_text: None,
            session_id: session_id.to_string(),
            scroll_offset: None,
        };
        let sessions = vec![session];
        let session_index_by_id = session_index_by_id(&sessions);
        let session_branch_names =
            HashMap::from([(session_id.to_string(), "agentty/legacy".to_string())]);

        // Act
        terminal
            .draw(|frame| {
                render_footer_bar(
                    frame,
                    frame.area(),
                    FooterBarRenderContext {
                        mode: &mode,
                        project: ProjectFooterContext {
                            git_branch: Some("main"),
                            git_status: Some((2, 1)),
                            git_upstream_ref: Some("origin/main"),
                            working_dir: Path::new("/tmp/workspace-root"),
                        },
                        session_branch_names: &session_branch_names,
                        session_git_statuses: &HashMap::new(),
                        session_index_by_id: &session_index_by_id,
                        sessions: &sessions,
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("agentty/legacy"));
        assert!(!text.contains("wt/legacy"));
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
