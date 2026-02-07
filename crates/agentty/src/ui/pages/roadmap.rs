use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::Page;

pub struct RoadmapPage;

impl Page for RoadmapPage {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Roadmap")
            .style(Style::default().fg(Color::White));

        let paragraph = Paragraph::new("Hello world!").block(block);
        f.render_widget(paragraph, main_area);

        let help_message = Paragraph::new("q: quit").style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}
