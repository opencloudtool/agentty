use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::app::setting::SettingsManager;
use crate::ui::state::help_action;
use crate::ui::{Page, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
/// Horizontal spacing between settings-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;

/// Renders the settings page table and inline editing hints.
pub struct SettingsPage<'a> {
    manager: &'a mut SettingsManager,
}

impl<'a> SettingsPage<'a> {
    /// Creates a settings page renderer bound to a mutable setting manager.
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

        let selected_style = Style::default().bg(style::palette::SURFACE);
        let header_style = Style::default()
            .bg(style::palette::SURFACE)
            .fg(style::palette::TEXT_MUTED)
            .add_modifier(Modifier::BOLD);
        let header_cells = ["Setting", "Value"].iter().map(|h| Cell::from(*h));
        let header = Row::new(header_cells)
            .style(header_style)
            .height(1)
            .bottom_margin(1);

        let rows = self
            .manager
            .settings_rows()
            .into_iter()
            .map(|(setting_name, setting_value)| {
                let row_line_count = setting_value.lines().count().max(1);
                let row_height = u16::try_from(row_line_count).unwrap_or(u16::MAX);

                Row::new(vec![Cell::from(setting_name), Cell::from(setting_value)])
                    .height(row_height)
            })
            .collect::<Vec<_>>();

        let table = Table::new(
            rows,
            [Constraint::Percentage(50), Constraint::Percentage(50)],
        )
        .column_spacing(TABLE_COLUMN_SPACING)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Settings"))
        .row_highlight_style(selected_style)
        .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        f.render_stateful_widget(table, main_area, &mut self.manager.table_state);

        let footer = Paragraph::new(settings_footer_line(self.manager));

        f.render_widget(footer, chunks[1]);
    }
}

/// Returns the footer help content for settings mode.
///
/// Inline text editing keeps using the manager-provided hint string, while
/// list mode uses the shared styled help-action rendering.
fn settings_footer_line(manager: &SettingsManager) -> Line<'static> {
    if manager.is_editing_text_input() {
        return Line::from(manager.footer_hint().to_string());
    }

    let actions = help_action::settings_footer_actions();

    help_action::footer_line(&actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_row_highlight_symbol_uses_background_only_selection() {
        // Arrange
        let highlight_symbol = ROW_HIGHLIGHT_SYMBOL;

        // Act
        let is_empty_symbol = highlight_symbol.is_empty();

        // Assert
        assert!(is_empty_symbol);
    }

    #[test]
    fn test_settings_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }
}
