use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::agent::AgentKind;
use crate::ui::util::compute_input_layout;

pub fn render_status_bar(f: &mut Frame, area: Rect, agent_kind: AgentKind) {
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
    let padding = area
        .width
        .saturating_sub(left_width.saturating_add(right_width));
    let status_bar = Paragraph::new(Line::from(vec![
        left_text,
        Span::raw(" ".repeat(padding as usize)),
        Span::styled(right_text, Style::default().fg(Color::Gray)),
    ]))
    .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(status_bar, area);
}

pub fn render_chat_input(f: &mut Frame, title: &str, input: &str, area: Rect) {
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
