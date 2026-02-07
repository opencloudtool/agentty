use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::ui::components::chat_input::ChatInput;
use crate::ui::util::calculate_input_height;
use crate::ui::{Component, Page};

pub struct NewSessionPage<'a> {
    pub input: &'a str,
}

impl<'a> NewSessionPage<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input }
    }
}

impl Page for NewSessionPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let input_height = calculate_input_height(area.width.saturating_sub(2), self.input);
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(input_height)])
            .margin(1)
            .split(area);

        // Top area (chunks[0]) remains empty for "New Chat" feel
        ChatInput::new(" New Chat ", self.input).render(f, chunks[1]);
    }
}
