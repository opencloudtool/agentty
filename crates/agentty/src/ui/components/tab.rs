use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::app::Tab;
use crate::domain::project::ProjectListItem;
use crate::ui::Component;

/// Header tabs rendered at the top of list mode pages.
pub struct Tabs<'a> {
    active_project_id: i64,
    current_tab: Tab,
    projects: &'a [ProjectListItem],
}

impl Tabs<'_> {
    /// Creates a tabs component with the provided active tab and project
    /// context used to label the sessions tab.
    pub fn new(current_tab: Tab, active_project_id: i64, projects: &[ProjectListItem]) -> Tabs<'_> {
        Tabs {
            active_project_id,
            current_tab,
            projects,
        }
    }
}

impl Component for Tabs<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let line = Line::from(tab_spans(
            self.current_tab,
            self.active_project_id,
            self.projects,
        ));
        let paragraph = Paragraph::new(line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .padding(Padding::top(1)),
        );
        f.render_widget(paragraph, area);
    }
}

/// Returns styled tab spans with a project-qualified sessions tab label.
fn tab_spans(
    current_tab: Tab,
    active_project_id: i64,
    projects: &[ProjectListItem],
) -> Vec<Span<'static>> {
    [Tab::Projects, Tab::Sessions, Tab::Stats, Tab::Settings]
        .iter()
        .map(|tab| {
            let label = format!(" {} ", tab_label(*tab, active_project_id, projects));
            if *tab == current_tab {
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(label, Style::default().fg(Color::Gray))
            }
        })
        .collect()
}

/// Returns the display label for a top-level tab.
fn tab_label(tab: Tab, active_project_id: i64, projects: &[ProjectListItem]) -> String {
    if tab == Tab::Sessions {
        return sessions_tab_title(active_project_id, projects);
    }

    tab.title().to_string()
}

/// Returns the sessions tab label with the selected project name when present.
fn sessions_tab_title(active_project_id: i64, projects: &[ProjectListItem]) -> String {
    projects
        .iter()
        .find(|project_item| project_item.project.id == active_project_id)
        .map_or_else(
            || Tab::Sessions.title().to_string(),
            |project_item| format!("Sessions ({})", project_item.project.display_label()),
        )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::project::Project;

    #[test]
    fn test_tab_spans_use_equal_spacing_between_labels() {
        // Arrange
        let current_tab = Tab::Projects;

        // Act
        let spans = tab_spans(current_tab, 0, &[]);
        let rendered_tabs: String = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        // Assert
        assert_eq!(rendered_tabs, " Projects  Sessions  Stats  Settings ");
    }

    #[test]
    fn test_tab_spans_highlight_the_active_tab() {
        // Arrange
        let current_tab = Tab::Stats;

        // Act
        let spans = tab_spans(current_tab, 0, &[]);

        // Assert
        assert_eq!(spans[0].style.fg, Some(Color::Gray));
        assert_eq!(spans[1].style.fg, Some(Color::Gray));
        assert_eq!(spans[2].style.fg, Some(Color::Yellow));
        assert_eq!(spans[3].style.fg, Some(Color::Gray));
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_tab_spans_include_selected_project_name_in_sessions_label() {
        // Arrange
        let current_tab = Tab::Sessions;
        let projects = vec![
            project_list_item(7, Some("Primary"), "/tmp/primary"),
            project_list_item(8, Some("Secondary"), "/tmp/secondary"),
        ];

        // Act
        let spans = tab_spans(current_tab, 7, &projects);
        let rendered_tabs: String = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        // Assert
        assert_eq!(
            rendered_tabs,
            " Projects  Sessions (Primary)  Stats  Settings "
        );
        assert_eq!(spans[1].style.fg, Some(Color::Yellow));
    }

    /// Creates a `ProjectListItem` for tab-label rendering tests.
    fn project_list_item(id: i64, display_name: Option<&str>, path: &str) -> ProjectListItem {
        ProjectListItem {
            last_session_updated_at: None,
            project: Project {
                created_at: 0,
                display_name: display_name.map(std::string::ToString::to_string),
                git_branch: None,
                id,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from(path),
                updated_at: 0,
            },
            session_count: 0,
        }
    }
}
