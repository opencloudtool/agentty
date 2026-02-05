use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::model::Tab;
use crate::ui::Component;

pub struct Tabs {
    current_tab: Tab,
}

impl Tabs {
    pub fn new(current_tab: Tab) -> Self {
        Self { current_tab }
    }
}

impl Component for Tabs {
    fn render(&self, f: &mut Frame, area: Rect) {
        let tabs = [Tab::Sessions, Tab::Roadmap];
        let tab_titles: Vec<_> = tabs
            .iter()
            .map(|tab| {
                let title = tab.title();
                if *tab == self.current_tab {
                    Span::styled(
                        format!(" {title} "),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(format!(" {title} "), Style::default().fg(Color::Gray))
                }
            })
            .collect();

        let line = Line::from(tab_titles);
        let paragraph = Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM));
        f.render_widget(paragraph, area);
    }
}
