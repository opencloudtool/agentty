use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::model::InputState;
use crate::ui::components::chat_input::ChatInput;
use crate::ui::util::calculate_input_height;
use crate::ui::{Component, Page};

pub struct NewSessionPage<'a> {
    input: &'a InputState,
}

impl<'a> NewSessionPage<'a> {
    pub fn new(input: &'a InputState) -> Self {
        Self { input }
    }
}

impl Page for NewSessionPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let input_height = calculate_input_height(area.width.saturating_sub(2), self.input.text());
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(input_height)])
            .margin(1)
            .split(area);

        // Top area (chunks[0]) remains empty for "New Chat" feel
        ChatInput::new(
            " New Chat ",
            self.input.text(),
            self.input.cursor,
            "Type your message",
        )
        .render(f, chunks[1]);
    }
}
