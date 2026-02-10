use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::Page;

pub struct StatsPage;

impl Page for StatsPage {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Stats")
            .style(Style::default().fg(Color::White));

        let paragraph = Paragraph::new("Stats coming soon...").block(block);
        f.render_widget(paragraph, main_area);

        let help_message = Paragraph::new("q: quit").style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}
