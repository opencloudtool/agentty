//! Helpers for parsing and formatting roadmap-backed task content.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::style;
use crate::ui::text_util::wrap_styled_line;

/// Supported roadmap queues rendered in the `Tasks` page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoadmapQueue {
    /// Fully expanded work ready for immediate execution.
    ReadyNow,
    /// Compact next-up work waiting for promotion.
    QueuedNext,
    /// Strategic backlog intentionally kept out of the active queue.
    Parked,
}

impl RoadmapQueue {
    /// Returns the queues in the rendered order.
    const ALL: [Self; 3] = [Self::ReadyNow, Self::QueuedNext, Self::Parked];

    /// Returns the exact roadmap section heading for one queue.
    fn title(self) -> &'static str {
        match self {
            Self::ReadyNow => "Ready Now",
            Self::QueuedNext => "Queued Next",
            Self::Parked => "Parked",
        }
    }

    /// Returns the accent color used for one queue heading.
    fn color(self) -> ratatui::style::Color {
        match self {
            Self::ReadyNow => style::palette::WARNING,
            Self::QueuedNext => style::palette::ACCENT,
            Self::Parked => style::palette::TEXT_MUTED,
        }
    }
}

/// Parsed roadmap overview ready for queue-by-queue rendering.
struct RoadmapOverview {
    /// Cards from the `Ready Now` section.
    ready_now: Vec<RoadmapCard>,
    /// Cards from the `Queued Next` section.
    queued_next: Vec<RoadmapCard>,
    /// Cards from the `Parked` section.
    parked: Vec<RoadmapCard>,
}

impl RoadmapOverview {
    /// Returns the cards for one queue.
    fn items(&self, queue: RoadmapQueue) -> &[RoadmapCard] {
        match queue {
            RoadmapQueue::ReadyNow => &self.ready_now,
            RoadmapQueue::QueuedNext => &self.queued_next,
            RoadmapQueue::Parked => &self.parked,
        }
    }
}

/// One rendered roadmap card extracted from markdown headings and subsections.
struct RoadmapCard {
    /// Optional assignee shown for `Ready Now` work.
    assignee: Option<String>,
    /// Optional short identifier extracted from the roadmap UUID heading.
    short_id: Option<String>,
    /// Compact extra detail line rendered under the outcome summary.
    detail: Option<String>,
    /// Outcome-style summary shown as the primary description.
    outcome: Option<String>,
    /// Stream prefix from the markdown heading.
    stream: String,
    /// Human-readable task title from the markdown heading.
    title: String,
}

/// One parsed subsection body inside a roadmap card.
struct RoadmapSubsection {
    /// Subsection title without markdown heading markers.
    title: String,
    /// Trimmed subsection body content.
    body: String,
}

/// Builds styled queue-and-card lines from one roadmap markdown document.
pub(crate) fn roadmap_task_lines(content: &str) -> Vec<Line<'static>> {
    let overview = parse_roadmap_overview(content);
    let mut lines = vec![
        Line::from(Span::styled(
            "Source: docs/plan/roadmap.md",
            Style::default().fg(style::palette::TEXT_MUTED),
        )),
        Line::from(vec![
            Span::styled(
                "Ready Now ",
                Style::default().fg(RoadmapQueue::ReadyNow.color()),
            ),
            Span::styled(
                overview.ready_now.len().to_string(),
                Style::default()
                    .fg(style::palette::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "Queued Next ",
                Style::default().fg(RoadmapQueue::QueuedNext.color()),
            ),
            Span::styled(
                overview.queued_next.len().to_string(),
                Style::default()
                    .fg(style::palette::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("Parked ", Style::default().fg(RoadmapQueue::Parked.color())),
            Span::styled(
                overview.parked.len().to_string(),
                Style::default()
                    .fg(style::palette::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    for queue in RoadmapQueue::ALL {
        lines.push(Line::default());
        lines.push(queue_header_line(queue, overview.items(queue).len()));

        if overview.items(queue).is_empty() {
            lines.push(indented_line(
                "No roadmap items.",
                Style::default().fg(style::palette::TEXT_MUTED),
            ));

            continue;
        }

        for card in overview.items(queue) {
            lines.extend(card_lines(queue, card));
        }
    }

    lines
}

/// Returns the maximum vertical scroll offset for rendered roadmap lines in
/// one viewport.
pub(crate) fn roadmap_task_max_scroll_offset(
    lines: &[Line<'static>],
    inner_width: u16,
    viewport_height: u16,
) -> u16 {
    if viewport_height == 0 {
        return 0;
    }

    let rendered_line_count = lines
        .iter()
        .map(|line| {
            u16::try_from(
                wrap_styled_line(line.spans.clone(), usize::from(inner_width.max(1))).len(),
            )
            .unwrap_or(u16::MAX)
        })
        .sum::<u16>();

    rendered_line_count.saturating_sub(viewport_height)
}

/// Parses one roadmap markdown document into queue cards.
fn parse_roadmap_overview(content: &str) -> RoadmapOverview {
    RoadmapOverview {
        ready_now: parse_queue_cards(
            &queue_section_body(content, RoadmapQueue::ReadyNow),
            RoadmapQueue::ReadyNow,
        ),
        queued_next: parse_queue_cards(
            &queue_section_body(content, RoadmapQueue::QueuedNext),
            RoadmapQueue::QueuedNext,
        ),
        parked: parse_queue_cards(
            &queue_section_body(content, RoadmapQueue::Parked),
            RoadmapQueue::Parked,
        ),
    }
}

/// Returns the raw markdown body for one roadmap queue section.
fn queue_section_body(content: &str, queue: RoadmapQueue) -> String {
    let mut body_lines = Vec::new();
    let mut is_in_queue = false;

    for line in content.lines() {
        if let Some(section_title) = line.strip_prefix("## ").map(str::trim) {
            is_in_queue = section_title == queue.title();

            continue;
        }

        if is_in_queue {
            body_lines.push(line.to_string());
        }
    }

    body_lines.join("\n")
}

/// Parses all cards inside one queue body.
fn parse_queue_cards(content: &str, queue: RoadmapQueue) -> Vec<RoadmapCard> {
    let mut items = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_body = Vec::new();

    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("### ").map(str::trim) {
            if let Some(previous_heading) = current_heading.replace(heading.to_string()) {
                let card_body = current_body.join("\n");
                items.push(parse_card(queue, &previous_heading, &card_body));
                current_body.clear();
            }

            continue;
        }

        if current_heading.is_some() {
            current_body.push(line.to_string());
        }
    }

    if let Some(heading) = current_heading {
        let card_body = current_body.join("\n");
        items.push(parse_card(queue, &heading, &card_body));
    }

    items
}

/// Parses one queue card from its heading and subsection markdown body.
fn parse_card(queue: RoadmapQueue, heading: &str, content: &str) -> RoadmapCard {
    let (short_id, stream, title) = parse_card_heading(heading);
    let subsections = parse_subsections(content);
    let assignee = subsection_body(&subsections, "Assignee").map(clean_inline_text);
    let outcome = match queue {
        RoadmapQueue::ReadyNow => subsection_body(&subsections, "Usable outcome"),
        RoadmapQueue::QueuedNext | RoadmapQueue::Parked => subsection_body(&subsections, "Outcome"),
    }
    .map(clean_inline_text);
    let detail = match queue {
        RoadmapQueue::ReadyNow => subsection_body(&subsections, "Substeps")
            .map(checklist_summary)
            .filter(|summary| !summary.is_empty())
            .map(|summary| format!("Substeps: {summary}")),
        RoadmapQueue::QueuedNext | RoadmapQueue::Parked => {
            let promote_when = subsection_body(&subsections, "Promote when")
                .map(clean_inline_text)
                .filter(|text| !text.is_empty())
                .map(|text| format!("Promote when: {text}"));
            let depends_on = subsection_body(&subsections, "Depends on")
                .map(clean_inline_text)
                .filter(|text| !text.is_empty())
                .map(|text| format!("Depends on: {text}"));

            promote_when.or(depends_on)
        }
    };

    RoadmapCard {
        assignee,
        short_id,
        detail,
        outcome,
        stream,
        title,
    }
}

/// Parses `[UUID] Stream: Title` into a short id, stream, and title.
fn parse_card_heading(heading: &str) -> (Option<String>, String, String) {
    let mut short_id = None;
    let mut remainder = heading.trim();

    if remainder.starts_with('[')
        && let Some(close_bracket_index) = remainder.find(']')
    {
        let id = &remainder[1..close_bracket_index];
        short_id = Some(id.chars().take(8).collect());
        remainder = remainder[(close_bracket_index + 1)..].trim();
    }

    if let Some(separator_index) = remainder.find(": ") {
        return (
            short_id,
            remainder[..separator_index].trim().to_string(),
            remainder[(separator_index + 2)..].trim().to_string(),
        );
    }

    (short_id, String::new(), remainder.to_string())
}

/// Parses `####` subsection blocks inside one roadmap card body.
fn parse_subsections(content: &str) -> Vec<RoadmapSubsection> {
    let mut subsections = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body = Vec::new();

    for line in content.lines() {
        if let Some(title) = line.strip_prefix("#### ").map(str::trim) {
            if let Some(previous_title) = current_title.replace(title.to_string()) {
                subsections.push(RoadmapSubsection {
                    title: previous_title,
                    body: current_body.join("\n").trim().to_string(),
                });
                current_body.clear();
            }

            continue;
        }

        if current_title.is_some() {
            current_body.push(line.to_string());
        }
    }

    if let Some(title) = current_title {
        subsections.push(RoadmapSubsection {
            title,
            body: current_body.join("\n").trim().to_string(),
        });
    }

    subsections
}

/// Returns the body for one subsection title.
fn subsection_body<'a>(subsections: &'a [RoadmapSubsection], title: &str) -> Option<&'a str> {
    subsections
        .iter()
        .find(|subsection| subsection.title == title)
        .map(|subsection| subsection.body.as_str())
}

/// Collapses markdown-ish inline text into one compact display line.
fn clean_inline_text(content: &str) -> String {
    content
        .replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Collapses checklist bullets into one semicolon-delimited summary line.
fn checklist_summary(content: &str) -> String {
    content
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("- [ ] ")
                .or_else(|| line.trim().strip_prefix("- [x] "))
        })
        .map(clean_inline_text)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Builds the styled header line for one queue.
fn queue_header_line(queue: RoadmapQueue, item_count: usize) -> Line<'static> {
    let noun = if item_count == 1 { "item" } else { "items" };

    Line::from(vec![
        Span::styled(
            queue.title(),
            Style::default()
                .fg(queue.color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({item_count} {noun})"),
            Style::default().fg(style::palette::TEXT_MUTED),
        ),
    ])
}

/// Builds the rendered lines for one roadmap card.
fn card_lines(queue: RoadmapQueue, card: &RoadmapCard) -> Vec<Line<'static>> {
    let mut lines = vec![card_heading_line(card)];
    let metadata_line = card_metadata_line(card);
    if let Some(line) = metadata_line {
        lines.push(line);
    }

    if let Some(outcome) = card.outcome.as_deref() {
        lines.push(indented_label_line("Outcome", outcome));
    }

    if let Some(detail) = card.detail.as_deref() {
        lines.push(indented_line(
            detail,
            Style::default().fg(style::palette::TEXT_MUTED),
        ));
    }

    if queue != RoadmapQueue::ReadyNow
        && card.detail.is_none()
        && let Some(card_id) = card.short_id.as_deref()
    {
        lines.push(indented_line(
            &format!("Roadmap id: {card_id}"),
            Style::default().fg(style::palette::TEXT_MUTED),
        ));
    }

    lines.push(Line::default());

    lines
}

/// Builds the styled bullet heading line for one roadmap card.
fn card_heading_line(card: &RoadmapCard) -> Line<'static> {
    let mut spans = vec![
        Span::raw("• "),
        Span::styled(
            card.stream.clone(),
            Style::default()
                .fg(style::palette::ACCENT_SOFT)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if !card.stream.is_empty() {
        spans.push(Span::raw(": "));
    }

    spans.push(Span::styled(
        card.title.clone(),
        Style::default()
            .fg(style::palette::TEXT)
            .add_modifier(Modifier::BOLD),
    ));

    Line::from(spans)
}

/// Builds the optional metadata line for one roadmap card.
fn card_metadata_line(card: &RoadmapCard) -> Option<Line<'static>> {
    let mut segments = Vec::new();

    if let Some(short_id) = card.short_id.as_deref() {
        segments.push(format!("[{short_id}]"));
    }

    if let Some(assignee) = card.assignee.as_deref() {
        segments.push(assignee.to_string());
    }

    if segments.is_empty() {
        return None;
    }

    Some(indented_line(
        &segments.join("  "),
        Style::default().fg(style::palette::TEXT_MUTED),
    ))
}

/// Builds one indented label-and-text line.
fn indented_label_line(label: &str, content: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{label}: "),
            Style::default().fg(style::palette::TEXT_MUTED),
        ),
        Span::raw(content.to_string()),
    ])
}

/// Builds one indented text-only line.
fn indented_line(content: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(content.to_string(), style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Converts styled lines to plain text for stable assertions.
    fn line_text(lines: Vec<Line<'static>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn roadmap_task_lines_include_all_queue_summaries() {
        // Arrange
        let roadmap = r"
## Ready Now
### [ca014af3-5cd0-4567-bf11-3495765dcf6f] Forge: Add GitHub pull-request publish shortcut in session chat
#### Assignee
`@minev-dev`
#### Why now
Existing workflow pieces already exist.
#### Usable outcome
Pressing `Shift+P` creates or refreshes the pull request.
#### Substeps
- [ ] **Add a dedicated `Shift+P` session-chat action for GitHub review requests.**
- [ ] **Reuse the existing review-request workflow for GitHub sessions.**
#### Tests
- [ ] Cover the new shortcut.
#### Docs
- [ ] Update workflow docs.

## Queued Next
### [17a9e2ba-0b7d-407d-9cd4-72807ef7bc1f] Delivery: Add project commit strategy selection
#### Outcome
Let each project stored in Agentty choose its expected landing path.
#### Promote when
Promote when maintainers want review and publish actions to respect project delivery flow.
#### Depends on
`[ca014af3] Forge`

## Parked
### [6bb0cae7-c07c-4fab-ae6b-e74444d3f0f0] Planning: Move roadmap tasks to a single canonical TOML plan
#### Outcome
Let Agentty manage roadmap tasks through one canonical `docs/plan/roadmap.toml` file.
#### Promote when
Promote when maintainers want direct task management in Agentty.
#### Depends on
`None`
";

        // Act
        let lines = line_text(roadmap_task_lines(roadmap));

        // Assert
        assert!(lines.iter().any(|line| line.contains("Ready Now 1")));
        assert!(lines.iter().any(|line| {
            line.contains("Forge: Add GitHub pull-request publish shortcut in session chat")
        }));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Substeps: Add a dedicated `Shift+P`"))
        );
        assert!(
            lines
                .iter()
                .any(|line| { line.contains("Delivery: Add project commit strategy selection") })
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Promote when: Promote when maintainers want"))
        );
        assert!(lines.iter().any(|line| {
            line.contains("Planning: Move roadmap tasks to a single canonical TOML plan")
        }));
    }

    #[test]
    fn roadmap_task_lines_show_empty_queue_state() {
        // Arrange
        let roadmap = "## Ready Now\n\n## Queued Next\n\n## Parked\n";

        // Act
        let lines = line_text(roadmap_task_lines(roadmap));

        // Assert
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("No roadmap items."))
                .count(),
            3
        );
    }

    #[test]
    fn roadmap_task_max_scroll_offset_counts_wrapped_lines() {
        // Arrange
        let lines = vec![Line::from("alpha beta gamma"), Line::from("delta epsilon")];

        // Act
        let max_scroll_offset = roadmap_task_max_scroll_offset(&lines, 5, 2);

        // Assert
        assert_eq!(max_scroll_offset, 3);
    }
}
