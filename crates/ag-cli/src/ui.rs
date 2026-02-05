use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};

use crate::agent::AgentKind;
use crate::model::{AppMode, Session, Status};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render(
    f: &mut Frame,
    mode: &AppMode,
    sessions: &[Session],
    table_state: &mut TableState,
    agent_kind: AgentKind,
) {
    let area = f.area();

    // Top status bar (all modes)
    let outer_chunks = Layout::default()
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let status_bar_area = outer_chunks[0];
    let content_area = outer_chunks[1];

    let version = env!("CARGO_PKG_VERSION");
    let left_text = Span::styled(
        format!(" Agentty v{version}"),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let right_text = format!("Agent: {agent_kind} ");
    let left_width = u16::try_from(left_text.width()).unwrap_or(u16::MAX);
    let right_width = u16::try_from(right_text.len()).unwrap_or(u16::MAX);
    let padding = status_bar_area
        .width
        .saturating_sub(left_width.saturating_add(right_width));
    let status_bar = Paragraph::new(Line::from(vec![
        left_text,
        Span::raw(" ".repeat(padding as usize)),
        Span::styled(right_text, Style::default().fg(Color::Gray)),
    ]))
    .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(status_bar, status_bar_area);

    match mode {
        AppMode::List => {
            let chunks = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .margin(1)
                .split(content_area);

            let main_area = chunks[0];
            let footer_area = chunks[1];

            // 1. Render Main Area (List or Welcome Hint)
            if sessions.is_empty() {
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
                let rows = sessions.iter().map(|session| {
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

                f.render_stateful_widget(t, main_area, table_state);
            }

            let help_message =
                Paragraph::new("q: quit | a: add | d: delete | o: open | Enter: view | j/k: nav")
                    .style(Style::default().fg(Color::Gray));
            f.render_widget(help_message, footer_area);
        }
        AppMode::View {
            session_index,
            scroll_offset,
        }
        | AppMode::Reply {
            session_index,
            scroll_offset,
            ..
        } => {
            if let Some(session) = sessions.get(*session_index) {
                let bottom_height = if let AppMode::Reply { input, .. } = mode {
                    calculate_input_height(content_area.width.saturating_sub(2), input)
                } else {
                    1
                };

                let chunks = Layout::default()
                    .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
                    .margin(1)
                    .split(content_area);

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
                let final_scroll = if let Some(offset) = scroll_offset {
                    *offset
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

                if let AppMode::Reply { input, .. } = mode {
                    render_chat_input(f, " Reply ", input, bottom_area);
                } else {
                    let help_message = Paragraph::new("q: back | r: reply | j/k: scroll")
                        .style(Style::default().fg(Color::Gray));
                    f.render_widget(help_message, bottom_area);
                }
            }
        }
        AppMode::Prompt { input } => {
            let input_height = calculate_input_height(content_area.width.saturating_sub(2), input);
            let chunks = Layout::default()
                .constraints([Constraint::Min(0), Constraint::Length(input_height)])
                .margin(1)
                .split(content_area);

            // Top area (chunks[0]) remains empty for "New Chat" feel
            render_chat_input(f, " New Chat ", input, chunks[1]);
        }
    }
}

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

fn calculate_input_height(width: u16, input: &str) -> u16 {
    let (_, _, cursor_y) = compute_input_layout(input, width);
    cursor_y + 3
}

fn compute_input_layout(input: &str, width: u16) -> (Vec<Line<'static>>, u16, u16) {
    let inner_width = width.saturating_sub(2) as usize;
    let prefix = " › ";
    let prefix_span = Span::styled(
        prefix,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let prefix_width = prefix_span.width();

    let mut display_lines = Vec::new();
    let mut current_line_spans = vec![prefix_span];
    let mut current_width = prefix_width;

    let mut cursor_x: usize = current_width;
    let mut cursor_y: usize = 0;

    for c in input.chars() {
        let char_str = c.to_string();
        let char_span = Span::raw(char_str);
        let char_width = char_span.width();

        if current_width + char_width > inner_width {
            display_lines.push(Line::from(std::mem::take(&mut current_line_spans)));
            current_width = 0;
            cursor_y += 1;
        }

        current_line_spans.push(char_span);
        current_width += char_width;
        cursor_x = current_width;
    }

    if current_width >= inner_width {
        cursor_x = 0;
        cursor_y += 1;
    }

    if !current_line_spans.is_empty() {
        display_lines.push(Line::from(current_line_spans));
    }

    (
        display_lines,
        u16::try_from(cursor_x).unwrap_or(u16::MAX),
        u16::try_from(cursor_y).unwrap_or(u16::MAX),
    )
}

fn wrap_lines(text: &str, width: usize) -> Vec<Line<'_>> {
    let mut wrapped = Vec::new();
    for line in text.split('\n') {
        let mut current_line = String::new();
        let mut current_width = 0;

        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            if line.is_empty() {
                wrapped.push(Line::from(""));
            } else {
                // Preserves lines that are just whitespace if needed,
                // but usually split_whitespace eats them.
                // If the original line was just spaces, we might want an empty line or ignored.
                // For a chat log, empty lines (paragraphs) are important.
                wrapped.push(Line::from(""));
            }
            continue;
        }

        for word in words {
            let word_len = word.chars().count();
            let space_len = usize::from(current_width != 0);

            if current_width + space_len + word_len > width {
                if !current_line.is_empty() {
                    wrapped.push(Line::from(current_line));
                    current_line = String::new();
                    current_width = 0;
                }
            }

            if current_width > 0 {
                current_line.push(' ');
                current_width += 1;
            }
            current_line.push_str(word);
            current_width += word_len;
        }
        if !current_line.is_empty() {
            wrapped.push(Line::from(current_line));
        }
    }
    wrapped
}

fn render_chat_input(f: &mut Frame, title: &str, input: &str, area: ratatui::layout::Rect) {
    let (display_lines, cursor_x, cursor_y) = compute_input_layout(input, area.width);

    let input_widget = Paragraph::new(display_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(title, Style::default().fg(Color::Cyan))),
    );
    f.render_widget(Clear, area);
    f.render_widget(input_widget, area);

    // Set cursor position
    f.set_cursor_position((
        area.x.saturating_add(1).saturating_add(cursor_x),
        area.y.saturating_add(1).saturating_add(cursor_y),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_input_height() {
        // Arrange & Act & Assert
        assert_eq!(calculate_input_height(20, ""), 3);
        assert_eq!(calculate_input_height(12, "1234567"), 4);
        assert_eq!(calculate_input_height(12, "12345678"), 4);
        assert_eq!(calculate_input_height(12, "12345671234567890"), 5);
    }

    #[test]
    fn test_compute_input_layout_empty() {
        // Arrange
        let input = "";
        let width = 20;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width(), 3); // " › "
        assert_eq!(cursor_x, 3);
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_single_line() {
        // Arrange
        let input = "test";
        let width = 20;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_x, 7); // 3 (prefix) + 4 (text)
        assert_eq!(cursor_y, 0);
    }

    #[test]
    fn test_compute_input_layout_exact_fit() {
        // Arrange
        let input = "1234567";
        let width = 12; // Inner width 10, Prefix 3, Available 7

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width);

        // Assert
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(cursor_x, 0);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_wrap() {
        // Arrange
        let input = "12345678";
        let width = 12;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(input, width);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 1);
        assert_eq!(lines[1].to_string(), "8");
        assert_eq!(cursor_x, 1);
        assert_eq!(cursor_y, 1);
    }

    #[test]
    fn test_compute_input_layout_multiline_exact_fit() {
        // Arrange
        let input = "1234567".to_owned() + "1234567890";
        let width = 12;

        // Act
        let (lines, cursor_x, cursor_y) = compute_input_layout(&input, width);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 10);
        assert_eq!(lines[1].width(), 10);
        assert_eq!(cursor_x, 0);
        assert_eq!(cursor_y, 2);
    }

    #[test]
    fn test_wrap_lines_basic() {
        // Arrange
        let text = "hello world";
        let width = 20;

        // Act
        let wrapped = wrap_lines(text, width);

        // Assert
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].to_string(), "hello world");
    }

    #[test]
    fn test_wrap_lines_wrapping() {
        // Arrange
        let text = "hello world";
        let width = 5;

        // Act
        let wrapped = wrap_lines(text, width);

        // Assert
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].to_string(), "hello");
        assert_eq!(wrapped[1].to_string(), "world");
    }
}
