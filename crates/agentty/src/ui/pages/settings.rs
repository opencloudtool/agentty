use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::app::settings::SettingsManager;
use crate::ui::Page;
use crate::ui::state::help_action;

/// Renders the settings page table and inline editing hints.
pub struct SettingsPage<'a> {
    manager: &'a mut SettingsManager,
}

impl<'a> SettingsPage<'a> {
    /// Creates a settings page renderer bound to a mutable settings manager.
    pub fn new(manager: &'a mut SettingsManager) -> Self {
        Self { manager }
    }
}

impl Page for SettingsPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        // Footer area can be used for help text later

        let selected_style = Style::default().bg(Color::DarkGray);
        let normal_style = Style::default().bg(Color::Gray).fg(Color::Black);
        let header_cells = ["Setting", "Value"].iter().map(|h| Cell::from(*h));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);

        let rows = self
            .manager
            .settings_rows()
            .into_iter()
            .map(|(setting_name, setting_value)| {
                Row::new(vec![Cell::from(setting_name), Cell::from(setting_value)]).height(1)
            })
            .collect::<Vec<_>>();

        let table = Table::new(
            rows,
            [Constraint::Percentage(50), Constraint::Percentage(50)],
        )
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Settings"))
        .row_highlight_style(selected_style)
        .highlight_symbol(">> ");

        f.render_stateful_widget(table, main_area, &mut self.manager.table_state);

        let footer = Paragraph::new(settings_footer_text(self.manager));

        f.render_widget(footer, chunks[1]);
    }
}

/// Returns the footer help text for settings mode.
fn settings_footer_text(manager: &SettingsManager) -> String {
    if manager.is_editing_text_input() {
        return manager.footer_hint().to_string();
    }

    let actions = help_action::settings_footer_actions();

    help_action::footer_text(&actions)
}
