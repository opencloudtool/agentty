use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::ui::Screen;
use crate::ui::components::render_chat_input;
use crate::ui::util::calculate_input_height;

pub struct PromptScreen<'a> {
    pub input: &'a str,
}

impl<'a> PromptScreen<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input }
    }
}

impl Screen for PromptScreen<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let input_height = calculate_input_height(area.width.saturating_sub(2), self.input);
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(input_height)])
            .margin(1)
            .split(area);

        // Top area (chunks[0]) remains empty for "New Chat" feel
        render_chat_input(f, " New Chat ", self.input, chunks[1]);
    }
}
