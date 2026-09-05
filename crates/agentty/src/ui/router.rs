use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;

use crate::app::Tab;
use crate::app::session_state::SessionGitStatus;
use crate::domain::agent::{AgentCliInfo, ReasoningLevel};
use crate::domain::project::{ProjectListItem, ordered_project_items};
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    DailyActivity, Session, SessionId, activity_day_key_with_offset, can_append_session_to_stack,
    can_create_stacked_child,
};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget,
    DiffReviewComments, DiffSidebarFocus, HelpContext, allows_diff_line_comment_reply,
};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::settings::SettingsScreenSnapshot;
use crate::ui::{
    Component, Page, RenderContext, SessionReviewSnapshot, component, markdown, overlay, page,
};

/// Shared mutable routing data reused across app modes in `route_frame`.
struct RouteSharedContext<'a> {
    /// Identifier for the active project shared across list-mode renders.
    active_project_id: i64,
    /// Locally available agent CLI executables and detected versions.
    available_agent_clis: &'a [AgentCliInfo],
    current_tab: Tab,
    default_reasoning_level: ReasoningLevel,
    /// Cached most-recently-opened ordering over `projects`.
    mru_project_order: &'a [usize],
    project_table_state: &'a mut TableState,
    projects: &'a [ProjectListItem],
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    sessions: &'a [Session],
    settings_screen: Option<&'a SettingsScreenSnapshot>,
    stats_activity: &'a [DailyActivity],
    table_state: &'a mut TableState,
}

impl RouteSharedContext<'_> {
    /// Returns whether the selected session can be used as a stacked parent.
    fn can_create_stacked_session(&self) -> bool {
        self.current_tab == Tab::Sessions
            && self
                .table_state
                .selected()
                .and_then(|selected_index| self.sessions.get(selected_index))
                .is_some_and(|session| can_create_stacked_child(self.sessions, session.id.as_str()))
    }

    /// Returns whether the selected session has at least one eligible stack
    /// parent.
    fn can_append_selected_session(&self) -> bool {
        let Some(session_id) = self
            .table_state
            .selected()
            .and_then(|selected_index| self.sessions.get(selected_index))
            .map(|session| session.id.as_str())
        else {
            return false;
        };

        self.sessions.iter().any(|candidate| {
            can_append_session_to_stack(self.sessions, session_id, candidate.id.as_str())
        })
    }

    /// Returns eligible parent rows for one source session.
    fn stack_append_parent_sessions(&self, session_id: &str) -> Vec<&Session> {
        self.sessions
            .iter()
            .filter(|candidate| {
                can_append_session_to_stack(self.sessions, session_id, candidate.id.as_str())
            })
            .collect()
    }
}

/// UI-private base page selected for the active mode.
#[derive(Clone, Copy)]
enum Surface<'a> {
    Diff {
        diff: &'a str,
        file_explorer_selected_index: usize,
        focus: DiffFocus,
        line_comments: &'a DiffLineComments,
        preview: &'a DiffPreview,
        review_comments: Option<&'a DiffReviewComments>,
        restore: Option<&'a DiffRestoreTarget>,
        scroll_offset: u16,
        selected_diff_line_index: usize,
        session_id: &'a str,
        sidebar_focus: DiffSidebarFocus,
    },
    DiffLoading {
        session_id: &'a str,
        sidebar_focus: DiffSidebarFocus,
    },
    List,
    Session {
        mode: SessionSurfaceMode<'a>,
        scroll_offset: Option<u16>,
        session_id: &'a str,
    },
}

impl Surface<'_> {
    /// Returns the stable base-page identity used across mode overlays.
    fn kind(self) -> SurfaceKind {
        match self {
            Self::Diff { .. } | Self::DiffLoading { .. } => SurfaceKind::Diff,
            Self::List => SurfaceKind::List,
            Self::Session { .. } => SurfaceKind::Session,
        }
    }
}

/// Stable identity for the base page painted beneath mode-specific overlays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceKind {
    Diff,
    List,
    Session,
}

/// Session mode used while rendering a chat surface.
#[derive(Clone, Copy)]
enum SessionSurfaceMode<'a> {
    Interactive(&'a AppMode),
    View,
}

/// Borrowed inputs for rendering a session chat page.
#[derive(Clone, Copy)]
struct SessionChatRenderContext<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    is_tmux_session: bool,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    mode: &'a AppMode,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    session_id: &'a str,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    session_progress_messages: &'a HashMap<SessionId, String>,
    /// Latest tracked process-tree totals.
    session_resources: &'a HashMap<SessionId, SessionResources>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    sessions: &'a [Session],
    scroll_offset: Option<u16>,
    frame_time: FrameTime,
}

/// UI-owned immutable resources shared by every surface in one frame.
///
/// Cache handles and session-derived display snapshots stay bundled until the
/// selected surface projects the narrower page input it needs.
#[derive(Clone, Copy)]
struct FrameResources<'a> {
    active_prompt_outputs: &'a HashMap<SessionId, String>,
    default_reasoning_level: ReasoningLevel,
    diff_layout_cache: &'a page::diff::DiffLayoutCache,
    is_tmux_session: bool,
    markdown_render_cache: &'a markdown::MarkdownRenderCache,
    output_layout_cache: &'a component::session_output::SessionOutputLayoutCache,
    review_snapshot: Option<&'a SessionReviewSnapshot<'a>>,
    session_git_statuses: &'a HashMap<SessionId, SessionGitStatus>,
    session_progress_messages: &'a HashMap<SessionId, String>,
    /// Latest tracked process-tree totals.
    session_resources: &'a HashMap<SessionId, SessionResources>,
    session_update_versions: &'a HashMap<SessionId, u64>,
    session_worktree_availability: &'a HashMap<SessionId, bool>,
    frame_time: FrameTime,
}

impl<'a> FrameResources<'a> {
    /// Creates a session chat render context from shared route inputs and the
    /// mode-specific session selection.
    fn session_chat<'b>(
        self,
        sessions: &'b [Session],
        mode: &'b AppMode,
        session_id: &'b str,
        scroll_offset: Option<u16>,
    ) -> SessionChatRenderContext<'b>
    where
        'a: 'b,
    {
        SessionChatRenderContext {
            active_prompt_outputs: self.active_prompt_outputs,
            default_reasoning_level: self.default_reasoning_level,
            is_tmux_session: self.is_tmux_session,
            markdown_render_cache: self.markdown_render_cache,
            mode,
            output_layout_cache: self.output_layout_cache,
            review_snapshot: self.review_snapshot,
            session_id,
            session_git_statuses: self.session_git_statuses,
            session_resources: self.session_resources,
            session_progress_messages: self.session_progress_messages,
            session_update_versions: self.session_update_versions,
            session_worktree_availability: self.session_worktree_availability,
            sessions,
            scroll_offset,
            frame_time: self.frame_time,
        }
    }
}

/// Routes the content-area render path by active `AppMode`.
pub(crate) fn route_frame(f: &mut Frame, area: Rect, context: RenderContext<'_>) {
    let RenderContext {
        active_project_id,
        active_prompt_outputs,
        available_agent_clis,
        current_tab,
        default_reasoning_level,
        mode,
        mru_project_order,
        render_cache_store,
        project_table_state,
        projects,
        session_review_snapshot,
        session_git_statuses,
        session_resources,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        settings_screen,
        stats_activity,
        sessions,
        table_state,
        frame_time,
        is_tmux_session,
        ..
    } = context;

    let mut shared = RouteSharedContext {
        active_project_id,
        available_agent_clis,
        current_tab,
        default_reasoning_level,
        mru_project_order,
        project_table_state,
        projects,
        session_git_statuses,
        sessions,
        settings_screen,
        stats_activity,
        table_state,
    };

    let resources = FrameResources {
        active_prompt_outputs,
        default_reasoning_level,
        diff_layout_cache: render_cache_store.diff_layout_cache(),
        is_tmux_session,
        markdown_render_cache: render_cache_store.markdown_render_cache(),
        output_layout_cache: render_cache_store.session_output_layout_cache(),
        review_snapshot: session_review_snapshot,
        session_git_statuses,
        session_resources,
        session_progress_messages,
        session_update_versions,
        session_worktree_availability,
        frame_time,
    };

    render_surface(f, area, surface_for_mode(mode), &mut shared, resources);
    render_mode_overlay(f, area, mode, &shared, resources);
}

/// Resolves the stable base-page identity for terminal transition handling.
pub(crate) fn surface_kind_for_mode(mode: &AppMode) -> SurfaceKind {
    surface_for_mode(mode).kind()
}

/// Resolves the base page painted for every mode before any overlay.
fn surface_for_mode(mode: &AppMode) -> Surface<'_> {
    match mode {
        AppMode::Confirmation {
            confirmation_intent:
                ConfirmationIntent::ContinueSession
                | ConfirmationIntent::ForkSession
                | ConfirmationIntent::MergeSession
                | ConfirmationIntent::RegenerateReview
                | ConfirmationIntent::DetachManagedSession
                | ConfirmationIntent::OpenManagedWorktree
                | ConfirmationIntent::ChooseIntegrationApproach,
            restore_view: Some(restore_view),
            ..
        }
        | AppMode::ViewInfoPopup { restore_view, .. }
        | AppMode::LaunchConfigurationSelector { restore_view, .. }
        | AppMode::PublishBranchInput { restore_view, .. } => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: restore_view.scroll_offset,
            session_id: &restore_view.session_id,
        },
        AppMode::List
        | AppMode::SessionCreation { .. }
        | AppMode::StackAppendParentSelection { .. }
        | AppMode::PreCommitHookWarning { .. }
        | AppMode::ProjectSwitcher { .. }
        | AppMode::SyncBlockedPopup { .. }
        | AppMode::Confirmation { .. } => Surface::List,
        AppMode::Help { context, .. } => surface_for_help_context(context),
        AppMode::View {
            session_id,
            scroll_offset,
        } => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: *scroll_offset,
            session_id,
        },
        AppMode::Prompt {
            session_id,
            scroll_offset,
            ..
        }
        | AppMode::Question {
            session_id,
            scroll_offset,
            ..
        } => Surface::Session {
            mode: SessionSurfaceMode::Interactive(mode),
            scroll_offset: *scroll_offset,
            session_id,
        },
        AppMode::DiffLoading {
            session_id,
            sidebar_focus,
            ..
        } => Surface::DiffLoading {
            session_id,
            sidebar_focus: *sidebar_focus,
        },
        AppMode::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments,
            restore,
            scroll_offset,
            selected_diff_line_index,
            session_id,
            ..
        } => Surface::Diff {
            diff,
            file_explorer_selected_index: *file_explorer_selected_index,
            focus: *focus,
            line_comments,
            preview,
            review_comments: review_comments.as_ref(),
            restore: restore.as_deref(),
            scroll_offset: *scroll_offset,
            selected_diff_line_index: *selected_diff_line_index,
            session_id,
            sidebar_focus: review_comments
                .as_ref()
                .map_or(DiffSidebarFocus::Files, |review_comments| {
                    review_comments.sidebar_focus
                }),
        },
    }
}

/// Resolves the page restored behind a context-aware help overlay.
fn surface_for_help_context(context: &HelpContext) -> Surface<'_> {
    match context {
        HelpContext::List { .. } => Surface::List,
        HelpContext::View {
            session_id,
            scroll_offset,
            ..
        } => Surface::Session {
            mode: SessionSurfaceMode::View,
            scroll_offset: *scroll_offset,
            session_id,
        },
        HelpContext::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments,
            restore,
            scroll_offset,
            selected_diff_line_index,
            session_id,
            ..
        } => Surface::Diff {
            diff,
            file_explorer_selected_index: *file_explorer_selected_index,
            focus: *focus,
            line_comments,
            preview,
            review_comments: review_comments.as_deref(),
            restore: restore.as_deref(),
            scroll_offset: *scroll_offset,
            selected_diff_line_index: *selected_diff_line_index,
            session_id,
            sidebar_focus: review_comments
                .as_deref()
                .map_or(DiffSidebarFocus::Files, |review_comments| {
                    review_comments.sidebar_focus
                }),
        },
    }
}

/// Paints one base surface through the only page-construction boundary.
fn render_surface(
    f: &mut Frame,
    area: Rect,
    surface: Surface<'_>,
    shared: &mut RouteSharedContext<'_>,
    resources: FrameResources<'_>,
) {
    match surface {
        Surface::List => render_list_background(f, area, shared, resources.frame_time),
        Surface::Session {
            mode,
            scroll_offset,
            session_id,
        } => render_session_surface(
            f,
            area,
            mode,
            scroll_offset,
            session_id,
            shared.sessions,
            resources,
        ),
        Surface::DiffLoading {
            session_id,
            sidebar_focus,
        } => {
            let preview = DiffPreview::default();
            render_diff_surface(
                f,
                area,
                DiffSurfaceInput {
                    diff: "Loading diff...",
                    file_explorer_selected_index: 0,
                    focus: DiffFocus::Files,
                    is_loading: true,
                    line_comments: &DiffLineComments::default(),
                    preview: &preview,
                    review_comments: None,
                    restore: None,
                    scroll_offset: 0,
                    selected_diff_line_index: 0,
                    session_id,
                    sidebar_focus,
                },
                shared.sessions,
                resources,
            );
        }
        Surface::Diff {
            diff,
            file_explorer_selected_index,
            focus,
            line_comments,
            preview,
            review_comments,
            restore,
            scroll_offset,
            selected_diff_line_index,
            session_id,
            sidebar_focus,
        } => render_diff_surface(
            f,
            area,
            DiffSurfaceInput {
                diff,
                file_explorer_selected_index,
                focus,
                is_loading: false,
                line_comments,
                preview,
                review_comments,
                restore,
                scroll_offset,
                selected_diff_line_index,
                session_id,
                sidebar_focus,
            },
            shared.sessions,
            resources,
        ),
    }
}

/// Paints the overlay portion of modes after their base surface.
fn render_mode_overlay(
    f: &mut Frame,
    area: Rect,
    mode: &AppMode,
    shared: &RouteSharedContext<'_>,
    resources: FrameResources<'_>,
) {
    match mode {
        AppMode::List
        | AppMode::View { .. }
        | AppMode::Prompt { .. }
        | AppMode::Question { .. }
        | AppMode::DiffLoading { .. }
        | AppMode::Diff { .. } => {}
        AppMode::SessionCreation {
            selected_option_index,
        } => component::session_creation_overlay::SessionCreationOverlay::new(
            *selected_option_index,
            shared.can_create_stacked_session(),
            shared.can_append_selected_session(),
        )
        .render(f, area),
        AppMode::StackAppendParentSelection {
            selected_parent_index,
            session_id,
        } => {
            render_stack_append_parent_overlay(f, area, shared, session_id, *selected_parent_index);
        }
        AppMode::PreCommitHookWarning { message } => {
            component::info_overlay::InfoOverlay::new("Pre-commit hook warning", message)
                .render(f, area);
        }
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => {
            let mru_project_items =
                ordered_project_items(shared.projects, shared.mru_project_order);
            component::project_switcher_overlay::ProjectSwitcherOverlay::new(
                &mru_project_items,
                shared.active_project_id,
                *selected_option_index,
            )
            .render(f, area);
        }
        AppMode::Confirmation { .. } => render_confirmation_overlay(f, area, mode),
        AppMode::SyncBlockedPopup {
            default_branch,
            is_loading,
            message,
            project_name,
            title,
        } => {
            let popup_message = overlay::sync_popup_message(
                default_branch.as_deref(),
                message,
                project_name.as_deref(),
            );
            component::info_overlay::InfoOverlay::new(title, &popup_message)
                .is_loading(*is_loading)
                .loading_label("Sync in progress...")
                .spinner_frame(crate::ui::icon::Icon::spinner_frame_from_millis(
                    resources.frame_time.unix_millis(),
                ))
                .render(f, area);
        }
        AppMode::ViewInfoPopup {
            is_loading,
            loading_label,
            message,
            title,
            ..
        } => {
            component::info_overlay::InfoOverlay::new(title, message)
                .is_loading(*is_loading)
                .loading_label(loading_label)
                .spinner_frame(crate::ui::icon::Icon::spinner_frame_from_millis(
                    resources.frame_time.unix_millis(),
                ))
                .render(f, area);
        }
        AppMode::Help {
            context: help_context,
            scroll_offset,
        } => component::help_overlay::HelpOverlay::new(help_context)
            .scroll_offset(*scroll_offset)
            .render(f, area),
        AppMode::LaunchConfigurationSelector {
            commands,
            selected_command_index,
            ..
        } => component::launch_configuration_overlay::LaunchConfigurationOverlay::new(commands)
            .selected_command_index(*selected_command_index)
            .render(f, area),
        AppMode::PublishBranchInput {
            default_branch_name,
            input,
            locked_upstream_ref,
            ..
        } => component::publish_branch_overlay::PublishBranchOverlay::new(
            input,
            default_branch_name,
            locked_upstream_ref.as_deref(),
        )
        .render(f, area),
    }
}

/// Renders the eligible parent choices for the session being appended.
fn render_stack_append_parent_overlay(
    f: &mut Frame,
    area: Rect,
    shared: &RouteSharedContext<'_>,
    session_id: &SessionId,
    selected_parent_index: usize,
) {
    let parent_sessions = shared.stack_append_parent_sessions(session_id);
    component::stack_append_parent_overlay::StackAppendParentOverlay::new(
        &parent_sessions,
        selected_parent_index,
    )
    .render(f, area);
}

/// Renders the confirmation overlay after its classified base surface.
fn render_confirmation_overlay(f: &mut Frame, area: Rect, mode: &AppMode) {
    let AppMode::Confirmation {
        confirmation_intent,
        confirmation_message,
        confirmation_title,
        selected_confirmation_index,
        ..
    } = mode
    else {
        return;
    };

    let overlay = component::confirmation_overlay::ConfirmationOverlay::new(
        confirmation_title,
        confirmation_message,
    )
    .selected_first(*selected_confirmation_index == 0);
    if *confirmation_intent == ConfirmationIntent::ChooseIntegrationApproach {
        overlay
            .option_labels("Local merges", "Review requests")
            .render(f, area);
    } else {
        overlay.render(f, area);
    }
}

/// Renders a session surface in either interactive or restored-view mode.
fn render_session_surface(
    f: &mut Frame,
    area: Rect,
    mode: SessionSurfaceMode<'_>,
    scroll_offset: Option<u16>,
    session_id: &str,
    sessions: &[Session],
    resources: FrameResources<'_>,
) {
    match mode {
        SessionSurfaceMode::Interactive(mode) => render_session_chat(
            f,
            area,
            resources.session_chat(sessions, mode, session_id, scroll_offset),
        ),
        SessionSurfaceMode::View => {
            let view_mode = AppMode::View {
                scroll_offset,
                session_id: session_id.into(),
            };
            render_session_chat(
                f,
                area,
                resources.session_chat(sessions, &view_mode, session_id, scroll_offset),
            );
        }
    }
}

/// Borrowed inputs for one diff surface.
#[derive(Clone, Copy)]
struct DiffSurfaceInput<'a> {
    diff: &'a str,
    file_explorer_selected_index: usize,
    focus: DiffFocus,
    is_loading: bool,
    line_comments: &'a DiffLineComments,
    preview: &'a DiffPreview,
    review_comments: Option<&'a DiffReviewComments>,
    restore: Option<&'a DiffRestoreTarget>,
    scroll_offset: u16,
    selected_diff_line_index: usize,
    session_id: &'a str,
    sidebar_focus: DiffSidebarFocus,
}

/// Renders a diff page for a resolved session.
fn render_diff_surface(
    f: &mut Frame,
    area: Rect,
    input: DiffSurfaceInput<'_>,
    sessions: &[Session],
    resources: FrameResources<'_>,
) {
    let Some(session) = sessions
        .iter()
        .find(|session| session.id == input.session_id)
    else {
        return;
    };

    let mut page = page::diff::DiffPage::new(page::diff::DiffPageInput {
        can_comment: allows_diff_line_comment_reply(session, sessions, input.restore),
        diff: input.diff,
        diff_layout_cache: resources.diff_layout_cache,
        file_explorer_selected_index: input.file_explorer_selected_index,
        focus: input.focus,
        line_comments: input.line_comments,
        markdown_render_cache: resources.markdown_render_cache,
        preview: input.preview,
        review_comments: input.review_comments,
        scroll_offset: input.scroll_offset,
        selected_diff_line_index: input.selected_diff_line_index,
        session,
        sidebar_focus: input.sidebar_focus,
    });
    if input.is_loading {
        page.loading().render(f, area);
    } else {
        page.render(f, area);
    }
}

/// Renders the session chat page for all session-chat modes.
fn render_session_chat(f: &mut Frame, area: Rect, context: SessionChatRenderContext<'_>) {
    let SessionChatRenderContext {
        active_prompt_outputs,
        default_reasoning_level,
        is_tmux_session,
        markdown_render_cache,
        mode,
        output_layout_cache,
        review_snapshot,
        session_id,
        session_resources,
        session_progress_messages,
        session_git_statuses,
        session_update_versions,
        session_worktree_availability,
        sessions,
        scroll_offset,
        frame_time,
    } = context;

    let Some(session_index) = sessions.iter().position(|session| session.id == session_id) else {
        return;
    };

    let active_progress = session_progress_messages
        .get(session_id)
        .map(std::string::String::as_str);
    let active_prompt_output = active_prompt_outputs
        .get(session_id)
        .map(std::string::String::as_str);
    let session_update_version = session_update_versions
        .get(session_id)
        .copied()
        .unwrap_or_default();
    let has_merge_conflict = session_git_statuses
        .get(session_id)
        .and_then(|status| status.has_merge_conflict)
        .unwrap_or(false);

    let page_input = page::session_chat::SessionChatPageInput {
        active_prompt_output,
        active_progress,
        resources: session_resources.get(session_id).copied(),
        default_reasoning_level,
        has_merge_conflict,
        markdown_render_cache,
        mode,
        output_layout_cache,
        review_text: review_snapshot
            .filter(|snapshot| snapshot.session_id == session_id)
            .and_then(|snapshot| snapshot.text),
        scroll_offset,
        session_index,
        session_update_version,
        sessions,
        frame_time,
    };
    let can_open_worktree = is_tmux_session
        && *session_worktree_availability
            .get(session_id)
            .unwrap_or(&false);
    if sessions[session_index].role == crate::domain::session::SessionRole::Orchestrator {
        page::orchestration::OrchestrationPage::new(page_input)
            .can_open_worktree(can_open_worktree)
            .render(f, area);
    } else {
        page::session_chat::SessionChatPage::new(page_input)
            .can_open_worktree(can_open_worktree)
            .render(f, area);
    }
}

/// Renders base list tabs and the currently selected list tab content.
fn render_list_background(
    f: &mut Frame,
    content_area: Rect,
    shared: &mut RouteSharedContext<'_>,
    frame_time: FrameTime,
) {
    let chunks = Layout::default()
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(content_area);

    component::tab::Tabs::new(
        shared.current_tab,
        shared.active_project_id,
        shared.projects,
    )
    .render(f, chunks[0]);

    match shared.current_tab {
        Tab::Projects => {
            page::project_list::ProjectListPage::new(
                shared.projects,
                shared.available_agent_clis,
                shared.stats_activity,
                &mut *shared.project_table_state,
                shared.active_project_id,
                activity_day_key_with_offset(
                    frame_time.unix_seconds(),
                    frame_time.local_utc_offset_seconds(),
                ),
            )
            .render(f, chunks[1]);
        }
        Tab::Sessions => {
            page::session_list::SessionListPage::new(
                shared.sessions,
                &mut *shared.table_state,
                shared.default_reasoning_level,
                frame_time.unix_seconds(),
            )
            .session_git_statuses(shared.session_git_statuses)
            .render(f, chunks[1]);
        }
        Tab::Settings => {
            let active_project_name =
                active_project_name(shared.active_project_id, shared.projects);
            if let Some(settings_screen) = shared.settings_screen {
                page::setting::SettingsPage::new(settings_screen, active_project_name)
                    .render(f, chunks[1]);
            }
        }
    }
}

/// Returns the active project's display label for scoped page titles.
fn active_project_name(active_project_id: i64, projects: &[ProjectListItem]) -> Option<String> {
    projects
        .iter()
        .find(|project_item| project_item.project.id == active_project_id)
        .map(|project_item| project_item.project.display_label())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::widgets::Paragraph;

    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::input::InputState;
    use crate::domain::question::QuestionItem;
    use crate::domain::session::Status;
    use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
    use crate::presentation::app_mode::{ChatFocus, ConfirmationViewMode};
    use crate::presentation::prompt::{
        PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };
    use crate::presentation::settings::{
        LaunchConfigurationListEditorMode, LaunchConfigurationListEditorSnapshot,
        SettingsSelectorDropdown, SettingsSelectorDropdownOption,
    };
    use crate::test_support::SessionFixtureBuilder;

    /// Builds one deterministic session fixture for router render tests.
    fn session_fixture(session_id: &str) -> Session {
        let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "Captured output",
        )]);
        let mut session = SessionFixtureBuilder::new()
            .id(session_id)
            .folder(PathBuf::from(format!("/tmp/{session_id}")))
            .prompt("Prompt")
            .status(Status::Review)
            .title(Some("Router Session".to_string()))
            .build();
        session.transcript = Some(transcript);

        session
    }

    #[test]
    fn route_shared_context_finds_appendable_parent_for_selected_review_session() {
        // Arrange
        let sessions = vec![session_fixture("source"), session_fixture("parent")];
        let mut project_table_state = TableState::default();
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let shared = RouteSharedContext {
            active_project_id: 1,
            available_agent_clis: &[],
            current_tab: Tab::Sessions,
            default_reasoning_level: ReasoningLevel::High,
            mru_project_order: &[],
            project_table_state: &mut project_table_state,
            projects: &[],
            session_git_statuses: &HashMap::new(),
            sessions: &sessions,
            settings_screen: None,
            stats_activity: &[],
            table_state: &mut table_state,
        };

        // Act
        let can_append = shared.can_append_selected_session();
        let parent_sessions = shared.stack_append_parent_sessions("source");

        // Assert
        assert!(can_append);
        assert_eq!(parent_sessions.len(), 1);
        assert_eq!(parent_sessions[0].id.as_str(), "parent");
    }

    /// Flattens a rendered test buffer into a plain string for text assertions.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn settings_screen_with_overlays() -> SettingsScreenSnapshot {
        SettingsScreenSnapshot {
            footer_hint: "Editing settings",
            global_rows: vec![("Theme", "Agentty Default".to_string())],
            launch_configuration_list_editor: Some(LaunchConfigurationListEditorSnapshot {
                commands: vec!["cargo test".to_string()],
                input: None,
                mode: LaunchConfigurationListEditorMode::Browse,
                selected_index: 0,
            }),
            project_rows: vec![("Launch Configurations", "cargo test".to_string())],
            selected_row_index: Some(0),
            selector_dropdown: Some(SettingsSelectorDropdown {
                options: vec![SettingsSelectorDropdownOption {
                    label: "Agentty Default".to_string(),
                }],
                row_index: 0,
                selected_index: 0,
                title: "Select setting value",
            }),
        }
    }

    fn render_list_tab(
        current_tab: Tab,
        settings_screen: Option<&SettingsScreenSnapshot>,
    ) -> (String, ReasoningLevel) {
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let available_agent_clis = Vec::new();
        let mut project_table_state = TableState::default();
        let projects = Vec::new();
        let sessions = Vec::new();
        let stats_activity = Vec::new();
        let mut table_state = TableState::default();
        let mut shared = RouteSharedContext {
            active_project_id: 1,
            available_agent_clis: &available_agent_clis,
            current_tab,
            default_reasoning_level: ReasoningLevel::Max,
            mru_project_order: &[],
            project_table_state: &mut project_table_state,
            projects: &projects,
            session_git_statuses: &HashMap::new(),
            sessions: &sessions,
            settings_screen,
            stats_activity: &stats_activity,
            table_state: &mut table_state,
        };
        let default_reasoning_level = shared.default_reasoning_level;

        terminal
            .draw(|frame| {
                render_list_background(frame, frame.area(), &mut shared, FrameTime::new(0, 0, 0));
            })
            .expect("failed to draw list tab");

        (
            buffer_text(terminal.backend().buffer()),
            default_reasoning_level,
        )
    }

    /// Renders one list-backed mode through the production router.
    fn render_list_backed_mode(mode: &AppMode) -> (bool, String) {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mut project_table_state = TableState::default();
        let sessions = vec![session_fixture("session-overlay")];
        let mut table_state = TableState::default();
        let mut shared = RouteSharedContext {
            active_project_id: 1,
            available_agent_clis: &[],
            current_tab: Tab::Sessions,
            default_reasoning_level: ReasoningLevel::High,
            mru_project_order: &[],
            project_table_state: &mut project_table_state,
            projects: &[],
            session_git_statuses: &HashMap::new(),
            sessions: &sessions,
            settings_screen: None,
            stats_activity: &[],
            table_state: &mut table_state,
        };
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let active_prompt_outputs = HashMap::new();
        let session_progress_messages = HashMap::new();
        let session_update_versions = HashMap::new();
        let session_worktree_availability = HashMap::new();
        let mut handled = false;

        terminal
            .draw(|frame| {
                render_surface(
                    frame,
                    frame.area(),
                    surface_for_mode(mode),
                    &mut shared,
                    FrameResources {
                        active_prompt_outputs: &active_prompt_outputs,
                        default_reasoning_level: ReasoningLevel::High,
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &markdown_render_cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &session_progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &session_worktree_availability,
                        frame_time: FrameTime::new(90, 90_000, -28_800),
                    },
                );
                render_mode_overlay(
                    frame,
                    frame.area(),
                    mode,
                    &shared,
                    FrameResources {
                        active_prompt_outputs: &active_prompt_outputs,
                        default_reasoning_level: ReasoningLevel::High,
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &markdown_render_cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &session_progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &session_worktree_availability,
                        frame_time: FrameTime::new(90, 90_000, -28_800),
                    },
                );
                handled = true;
            })
            .expect("failed to draw list-backed mode");

        (handled, buffer_text(terminal.backend().buffer()))
    }

    #[test]
    fn surface_for_help_context_classifies_each_restored_page() {
        // Arrange
        let list_context = HelpContext::List {
            keybindings: vec![],
        };
        let view_context = HelpContext::View {
            can_fork_session: true,
            can_merge_session_branch: true,
            can_mutate_session_branch: true,
            can_open_worktree: true,
            can_rebase_session_branch: true,
            can_show_diff: true,
            can_reply_to_session: true,
            can_start_staged_session: false,
            can_view_review_comments: false,
            publish_pull_request_action: None,
            scroll_offset: Some(3),
            session_id: "session-help".into(),
            session_state: crate::presentation::help_action::ViewSessionState::Review,
        };
        let diff_context = HelpContext::Diff {
            can_comment: true,
            diff: "diff --git a/file b/file".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(Box::new(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..DiffReviewComments::loading(1)
            })),
            restore: None,
            scroll_offset: 2,
            session_id: "session-help".into(),
        };

        // Act
        let list_surface = surface_for_help_context(&list_context);
        let view_surface = surface_for_help_context(&view_context);
        let diff_surface = surface_for_help_context(&diff_context);

        // Assert
        assert!(matches!(list_surface, Surface::List));
        assert!(matches!(view_surface, Surface::Session { .. }));
        assert!(matches!(
            diff_surface,
            Surface::Diff {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            }
        ));
    }

    #[test]
    fn surface_kind_classifies_each_base_page() {
        // Arrange
        let diff = String::new();
        let line_comments = DiffLineComments::default();
        let preview = DiffPreview::default();
        let surfaces = [
            Surface::Diff {
                diff: &diff,
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                line_comments: &line_comments,
                selected_diff_line_index: 0,
                preview: &preview,
                review_comments: None,
                restore: None,
                scroll_offset: 0,
                session_id: "session-surface-kind",
                sidebar_focus: DiffSidebarFocus::Files,
            },
            Surface::DiffLoading {
                session_id: "session-surface-kind",
                sidebar_focus: DiffSidebarFocus::Comments,
            },
            Surface::List,
            Surface::Session {
                mode: SessionSurfaceMode::View,
                scroll_offset: None,
                session_id: "session-surface-kind",
            },
        ];

        // Act
        let surface_kinds = surfaces.map(Surface::kind);

        // Assert
        assert_eq!(
            surface_kinds,
            [
                SurfaceKind::Diff,
                SurfaceKind::Diff,
                SurfaceKind::List,
                SurfaceKind::Session,
            ]
        );
    }

    #[test]
    fn surface_for_mode_uses_confirmation_restore_only_for_session_intents() {
        // Arrange
        let session_confirmation = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::MergeSession,
            confirmation_message: "Merge?".to_string(),
            confirmation_title: "Confirm".to_string(),
            restore_view: Some(ConfirmationViewMode {
                scroll_offset: Some(4),
                session_id: "session-confirm".into(),
            }),
            selected_confirmation_index: 0,
            session_id: Some("session-confirm".into()),
        };
        let list_confirmation = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::Quit,
            confirmation_message: "Quit?".to_string(),
            confirmation_title: "Confirm".to_string(),
            restore_view: None,
            selected_confirmation_index: 1,
            session_id: None,
        };

        // Act
        let session_surface = surface_for_mode(&session_confirmation);
        let list_surface = surface_for_mode(&list_confirmation);

        // Assert
        assert!(matches!(session_surface, Surface::Session { .. }));
        assert!(matches!(list_surface, Surface::List));
    }

    #[test]
    fn surface_for_mode_classifies_primary_page_modes() {
        // Arrange
        let help_mode = AppMode::Help {
            context: HelpContext::List {
                keybindings: vec![],
            },
            scroll_offset: 0,
        };
        let view_mode = AppMode::View {
            scroll_offset: Some(1),
            session_id: "session-overlay".into(),
        };
        let prompt_mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            focus: ChatFocus::Input,
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::default(),
            scroll_offset: Some(2),
            session_id: "session-overlay".into(),
            slash_state: PromptSlashState::default(),
        };
        let question_mode = AppMode::Question {
            at_mention_state: None,
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            questions: vec![QuestionItem {
                options: vec![],
                text: "Which path?".to_string(),
            }],
            responses: vec![],
            scroll_offset: Some(3),
            selected_option_index: None,
            session_id: "session-overlay".into(),
        };
        let diff_mode = AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(DiffReviewComments {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..DiffReviewComments::loading(1)
            }),
            restore: None,
            scroll_cache: None,
            scroll_offset: 4,
            session_id: "session-overlay".into(),
        };
        let diff_loading_mode = AppMode::DiffLoading {
            fallback_view_scroll_offset: Some(4),
            request_id: 1,
            restore: None,
            session_id: "session-overlay".into(),
            sidebar_focus: DiffSidebarFocus::Comments,
        };

        // Act
        let help_is_list = matches!(surface_for_mode(&help_mode), Surface::List);
        let view_is_session = matches!(surface_for_mode(&view_mode), Surface::Session { .. });
        let prompt_is_session = matches!(surface_for_mode(&prompt_mode), Surface::Session { .. });
        let question_is_session =
            matches!(surface_for_mode(&question_mode), Surface::Session { .. });
        let diff_is_diff = matches!(surface_for_mode(&diff_mode), Surface::Diff { .. });
        let (_, comments_text) = render_list_backed_mode(&diff_mode);
        let diff_loading_is_diff = matches!(
            surface_for_mode(&diff_loading_mode),
            Surface::DiffLoading {
                sidebar_focus: DiffSidebarFocus::Comments,
                ..
            }
        );
        let (_, loading_text) = render_list_backed_mode(&diff_loading_mode);

        // Assert
        assert!(help_is_list);
        assert!(view_is_session);
        assert!(prompt_is_session);
        assert!(question_is_session);
        assert!(diff_is_diff);
        assert!(diff_loading_is_diff);
        assert!(comments_text.contains("Comment — Router Session"));
        assert!(loading_text.contains("Loading diff..."));
        assert!(!loading_text.contains("No files"));
    }

    #[test]
    fn render_list_background_renders_settings_snapshot_and_overlays() {
        // Arrange
        let settings_screen = settings_screen_with_overlays();

        // Act
        let (text, _) = render_list_tab(Tab::Settings, Some(&settings_screen));

        // Assert
        assert!(text.contains("Launch Configurations"));
        assert!(text.contains("cargo test"));
    }

    #[test]
    fn render_list_background_projects_reasoning_into_sessions_page() {
        // Arrange

        // Act
        let (_, default_reasoning_level) = render_list_tab(Tab::Sessions, None);

        // Assert
        assert_eq!(default_reasoning_level, ReasoningLevel::Max);
    }

    #[test]
    fn render_help_mode_restores_markdown_diff_preview_background() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-help-diff";
        let sessions = vec![session_fixture(session_id)];
        let mode = AppMode::Help {
            context: crate::presentation::app_mode::HelpContext::Diff {
                can_comment: true,
                diff: "diff --git a/README.md b/README.md\n+# Preview".to_string(),
                file_explorer_selected_index: 0,
                focus: DiffFocus::Files,
                line_comments: DiffLineComments::default(),
                selected_diff_line_index: 0,
                preview: crate::presentation::app_mode::DiffPreview::Ready {
                    content: "# Preview".to_string(),
                    path: "README.md".to_string(),
                    request_id: 1,
                },
                review_comments: None,
                restore: None,
                scroll_offset: 0,
                session_id: session_id.into(),
            },
            scroll_offset: 0,
        };
        let mut project_table_state = TableState::default();
        let mut table_state = TableState::default();
        let mut shared = RouteSharedContext {
            active_project_id: 1,
            available_agent_clis: &[],
            current_tab: Tab::Sessions,
            default_reasoning_level: ReasoningLevel::default(),
            mru_project_order: &[],
            project_table_state: &mut project_table_state,
            projects: &[],
            session_git_statuses: &HashMap::new(),
            sessions: &sessions,
            settings_screen: None,
            stats_activity: &[],
            table_state: &mut table_state,
        };
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();

        // Act
        let mut handled = false;
        terminal
            .draw(|frame| {
                render_surface(
                    frame,
                    frame.area(),
                    surface_for_mode(&mode),
                    &mut shared,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &markdown_render_cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &HashMap::new(),
                        session_update_versions: &HashMap::new(),
                        session_worktree_availability: &HashMap::new(),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
                render_mode_overlay(
                    frame,
                    frame.area(),
                    &mode,
                    &shared,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &markdown_render_cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &HashMap::new(),
                        session_update_versions: &HashMap::new(),
                        session_worktree_availability: &HashMap::new(),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
                handled = true;
            })
            .expect("failed to draw diff help mode");

        // Assert
        assert!(handled);
        assert!(buffer_text(terminal.backend().buffer()).contains("Keybindings"));
    }

    #[test]
    fn render_list_backed_modes_forward_frame_time_to_every_overlay() {
        // Arrange
        let restore_view = ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-overlay".into(),
        };
        let modes = [
            (
                AppMode::SessionCreation {
                    selected_option_index: 0,
                },
                "New Session",
            ),
            (
                AppMode::StackAppendParentSelection {
                    selected_parent_index: 0,
                    session_id: "session-overlay".into(),
                },
                "Append to stack",
            ),
            (
                AppMode::PreCommitHookWarning {
                    message: "Install the hook".to_string(),
                },
                "Pre-commit hook warning",
            ),
            (
                AppMode::ProjectSwitcher {
                    selected_option_index: 0,
                },
                "Switch project",
            ),
            (
                AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::Quit,
                    confirmation_message: "Quit now?".to_string(),
                    confirmation_title: "Confirm Quit".to_string(),
                    restore_view: None,
                    session_id: None,
                    selected_confirmation_index: 0,
                },
                "Confirm Quit",
            ),
            (
                AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::MergeSession,
                    confirmation_message: "Merge now?".to_string(),
                    confirmation_title: "Confirm Merge".to_string(),
                    restore_view: Some(restore_view.clone()),
                    session_id: Some("session-overlay".into()),
                    selected_confirmation_index: 0,
                },
                "Confirm Merge",
            ),
            (
                AppMode::SyncBlockedPopup {
                    default_branch: Some("main".to_string()),
                    is_loading: true,
                    message: "Waiting for sync".to_string(),
                    project_name: Some("agentty".to_string()),
                    title: "Syncing".to_string(),
                },
                "Syncing",
            ),
            (
                AppMode::ViewInfoPopup {
                    is_loading: true,
                    loading_label: "Publishing branch".to_string(),
                    message: "Waiting for forge".to_string(),
                    restore_view,
                    title: "Publishing".to_string(),
                },
                "Publishing",
            ),
            (
                AppMode::LaunchConfigurationSelector {
                    commands: vec!["cargo test".to_string()],
                    restore_view: ConfirmationViewMode {
                        scroll_offset: None,
                        session_id: "session-overlay".into(),
                    },
                    selected_command_index: 0,
                },
                "Launch Configuration",
            ),
        ];

        // Act
        let rendered_modes = modes
            .iter()
            .map(|(mode, expected_text)| {
                let (handled, text) = render_list_backed_mode(mode);

                (handled, text, *expected_text)
            })
            .collect::<Vec<_>>();

        // Assert
        for (handled, text, expected_text) in rendered_modes {
            assert!(handled);
            assert!(
                text.contains(expected_text),
                "rendered output should contain `{expected_text}`"
            );
        }
    }

    #[test]
    fn render_help_modes_forward_frame_time_to_list_and_view_backgrounds() {
        // Arrange
        let modes = [
            (
                AppMode::Help {
                    context: crate::presentation::app_mode::HelpContext::List {
                        keybindings: vec![],
                    },
                    scroll_offset: 0,
                },
                "Keybindings",
            ),
            (
                AppMode::Help {
                    context: crate::presentation::app_mode::HelpContext::View {
                        can_fork_session: true,
                        can_merge_session_branch: true,
                        can_mutate_session_branch: true,
                        can_open_worktree: true,
                        can_rebase_session_branch: true,
                        can_show_diff: true,
                        can_reply_to_session: true,
                        can_start_staged_session: false,
                        can_view_review_comments: false,
                        publish_pull_request_action: None,
                        session_id: "session-overlay".into(),
                        session_state: crate::presentation::help_action::ViewSessionState::Review,
                        scroll_offset: None,
                    },
                    scroll_offset: 0,
                },
                "Keybindings",
            ),
        ];

        // Act
        let rendered_modes = modes
            .iter()
            .map(|(mode, expected_text)| {
                let (handled, text) = render_list_backed_mode(mode);

                (handled, text, *expected_text)
            })
            .collect::<Vec<_>>();

        // Assert
        for (handled, text, expected_text) in rendered_modes {
            assert!(handled);
            assert!(
                text.contains(expected_text),
                "rendered output should contain `{expected_text}`"
            );
        }
    }

    #[test]
    fn render_session_surface_renders_view_session_content() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-1234";
        let sessions = vec![session_fixture(session_id)];
        let mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: None,
        };
        let progress_messages = HashMap::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_git_statuses = HashMap::from([(
            session_id.into(),
            SessionGitStatus {
                base_status: Some((1, 1)),
                has_merge_conflict: Some(true),
                remote_status: None,
            },
        )]);
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_session_surface(
                    frame,
                    frame.area(),
                    SessionSurfaceMode::Interactive(&mode),
                    None,
                    session_id,
                    &sessions,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: false,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &session_git_statuses,
                        session_resources: &HashMap::new(),
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::from([(
                            session_id.to_string().into(),
                            true,
                        )]),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Router Session"));
        assert!(text.contains("Captured output"));
        assert!(text.contains("Merge conflict with main"));
        assert!(!text.contains("o: open"));
    }

    #[test]
    fn render_session_surface_uses_campaign_page_for_orchestrators() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "campaign-1234";
        let mut session = session_fixture(session_id);
        session.role = crate::domain::session::SessionRole::Orchestrator;
        session.orchestration_progress = Some("Phase: AwaitingApproval".to_string());
        let sessions = [session];
        let mode = AppMode::View {
            session_id: session_id.into(),
            scroll_offset: None,
        };
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();

        // Act
        terminal
            .draw(|frame| {
                render_session_surface(
                    frame,
                    frame.area(),
                    SessionSurfaceMode::Interactive(&mode),
                    None,
                    session_id,
                    &sessions,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &HashMap::new(),
                        session_update_versions: &HashMap::new(),
                        session_worktree_availability: &HashMap::from([(
                            session_id.to_string().into(),
                            true,
                        )]),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Campaign: Router Session"));
        assert!(text.contains("Phase: AwaitingApproval"));
    }

    #[test]
    fn render_session_surface_keeps_background_when_session_is_missing() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mode = AppMode::View {
            session_id: "missing-session".into(),
            scroll_offset: None,
        };
        let progress_messages = HashMap::new();
        let sessions = Vec::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(Paragraph::new("sentinel"), area);
                render_session_surface(
                    frame,
                    area,
                    SessionSurfaceMode::Interactive(&mode),
                    None,
                    "missing-session",
                    &sessions,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("sentinel"));
    }

    #[test]
    fn render_diff_surface_renders_page_for_matching_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-diff";
        let mut session = session_fixture(session_id);
        session.title = Some("Diff Session".to_string());
        let sessions = vec![session];
        let progress_messages = HashMap::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_diff_surface(
                    frame,
                    frame.area(),
                    DiffSurfaceInput {
                        diff: "",
                        file_explorer_selected_index: 0,
                        focus: DiffFocus::Files,
                        is_loading: false,
                        line_comments: &DiffLineComments::default(),
                        selected_diff_line_index: 0,
                        preview: &DiffPreview::default(),
                        review_comments: None,
                        restore: None,
                        scroll_offset: 0,
                        session_id,
                        sidebar_focus: DiffSidebarFocus::Files,
                    },
                    &sessions,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        frame_time: FrameTime::new(0, 0, 0),
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
    fn render_diff_surface_preserves_frame_for_missing_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();

        // Act
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("sentinel"), frame.area());
                render_diff_surface(
                    frame,
                    frame.area(),
                    DiffSurfaceInput {
                        diff: "",
                        file_explorer_selected_index: 0,
                        focus: DiffFocus::Files,
                        is_loading: false,
                        line_comments: &DiffLineComments::default(),
                        selected_diff_line_index: 0,
                        preview: &DiffPreview::default(),
                        review_comments: None,
                        restore: None,
                        scroll_offset: 0,
                        session_id: "missing-session",
                        sidebar_focus: DiffSidebarFocus::Files,
                    },
                    &[],
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &markdown_render_cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &HashMap::new(),
                        session_update_versions: &HashMap::new(),
                        session_worktree_availability: &HashMap::new(),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        assert!(buffer_text(terminal.backend().buffer()).contains("sentinel"));
    }

    #[test]
    fn render_diff_surface_renders_linked_comments_for_matching_session() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let session_id = "session-comments";
        let sessions = vec![session_fixture(session_id)];
        let review_comments = DiffReviewComments {
            sidebar_focus: DiffSidebarFocus::Comments,
            ..DiffReviewComments::loading(1)
        };
        let progress_messages = HashMap::new();
        let cache = markdown::MarkdownRenderCache::default();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let output_layout_cache = component::session_output::SessionOutputLayoutCache::default();
        let session_update_versions = HashMap::new();

        // Act
        terminal
            .draw(|frame| {
                render_diff_surface(
                    frame,
                    frame.area(),
                    DiffSurfaceInput {
                        diff: "",
                        file_explorer_selected_index: 0,
                        focus: DiffFocus::Files,
                        is_loading: false,
                        line_comments: &DiffLineComments::default(),
                        selected_diff_line_index: 0,
                        preview: &DiffPreview::default(),
                        review_comments: Some(&review_comments),
                        restore: None,
                        scroll_offset: 0,
                        session_id,
                        sidebar_focus: DiffSidebarFocus::Comments,
                    },
                    &sessions,
                    FrameResources {
                        active_prompt_outputs: &HashMap::new(),
                        default_reasoning_level: ReasoningLevel::default(),
                        diff_layout_cache: &diff_layout_cache,
                        is_tmux_session: true,
                        markdown_render_cache: &cache,
                        output_layout_cache: &output_layout_cache,
                        review_snapshot: None,
                        session_git_statuses: &HashMap::new(),
                        session_resources: &HashMap::new(),
                        session_progress_messages: &progress_messages,
                        session_update_versions: &session_update_versions,
                        session_worktree_availability: &HashMap::new(),
                        frame_time: FrameTime::new(0, 0, 0),
                    },
                );
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Comment — Router Session"));
        assert!(text.contains("Loading review comments..."));
    }

    #[test]
    fn render_confirmation_overlay_renders_integration_choices() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
            confirmation_message: "How should the campaign integrate?".to_string(),
            confirmation_title: "Integration Approach".to_string(),
            restore_view: None,
            selected_confirmation_index: 0,
            session_id: Some("session-controller".into()),
        };

        // Act
        terminal
            .draw(|frame| {
                render_confirmation_overlay(frame, frame.area(), &mode);
            })
            .expect("failed to draw");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Integration Approach"));
        assert!(text.contains("How should the campaign integrate?"));
        assert!(text.contains("Local merges"));
        assert!(text.contains("Review requests"));
    }

    #[test]
    fn render_mode_overlay_renders_publish_input() {
        // Arrange
        let session_id = "session-publish";
        let mode = AppMode::PublishBranchInput {
            default_branch_name: "wt/session-publish".to_string(),
            input: InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: crate::domain::session::PublishBranchAction::PublishPullRequest,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: session_id.into(),
            },
        };

        // Act
        let (_, text) = render_list_backed_mode(&mode);

        // Assert
        assert!(text.contains("Publish Review Request"));
        assert!(text.contains("wt/session-publish"));
    }
}
