use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use uuid::Uuid;

const READY_NOW_MAX_STEPS: usize = 5;
const ROADMAP_PATH: &str = "docs/plan/roadmap.md";
const REQUIRED_SECTION_TITLES: [&str; 9] = [
    "Current State Snapshot",
    "Active Streams",
    "Planning Model",
    "Ready Now",
    "Ready Now Execution Order",
    "Queued Next",
    "Parked",
    "Context Notes",
    "Status Maintenance Rule",
];

/// Runs roadmap-structure validation against `docs/plan/roadmap.md`.
///
/// # Errors
/// Returns an error when the roadmap shape, section ordering, or queue rules
/// do not match the planning workflow.
pub(crate) fn lint() -> Result<(), String> {
    let content = fs::read_to_string(ROADMAP_PATH).map_err(|err| err.to_string())?;
    let roadmap = RoadmapDocument::parse(&content)?;

    validate_section_order(&roadmap)?;
    validate_unique_ids(&roadmap)?;
    validate_ready_now(&roadmap)?;
    validate_candidate_queue(&roadmap.queued_next, QueueKind::QueuedNext)?;
    validate_candidate_queue(&roadmap.parked, QueueKind::Parked)
}

/// Builds a context digest from the roadmap and read-only git state.
///
/// # Errors
/// Returns an error when the roadmap cannot be parsed or a required git query
/// fails.
pub(crate) fn context_digest() -> Result<String, String> {
    let content = fs::read_to_string(ROADMAP_PATH).map_err(|err| err.to_string())?;
    let roadmap = RoadmapDocument::parse(&content)?;
    let snapshot = GitSnapshot::load(&RealGitCommandRunner)?;

    Ok(render_context_digest(&roadmap, &snapshot))
}

/// Queue kinds supported by the roadmap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueKind {
    ReadyNow,
    QueuedNext,
    Parked,
}

impl QueueKind {
    /// Returns the section title used in the roadmap for this queue.
    fn section_title(self) -> &'static str {
        match self {
            Self::ReadyNow => "Ready Now",
            Self::QueuedNext => "Queued Next",
            Self::Parked => "Parked",
        }
    }

    /// Returns the subsection titles required for this queue.
    fn required_subsections(self) -> &'static [&'static str] {
        match self {
            Self::ReadyNow => &[
                "Assignee",
                "Why now",
                "Usable outcome",
                "Substeps",
                "Tests",
                "Docs",
            ],
            Self::QueuedNext | Self::Parked => &["Outcome", "Promote when", "Depends on"],
        }
    }
}

/// Parsed representation of the roadmap file.
struct RoadmapDocument {
    section_titles: Vec<String>,
    ready_now: Vec<RoadmapItem>,
    queued_next: Vec<RoadmapItem>,
    parked: Vec<RoadmapItem>,
}

impl RoadmapDocument {
    /// Parses the roadmap markdown into structured queue items.
    ///
    /// # Errors
    /// Returns an error when required sections are missing or queue items do
    /// not match the expected heading structure.
    fn parse(content: &str) -> Result<Self, String> {
        let sections = parse_sections(content);
        let section_titles = sections
            .iter()
            .map(|section| section.title.clone())
            .collect::<Vec<_>>();
        let ready_now = parse_queue(section_body(
            &sections,
            QueueKind::ReadyNow.section_title(),
        )?)?;
        let queued_next = parse_queue(section_body(
            &sections,
            QueueKind::QueuedNext.section_title(),
        )?)?;
        let parked = parse_queue(section_body(&sections, QueueKind::Parked.section_title())?)?;

        Ok(Self {
            section_titles,
            ready_now,
            queued_next,
            parked,
        })
    }
}

/// One top-level `##` section from the roadmap.
struct SectionBlock {
    title: String,
    body: String,
}

/// One parsed markdown heading with source offsets.
struct ParsedHeading {
    level: usize,
    title: String,
    start: usize,
    end: usize,
}

/// One roadmap item card or step.
struct RoadmapItem {
    id: Uuid,
    stream: String,
    title: String,
    subsections: Vec<RoadmapSubsection>,
}

impl RoadmapItem {
    /// Returns the heading rendered in roadmap markdown.
    fn heading(&self) -> String {
        format!("[{}] {}: {}", self.id, self.stream, self.title)
    }

    /// Returns the subsection titles in source order.
    fn subsection_titles(&self) -> Vec<&str> {
        self.subsections
            .iter()
            .map(|subsection| subsection.title.as_str())
            .collect()
    }

    /// Returns one subsection body by name.
    fn subsection_body(&self, title: &str) -> Option<&str> {
        self.subsections
            .iter()
            .find(|subsection| subsection.title == title)
            .map(|subsection| subsection.body.as_str())
    }
}

/// One `####` subsection inside a roadmap item.
struct RoadmapSubsection {
    title: String,
    body: String,
}

/// Read-only git data used for context digests.
struct GitSnapshot {
    branch: String,
    recent_commits: Vec<String>,
    status_lines: Vec<String>,
}

impl GitSnapshot {
    /// Loads the current branch, recent commits, and working tree status.
    ///
    /// # Errors
    /// Returns an error when one of the required git commands fails.
    fn load(runner: &dyn GitCommandRunner) -> Result<Self, String> {
        let branch = runner.run(git_args(["rev-parse", "--abbrev-ref", "HEAD"]))?;
        let recent_commits = runner.run(git_args(["log", "--oneline", "--max-count=5"]))?;
        let status = runner.run(git_args(["status", "--short"]))?;

        Ok(Self {
            branch: branch.trim().to_string(),
            recent_commits: non_empty_lines(&recent_commits),
            status_lines: non_empty_raw_lines(&status),
        })
    }
}

/// Runs read-only git commands for context-digest generation.
#[cfg_attr(test, mockall::automock)]
trait GitCommandRunner {
    /// Executes one git command and returns trimmed stdout on success.
    fn run(&self, args: Vec<String>) -> Result<String, String>;
}

/// Real git command runner used by the CLI.
struct RealGitCommandRunner;

impl GitCommandRunner for RealGitCommandRunner {
    /// Executes one git command and returns stdout.
    fn run(&self, args: Vec<String>) -> Result<String, String> {
        let output = Command::new("git")
            .args(&args)
            .output()
            .map_err(|err| err.to_string())?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(stderr.trim().to_string());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Parses top-level roadmap sections by `##` headings.
fn parse_sections(content: &str) -> Vec<SectionBlock> {
    heading_blocks(content, 2)
        .into_iter()
        .map(|heading| SectionBlock {
            title: heading.title,
            body: heading.body,
        })
        .collect()
}

/// Returns the raw body for one named roadmap section.
///
/// # Errors
/// Returns an error when the section is missing.
fn section_body<'a>(sections: &'a [SectionBlock], title: &str) -> Result<&'a str, String> {
    sections
        .iter()
        .find(|section| section.title == title)
        .map(|section| section.body.as_str())
        .ok_or_else(|| format!("Missing roadmap section `## {title}`"))
}

/// Parses all `###` item blocks inside one roadmap queue.
///
/// # Errors
/// Returns an error when one item heading or subsection block is invalid.
fn parse_queue(body: &str) -> Result<Vec<RoadmapItem>, String> {
    heading_blocks(body, 3)
        .into_iter()
        .map(|heading| parse_item(&heading))
        .collect()
}

/// Parses one roadmap item heading and subsection body.
///
/// # Errors
/// Returns an error when the heading is not `[UUID] Stream: Title` or when no
/// subsections are present.
fn parse_item(heading: &HeadingBlock) -> Result<RoadmapItem, String> {
    let (id, stream, title) = parse_heading(&heading.title)?;
    let subsections = parse_subsections(&heading.body);

    if subsections.is_empty() {
        return Err(format!(
            "Roadmap item `{}` has no subsections",
            heading.title
        ));
    }

    Ok(RoadmapItem {
        id,
        stream,
        title,
        subsections,
    })
}

/// Parses a `[UUID] Stream: Title` heading.
///
/// # Errors
/// Returns an error when the heading does not use the canonical roadmap format.
fn parse_heading(heading: &str) -> Result<(Uuid, String, String), String> {
    let Some(close_bracket) = heading.find(']') else {
        return Err(format!(
            "Roadmap item heading `{heading}` must start with `[UUID] Stream: Title`"
        ));
    };

    if !heading.starts_with('[') || heading.as_bytes().get(close_bracket + 1) != Some(&b' ') {
        return Err(format!(
            "Roadmap item heading `{heading}` must start with `[UUID] Stream: Title`"
        ));
    }

    let id = Uuid::parse_str(&heading[1..close_bracket])
        .map_err(|_| format!("Roadmap item heading `{heading}` uses an invalid UUID"))?;
    let remainder = &heading[(close_bracket + 2)..];
    let Some(colon_index) = remainder.find(": ") else {
        return Err(format!(
            "Roadmap item heading `{heading}` must include `Stream: Title`"
        ));
    };

    let stream = remainder[..colon_index].trim();
    let title = remainder[(colon_index + 2)..].trim();
    if stream.is_empty() || title.is_empty() {
        return Err(format!(
            "Roadmap item heading `{heading}` must include both a stream and a title"
        ));
    }

    Ok((id, stream.to_string(), title.to_string()))
}

/// Parses all `####` subsections from one item block.
fn parse_subsections(body: &str) -> Vec<RoadmapSubsection> {
    heading_blocks(body, 4)
        .into_iter()
        .map(|heading| RoadmapSubsection {
            title: heading.title,
            body: heading.body,
        })
        .collect()
}

/// One heading title paired with the markdown body that follows it.
struct HeadingBlock {
    title: String,
    body: String,
}

/// Parses markdown headings with their source ranges.
fn parse_headings(content: &str) -> Vec<ParsedHeading> {
    let mut headings = Vec::new();
    let mut current_level = None;
    let mut current_start = 0;
    let mut current_title = String::new();

    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_level = Some(heading_level_value(level));
                current_start = range.start;
                current_title.clear();
            }
            Event::Text(text) | Event::Code(text) if current_level.is_some() => {
                current_title.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if current_level.is_some() => {
                current_title.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = current_level.take() {
                    headings.push(ParsedHeading {
                        level,
                        title: current_title.trim().to_string(),
                        start: current_start,
                        end: range.end,
                    });
                    current_title.clear();
                }
            }
            _ => {}
        }
    }

    headings
}

/// Converts a pulldown-cmark heading level into a numeric depth.
fn heading_level_value(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Extracts heading blocks for one markdown heading level.
fn heading_blocks(content: &str, level: usize) -> Vec<HeadingBlock> {
    let headings = parse_headings(content);
    let mut blocks = Vec::new();

    for (index, heading) in headings.iter().enumerate() {
        if heading.level != level {
            continue;
        }

        let next_start = headings
            .iter()
            .skip(index + 1)
            .find(|next_heading| next_heading.level <= level)
            .map_or(content.len(), |next_heading| next_heading.start);
        let body = trim_block(&content[heading.end..next_start]);
        blocks.push(HeadingBlock {
            title: heading.title.clone(),
            body,
        });
    }

    blocks
}

/// Validates the required top-level roadmap section order.
///
/// # Errors
/// Returns an error when the roadmap section titles do not exactly match the
/// planning workflow.
fn validate_section_order(roadmap: &RoadmapDocument) -> Result<(), String> {
    let expected = REQUIRED_SECTION_TITLES
        .iter()
        .map(|title| (*title).to_string())
        .collect::<Vec<_>>();

    if roadmap.section_titles != expected {
        return Err(format!(
            "Roadmap sections must be exactly {:?}, found {:?}",
            REQUIRED_SECTION_TITLES, roadmap.section_titles
        ));
    }

    Ok(())
}

/// Validates that roadmap UUIDs are unique across all queues.
///
/// # Errors
/// Returns an error when one UUID is reused.
fn validate_unique_ids(roadmap: &RoadmapDocument) -> Result<(), String> {
    let mut seen = BTreeSet::new();

    for item in roadmap
        .ready_now
        .iter()
        .chain(roadmap.queued_next.iter())
        .chain(roadmap.parked.iter())
    {
        if !seen.insert(item.id) {
            return Err(format!(
                "Duplicate roadmap UUID `{}` in `{}`",
                item.id,
                item.heading()
            ));
        }
    }

    Ok(())
}

/// Validates `Ready Now` queue structure and limits.
///
/// # Errors
/// Returns an error when one active step violates the canonical `Ready Now`
/// template.
fn validate_ready_now(roadmap: &RoadmapDocument) -> Result<(), String> {
    if roadmap.ready_now.len() > READY_NOW_MAX_STEPS {
        return Err(format!(
            "`## Ready Now` may contain at most {READY_NOW_MAX_STEPS} steps, found {}",
            roadmap.ready_now.len()
        ));
    }

    for item in &roadmap.ready_now {
        validate_subsection_order(item, QueueKind::ReadyNow)?;
        validate_ready_assignee(item)?;
        validate_ready_substeps(item)?;
    }

    Ok(())
}

/// Validates one compact backlog queue.
///
/// # Errors
/// Returns an error when queued or parked cards do not use the compact layout.
fn validate_candidate_queue(items: &[RoadmapItem], queue: QueueKind) -> Result<(), String> {
    for item in items {
        validate_subsection_order(item, queue)?;
    }

    Ok(())
}

/// Validates subsection ordering and non-empty bodies for one roadmap item.
///
/// # Errors
/// Returns an error when the queue-specific subsection titles do not match.
fn validate_subsection_order(item: &RoadmapItem, queue: QueueKind) -> Result<(), String> {
    let actual = item.subsection_titles();
    let expected = queue.required_subsections().to_vec();

    if actual != expected {
        return Err(format!(
            "`{}` in `## {}` must use subsections {:?}, found {:?}",
            item.heading(),
            queue.section_title(),
            expected,
            actual
        ));
    }

    for subsection in &item.subsections {
        if subsection.body.trim().is_empty() {
            return Err(format!(
                "`{}` in `## {}` has an empty `#### {}` block",
                item.heading(),
                queue.section_title(),
                subsection.title
            ));
        }
    }

    Ok(())
}

/// Validates the `#### Assignee` value for one `Ready Now` item.
///
/// # Errors
/// Returns an error when the assignee is not one of the accepted formats.
fn validate_ready_assignee(item: &RoadmapItem) -> Result<(), String> {
    let assignee = item
        .subsection_body("Assignee")
        .ok_or_else(|| format!("`{}` is missing `#### Assignee`", item.heading()))?;
    let assignee = trim_inline_code(assignee);

    if assignee == "No assignee" || is_github_handle(assignee) || is_agent_session(assignee) {
        return Ok(());
    }

    Err(format!(
        "`{}` has invalid assignee `{assignee}`; expected `No assignee`, `@handle`, or `wt/<hash>`",
        item.heading()
    ))
}

/// Validates the `#### Substeps` checklist count for one `Ready Now` item.
///
/// # Errors
/// Returns an error when the number of substeps falls outside `2..=5`.
fn validate_ready_substeps(item: &RoadmapItem) -> Result<(), String> {
    let substeps = item
        .subsection_body("Substeps")
        .ok_or_else(|| format!("`{}` is missing `#### Substeps`", item.heading()))?;
    let checklist_count = substeps
        .lines()
        .filter(|line| is_task_list_item(line.trim_start()))
        .count();

    if (2..=5).contains(&checklist_count) {
        return Ok(());
    }

    Err(format!(
        "`{}` must contain `2..=5` `#### Substeps` checklist items, found {checklist_count}",
        item.heading()
    ))
}

/// Returns whether one markdown line is a checked or unchecked task item.
fn is_task_list_item(line: &str) -> bool {
    line.starts_with("- [ ] ") || line.starts_with("- [x] ") || line.starts_with("- [X] ")
}

/// Renders the human-readable repository context digest.
fn render_context_digest(roadmap: &RoadmapDocument, snapshot: &GitSnapshot) -> String {
    let mut lines = vec![
        "# Roadmap Context Digest".to_string(),
        String::new(),
        format!("Branch: `{}`", snapshot.branch),
        String::new(),
        "Roadmap:".to_string(),
        format!("- Ready Now: {}", roadmap.ready_now.len()),
        format!("- Queued Next: {}", roadmap.queued_next.len()),
        format!("- Parked: {}", roadmap.parked.len()),
        String::new(),
        "Ready Now:".to_string(),
    ];

    lines.extend(render_item_lines(&roadmap.ready_now, true));
    lines.push(String::new());
    lines.push("Queued Next:".to_string());
    lines.extend(render_item_lines(&roadmap.queued_next, false));
    lines.push(String::new());
    lines.push("Parked:".to_string());
    lines.extend(render_item_lines(&roadmap.parked, false));
    lines.push(String::new());

    if snapshot.status_lines.is_empty() {
        lines.push("Working Tree:".to_string());
        lines.push("- Clean.".to_string());
    } else {
        let changed_files = status_paths(&snapshot.status_lines);
        let touched_areas = top_level_areas(&changed_files);

        lines.push("Working Tree:".to_string());
        lines.push(format!("- {} changed paths.", changed_files.len()));
        lines.push(String::new());
        lines.push("Touched Areas:".to_string());
        for area in touched_areas {
            lines.push(format!("- {area}"));
        }
        lines.push(String::new());
        lines.push("Changed Paths:".to_string());
        for path in changed_files {
            lines.push(format!("- {path}"));
        }
    }

    lines.push(String::new());
    lines.push("Recent Commits:".to_string());
    if snapshot.recent_commits.is_empty() {
        lines.push("- None.".to_string());
    } else {
        for commit in &snapshot.recent_commits {
            lines.push(format!("- {commit}"));
        }
    }

    lines.join("\n")
}

/// Renders queue items into digest bullet lines.
fn render_item_lines(items: &[RoadmapItem], include_assignee: bool) -> Vec<String> {
    if items.is_empty() {
        return vec!["- None.".to_string()];
    }

    items
        .iter()
        .map(|item| {
            if include_assignee {
                let assignee = item
                    .subsection_body("Assignee")
                    .map_or("No assignee", trim_inline_code);

                return format!(
                    "- [{}] {}: {} ({assignee})",
                    item.id, item.stream, item.title
                );
            }

            format!("- [{}] {}: {}", item.id, item.stream, item.title)
        })
        .collect()
}

/// Converts a static array of git arguments into owned strings for mocking.
fn git_args<const N: usize>(args: [&str; N]) -> Vec<String> {
    args.iter()
        .map(|argument| (*argument).to_string())
        .collect()
}

/// Returns non-empty trimmed lines from raw command output.
fn non_empty_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Returns non-empty lines without trimming leading status columns.
fn non_empty_raw_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Trims blank lines from the edges of one markdown block.
fn trim_block(block: &str) -> String {
    block.trim().to_string()
}

/// Removes one wrapping pair of markdown backticks from an inline-code value.
fn trim_inline_code(value: &str) -> &str {
    let trimmed = value.trim();

    if trimmed.starts_with('`') && trimmed.ends_with('`') && trimmed.len() >= 2 {
        return &trimmed[1..(trimmed.len() - 1)];
    }

    trimmed
}

/// Returns whether a string is a simple GitHub-style handle.
fn is_github_handle(value: &str) -> bool {
    let Some(handle) = value.strip_prefix('@') else {
        return false;
    };

    !handle.is_empty()
        && handle.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

/// Returns whether a string is an agent session identifier.
fn is_agent_session(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("wt/") else {
        return false;
    };

    !suffix.is_empty()
}

/// Extracts changed paths from `git status --short` output.
fn status_paths(status_lines: &[String]) -> Vec<String> {
    status_lines
        .iter()
        .filter_map(|line| {
            if line.len() <= 3 {
                return None;
            }

            let path = line[3..].trim();
            if let Some((_, new_path)) = path.split_once(" -> ") {
                return Some(new_path.to_string());
            }

            Some(path.to_string())
        })
        .collect()
}

/// Returns sorted top-level areas touched by changed paths.
fn top_level_areas(paths: &[String]) -> Vec<String> {
    let mut areas = BTreeSet::new();

    for path in paths {
        let area = path.split('/').next().unwrap_or(path);
        areas.insert(area.to_string());
    }

    areas.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a valid roadmap fixture for lint and digest tests.
    fn roadmap_fixture() -> String {
        r#"# Agentty Roadmap

Summary.

## Current State Snapshot

Snapshot.

## Active Streams

- `Workflow`: one thing.

## Planning Model

- Keep it small.

## Ready Now

### [cbf025d6-2d29-4be7-b393-4ed3092ae66d] Workflow: Persist and render emitted follow-up tasks

#### Assignee

`No assignee`

#### Why now

Need a base slice.

#### Usable outcome

The user sees persisted tasks.

#### Substeps

- [ ] **Extend the protocol.** Update the protocol shape.
- [ ] **Store the tasks.** Persist emitted tasks.

#### Tests

- [ ] Add protocol and persistence coverage.

#### Docs

- [ ] Update the architecture notes.

## Ready Now Execution Order

```mermaid
flowchart TD
    A["[cbf025d6] Workflow: Persist and render emitted follow-up tasks"]
```

## Queued Next

### [8f4402cd-beff-4b4d-b9f7-00efd834249b] Workflow: Launch sibling sessions from follow-up tasks and retain task state

#### Outcome

Launch sibling sessions from follow-up tasks.

#### Promote when

Promote after persistence lands.

#### Depends on

`[cbf025d6] Workflow: Persist and render emitted follow-up tasks`

## Parked

### [3e7f1a92-4b8d-4c6e-9a15-d2f8e0b71c34] Testty: Land proof report fundamentals

#### Outcome

Add proof report fundamentals.

#### Promote when

Promote when product work slows down.

#### Depends on

`None`

## Context Notes

- One note.

## Status Maintenance Rule

- One rule.
"#
        .to_string()
    }

    #[test]
    fn lint_accepts_valid_roadmap() {
        // Arrange
        let roadmap = RoadmapDocument::parse(&roadmap_fixture()).expect("parse");

        // Act
        let result = validate_section_order(&roadmap)
            .and_then(|()| validate_unique_ids(&roadmap))
            .and_then(|()| validate_ready_now(&roadmap))
            .and_then(|()| validate_candidate_queue(&roadmap.queued_next, QueueKind::QueuedNext))
            .and_then(|()| validate_candidate_queue(&roadmap.parked, QueueKind::Parked));

        // Assert
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn lint_rejects_invalid_assignee_format() {
        // Arrange
        let roadmap = roadmap_fixture().replace("`No assignee`", "`andagaev`");
        let roadmap = RoadmapDocument::parse(&roadmap).expect("parse");

        // Act
        let result = validate_ready_now(&roadmap);

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("expected invalid assignee error");
        assert!(error.contains("invalid assignee"), "{error}");
    }

    #[test]
    fn lint_counts_checked_substeps() {
        // Arrange
        let roadmap =
            roadmap_fixture().replace("- [ ] **Store the tasks.**", "- [x] **Store the tasks.**");
        let roadmap = RoadmapDocument::parse(&roadmap).expect("parse");

        // Act
        let result = validate_ready_now(&roadmap);

        // Assert
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn lint_rejects_too_many_ready_now_steps() {
        // Arrange
        let base_step = r"
### [11111111-1111-4111-8111-111111111111] Workflow: First ready step

#### Assignee

`No assignee`

#### Why now

Why now.

#### Usable outcome

One usable outcome.

#### Substeps

- [ ] **First substep.** Do one thing.
- [ ] **Second substep.** Do another thing.

#### Tests

- [ ] Add tests.

#### Docs

- [ ] Add docs.
";
        let mut roadmap = roadmap_fixture();
        let repeated_steps = (0..6)
            .map(|index| {
                let id = format!("00000000-0000-4000-8000-00000000000{index}");
                base_step
                    .replace("11111111-1111-4111-8111-111111111111", &id)
                    .replace("First ready step", &format!("Ready step {index}"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        roadmap = roadmap.replacen(
            "### [cbf025d6-2d29-4be7-b393-4ed3092ae66d] Workflow: Persist and render emitted \
             follow-up tasks\n\n#### Assignee\n\n`No assignee`\n\n#### Why now\n\nNeed a base \
             slice.\n\n#### Usable outcome\n\nThe user sees persisted tasks.\n\n#### \
             Substeps\n\n- [ ] **Extend the protocol.** Update the protocol shape.\n- [ ] **Store \
             the tasks.** Persist emitted tasks.\n\n#### Tests\n\n- [ ] Add protocol and \
             persistence coverage.\n\n#### Docs\n\n- [ ] Update the architecture notes.\n",
            &repeated_steps,
            1,
        );
        let roadmap = RoadmapDocument::parse(&roadmap).expect("parse");

        // Act
        let result = validate_ready_now(&roadmap);

        // Assert
        assert!(result.is_err());
        let error = result.expect_err("expected ready-now overflow");
        assert!(error.contains("at most"), "{error}");
    }

    #[test]
    fn parser_accepts_trailing_hash_headings() {
        // Arrange
        let roadmap = roadmap_fixture()
            .replace("## Ready Now", "  ## Ready Now ##")
            .replace(
                "### [cbf025d6-2d29-4be7-b393-4ed3092ae66d] Workflow: Persist and render emitted \
                 follow-up tasks",
                "### [cbf025d6-2d29-4be7-b393-4ed3092ae66d] Workflow: Persist and render emitted \
                 follow-up tasks ###",
            )
            .replace("#### Assignee", "#### Assignee ####");

        // Act
        let roadmap = RoadmapDocument::parse(&roadmap);

        // Assert
        assert!(roadmap.is_ok());
    }

    #[test]
    fn context_digest_renders_git_and_roadmap_state() {
        // Arrange
        let roadmap = RoadmapDocument::parse(&roadmap_fixture()).expect("parse");
        let mut runner = MockGitCommandRunner::new();
        runner
            .expect_run()
            .withf(|args| args == &git_args(["rev-parse", "--abbrev-ref", "HEAD"]))
            .return_once(|_| Ok("wt/test-branch\n".to_string()));
        runner
            .expect_run()
            .withf(|args| args == &git_args(["log", "--oneline", "--max-count=5"]))
            .return_once(|_| Ok("abc1234 Add roadmap lint\n".to_string()));
        runner
            .expect_run()
            .withf(|args| args == &git_args(["status", "--short"]))
            .return_once(|_| {
                Ok(" M docs/plan/roadmap.md\n?? crates/ag-xtask/src/roadmap.rs\n".to_string())
            });
        let snapshot = GitSnapshot::load(&runner).expect("snapshot");

        // Act
        let digest = render_context_digest(&roadmap, &snapshot);

        // Assert
        assert!(digest.contains("wt/test-branch"), "{digest}");
        assert!(digest.contains("Ready Now: 1"), "{digest}");
        assert!(digest.contains("Queued Next: 1"), "{digest}");
        assert!(digest.contains("Parked: 1"), "{digest}");
        assert!(digest.contains("docs"), "{digest}");
        assert!(digest.contains("crates"), "{digest}");
        assert!(digest.contains("abc1234 Add roadmap lint"), "{digest}");
    }

    #[test]
    fn status_paths_extracts_renamed_and_untracked_paths() {
        // Arrange
        let status_lines = vec![
            "R  old/path.rs -> new/path.rs".to_string(),
            "?? docs/plan/roadmap.md".to_string(),
        ];

        // Act
        let paths = status_paths(&status_lines);

        // Assert
        assert_eq!(
            paths,
            vec![
                "new/path.rs".to_string(),
                "docs/plan/roadmap.md".to_string()
            ]
        );
    }
}
