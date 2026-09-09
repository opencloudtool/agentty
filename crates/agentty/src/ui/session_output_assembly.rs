//! Pure display-block assembly for the session-output panel.
//!
//! This module owns transcript classification, display ordering, and line
//! spacing. The `session_output` component owns layout caching and Ratatui
//! painting, so callers can exercise transcript projection without a frame.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use ag_tui_text::text_util;
use ratatui::text::Line;

use crate::domain::session::{QueuedMessage, Session, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageSlot,
};
use crate::ui::markdown::{self, render_markdown};
use crate::ui::prompt_block::{self, USER_PROMPT_PREFIX, USER_PROMPT_RIGHT_GUTTER_WIDTH};
use crate::ui::session_format;
#[cfg(test)]
use crate::ui::style;

const DRAFT_PREVIEW_HEADER: &str = "## Draft Session";
const DRAFT_PREVIEW_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to stage the \
                                        first draft locally, then press `s` in session view to \
                                        start the bundle.";
const DRAFT_PREVIEW_STACKED_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to \
                                                stage the first draft locally. The `s` start \
                                                action appears after the parent is review-ready.";
const DRAFT_PREVIEW_STAGED_NOTE: &str =
    "Draft messages stay local until you press `s` in session view to start the staged bundle.";
const DRAFT_PREVIEW_STACKED_STAGED_NOTE: &str =
    "Draft messages stay local until the parent is review-ready and you press `s` in session view \
     to start the stacked bundle from its parent branch.";
const USER_PROMPT_TAB_WIDTH: usize = 4;

/// Fully assembled session-output lines plus metadata derived during assembly.
#[cfg(test)]
pub(crate) struct SessionOutputLines {
    pub(crate) active_loader_line_index: Option<usize>,
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) queued_line_indices: Vec<usize>,
    pub(crate) transient_loader_line_index: Option<usize>,
}

/// Cached transcript body that excludes the dynamic session-status tail.
#[derive(Clone)]
pub(crate) struct SessionOutputBody {
    pub(crate) lines: Arc<[Line<'static>]>,
    pub(crate) queued_line_indices: Arc<[usize]>,
    pub(crate) transient_loader_line_index: Option<usize>,
}

/// Assembles a complete session-output panel in canonical display order.
#[cfg(test)]
pub(crate) fn output_lines(
    session: &Session,
    inner_width: usize,
    active_progress: Option<&str>,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> SessionOutputLines {
    output_assembly(session, inner_width, active_progress, markdown_render_cache)
        .into_output_lines()
}

/// Assembles the stable transcript body without the dynamic status tail.
pub(crate) fn output_body(
    session: &Session,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> SessionOutputBody {
    output_assembly(session, inner_width, None, markdown_render_cache).into_output_body()
}

/// Dynamic rows rendered after the shared transcript body.
pub(crate) struct SessionOutputTail {
    pub(crate) active_loader_line_index: Option<usize>,
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) trim_body: bool,
}

/// Builds only the status rows; the cached transcript stays shared.
pub(crate) fn output_tail(session: &Session, active_progress: Option<&str>) -> SessionOutputTail {
    session_tail_lines(
        session.status,
        active_progress,
        review_loading_message(session),
        review_comment_resolution_loading_message(session),
    )
}

/// Returns whether the status owns a live or queued turn whose newest prompt
/// remains separate from completed transcript content.
pub(crate) fn status_has_active_turn(status: Status) -> bool {
    matches!(status, Status::InProgress | Status::Queued)
}

/// Returns display text for typed transcript sections in canonical order.
#[cfg(test)]
pub(crate) fn transcript_section_texts(
    status: Status,
    transcript: &SessionTranscript,
) -> (String, String, String) {
    let sections = typed_transcript_sections(status, transcript);

    (
        section_display_text(&sections.completed_turn),
        section_display_text(&sections.active_turn),
        section_display_text(&sections.trailing_notice),
    )
}

#[cfg(test)]
fn section_display_text(section: &SessionOutputTranscriptSection<'_>) -> String {
    match section {
        SessionOutputTranscriptSection::Empty => String::new(),
        SessionOutputTranscriptSection::Markdown(markdown) => markdown.clone(),
        SessionOutputTranscriptSection::Messages(messages) => {
            SessionTranscript::display_text_for_messages(messages)
        }
    }
}

/// Appends queued chat rows in submission order beneath the active turn.
#[cfg(test)]
pub(crate) fn append_queued_message_lines(
    lines: &mut Vec<Line<'static>>,
    queued_messages: &[QueuedMessage],
) {
    append_queued_entries(lines, &[], queued_messages);
}

/// Appends one user prompt block while retaining its prompt marker and shading.
#[cfg(test)]
pub(crate) fn append_user_prompt_markdown_lines(
    lines: &mut Vec<Line<'static>>,
    prompt_text: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    append_user_prompt(lines, prompt_text, inner_width, markdown_render_cache);
}

#[derive(Clone, Copy)]
enum SessionOutputBlock {
    ActiveTurn,
    CompletedTranscript,
    QueuedMessage,
    SessionTail,
    Transient(TransientMessageAnchor),
    TrailingTranscriptNotice(TrailingTranscriptNoticePlacement),
}

#[derive(Clone, Copy)]
enum TrailingTranscriptNoticePlacement {
    AfterReview,
    BeforeActiveTurn,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionOutputSeparator {
    Always,
    AfterPreviousContent,
}

const SESSION_OUTPUT_BLOCK_ORDER: [SessionOutputBlock; 9] = [
    SessionOutputBlock::CompletedTranscript,
    SessionOutputBlock::TrailingTranscriptNotice(
        TrailingTranscriptNoticePlacement::BeforeActiveTurn,
    ),
    SessionOutputBlock::Transient(TransientMessageAnchor::AfterCompletedTurn),
    SessionOutputBlock::ActiveTurn,
    SessionOutputBlock::Transient(TransientMessageAnchor::AfterActiveTurn),
    SessionOutputBlock::TrailingTranscriptNotice(TrailingTranscriptNoticePlacement::AfterReview),
    SessionOutputBlock::QueuedMessage,
    SessionOutputBlock::Transient(TransientMessageAnchor::Tail),
    SessionOutputBlock::SessionTail,
];

struct SessionOutputAssembly<'a> {
    active_loader_line_index: Option<usize>,
    active_progress: Option<&'a str>,
    active_turn_has_visible_text: bool,
    active_turn_section: SessionOutputTranscriptSection<'a>,
    completed_turn_section: SessionOutputTranscriptSection<'a>,
    inner_width: usize,
    lines: Vec<Line<'static>>,
    markdown_render_cache: Option<&'a markdown::MarkdownRenderCache>,
    queued_line_indices: Vec<usize>,
    session: &'a Session,
    status: Status,
    transient_loader_line_index: Option<usize>,
    trailing_notice_section: SessionOutputTranscriptSection<'a>,
}

struct SessionOutputTextSections<'a> {
    active_turn: SessionOutputTranscriptSection<'a>,
    completed_turn: SessionOutputTranscriptSection<'a>,
    trailing_notice: SessionOutputTranscriptSection<'a>,
}

enum SessionOutputTranscriptSection<'a> {
    Empty,
    Markdown(String),
    Messages(&'a [SessionMessage]),
}

impl SessionOutputTranscriptSection<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Markdown(text) => text.trim().is_empty(),
            Self::Messages(messages) => messages
                .iter()
                .all(|message| message.content.trim().is_empty()),
        }
    }
}

impl SessionOutputAssembly<'_> {
    #[cfg(test)]
    fn into_output_lines(mut self) -> SessionOutputLines {
        for block in SESSION_OUTPUT_BLOCK_ORDER {
            self.append_block(block);
        }

        SessionOutputLines {
            active_loader_line_index: self.active_loader_line_index,
            lines: self.lines,
            queued_line_indices: self.queued_line_indices,
            transient_loader_line_index: self.transient_loader_line_index,
        }
    }

    fn into_output_body(mut self) -> SessionOutputBody {
        for block in SESSION_OUTPUT_BLOCK_ORDER {
            if !matches!(block, SessionOutputBlock::SessionTail) {
                self.append_block(block);
            }
        }

        SessionOutputBody {
            lines: Arc::from(self.lines),
            queued_line_indices: Arc::from(self.queued_line_indices),
            transient_loader_line_index: self.transient_loader_line_index,
        }
    }

    fn append_block(&mut self, block: SessionOutputBlock) {
        match block {
            SessionOutputBlock::CompletedTranscript => self.append_completed_transcript(),
            SessionOutputBlock::TrailingTranscriptNotice(placement) => {
                self.append_trailing_transcript_notice(placement);
            }
            SessionOutputBlock::Transient(anchor) => self.append_transient_messages(anchor),
            SessionOutputBlock::ActiveTurn => self.append_active_turn(),
            SessionOutputBlock::QueuedMessage => self.append_queued_messages(),
            SessionOutputBlock::SessionTail => self.append_session_tail(),
        }
    }

    fn append_completed_transcript(&mut self) {
        append_transcript_section(
            &mut self.lines,
            &self.completed_turn_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_trailing_transcript_notice(&mut self, placement: TrailingTranscriptNoticePlacement) {
        let should_append = match placement {
            TrailingTranscriptNoticePlacement::BeforeActiveTurn => {
                self.active_turn_has_visible_text
            }
            TrailingTranscriptNoticePlacement::AfterReview => !self.active_turn_has_visible_text,
        };
        if should_append {
            append_transcript_section(
                &mut self.lines,
                &self.trailing_notice_section,
                self.inner_width,
                self.markdown_render_cache,
            );
        }
    }

    fn append_transient_messages(&mut self, anchor: TransientMessageAnchor) {
        let transient_messages = &self.session.transient_messages;
        let remaining_messages = transient_messages.messages().iter().filter(|message| {
            message.anchor == anchor && !matches!(&message.body, TransientMessageBody::Queued(_))
        });

        for message in remaining_messages {
            if let Some(loader_line_index) = append_transient_message(
                &mut self.lines,
                message,
                self.inner_width,
                self.markdown_render_cache,
            ) {
                self.transient_loader_line_index = Some(loader_line_index);
            }
        }
    }

    fn append_active_turn(&mut self) {
        append_transcript_section(
            &mut self.lines,
            &self.active_turn_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_queued_messages(&mut self) {
        self.queued_line_indices = append_queued_entries(
            &mut self.lines,
            self.session.transient_messages.messages(),
            &self.session.queued_messages,
        );
    }

    fn append_session_tail(&mut self) {
        self.active_loader_line_index = append_session_tail_lines(
            &mut self.lines,
            self.status,
            self.active_progress,
            review_loading_message(self.session),
            review_comment_resolution_loading_message(self.session),
        );
    }
}

fn output_assembly<'assembly>(
    session: &'assembly Session,
    inner_width: usize,
    active_progress: Option<&'assembly str>,
    markdown_render_cache: Option<&'assembly markdown::MarkdownRenderCache>,
) -> SessionOutputAssembly<'assembly> {
    let status = session.status;
    let transcript_sections = output_text_sections(session, status);
    let active_turn_has_visible_text = !transcript_sections.active_turn.is_empty();

    SessionOutputAssembly {
        active_loader_line_index: None,
        active_progress,
        active_turn_has_visible_text,
        active_turn_section: transcript_sections.active_turn,
        completed_turn_section: transcript_sections.completed_turn,
        inner_width,
        lines: Vec::new(),
        markdown_render_cache,
        queued_line_indices: Vec::new(),
        session,
        status,
        transient_loader_line_index: None,
        trailing_notice_section: transcript_sections.trailing_notice,
    }
}

fn append_block_separator(lines: &mut Vec<Line<'static>>, separator: SessionOutputSeparator) {
    trim_trailing_blank_lines(lines);

    if separator == SessionOutputSeparator::Always || !lines.is_empty() {
        lines.push(Line::from(""));
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
}

fn append_session_tail_lines(
    lines: &mut Vec<Line<'static>>,
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
    review_comment_resolution_message: Option<&str>,
) -> Option<usize> {
    let tail = session_tail_lines(
        status,
        active_progress,
        review_status_message,
        review_comment_resolution_message,
    );
    if tail.trim_body {
        trim_trailing_blank_lines(lines);
    }
    let active_loader_line_index = tail
        .active_loader_line_index
        .map(|index| lines.len() + index);
    lines.extend(tail.lines);

    active_loader_line_index
}

fn session_tail_lines(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
    review_comment_resolution_message: Option<&str>,
) -> SessionOutputTail {
    let status_lines = session_format::session_output_status_lines(
        status,
        active_progress,
        review_status_message,
        review_comment_resolution_message,
    );
    let trim_body = !status_lines.is_empty();
    let mut lines = vec![Line::from("")];
    let active_loader_line_index = if trim_body {
        lines.extend(status_lines);

        session_format::session_output_uses_tachyon_loader(status).then_some(1)
    } else {
        if status == Status::Done {
            lines.push(session_format::session_output_done_line());
            lines.push(Line::from(""));
        }

        None
    };

    SessionOutputTail {
        active_loader_line_index,
        lines,
        trim_body,
    }
}

fn review_loading_message(session: &Session) -> Option<&str> {
    session
        .transient_messages
        .get(TransientMessageSlot::Review)
        .and_then(|message| match &message.body {
            TransientMessageBody::Loading(message) => Some(message.as_str()),
            TransientMessageBody::Markdown(_)
            | TransientMessageBody::Plain(_)
            | TransientMessageBody::Queued(_) => None,
        })
}

fn review_comment_resolution_loading_message(session: &Session) -> Option<&str> {
    session
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentResolution)
        .and_then(|message| match &message.body {
            TransientMessageBody::Loading(message) => Some(message.as_str()),
            TransientMessageBody::Markdown(_)
            | TransientMessageBody::Plain(_)
            | TransientMessageBody::Queued(_) => None,
        })
}

fn append_transient_message(
    lines: &mut Vec<Line<'static>>,
    message: &TransientMessage,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> Option<usize> {
    match &message.body {
        TransientMessageBody::Markdown(markdown) => {
            let markdown = match message.slot {
                TransientMessageSlot::Review => session_format::format_review_markdown(markdown),
                _ => markdown.clone(),
            };
            append_markdown_lines(lines, &markdown, inner_width, markdown_render_cache);

            None
        }
        TransientMessageBody::Plain(status_message) => {
            // Empty bodies retain lifecycle state without occupying display
            // rows.
            if status_message.is_empty() {
                return None;
            }

            append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
            append_plain_status_lines(lines, status_message, inner_width);

            None
        }
        TransientMessageBody::Loading(status_message) => {
            if matches!(
                message.slot,
                TransientMessageSlot::Review | TransientMessageSlot::ReviewCommentResolution
            ) {
                return None;
            }

            append_block_separator(lines, SessionOutputSeparator::Always);
            let loader_line_index = lines.len();
            lines.extend(session_format::session_output_transient_loading_lines(
                status_message,
            ));

            Some(loader_line_index)
        }
        TransientMessageBody::Queued(_) => None,
    }
}

fn output_text_sections(session: &Session, status: Status) -> SessionOutputTextSections<'_> {
    if session.status == Status::Draft && session.is_draft_session() {
        return SessionOutputTextSections {
            active_turn: SessionOutputTranscriptSection::Empty,
            completed_turn: SessionOutputTranscriptSection::Markdown(render_draft_session_preview(
                session,
            )),
            trailing_notice: SessionOutputTranscriptSection::Empty,
        };
    }

    if let Some(transcript) = session
        .transcript
        .as_ref()
        .filter(|transcript| !transcript.is_empty())
    {
        return typed_transcript_sections(status, transcript);
    }

    SessionOutputTextSections {
        active_turn: SessionOutputTranscriptSection::Empty,
        completed_turn: SessionOutputTranscriptSection::Empty,
        trailing_notice: SessionOutputTranscriptSection::Empty,
    }
}

fn typed_transcript_sections(
    status: Status,
    transcript: &SessionTranscript,
) -> SessionOutputTextSections<'_> {
    let messages = transcript.messages();
    let active_prompt_index =
        active_prompt_message_index(status, messages).unwrap_or(messages.len());
    let (completed_messages, active_messages) = messages.split_at(active_prompt_index);
    let trailing_notice_start =
        trailing_workflow_notice_start(completed_messages).unwrap_or(completed_messages.len());
    let (completed_messages, trailing_notice_messages) =
        completed_messages.split_at(trailing_notice_start);

    SessionOutputTextSections {
        active_turn: messages_section(active_messages),
        completed_turn: messages_section(completed_messages),
        trailing_notice: messages_section(trailing_notice_messages),
    }
}

fn messages_section(messages: &[SessionMessage]) -> SessionOutputTranscriptSection<'_> {
    if messages.is_empty() {
        return SessionOutputTranscriptSection::Empty;
    }

    SessionOutputTranscriptSection::Messages(messages)
}

fn active_prompt_message_index(status: Status, messages: &[SessionMessage]) -> Option<usize> {
    if !status_has_active_turn(status) {
        return None;
    }

    messages
        .iter()
        .rposition(|message| message.kind.is_prompt())
}

fn trailing_workflow_notice_start(messages: &[SessionMessage]) -> Option<usize> {
    if messages.is_empty() {
        return None;
    }

    let Some(first_non_notice_from_end) = messages
        .iter()
        .rposition(|message| message.kind != SessionMessageKind::WorkflowNotice)
    else {
        return Some(0);
    };
    let notice_start = first_non_notice_from_end.saturating_add(1);

    (notice_start < messages.len()).then_some(notice_start)
}

fn render_draft_session_preview(session: &Session) -> String {
    let mut output = String::from(DRAFT_PREVIEW_HEADER);

    if session.has_staged_drafts() {
        let draft_note = if session.is_stacked_child() {
            DRAFT_PREVIEW_STACKED_STAGED_NOTE
        } else {
            DRAFT_PREVIEW_STAGED_NOTE
        };
        let _ = write!(output, "\n\n{draft_note}\n\n");
        output.push_str(&staged_draft_transcript_block(&session.prompt));
    } else {
        let draft_note = if session.is_stacked_child() {
            DRAFT_PREVIEW_STACKED_EMPTY_NOTE
        } else {
            DRAFT_PREVIEW_EMPTY_NOTE
        };
        let _ = write!(output, "\n\n{draft_note}\n");
    }

    if let Some(transcript_text) = session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
    {
        let _ = write!(output, "\n\n{transcript_text}");
    }

    output
}

fn staged_draft_transcript_block(prompt_text: &str) -> String {
    let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
    let mut formatted_lines = Vec::with_capacity(prompt_lines.len());
    let continuation_prefix = prompt_block::user_prompt_continuation_prefix();

    for (index, prompt_line) in prompt_lines.into_iter().enumerate() {
        let prefix = if index == 0 {
            USER_PROMPT_PREFIX
        } else {
            continuation_prefix.as_str()
        };
        formatted_lines.push(format!("{prefix}{prompt_line}"));
    }

    format!("{}\n\n", formatted_lines.join("\n"))
}

fn append_plain_status_lines(
    lines: &mut Vec<Line<'static>>,
    status_message: &str,
    inner_width: usize,
) {
    let rendered_lines = text_util::wrap_lines(status_message, inner_width)
        .into_iter()
        .map(|line| Line::from(line.to_string()));
    lines.extend(rendered_lines);
}

fn append_transcript_section(
    lines: &mut Vec<Line<'static>>,
    section: &SessionOutputTranscriptSection<'_>,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    match section {
        SessionOutputTranscriptSection::Empty => {}
        SessionOutputTranscriptSection::Markdown(markdown) => {
            append_markdown_lines(lines, markdown, inner_width, markdown_render_cache);
        }
        SessionOutputTranscriptSection::Messages(messages) => {
            append_transcript_messages(lines, messages, inner_width, markdown_render_cache);
        }
    }
}

fn append_transcript_messages(
    lines: &mut Vec<Line<'static>>,
    messages: &[SessionMessage],
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    for message in messages {
        match message.kind {
            SessionMessageKind::UserPrompt => {
                append_user_prompt(lines, &message.content, inner_width, markdown_render_cache);
            }
            SessionMessageKind::AgentPrompt => {}
            SessionMessageKind::AssistantAnswer | SessionMessageKind::WorkflowNotice => {
                append_markdown_lines(lines, &message.content, inner_width, markdown_render_cache);
            }
        }
    }
}

fn append_queued_entries(
    lines: &mut Vec<Line<'static>>,
    transient_messages: &[TransientMessage],
    queued_messages: &[QueuedMessage],
) -> Vec<usize> {
    let mut queued_line_indices = Vec::new();
    let mut has_rendered_message = false;
    let mut queued_entries = transient_messages
        .iter()
        .filter_map(|message| {
            if let TransientMessageBody::Queued(queued_action) = &message.body {
                Some((queued_action.order, queued_action.text.as_str(), ""))
            } else {
                None
            }
        })
        .chain(
            queued_messages
                .iter()
                .map(|message| (message.order(), message.transcript_text(), "queued › ")),
        )
        .collect::<Vec<_>>();
    queued_entries.sort_by_key(|(order, _, _)| *order);

    for (_, queued_text, first_line_prefix) in queued_entries {
        append_queued_entry(
            lines,
            &mut queued_line_indices,
            &mut has_rendered_message,
            queued_text,
            first_line_prefix,
        );
    }

    if has_rendered_message {
        lines.push(Line::from(""));
    }

    queued_line_indices
}

fn append_queued_entry(
    lines: &mut Vec<Line<'static>>,
    queued_line_indices: &mut Vec<usize>,
    has_rendered_message: &mut bool,
    message: &str,
    first_line_prefix: &str,
) {
    let queued_lines = session_format::session_output_queued_lines(message, first_line_prefix);
    if queued_lines.is_empty() {
        return;
    }
    if !*has_rendered_message {
        append_block_separator(lines, SessionOutputSeparator::Always);
    }

    queued_line_indices.push(lines.len());
    lines.extend(queued_lines);
    *has_rendered_message = true;
}

fn append_user_prompt(
    lines: &mut Vec<Line<'static>>,
    prompt_text: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    if prompt_text.trim().is_empty() {
        return;
    }

    let prompt_prefix_width = USER_PROMPT_PREFIX.chars().count();
    let prompt_content_width = inner_width
        .saturating_sub(prompt_prefix_width)
        .saturating_sub(USER_PROMPT_RIGHT_GUTTER_WIDTH)
        .max(1);
    let (protected_prompt_text, indent_marker) = protect_user_prompt_indentation(prompt_text);
    let rendered_lines = rendered_markdown_lines(
        &protected_prompt_text,
        prompt_content_width,
        markdown_render_cache,
    );
    let Some(first_visible_line_index) = rendered_lines.iter().position(|line| line.width() > 0)
    else {
        return;
    };
    let last_visible_line_index = rendered_lines
        .iter()
        .rposition(|line| line.width() > 0)
        .unwrap_or(first_visible_line_index);

    append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
    lines.push(prompt_block::user_prompt_padding_line(inner_width));

    let mut has_rendered_content_line = false;
    let continuation_prefix = prompt_block::user_prompt_continuation_prefix();
    for rendered_line in &rendered_lines[first_visible_line_index..=last_visible_line_index] {
        if rendered_line.width() == 0 {
            lines.push(prompt_block::user_prompt_padding_line(inner_width));

            continue;
        }

        let prefix = if has_rendered_content_line {
            continuation_prefix.as_str()
        } else {
            USER_PROMPT_PREFIX
        };
        let prefix_style = if has_rendered_content_line {
            prompt_block::user_prompt_content_style()
        } else {
            prompt_block::user_prompt_prefix_style()
        };
        lines.push(prompt_block::user_prompt_markdown_line(
            markdown::user_prompt_content_line_spans(restored_user_prompt_spans(
                rendered_line,
                indent_marker,
            )),
            prefix,
            prefix_style,
            inner_width,
        ));
        has_rendered_content_line = true;
    }

    lines.push(prompt_block::user_prompt_padding_line(inner_width));
}

fn protect_user_prompt_indentation(prompt_text: &str) -> (String, Option<char>) {
    let Some(indent_marker) = unused_private_use_character(prompt_text) else {
        return (prompt_text.to_string(), None);
    };

    let mut protected_text = String::with_capacity(prompt_text.len());
    let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
    let preservation_mask = markdown::markdown_block_preservation_mask(prompt_text);

    for (line_index, line) in prompt_lines.into_iter().enumerate() {
        if line_index > 0 {
            protected_text.push('\n');
        }

        if preservation_mask[line_index] {
            protected_text.push_str(line);
        } else {
            let (content_start, indentation_width) = leading_indentation(line);
            let content = &line[content_start..];
            protected_text.extend(std::iter::repeat_n(indent_marker, indentation_width));
            protected_text.push_str(content);
        }
    }

    (protected_text, Some(indent_marker))
}

fn unused_private_use_character(prompt_text: &str) -> Option<char> {
    const DEFAULT_INDENT_MARKER: char = '\u{e000}';

    if !prompt_text.contains(DEFAULT_INDENT_MARKER) {
        return Some(DEFAULT_INDENT_MARKER);
    }

    let used_characters = prompt_text
        .chars()
        .filter(|character| {
            matches!(
                u32::from(*character),
                0xe000..=0xf8ff | 0x000f_0000..=0x000f_fffd | 0x0010_0000..=0x0010_fffd
            )
        })
        .collect::<HashSet<_>>();
    [
        0xe000..=0xf8ff,
        0x000f_0000..=0x000f_fffd,
        0x0010_0000..=0x0010_fffd,
    ]
    .into_iter()
    .flatten()
    .filter_map(char::from_u32)
    .find(|character| !used_characters.contains(character))
}

fn leading_indentation(line: &str) -> (usize, usize) {
    let mut content_start = 0;
    let mut indentation_width = 0;

    for (byte_index, character) in line.char_indices() {
        match character {
            ' ' => indentation_width += 1,
            '\t' => {
                indentation_width +=
                    USER_PROMPT_TAB_WIDTH - (indentation_width % USER_PROMPT_TAB_WIDTH);
            }
            _ => break,
        }
        content_start = byte_index + character.len_utf8();
    }

    (content_start, indentation_width)
}

fn restored_user_prompt_spans<'line>(
    rendered_line: &'line Line<'static>,
    indent_marker: Option<char>,
) -> impl Iterator<Item = ratatui::text::Span<'static>> + 'line {
    rendered_line.spans.iter().cloned().map(move |mut span| {
        if let Some(indent_marker) = indent_marker
            && span.content.contains(indent_marker)
        {
            span.content = span.content.replace(indent_marker, " ").into();
        }

        span
    })
}

fn append_markdown_lines(
    lines: &mut Vec<Line<'static>>,
    markdown: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    let rendered_lines = rendered_markdown_lines(markdown, inner_width, markdown_render_cache);
    let Some(first_visible_line_index) = rendered_lines.iter().position(|line| line.width() > 0)
    else {
        return;
    };
    let last_visible_line_index = rendered_lines
        .iter()
        .rposition(|line| line.width() > 0)
        .unwrap_or(first_visible_line_index);

    append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
    lines.extend(
        rendered_lines[first_visible_line_index..=last_visible_line_index]
            .iter()
            .cloned(),
    );
}

fn rendered_markdown_lines(
    markdown: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> Arc<[Line<'static>]> {
    match markdown_render_cache {
        Some(cache) => cache.render(markdown, inner_width),
        None => Arc::from(render_markdown(markdown, inner_width)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::transient_message::QueuedAction;
    use crate::domain::turn_prompt::TurnPrompt;

    fn queued_message(order: u64, text: &str) -> QueuedMessage {
        QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
    }

    #[test]
    fn test_section_display_text_handles_empty_and_markdown_sections() {
        // Arrange
        let empty_section = SessionOutputTranscriptSection::Empty;
        let markdown_section =
            SessionOutputTranscriptSection::Markdown("draft preview".to_string());

        // Act
        let empty_text = section_display_text(&empty_section);
        let markdown_text = section_display_text(&markdown_section);

        // Assert
        assert!(empty_section.is_empty());
        assert!(!markdown_section.is_empty());
        assert_eq!(empty_text, "");
        assert_eq!(markdown_text, "draft preview");
    }

    #[test]
    fn test_queued_messages_skip_blank_entries() {
        // Arrange
        let mut lines = Vec::new();
        let queued_messages = vec![
            queued_message(0, " \n\t"),
            queued_message(1, "queued reply"),
        ];

        // Act
        append_queued_entries(&mut lines, &[], &queued_messages);

        // Assert
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["", "≡ queued › queued reply", ""]
        );
    }

    #[test]
    fn test_empty_preparation_marker_preserves_draft_output_rows() {
        for messages in [
            vec![],
            vec![SessionMessage::conversation(
                0,
                SessionMessageKind::UserPrompt,
                "A saved prompt",
            )],
        ] {
            // Arrange
            let mut session = crate::test_support::session_fixture("preparing", Status::Draft);
            session.transcript = Some(SessionTranscript::new(messages));
            let expected = output_lines(&session, 80, None, None);
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Plain(String::new()),
                lifecycle:
                    crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::WorkspacePreparation,
                turn_position: None,
            });

            // Act
            let actual = output_lines(&session, 80, None, None);

            // Assert
            assert_eq!(actual.lines, expected.lines);
            assert_eq!(actual.transient_loader_line_index, None);
            assert!(session.allows_cancel_action());
        }
    }

    #[test]
    fn test_transient_message_appender_skips_queued_actions() {
        // Arrange
        let mut lines = Vec::new();
        let message = TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Queued(QueuedAction::new(
                0,
                "sync after this turn".to_string(),
            )),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::SyncQueue,
            turn_position: None,
        };

        // Act
        let loader_line_index = append_transient_message(&mut lines, &message, 80, None);

        // Assert
        assert_eq!(loader_line_index, None);
        assert_eq!(lines, []);
    }

    #[test]
    fn test_generated_review_prompt_is_hidden_behind_resolution_loader() {
        // Arrange
        let mut session = crate::test_support::SessionFixtureBuilder::new()
            .status(Status::InProgress)
            .build();
        session.transcript = Some(SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "initial request"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "initial answer"),
            SessionMessage::conversation(
                2,
                SessionMessageKind::AgentPrompt,
                "Process the following selected forge review comments",
            ),
        ]));
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Resolving 3 review comments...".to_string()),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::ReviewCommentResolution,
            turn_position: None,
        });

        // Act
        let output = output_lines(&session, 80, Some("Inspecting files"), None);
        let rendered_text = output
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered_text.contains("initial request"));
        assert!(rendered_text.contains("initial answer"));
        assert!(rendered_text.contains("Resolving 3 review comments..."));
        assert!(!rendered_text.contains("Process the following"));
        assert!(!rendered_text.contains("Inspecting files"));
        assert!(output.active_loader_line_index.is_some());
        assert_eq!(output.transient_loader_line_index, None);
    }

    #[test]
    fn test_review_comment_resolution_loader_ignores_non_loading_body() {
        // Arrange
        let mut session = crate::test_support::SessionFixtureBuilder::new().build();
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Plain("not loading".to_string()),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::ReviewCommentResolution,
            turn_position: None,
        });

        // Act
        let loading_message = review_comment_resolution_loading_message(&session);

        // Assert
        assert_eq!(loading_message, None);
    }

    #[test]
    fn test_queued_entries_follow_shared_submission_order() {
        // Arrange
        let mut lines = vec![Line::from("Active turn")];
        let transient_messages = vec![
            TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Queued(QueuedAction::new(
                    0,
                    "sync after this turn".to_string(),
                )),
                lifecycle:
                    crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::SyncQueue,
                turn_position: Some(1),
            },
            TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body: TransientMessageBody::Queued(QueuedAction::new(
                    2,
                    "publish review request".to_string(),
                )),
                lifecycle:
                    crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::BranchPublish,
                turn_position: Some(1),
            },
        ];
        let queued_messages = vec![queued_message(1, "follow up")];

        // Act
        let queued_line_indices =
            append_queued_entries(&mut lines, &transient_messages, &queued_messages);

        // Assert
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Active turn",
                "",
                "≡ sync after this turn",
                "≡ queued › follow up",
                "≡ publish review request",
                "",
            ]
        );
        assert_eq!(queued_line_indices, [2, 3, 4]);
    }

    #[test]
    fn test_blank_user_prompt_does_not_add_output_lines() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        append_user_prompt(&mut lines, " \n\t", 80, None);

        // Assert
        assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
    }

    #[test]
    fn test_user_prompt_highlights_at_lookup_file() {
        // Arrange
        let mut lines = Vec::new();
        let lookup = "@crates/agentty/src/ui/markdown.rs";

        // Act
        append_user_prompt(
            &mut lines,
            &format!("Review {lookup} before replying"),
            80,
            None,
        );

        // Assert
        let lookup_span = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.as_ref() == lookup)
            .expect("file lookup should render as one highlighted span");
        assert_eq!(lookup_span.style.fg, Some(style::palette::info()));
        assert_eq!(lookup_span.style.bg, Some(style::palette::surface_prompt()));
    }

    #[test]
    fn test_zero_width_user_prompt_does_not_add_output_lines() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        append_user_prompt(&mut lines, "\u{200b}", 80, None);

        // Assert
        assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
    }

    #[test]
    fn test_zero_width_markdown_does_not_add_output_lines() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        append_markdown_lines(&mut lines, "\u{200b}", 80, None);

        // Assert
        assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
    }

    #[test]
    fn test_protected_prompt_falls_back_when_private_use_is_exhausted() {
        // Arrange
        let prompt_text = [
            0xe000..=0xf8ff,
            0x000f_0000..=0x000f_fffd,
            0x0010_0000..=0x0010_fffd,
        ]
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .collect::<String>();

        // Act
        let (protected_text, indent_marker) = protect_user_prompt_indentation(&prompt_text);

        // Assert
        assert_eq!(indent_marker, None);
        assert_eq!(protected_text, prompt_text);
    }

    #[test]
    fn test_output_lines_places_queued_messages_after_active_turn() {
        // Arrange
        let mut session = crate::test_support::SessionFixtureBuilder::new()
            .status(Status::InProgress)
            .build();
        session.transcript = Some(SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "first prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "first answer"),
            SessionMessage::conversation(2, SessionMessageKind::UserPrompt, "active prompt"),
        ]));
        session.queued_messages = vec![queued_message(0, "queued reply")];

        // Act
        let output = output_lines(&session, 80, None, None)
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        let active_prompt_index = output
            .find("active prompt")
            .expect("active prompt should be rendered");
        let queued_reply_index = output
            .find("queued › queued reply")
            .expect("queued reply should be rendered");

        assert!(active_prompt_index < queued_reply_index);
    }
}
