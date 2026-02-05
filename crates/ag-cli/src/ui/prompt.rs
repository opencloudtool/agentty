use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::ui::components::render_chat_input;
use crate::ui::util::calculate_input_height;

pub fn render(f: &mut Frame, area: Rect, input: &str) {
    let input_height = calculate_input_height(area.width.saturating_sub(2), input);
    let chunks = Layout::default()
        .constraints([Constraint::Min(0), Constraint::Length(input_height)])
        .margin(1)
        .split(area);

    // Top area (chunks[0]) remains empty for "New Chat" feel
    render_chat_input(f, " New Chat ", input, chunks[1]);
}
