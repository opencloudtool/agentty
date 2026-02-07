use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::model::{AppMode, Session, Status};
use crate::ui::components::chat_input::ChatInput;
use crate::ui::util::{calculate_input_height, wrap_lines};
use crate::ui::{Component, Page};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct SessionChatPage<'a> {
    pub sessions: &'a [Session],
    pub session_index: usize,
    pub scroll_offset: Option<u16>,
    pub mode: &'a AppMode,
}

impl<'a> SessionChatPage<'a> {
    pub fn new(
        sessions: &'a [Session],
        session_index: usize,
        scroll_offset: Option<u16>,
        mode: &'a AppMode,
    ) -> Self {
        Self {
            sessions,
            session_index,
            scroll_offset,
            mode,
        }
    }
}

impl Page for SessionChatPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        if let Some(session) = self.sessions.get(self.session_index) {
            let bottom_height = if let AppMode::Reply { input, .. } = self.mode {
                calculate_input_height(area.width.saturating_sub(2), input)
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
            let status_label = match status {
                Status::InProgress => "In Progress",
                Status::Done => "Done",
            };
            let title = format!(" {} — {} ", session.name, status_label);

            let output_text = session
                .output
                .lock()
                .map(|buf| buf.clone())
                .unwrap_or_default();

            let inner_width = output_area.width.saturating_sub(2) as usize;
            let mut lines = wrap_lines(&output_text, inner_width);

            if status == Status::InProgress {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let frame_idx = (now / 100) as usize % SPINNER_FRAMES.len();
                let spinner = SPINNER_FRAMES[frame_idx];

                if let Some(last) = lines.last() {
                    if last.width() == 0 {
                        lines.pop();
                    }
                }

                lines.push(Line::from(vec![Span::styled(
                    format!("{spinner} Thinking..."),
                    Style::default().fg(Color::Yellow),
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

            if let AppMode::Reply { input, .. } = self.mode {
                ChatInput::new(" Reply ", input).render(f, bottom_area);
            } else {
                let help_message = Paragraph::new(
                    "q: back | r: reply | d: diff | c: commit | m: merge | j/k: scroll",
                )
                .style(Style::default().fg(Color::Gray));
                f.render_widget(help_message, bottom_area);
            }
        }
    }
}
