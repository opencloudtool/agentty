use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::model::{Agent, AppMode};

pub fn render(f: &mut Frame, mode: &AppMode, agents: &[Agent], table_state: &mut TableState) {
    let area = f.area();

    match mode {
        AppMode::List => {
            let chunks = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .margin(2)
                .split(area);

            let main_area = chunks[0];
            let footer_area = chunks[1];

            // 1. Render Main Area (List or Welcome Hint)
            if agents.is_empty() {
                let vertical_chunks = Layout::default()
                    .constraints([
                        Constraint::Min(0),
                        Constraint::Length(5),
                        Constraint::Min(0),
                    ])
                    .split(main_area);

                let horizontal_chunks = Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        Constraint::Min(2),
                        Constraint::Percentage(80),
                        Constraint::Min(2),
                    ])
                    .split(vertical_chunks[1]);

                let hint = Paragraph::new(vec![
                    Line::from("Welcome to Agent Manager!"),
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
                        .title(" AM "),
                );
                f.render_widget(hint, horizontal_chunks[1]);
            } else {
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
                let t = Table::new(rows, [Constraint::Min(20), Constraint::Length(10)])
                    .header(header)
                    .block(Block::default().borders(Borders::ALL).title("Agents"))
                    .row_highlight_style(selected_style)
                    .highlight_symbol(">> ");

                f.render_stateful_widget(t, main_area, table_state);
            }

            let help_message = Paragraph::new("q: quit | a: add | j/k: nav")
                .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, footer_area);
        }
        AppMode::Prompt { input } => {
            // Define horizontal layout first to get inner width
            let horizontal_chunks = Layout::default()
                .direction(ratatui::layout::Direction::Horizontal)
                .constraints([
                    Constraint::Min(2),
                    Constraint::Percentage(80),
                    Constraint::Min(2),
                ])
                .split(area);

            let input_width = horizontal_chunks[1].width;
            let inner_width = input_width.saturating_sub(2);

            // Calculate exact wrapping
            let prefix = " › ";
            let prefix_len = u16::try_from(prefix.len()).unwrap_or(3);
            let total_chars = u16::try_from(input.len())
                .unwrap_or(u16::MAX)
                .saturating_add(prefix_len);

            // num_lines is the number of lines required.
            // We account for the cursor at the end by using total_chars + 1 for height
            // calculation.
            let num_lines = if inner_width > 0 {
                (total_chars + 1).div_ceil(inner_width)
            } else {
                1
            };

            let box_height = num_lines.saturating_add(2); // +2 for borders

            let vertical_chunks = Layout::default()
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(box_height),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(area);

            let input_area = Layout::default()
                .direction(ratatui::layout::Direction::Horizontal)
                .constraints([
                    Constraint::Min(2),
                    Constraint::Percentage(80),
                    Constraint::Min(2),
                ])
                .split(vertical_chunks[1])[1];

            let prompt_line = Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(input),
            ]);

            let input_widget = Paragraph::new(prompt_line)
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(Span::styled(
                            " New Agent Prompt ",
                            Style::default().fg(Color::Cyan),
                        )),
                );
            f.render_widget(input_widget, input_area);

            let help_message = Paragraph::new("Enter: confirm | Esc: cancel")
                .style(Style::default().fg(Color::Gray))
                .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(help_message, vertical_chunks[2]);

            // Set cursor position
            if let Some(cursor_y) = total_chars.checked_div(inner_width) {
                let cursor_x = total_chars % inner_width;

                f.set_cursor_position((
                    input_area.x.saturating_add(1).saturating_add(cursor_x),
                    input_area.y.saturating_add(1).saturating_add(cursor_y),
                ));
            }
        }
    }
}
