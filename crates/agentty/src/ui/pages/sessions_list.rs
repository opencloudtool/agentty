use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::model::Session;
use crate::ui::Page;
use crate::ui::util::centered_horizontal_layout;

pub struct SessionsListPage<'a> {
    pub sessions: &'a [Session],
    pub table_state: &'a mut TableState,
}

impl<'a> SessionsListPage<'a> {
    pub fn new(sessions: &'a [Session], table_state: &'a mut TableState) -> Self {
        Self {
            sessions,
            table_state,
        }
    }
}

impl Page for SessionsListPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        // 1. Render Main Area (List or Welcome Hint)
        if self.sessions.is_empty() {
            let vertical_chunks = Layout::default()
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(5),
                    Constraint::Min(0),
                ])
                .split(main_area);

            let horizontal_chunks = centered_horizontal_layout(vertical_chunks[1]);

            let title = Span::styled(
                " Agentty ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
            let hint = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Press 'a' to initiate",
                    Style::default().fg(Color::Cyan),
                )),
            ])
            .alignment(ratatui::layout::Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            );
            f.render_widget(hint, horizontal_chunks[1]);
        } else {
            let selected_style = Style::default().bg(Color::DarkGray);
            let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
            let header_cells = ["Session", "Status"].iter().map(|h| Cell::from(*h));
            let header = Row::new(header_cells)
                .style(normal_style)
                .height(1)
                .bottom_margin(1);
            let rows = self.sessions.iter().map(|session| {
                let status = session.status();
                let cells = vec![
                    Cell::from(format!("{} [{}]", session.name, session.agent)),
                    Cell::from(status.icon()).style(Style::default().fg(status.color())),
                ];
                Row::new(cells).height(1)
            });
            let t = Table::new(
                rows,
                [
                    Constraint::Min(0),
                    Constraint::Max(6),
                    Constraint::Length(1),
                ],
            )
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Sessions"))
            .row_highlight_style(selected_style)
            .highlight_symbol(">> ");

            f.render_stateful_widget(t, main_area, self.table_state);
        }

        let help_message =
            Paragraph::new("q: quit | a: add | d: delete | o: open | Enter: view | j/k: nav")
                .style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}
