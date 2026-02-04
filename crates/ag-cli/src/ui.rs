use std::cmp::Ordering;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::model::{Agent, AppMode};

fn centered_horizontal_layout(area: ratatui::layout::Rect) -> std::rc::Rc<[ratatui::layout::Rect]> {
    Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([
            Constraint::Min(2),
            Constraint::Percentage(80),
            Constraint::Min(2),
        ])
        .split(area)
}

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

                let horizontal_chunks = centered_horizontal_layout(vertical_chunks[1]);

                let hint = Paragraph::new(vec![
                    Line::from("Welcome to Agentty!"),
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
                let header_cells = ["Agent Name", "Folder", "Status"]
                    .iter()
                    .map(|h| Cell::from(*h));
                let header = Row::new(header_cells)
                    .style(normal_style)
                    .height(1)
                    .bottom_margin(1);
                let rows = agents.iter().map(|agent| {
                    let cells = vec![
                        Cell::from(agent.name.as_str()),
                        Cell::from(Span::styled(
                            agent.folder.display().to_string(),
                            Style::default().fg(Color::Cyan),
                        )),
                        Cell::from(agent.status.icon())
                            .style(Style::default().fg(agent.status.color())),
                    ];
                    Row::new(cells).height(1)
                });
                let t = Table::new(
                    rows,
                    [
                        Constraint::Length(15),
                        Constraint::Min(0),
                        Constraint::Max(6),
                        Constraint::Length(1),
                    ],
                )
                .header(header)
                .block(Block::default().borders(Borders::ALL).title("Agents"))
                .row_highlight_style(selected_style)
                .highlight_symbol(">> ");

                f.render_stateful_widget(t, main_area, table_state);
            }

            let help_message = Paragraph::new("q: quit | a: add | o: open | j/k: nav")
                .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, footer_area);
        }
        AppMode::Prompt { input } => {
            // First, determine horizontal layout to get available width
            let horizontal_chunks = centered_horizontal_layout(area);
            let input_width = horizontal_chunks[1].width;
            let inner_width = input_width.saturating_sub(2);

            // Manual wrapping logic to match Gemini/Claude CLI behavior
            let prefix = " › ";
            let prefix_len = u16::try_from(prefix.chars().count()).unwrap_or(0);

            let mut display_lines = Vec::new();
            let mut cursor_x = 0;
            let mut cursor_y = 0;

            if inner_width > prefix_len {
                let first_line_max_input = (inner_width - prefix_len) as usize;
                let input_chars: Vec<char> = input.chars().collect();

                // First line contains prefix + part of input
                let first_line_part: String =
                    input_chars.iter().take(first_line_max_input).collect();
                let first_line_part_len =
                    u16::try_from(first_line_part.chars().count()).unwrap_or(0);

                display_lines.push(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(first_line_part),
                ]));

                match input_chars.len().cmp(&first_line_max_input) {
                    Ordering::Less => {
                        cursor_x = prefix_len + u16::try_from(input_chars.len()).unwrap_or(0);
                        cursor_y = 0;
                    }
                    Ordering::Equal => {
                        cursor_x = prefix_len + first_line_part_len;
                        cursor_y = 0;
                        if cursor_x >= inner_width {
                            cursor_x = 0;
                            cursor_y = 1;
                        }
                    }
                    Ordering::Greater => {
                        let remaining_input = &input_chars[first_line_max_input..];
                        for (i, chunk) in remaining_input.chunks(inner_width as usize).enumerate() {
                            display_lines.push(Line::from(chunk.iter().collect::<String>()));
                            if chunk.len() < inner_width as usize {
                                cursor_x = u16::try_from(chunk.len()).unwrap_or(0);
                                cursor_y = u16::try_from(i + 1).unwrap_or(0);
                            } else if chunk.len() == inner_width as usize {
                                cursor_x = 0;
                                cursor_y = u16::try_from(i + 2).unwrap_or(0);
                            }
                        }
                    }
                }
            } else {
                display_lines.push(Line::from(prefix));
                display_lines.push(Line::from(input.as_str()));
                cursor_y = 1;
                cursor_x = u16::try_from(input.chars().count()).unwrap_or(0);
            }

            let box_height = (cursor_y + 1).saturating_add(2);

            let vertical_chunks = Layout::default()
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(box_height),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(area);

            let input_area = centered_horizontal_layout(vertical_chunks[1])[1];

            let input_widget = Paragraph::new(display_lines).block(
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
            f.set_cursor_position((
                input_area.x.saturating_add(1).saturating_add(cursor_x),
                input_area.y.saturating_add(1).saturating_add(cursor_y),
            ));
        }
    }
}
