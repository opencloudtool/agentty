use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::Page;

pub struct RoadmapPage;

impl Page for RoadmapPage {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Roadmap")
            .style(Style::default().fg(Color::White));

        let paragraph = Paragraph::new("").block(block);
        f.render_widget(paragraph, area);
    }
}
