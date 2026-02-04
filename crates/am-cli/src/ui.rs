use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
};

use crate::model::{Agent, AppMode};

pub fn render(f: &mut Frame, mode: &AppMode, agents: &[Agent], table_state: &mut TableState) {
    let area = f.area();
    match mode {
        AppMode::List => {
            let rects = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .margin(2)
                .split(area);

            let selected_style = Style::default().bg(Color::DarkGray);
            let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
            let header_cells = ["Agent Name", "Status"].iter().map(|h| Cell::from(*h));
            let header = Row::new(header_cells)
                .style(normal_style)
                .height(1)
                .bottom_margin(1);
            let rows = agents.iter().map(|agent| {
                let cells = vec![
                    Cell::from(agent.name.as_str()),
                    Cell::from(agent.status.icon())
                        .style(Style::default().fg(agent.status.color())),
                ];
                Row::new(cells).height(1)
            });
            let t = Table::new(
                rows,
                [Constraint::Percentage(50), Constraint::Percentage(50)],
            )
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Agents"))
            .row_highlight_style(selected_style)
            .highlight_symbol(">> ");

            f.render_stateful_widget(t, rects[0], table_state);

            let help_message = ratatui::widgets::Paragraph::new(
                "Press 'a' to add agent, 'j'/'k' to navigate, 'q' to quit",
            )
            .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, rects[1]);
        }
        AppMode::Prompt { input } => {
            let rects = Layout::default()
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .margin(2)
                .split(area);

            let input_widget = ratatui::widgets::Paragraph::new(input.as_str()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("New Agent Name"),
            );
            f.render_widget(input_widget, rects[0]);

            let help_message =
                ratatui::widgets::Paragraph::new("Press 'Enter' to confirm, 'Esc' to cancel")
                    .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, rects[2]);
        }
    }
}
