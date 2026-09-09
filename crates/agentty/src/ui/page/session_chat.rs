use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::domain::agent::ReasoningLevel;
use crate::domain::question::QuestionItem;
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    Session, can_merge_session_branch_in_stack, can_mutate_session_branch_in_stack,
    can_rebase_session_branch_in_stack, can_reply_to_session_in_stack,
    can_start_staged_session_in_stack,
};
use crate::domain::{input, review};
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::help_action::{self, ViewActionAvailability, ViewHelpState};
use crate::presentation::prompt::PromptAtMentionState;
use crate::ui::component::chat_input::{ChatInput, SuggestionList};
use crate::ui::component::session_output::{
    SessionOutput, SessionOutputLayoutCache, SessionOutputLineContext,
};
use crate::ui::icon::Icon;
use crate::ui::input_layout::{
    calculate_input_height, overlay_area_above, panel_inner_height, suggestion_dropdown_height,
};
use crate::ui::{
    Component, Page, layout, markdown, prompt_format, question_format, session_format,
};

/// Maximum rendered height of the prompt input panel, including borders.
const CHAT_INPUT_MAX_PANEL_HEIGHT: u16 = 10;
/// Header height assumed when the rendered header line count does not fit
/// `u16`.
const SESSION_HEADER_FALLBACK_HEIGHT: u16 = 2;
/// Height of the single-row footer reserved by non-prompt, non-question modes.
const SINGLE_ROW_FOOTER_HEIGHT: u16 = 1;

/// Prompt-panel data prepared once per render pass so layout and painting use
/// the same suggestion set.
struct PreparedPromptPanel {
    footer_text: Line<'static>,
    /// Whether the transcript above the composer currently holds focus, which
    /// dims the composer border and hides its cursor.
    is_chat_focused: bool,
    status: Option<String>,
    suggestion_list: Option<SuggestionList>,
    title: String,
    total_height: u16,
}

impl PreparedPromptPanel {
    /// Returns the reserved prompt-panel height for the current render pass.
    fn panel_height(&self) -> u16 {
        self.total_height
    }
}

/// Complete geometry and prepared panel data for one session-chat frame.
///
/// Both painting and runtime scroll metrics construct this plan, keeping the
/// transcript viewport, prompt dropdown, and question-panel allocation on one
/// deterministic path.
struct SessionChatLayoutPlan {
    areas: layout::SessionChatAreas,
    header_lines: Vec<Line<'static>>,
    prompt_panel: Option<PreparedPromptPanel>,
    question_panel_areas: Option<layout::QuestionPanelAreas>,
}

impl SessionChatLayoutPlan {
    /// Resolves all session-chat geometry and prepared prompt data once.
    fn new(input: SessionChatLayoutInput<'_>) -> Self {
        let mut header_lines = session_format::session_header_lines(
            input.session,
            input.area.width.saturating_sub(2),
            input.default_reasoning_level,
            input.wall_clock_unix_seconds,
            input.has_merge_conflict,
        );
        header_lines.push(session_format::session_resources_line(
            None,
            input.area.width.saturating_sub(2),
        ));
        let prompt_panel =
            prepare_prompt_panel(input.area, input.mode, input.review_text, input.session);
        let bottom_height = prompt_panel.as_ref().map_or_else(
            || non_prompt_bottom_height(input.area, input.mode),
            PreparedPromptPanel::panel_height,
        );
        let areas =
            layout::session_chat_areas(input.area, bottom_height, header_height(&header_lines));
        let question_panel_areas = question_panel_areas(areas.bottom_area, input.mode);

        Self {
            areas,
            header_lines,
            prompt_panel,
            question_panel_areas,
        }
    }

    /// Returns the transcript rows inside the bordered output panel.
    fn transcript_view_height(&self) -> u16 {
        panel_inner_height(
            self.areas.output_area,
            session_format::session_output_panel_borders(),
        )
    }
}

/// Chat page renderer for a single session.
pub struct SessionChatPage<'a> {
    /// Exact prompt transcript block for the active turn, when available.
    pub active_prompt_output: Option<&'a str>,
    /// Transient progress text for the active agent turn.
    pub active_progress: Option<&'a str>,
    /// Whether the session worktree can be opened externally.
    pub can_open_worktree: bool,
    /// Active project-scoped default reasoning level.
    pub default_reasoning_level: ReasoningLevel,
    /// Whether the session branch currently conflicts with its base branch.
    pub has_merge_conflict: bool,
    /// One coherent render-time clock snapshot.
    pub(crate) frame_time: FrameTime,
    /// Shared markdown cache reused across transcript renders in this page.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current UI mode that controls the bottom panel and focus.
    pub mode: &'a AppMode,
    /// Shared fully assembled output-layout cache for scroll metrics and
    /// frame rendering.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    /// Most recent tracked agent process-tree totals.
    pub resources: Option<SessionResources>,
    /// Focused-review output for the rendered session.
    pub review_text: Option<&'a str>,
    /// Current vertical transcript scroll offset.
    pub scroll_offset: Option<u16>,
    /// Index of the session being rendered.
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    /// Session rows available to the page.
    pub sessions: &'a [Session],
}

/// Borrowed inputs needed to construct one session chat page renderer.
#[derive(Clone, Copy)]
pub struct SessionChatPageInput<'a> {
    /// Exact prompt transcript block for the currently active turn, when one
    /// has been submitted in this app process.
    pub active_prompt_output: Option<&'a str>,
    /// Transient progress text rendered in the active-status loader row.
    pub active_progress: Option<&'a str>,
    /// Active project-scoped default reasoning level.
    pub default_reasoning_level: ReasoningLevel,
    /// Whether the session branch currently conflicts with its base branch.
    pub has_merge_conflict: bool,
    /// One coherent render-time clock snapshot.
    pub(crate) frame_time: FrameTime,
    /// Shared render cache for session transcript markdown.
    pub markdown_render_cache: &'a markdown::MarkdownRenderCache,
    /// Current UI mode that determines view, prompt, and question rendering.
    pub mode: &'a AppMode,
    /// Shared output-layout cache for this render pass.
    pub output_layout_cache: &'a SessionOutputLayoutCache,
    /// Most recent tracked agent process-tree totals.
    pub resources: Option<SessionResources>,
    /// Focused-review output for the rendered session.
    pub review_text: Option<&'a str>,
    /// Current vertical output scroll offset.
    pub scroll_offset: Option<u16>,
    /// Index of the session being rendered.
    pub session_index: usize,
    /// Observable update version for the rendered session snapshot.
    pub session_update_version: u64,
    /// Session rows available to the page.
    pub sessions: &'a [Session],
}

impl<'a> SessionChatPage<'a> {
    /// Creates a session chat page renderer.
    pub fn new(input: SessionChatPageInput<'a>) -> Self {
        let SessionChatPageInput {
            active_prompt_output,
            active_progress,
            resources,
            default_reasoning_level,
            frame_time,
            has_merge_conflict,
            markdown_render_cache,
            mode,
            output_layout_cache,
            review_text,
            scroll_offset,
            session_index,
            session_update_version,
            sessions,
        } = input;

        Self {
            active_prompt_output,
            active_progress,
            resources,
            can_open_worktree: false,
            default_reasoning_level,
            frame_time,
            has_merge_conflict,
            markdown_render_cache,
            mode,
            output_layout_cache,
            review_text,
            scroll_offset,
            session_index,
            session_update_version,
            sessions,
        }
    }

    /// Sets whether the rendered session currently exposes a materialized
    /// worktree that can be opened from the footer/help affordances.
    #[must_use]
    pub fn can_open_worktree(mut self, can_open_worktree: bool) -> Self {
        self.can_open_worktree = can_open_worktree;
        self
    }

    /// Returns the rendered output line count for chat content at a given
    /// width and viewport height.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering, including review text, generic active-status loaders, and
    /// conditional scrollbar gutter reservation, so scroll math can stay in
    /// sync with what users see.
    pub(crate) fn rendered_output_line_count(
        session: &Session,
        output_width: u16,
        viewport_height: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: &markdown::MarkdownRenderCache,
        output_layout_cache: &SessionOutputLayoutCache,
    ) -> u16 {
        SessionOutput::rendered_line_count(
            session,
            output_width,
            viewport_height,
            context,
            Some(markdown_render_cache),
            Some(output_layout_cache),
        )
    }

    /// Renders the session header, output panel, and context-aware bottom
    /// panel.
    fn render_session(&self, f: &mut Frame, area: Rect, session: &Session) {
        let mut layout_plan = SessionChatLayoutPlan::new(SessionChatLayoutInput {
            area,
            default_reasoning_level: self.default_reasoning_level,
            has_merge_conflict: self.has_merge_conflict,
            mode: self.mode,
            review_text: self.review_text,
            session,
            wall_clock_unix_seconds: self.frame_time.unix_seconds(),
        });

        if let Some(line) = layout_plan.header_lines.last_mut() {
            *line = session_format::session_resources_line(
                self.resources,
                area.width.saturating_sub(2),
            );
        }

        let mut output = SessionOutput::new(session)
            .markdown_render_cache(self.markdown_render_cache)
            .output_layout_cache(self.output_layout_cache)
            .session_update_version(self.session_update_version)
            .spinner_frame(Icon::spinner_frame_from_millis(
                self.frame_time.unix_millis(),
            ));
        output = output.active_prompt_output(self.active_prompt_output);
        if let Some(scroll_offset) = self.scroll_offset {
            output = output.scroll_offset(scroll_offset);
        }
        if let Some(active_progress) = self.active_progress {
            output = output.active_progress(active_progress);
        }
        output.render(f, layout_plan.areas.output_area);
        self.render_bottom_panel(f, session, &layout_plan);
        Self::render_session_header(f, layout_plan.areas.header_area, layout_plan.header_lines);
    }

    /// Renders the header above the output panel border.
    fn render_session_header(f: &mut Frame, header_area: Rect, header_lines: Vec<Line<'static>>) {
        let header = Paragraph::new(header_lines);

        f.render_widget(header, header_area);
    }

    /// Renders the context-aware bottom panel for prompt and question modes.
    fn render_bottom_panel(
        &self,
        f: &mut Frame,
        session: &Session,
        layout_plan: &SessionChatLayoutPlan,
    ) {
        let bottom_area = layout_plan.areas.bottom_area;

        if let AppMode::Prompt { input, .. } = self.mode {
            let Some(prepared_prompt_panel) = layout_plan.prompt_panel.as_ref() else {
                return;
            };

            let mut chat_input =
                ChatInput::new(&prepared_prompt_panel.title, input.text(), input.cursor)
                    .placeholder("Type your message")
                    .active(!prepared_prompt_panel.is_chat_focused);

            if let Some(status) = prepared_prompt_panel.status.as_deref() {
                chat_input = chat_input.status(status);
            }

            if let Some(suggestion_list) = &prepared_prompt_panel.suggestion_list {
                chat_input = chat_input.suggestion_list(suggestion_list);
            }

            if bottom_area.height <= 1 {
                chat_input.render(f, bottom_area);

                return;
            }

            let panel_areas = layout::prompt_panel_areas(bottom_area);

            chat_input.render(f, panel_areas.input_area);
            f.render_widget(
                Paragraph::new(prepared_prompt_panel.footer_text.clone()),
                panel_areas.footer_area,
            );

            return;
        }

        if let AppMode::Question {
            at_mention_state,
            focus,
            questions,
            current_index,
            input,
            selected_option_index,
            ..
        } = self.mode
        {
            render_question_panel(
                f,
                bottom_area,
                layout_plan.question_panel_areas,
                &QuestionPanelState {
                    at_mention_state: at_mention_state.as_ref(),
                    current_index: *current_index,
                    focus: *focus,
                    has_session_diff: session.stats.should_show_diff(),
                    input,
                    questions,
                    selected_option_index: *selected_option_index,
                },
            );

            return;
        }

        let can_start_staged_session =
            can_start_staged_session_in_stack(self.sessions, session.id.as_str());
        let can_reply_to_session =
            can_reply_to_session_in_stack(self.sessions, session.id.as_str());
        let can_merge_session_branch =
            can_merge_session_branch_in_stack(self.sessions, session.id.as_str());
        let can_mutate_session_branch =
            can_mutate_session_branch_in_stack(self.sessions, session.id.as_str());
        let can_rebase_session_branch =
            can_rebase_session_branch_in_stack(self.sessions, session.id.as_str());
        let view_help_state = ViewHelpState {
            can_fork_session: ViewActionAvailability::from_bool(session.allows_fork_action()),
            can_merge_session_branch: ViewActionAvailability::from_bool(can_merge_session_branch),
            can_mutate_session_branch: ViewActionAvailability::from_bool(can_mutate_session_branch),
            can_open_worktree: ViewActionAvailability::from_bool(
                self.can_open_worktree && session.allows_worktree_open_action(),
            ),
            can_rebase_session_branch: ViewActionAvailability::from_bool(can_rebase_session_branch),
            can_show_diff: ViewActionAvailability::from_bool(session.stats.should_show_diff()),
            reply_to_session: ViewActionAvailability::from_bool(can_reply_to_session),
            can_start_staged_session: ViewActionAvailability::from_bool(can_start_staged_session),
            publish_pull_request_action: session.publish_pull_request_action(),
            session_state: help_action::session_view_state(session),
        };
        let help_message =
            Paragraph::new(session_format::session_view_footer_line(view_help_state));
        f.render_widget(help_message, bottom_area);
    }
}

/// Session-chat geometry inputs available outside a render pass.
///
/// Runtime scroll handlers hold app state instead of a page instance, so the
/// values the page derives its geometry from are passed explicitly.
#[derive(Clone, Copy)]
pub(crate) struct SessionChatLayoutInput<'a> {
    /// Page area routed to the session chat page, excluding the status and
    /// footer bars.
    pub(crate) area: Rect,
    /// Active project-scoped default reasoning level shown in the header.
    pub(crate) default_reasoning_level: ReasoningLevel,
    /// Whether the session branch currently conflicts with its base branch.
    pub(crate) has_merge_conflict: bool,
    /// Current UI mode, which determines the reserved bottom-panel height.
    pub(crate) mode: &'a AppMode,
    /// Focused-review output for the rendered session.
    pub(crate) review_text: Option<&'a str>,
    /// Session whose transcript is rendered.
    pub(crate) session: &'a Session,
    /// Render-time clock used for the header's deterministic timers.
    pub(crate) wall_clock_unix_seconds: i64,
}

/// Returns the transcript rows the session output panel paints for `input`.
///
/// Scroll math routes through the same header, bottom-panel, and border
/// geometry the renderer uses, so a tall composer or an open suggestion
/// dropdown shrinks the scroll viewport exactly as it does on screen.
pub(crate) fn transcript_view_height(input: SessionChatLayoutInput<'_>) -> u16 {
    SessionChatLayoutPlan::new(input).transcript_view_height()
}

/// Returns the header rows reserved above the session output panel.
fn header_height(header_lines: &[Line<'static>]) -> u16 {
    u16::try_from(header_lines.len()).unwrap_or(SESSION_HEADER_FALLBACK_HEIGHT)
}

/// Prepares prompt-panel layout and suggestion data once for a render pass.
///
/// Returns `None` outside prompt mode.
fn prepare_prompt_panel(
    area: Rect,
    mode: &AppMode,
    review_text: Option<&str>,
    session: &Session,
) -> Option<PreparedPromptPanel> {
    let AppMode::Prompt {
        at_mention_state,
        attachment_state,
        focus,
        input,
        slash_state,
        ..
    } = mode
    else {
        return None;
    };

    // While the session is `InProgress` the composer queues a leading
    // `/` as plain text instead of running a slash command, so suppress
    // the slash dropdown to avoid implying the menu is actionable. The
    // `@` mention dropdown is still useful for editing queued messages.
    let suppress_slash_dropdown = session.status == crate::domain::session::Status::InProgress;
    let allow_apply_command = review::has_actionable_review_suggestions(review_text);
    let suggestion_list = if suppress_slash_dropdown && input.text().starts_with('/') {
        None
    } else {
        prompt_format::prompt_suggestion_list(
            input,
            slash_state,
            at_mention_state.as_ref(),
            session.agent.kind(),
            allow_apply_command,
        )
    };
    let dropdown_row_count =
        prompt_format::prompt_suggestion_dropdown_rows(suggestion_list.as_ref());
    let input_height = calculate_input_height(area.width.saturating_sub(2), input.text())
        .min(CHAT_INPUT_MAX_PANEL_HEIGHT);
    let desired_bottom_height = input_height
        .saturating_add(u16::try_from(dropdown_row_count).unwrap_or(u16::MAX))
        .saturating_add(1);
    let max_bottom_height = area.height.saturating_sub(1);

    Some(PreparedPromptPanel {
        footer_text: prompt_format::prompt_footer_line(
            session,
            attachment_state.attachments.len(),
            *focus,
        ),
        is_chat_focused: *focus == ChatFocus::Chat,
        status: Some(session_format::prompt_session_status(session)),
        suggestion_list,
        title: format!("[{}]", session.agent.model().as_str()),
        total_height: desired_bottom_height.min(max_bottom_height),
    })
}

/// Returns the bottom-panel height reserved for non-prompt page modes.
///
/// Question mode derives its height from the question layout helper and the
/// visible option list. All other modes reserve a single footer row.
fn non_prompt_bottom_height(area: Rect, mode: &AppMode) -> u16 {
    let AppMode::Question {
        questions,
        current_index,
        input,
        selected_option_index,
        ..
    } = mode
    else {
        return SINGLE_ROW_FOOTER_HEIGHT;
    };

    let question_item = questions.get(*current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
    let is_free_text_mode = selected_option_index.is_none();
    let input_text = if is_free_text_mode { input.text() } else { "" };

    layout::question_panel_reserved_height(
        area.width,
        area.height.saturating_sub(SINGLE_ROW_FOOTER_HEIGHT),
        question,
        input_text,
        options.len(),
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    )
}

/// Resolves question sub-areas from the already reserved bottom-panel area.
fn question_panel_areas(bottom_area: Rect, mode: &AppMode) -> Option<layout::QuestionPanelAreas> {
    let AppMode::Question {
        questions,
        current_index,
        input,
        selected_option_index,
        ..
    } = mode
    else {
        return None;
    };
    let question_item = questions.get(*current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
    let input_text = if selected_option_index.is_none() {
        input.text()
    } else {
        ""
    };

    Some(layout::question_panel_areas(
        bottom_area,
        question,
        input_text,
        options.len(),
        CHAT_INPUT_MAX_PANEL_HEIGHT,
    ))
}

/// Bundled question-mode state passed to the panel renderer.
#[derive(Clone, Copy)]
struct QuestionPanelState<'a> {
    at_mention_state: Option<&'a PromptAtMentionState>,
    current_index: usize,
    focus: ChatFocus,
    has_session_diff: bool,
    input: &'a input::InputState,
    questions: &'a [QuestionItem],
    selected_option_index: Option<usize>,
}

/// Renders the question-mode bottom panel with question text, options, input,
/// and help footer.
fn render_question_panel(
    f: &mut Frame,
    bottom_area: Rect,
    panel_areas: Option<layout::QuestionPanelAreas>,
    state: &QuestionPanelState<'_>,
) {
    let QuestionPanelState {
        at_mention_state,
        current_index,
        focus,
        has_session_diff,
        input,
        questions,
        selected_option_index,
    } = *state;
    let question_item = questions.get(current_index);
    let question = question_item.map_or("", |item| item.text.as_str());
    let options = question_item
        .map(|item| item.options.as_slice())
        .unwrap_or_default();
    let is_free_text_mode = selected_option_index.is_none();
    let Some(panel_areas) = panel_areas else {
        return;
    };

    let is_chat_focused = focus == ChatFocus::Chat;
    let question_title = format!("Question {}/{}", current_index + 1, questions.len());
    if panel_areas.question_area.height > 0 {
        let question_para = Paragraph::new(question_format::question_panel_lines(
            &question_title,
            question,
            is_chat_focused,
            bottom_area.width,
        ));
        f.render_widget(question_para, panel_areas.question_area);
    }

    if panel_areas.options_area.height > 0 {
        f.render_widget(
            Paragraph::new(question_format::question_option_lines(
                options,
                selected_option_index,
                is_chat_focused,
            )),
            panel_areas.options_area,
        );
    }

    // Always render the input widget so the panel height stays stable
    // across mode transitions. When navigating options the input shows a
    // placeholder; when in free-text mode it is fully editable.
    let (display_text, display_cursor) = if is_free_text_mode {
        (input.text(), input.cursor)
    } else {
        ("", 0)
    };
    let input_placeholder = "Type answer";
    let at_mention_max_visible =
        layout::question_at_mention_max_visible(bottom_area, panel_areas.input_area, 10);
    let at_mention_menu = if is_free_text_mode && at_mention_max_visible > 0 {
        at_mention_state.and_then(|state| {
            prompt_format::file_lookup_suggestion_list(
                display_text,
                display_cursor,
                state,
                at_mention_max_visible,
            )
        })
    } else {
        None
    };
    let chat_input = ChatInput::new("Answer", display_text, display_cursor)
        .placeholder(input_placeholder)
        .active(is_free_text_mode && !is_chat_focused);
    if panel_areas.input_area.height > 0 {
        chat_input.render(f, panel_areas.input_area);
    }

    let is_at_mention_open =
        is_free_text_mode && at_mention_state.is_some() && input.at_mention_query().is_some();
    let lookup_state = if !is_at_mention_open {
        question_format::QuestionLookupState::Closed
    } else if at_mention_max_visible == 0 {
        question_format::QuestionLookupState::Clipped
    } else if at_mention_menu.is_some() {
        question_format::QuestionLookupState::Matches
    } else {
        question_format::QuestionLookupState::Empty
    };
    render_question_at_mention_overlay(f, bottom_area, panel_areas.input_area, at_mention_menu);
    render_question_help_footer(
        f,
        panel_areas.help_area,
        panel_areas.help_area.height,
        focus,
        has_session_diff,
        !is_free_text_mode,
        lookup_state,
    );
}

/// Renders the question-mode help footer with context-aware action hints.
///
/// `has_session_diff` controls whether the chat-focused footer advertises the
/// diff preview.
///
/// `is_navigating_options` mirrors the runtime predicate that treats plain `q`
/// as a sessions-list shortcut, so the footer can surface it whenever the
/// shortcut is actually wired up. `lookup_state` only advertises selection
/// and navigation when the prepared suggestion list contains matches.
fn render_question_help_footer(
    f: &mut Frame,
    area: Rect,
    help_height: u16,
    focus: ChatFocus,
    has_session_diff: bool,
    is_navigating_options: bool,
    lookup_state: question_format::QuestionLookupState,
) {
    if help_height == 0 {
        return;
    }

    let help_para = Paragraph::new(question_format::question_help_footer_line(
        focus,
        has_session_diff,
        is_navigating_options,
        lookup_state,
    ))
    .alignment(ratatui::layout::Alignment::Left);
    f.render_widget(help_para, area);
}

/// Renders the at-mention file dropdown as an overlay above the input area.
///
/// The dropdown covers the options section so the file list is fully visible
/// without pushing the input line out of view.
fn render_question_at_mention_overlay(
    f: &mut Frame,
    bottom_area: Rect,
    input_area: Rect,
    at_mention_menu: Option<SuggestionList>,
) {
    let Some(menu) = at_mention_menu else {
        return;
    };

    let dropdown_height = suggestion_dropdown_height(menu.items.len());
    let Some(dropdown_area) = overlay_area_above(bottom_area, input_area, dropdown_height) else {
        return;
    };

    ChatInput::render_suggestion_dropdown(f, dropdown_area, &menu);
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            self.render_session(f, area, session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::input::InputState;
    use crate::domain::question::QuestionItem;
    use crate::domain::session::Status;
    use crate::domain::session_message::SessionTranscript;
    use crate::presentation::app_mode::ChatFocus;
    use crate::presentation::prompt::{
        PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };

    fn session_fixture() -> Session {
        crate::test_support::SessionFixtureBuilder::new()
            .folder(std::env::temp_dir())
            .status(Status::Draft)
            .build()
    }

    /// Builds a default test page for one session and mode.
    fn test_session_chat_page<'a>(session: &'a Session, mode: &'a AppMode) -> SessionChatPage<'a> {
        SessionChatPage::new(SessionChatPageInput {
            active_prompt_output: None,
            active_progress: None,
            resources: None,
            default_reasoning_level: ReasoningLevel::default(),
            frame_time: FrameTime::new(0, 0, 0),
            has_merge_conflict: false,
            markdown_render_cache: test_markdown_render_cache(),
            mode,
            output_layout_cache: test_output_layout_cache(),
            review_text: None,
            scroll_offset: None,
            session_index: 0,
            session_update_version: 0,
            sessions: std::slice::from_ref(session),
        })
    }

    /// Builds prompt mode with `input_text` staged in the composer.
    fn prompt_mode(input_text: &str) -> AppMode {
        AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            focus: ChatFocus::Chat,
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::with_text(input_text.to_string()),
            scroll_offset: None,
            session_id: "session-id".into(),
            slash_state: PromptSlashState::default(),
        }
    }

    /// Builds page geometry inputs for one session and mode.
    fn layout_input<'a>(
        area: Rect,
        session: &'a Session,
        mode: &'a AppMode,
    ) -> SessionChatLayoutInput<'a> {
        SessionChatLayoutInput {
            area,
            default_reasoning_level: ReasoningLevel::default(),
            has_merge_conflict: false,
            mode,
            review_text: None,
            session,
            wall_clock_unix_seconds: 0,
        }
    }

    #[test]
    fn test_transcript_view_height_shrinks_for_multiline_prompt_draft() {
        // Arrange
        let session = session_fixture();
        let area = Rect::new(0, 0, 80, 30);
        let single_line_mode = prompt_mode("draft");
        let multiline_mode = prompt_mode("draft\nsecond line\nthird line");

        // Act
        let single_line_height =
            transcript_view_height(layout_input(area, &session, &single_line_mode));
        let multiline_height =
            transcript_view_height(layout_input(area, &session, &multiline_mode));

        // Assert
        assert_eq!(multiline_height, single_line_height.saturating_sub(2));
    }

    #[test]
    fn test_transcript_view_height_shrinks_for_open_suggestion_dropdown() {
        // Arrange
        let session = session_fixture();
        let area = Rect::new(0, 0, 80, 30);
        let plain_mode = prompt_mode("draft");
        let slash_mode = prompt_mode("/");

        // Act
        let plain_height = transcript_view_height(layout_input(area, &session, &plain_mode));
        let slash_height = transcript_view_height(layout_input(area, &session, &slash_mode));

        // Assert
        assert!(
            slash_height < plain_height,
            "slash dropdown rows must shrink the transcript viewport: {slash_height} < \
             {plain_height}"
        );
    }

    /// Returns a leaked markdown cache for test page builders that need a
    /// stable borrow across the page lifetime.
    fn test_markdown_render_cache() -> &'static markdown::MarkdownRenderCache {
        Box::leak(Box::new(markdown::MarkdownRenderCache::default()))
    }

    /// Returns a leaked output-layout cache for test page builders that need a
    /// stable borrow across the page lifetime.
    fn test_output_layout_cache() -> &'static SessionOutputLayoutCache {
        Box::leak(Box::new(SessionOutputLayoutCache::default()))
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        let start = usize::from(row) * usize::from(width);
        let end = start + usize::from(width);

        buffer.content()[start..end]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Verifies the chat footer advertises continuation for completed
    /// sessions.
    #[test]
    fn test_render_done_session_shows_continue_footer_action() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Done;
        let mode = AppMode::View {
            session_id: "session-id".into(),
            scroll_offset: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw done session");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("c: continue"));
        assert!(!text.contains("comments"));
    }

    /// Verifies canceled terminal sessions advertise continuation in the chat
    /// footer.
    #[test]
    fn test_render_canceled_session_shows_continue_footer_action() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Canceled;
        let mode = AppMode::View {
            session_id: "session-id".into(),
            scroll_offset: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw canceled session");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("c: continue"));
        assert!(!text.contains("comments"));
    }

    /// Verifies running sessions advertise sync as a queued action while
    /// keeping the running-turn stop action visible.
    #[test]
    fn test_render_in_progress_session_shows_sync_and_stop_footer_actions() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;
        let mode = AppMode::View {
            session_id: "session-id".into(),
            scroll_offset: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw in-progress session");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("r: sync"));
        assert!(text.contains("Ctrl+c: stop"));
    }

    #[test]
    fn test_status_bar_fyi_rotates_between_session_chat_messages() {
        // Arrange
        let actual_messages = crate::ui::page::fyi::session_chat_messages();

        // Act
        let wrapped_message = crate::ui::page::fyi::rotating_message(
            actual_messages,
            u64::try_from(actual_messages.len()).unwrap_or_default(),
        );

        // Assert
        assert_eq!(actual_messages.len(), 9);
        assert_eq!(
            wrapped_message,
            Some("Queued replies run one by one after the active turn finishes.")
        );
    }

    #[test]
    fn test_rendered_output_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.transcript = Some(crate::test_support::assistant_transcript(
            "word ".repeat(40),
        ));
        let raw_line_count = u16::try_from(
            session
                .transcript
                .as_ref()
                .and_then(SessionTranscript::replay_text)
                .unwrap_or_default()
                .lines()
                .count(),
        )
        .unwrap_or(u16::MAX);
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();

        // Act
        let rendered_line_count = SessionChatPage::rendered_output_line_count(
            &session,
            20,
            5,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                session_update_version: 0,
            },
            &markdown_render_cache,
            &output_layout_cache,
        );

        // Assert
        assert!(rendered_line_count > raw_line_count);
    }

    #[test]
    fn test_review_text_reads_snapshot_for_prompt_output() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            focus: ChatFocus::Input,
            history_state: PromptHistoryState::default(),
            slash_state: PromptSlashState::default(),
            session_id: "session-id".into(),
            input: InputState::default(),
            scroll_offset: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        page.review_text = Some("Focused review");

        // Act
        let review_text = page.review_text;

        // Assert
        assert_eq!(review_text, Some("Focused review"));
    }

    #[test]
    fn test_review_text_reads_snapshot_for_question_output() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem::new("Need tests?")],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        page.review_text = Some("Focused review");

        // Act
        let review_text = page.review_text;

        // Assert
        assert_eq!(review_text, Some("Focused review"));
    }

    #[test]
    fn test_rendered_output_line_count_includes_question_review_output() {
        // Arrange
        let mut session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let without_review = SessionChatPage::rendered_output_line_count(
            &session,
            40,
            5,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                session_update_version: 0,
            },
            &markdown_render_cache,
            &output_layout_cache,
        );
        session
            .transient_messages
            .upsert(crate::domain::transient_message::TransientMessage {
                anchor:
                    crate::domain::transient_message::TransientMessageAnchor::AfterCompletedTurn,
                body: crate::domain::transient_message::TransientMessageBody::Markdown(
                    "## Review\n\n- Finding".to_string(),
                ),
                lifecycle:
                    crate::domain::transient_message::TransientMessageLifecycle::ClearOnNewTurn,
                slot: crate::domain::transient_message::TransientMessageSlot::Review,
                turn_position: None,
            });

        // Act
        let with_review = SessionChatPage::rendered_output_line_count(
            &session,
            40,
            5,
            SessionOutputLineContext {
                active_prompt_output: None,
                active_progress: None,
                session_update_version: 0,
            },
            &markdown_render_cache,
            &output_layout_cache,
        );

        // Assert
        assert!(with_review > without_review);
    }

    /// Renders prompt mode for one session and returns the painted text.
    fn rendered_prompt_mode_text(session: &Session) -> String {
        let mode = prompt_mode("draft");
        let mut page = test_session_chat_page(session, &mode);
        let backend = ratatui::backend::TestBackend::new(80, 14);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw prompt mode");

        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn test_render_prompt_composer_shows_speed_and_auto_edit_for_supported_provider() {
        // Arrange
        let mut session = session_fixture();
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            crate::domain::agent::AgentModel::Gpt56Sol,
        );

        // Act
        let text = rendered_prompt_mode_text(&session);

        // Assert
        assert!(text.contains("[gpt-5.6-sol] · Normal · Auto Edit"));
        assert!(!text.contains("[gpt-5.6-sol]  · Normal · Auto Edit"));
    }

    #[test]
    fn test_prompt_footer_shows_permission_mode_shortcut() {
        // Arrange
        let session = session_fixture();

        // Act
        let footer = prompt_format::prompt_footer_line(&session, 0, ChatFocus::Input);

        // Assert
        assert!(footer.to_string().contains("Shift+Tab: switch mode"));
    }

    #[test]
    fn test_render_prompt_composer_shows_read_only_after_speed_status() {
        // Arrange
        let mut session = session_fixture();
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            crate::domain::agent::AgentModel::Gpt56Sol,
        );
        session.permission_mode = crate::domain::permission::PermissionMode::ReadOnly;

        // Act
        let text = rendered_prompt_mode_text(&session);

        // Assert
        assert!(text.contains("· Normal · Read Only"));
    }

    #[test]
    fn test_render_prompt_composer_shows_auto_edit_without_unsupported_speed_status() {
        // Arrange
        let mut session = session_fixture();
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Gemini,
            crate::domain::agent::AgentModel::Gemini31Pro,
        );

        // Act
        let text = rendered_prompt_mode_text(&session);

        // Assert
        assert!(text.contains("· Auto Edit"));
        assert!(!text.contains("· Normal"));
    }

    #[test]
    fn test_render_prompt_composer_shows_read_only_without_speed_status() {
        // Arrange
        let mut session = session_fixture();
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Gemini,
            crate::domain::agent::AgentModel::Gemini31Pro,
        );
        session.permission_mode = crate::domain::permission::PermissionMode::ReadOnly;

        // Act
        let text = rendered_prompt_mode_text(&session);

        // Assert
        assert!(text.contains("· Read Only"));
        assert!(!text.contains("· Normal"));
    }

    #[test]
    fn test_render_question_panel_preserves_frame_without_prepared_areas() {
        // Arrange
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
        let input = InputState::default();
        let questions = vec![QuestionItem {
            options: vec![],
            text: "Which path?".to_string(),
        }];
        let state = QuestionPanelState {
            at_mention_state: None,
            current_index: 0,
            focus: ChatFocus::Input,
            has_session_diff: false,
            input: &input,
            questions: &questions,
            selected_option_index: None,
        };

        // Act
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("sentinel"), frame.area());
                render_question_panel(frame, frame.area(), None, &state);
            })
            .expect("failed to draw question panel");

        // Assert
        assert!(buffer_text(terminal.backend().buffer()).contains("sentinel"));
    }

    #[test]
    fn test_render_question_mode_keeps_typed_answer_visible_in_tight_layout() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: "Need a detailed migration plan with rollback guidance?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("typed answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(32, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("typed answer"));
    }

    #[test]
    fn test_render_question_mode_includes_blank_row_between_question_and_input() {
        // Arrange
        let session = session_fixture();
        let question = "Need a detailed migration plan with rollback guidance?".to_string();
        let mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: Vec::new(),
                text: question.clone(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("typed answer".to_string()),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let width = 40;
        let height = 12;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode");

        // Assert
        let area = Rect::new(0, 0, width, height);
        let bottom_height = non_prompt_bottom_height(area, &mode);
        let bottom_top = 1 + height.saturating_sub(2).saturating_sub(bottom_height);
        let panel_areas = layout::question_panel_areas(
            Rect::new(1, bottom_top, width.saturating_sub(2), bottom_height),
            &question,
            "typed answer",
            0,
            CHAT_INPUT_MAX_PANEL_HEIGHT,
        );
        let spacer_row = panel_areas.spacer_area.y;
        let spacer_text = buffer_row_text(terminal.backend().buffer(), spacer_row, width);
        assert_eq!(spacer_text.trim(), "");
    }

    #[test]
    fn test_render_question_mode_with_options_shows_option_text() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: vec!["Yes".to_string(), "No".to_string()],
                text: "Continue?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: Some(0),
        };
        let mut page = test_session_chat_page(&session, &mode);
        let width = 50;
        let height = 14;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode with options");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Options:"), "should render options header");
        assert!(text.contains("Yes"), "should render first option");
        assert!(text.contains("No"), "should render second option");
    }

    #[test]
    fn test_render_question_lookup_shows_file_and_selection_controls() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: Some(PromptAtMentionState::new(vec![
                crate::domain::file_entry::FileEntry {
                    is_dir: false,
                    path: "src/session_chat.rs".to_string(),
                },
            ])),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::with_text("@session".to_string()),
            questions: vec![QuestionItem::new("Which file?")],
            responses: Vec::new(),
            scroll_offset: None,
            selected_option_index: None,
            session_id: "session-id".into(),
        };
        let mut page = test_session_chat_page(&session, &mode);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))
            .expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| Page::render(&mut page, frame, frame.area()))
            .expect("failed to draw question lookup");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("src/session_chat.rs"));
        assert!(text.contains("Tab/Enter: select"));
        assert!(text.contains("Up/Down: navigate"));
        assert!(text.contains("Esc: cancel @"));
        assert!(!text.contains("Enter: send"));
    }

    #[test]
    fn test_render_question_lookup_requires_space_for_a_complete_result_row() {
        for height in 4..=7 {
            // Arrange
            let input = InputState::with_text("@src".to_string());
            let questions = vec![QuestionItem::new("Which file?")];
            let lookup = PromptAtMentionState::new(vec![crate::domain::file_entry::FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            }]);
            let state = QuestionPanelState {
                at_mention_state: Some(&lookup),
                current_index: 0,
                focus: ChatFocus::Input,
                has_session_diff: false,
                input: &input,
                questions: &questions,
                selected_option_index: None,
            };
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, height))
                    .expect("failed to create terminal");

            // Act
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    let panel_areas = layout::question_panel_areas(
                        area,
                        "Which file?",
                        input.text(),
                        0,
                        CHAT_INPUT_MAX_PANEL_HEIGHT,
                    );
                    render_question_panel(frame, area, Some(panel_areas), &state);
                })
                .expect("failed to draw constrained lookup");

            // Assert
            let text = buffer_text(terminal.backend().buffer());
            assert!(text.contains("@src"));
            assert!(text.contains("Esc: cancel @"));
            assert!(!text.contains("Tab/Enter: close @"));
            assert_eq!(text.contains("src/lib.rs"), height == 7);
            assert_eq!(text.contains("Tab/Enter: select"), height == 7);
            assert_eq!(text.contains("Up/Down: navigate"), height == 7);
        }
    }

    #[test]
    fn test_render_question_empty_lookup_shows_only_dismissal_controls() {
        for entries in [
            Vec::new(),
            vec![crate::domain::file_entry::FileEntry {
                is_dir: false,
                path: "src/session_chat.rs".to_string(),
            }],
        ] {
            // Arrange
            let session = session_fixture();
            let mode = AppMode::Question {
                at_mention_state: Some(PromptAtMentionState::new(entries)),
                current_index: 0,
                focus: ChatFocus::Input,
                input: InputState::with_text("@missing".to_string()),
                questions: vec![QuestionItem::new("Which file?")],
                responses: Vec::new(),
                scroll_offset: None,
                selected_option_index: None,
                session_id: "session-id".into(),
            };
            let mut page = test_session_chat_page(&session, &mode);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))
                .expect("failed to create terminal");

            // Act
            terminal
                .draw(|frame| Page::render(&mut page, frame, frame.area()))
                .expect("failed to draw empty question lookup");

            // Assert
            let text = buffer_text(terminal.backend().buffer());
            assert!(text.contains("@missing"));
            assert!(text.contains("Tab/Enter: close @"));
            assert!(text.contains("Esc: cancel @"));
            assert!(!text.contains("Tab/Enter: select"));
            assert!(!text.contains("Up/Down: navigate"));
            assert!(!text.contains("Enter: send"));
        }
    }

    #[test]
    fn test_render_question_mode_with_options_in_small_terminal_does_not_panic() {
        // Arrange
        let session = session_fixture();
        let mode = AppMode::Question {
            at_mention_state: None,
            session_id: "session-id".into(),
            questions: vec![QuestionItem {
                options: vec![
                    "A".to_string(),
                    "B".to_string(),
                    "C".to_string(),
                    "D".to_string(),
                    "E".to_string(),
                ],
                text: "Pick one of the many options?".to_string(),
            }],
            responses: Vec::new(),
            current_index: 0,
            focus: ChatFocus::Input,
            input: InputState::default(),
            scroll_offset: None,
            selected_option_index: None,
        };
        let mut page = test_session_chat_page(&session, &mode);
        let backend = ratatui::backend::TestBackend::new(30, 6);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act + Assert (should not panic)
        terminal
            .draw(|frame| {
                let area = frame.area();
                Page::render(&mut page, frame, area);
            })
            .expect("failed to draw question mode with many options in small terminal");
    }
}
