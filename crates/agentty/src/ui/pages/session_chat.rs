use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::icon::Icon;
use crate::model::{AppMode, Session, Status};
use crate::ui::components::chat_input::ChatInput;
use crate::ui::util::{calculate_input_height, wrap_lines};
use crate::ui::{Component, Page};

pub struct SessionChatPage<'a> {
    pub mode: &'a AppMode,
    pub scroll_offset: Option<u16>,
    pub session_index: usize,
    pub sessions: &'a [Session],
}

impl<'a> SessionChatPage<'a> {
    pub fn new(
        sessions: &'a [Session],
        session_index: usize,
        scroll_offset: Option<u16>,
        mode: &'a AppMode,
    ) -> Self {
        Self {
            mode,
            scroll_offset,
            session_index,
            sessions,
        }
    }
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            let bottom_height = if let AppMode::Prompt { input, .. } = self.mode {
                calculate_input_height(area.width.saturating_sub(2), input.text())
            } else {
                1
            };

            let chunks = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
                .margin(1)
                .split(area);

            let output_area = chunks[0];
            let bottom_area = chunks[1];

            let status = session.status();
            let title = format!(" {} — {status} ", session.display_title());

            let output_text = session
                .output
                .lock()
                .map(|buf| buf.clone())
                .unwrap_or_default();

            let inner_width = output_area.width.saturating_sub(2) as usize;
            let mut lines = wrap_lines(&output_text, inner_width);

            if matches!(status, Status::InProgress | Status::PullRequest) {
                while lines.last().is_some_and(|last| last.width() == 0) {
                    lines.pop();
                }
                lines.push(Line::from(""));

                let msg = match status {
                    Status::New => "",
                    Status::InProgress => "Thinking...",
                    Status::Review => "",
                    Status::PullRequest => "Waiting for PR merge...",
                    Status::Done => "",
                };

                lines.push(Line::from(vec![Span::styled(
                    format!("{} {}", Icon::current_spinner(), msg),
                    Style::default().fg(status.color()),
                )]));
            }

            // Auto-scroll logic or manual override
            let final_scroll = if let Some(offset) = self.scroll_offset {
                offset
            } else {
                let inner_height = output_area.height.saturating_sub(2) as usize;
                u16::try_from(lines.len().saturating_sub(inner_height)).unwrap_or(u16::MAX)
            };

            let paragraph = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(status.color()))
                        .title(Span::styled(title, Style::default().fg(status.color()))),
                )
                .scroll((final_scroll, 0));

            f.render_widget(paragraph, output_area);

            if let AppMode::Prompt { input, .. } = self.mode {
                let title = if session.prompt.is_empty() {
                    " New Chat "
                } else {
                    " Reply "
                };
                ChatInput::new(title, input.text(), input.cursor, "Type your message")
                    .render(f, bottom_area);
            } else {
                let help_message = Paragraph::new(
                    "q: back | r: reply | d: diff | c: commit | p: pr | m: merge | j/k: scroll",
                )
                .style(Style::default().fg(Color::Gray));
                f.render_widget(help_message, bottom_area);
            }
        }
    }
}
