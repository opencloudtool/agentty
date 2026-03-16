use crate::infra::agent::protocol::AgentResponseSummary;

/// The kind of a line in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    FileHeader,
    HunkHeader,
    Context,
    Addition,
    Deletion,
}

/// A parsed line from a unified diff, with optional old/new line numbers.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffLine<'a> {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: &'a str,
}

/// Extract `(old_start, old_count, new_start, new_count)` from a hunk header
/// like `@@ -10,5 +20,7 @@`.
pub fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let line = line.strip_prefix("@@ -")?;
    let at_idx = line.find(" @@")?;
    let range_part = &line[..at_idx];
    let mut parts = range_part.split(" +");
    let old_range = parts.next()?;
    let new_range = parts.next()?;

    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    Some((old_start, old_count, new_start, new_count))
}

/// Parse a full unified diff into structured [`DiffLine`] entries with line
/// numbers.
pub fn parse_diff_lines(diff: &str) -> Vec<DiffLine<'_>> {
    let mut result = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for line in diff.lines() {
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            result.push(DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if line.starts_with("@@") {
            if let Some((old_start, _, new_start, _)) = parse_hunk_header(line) {
                old_line = old_start;
                new_line = new_start;
            }
            result.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if let Some(rest) = line.strip_prefix('+') {
            result.push(DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(new_line),
                content: rest,
            });
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            result.push(DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(old_line),
                new_line: None,
                content: rest,
            });
            old_line += 1;
        } else {
            let content = line.strip_prefix(' ').unwrap_or(line);
            result.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_line),
                new_line: Some(new_line),
                content,
            });
            old_line += 1;
            new_line += 1;
        }
    }

    result
}

/// Find the maximum line number across all parsed diff lines for gutter width
/// calculation.
pub fn max_diff_line_number(lines: &[DiffLine<'_>]) -> u32 {
    lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(0)
}

/// Counts total added and removed lines across parsed diff content.
pub fn diff_line_change_totals(lines: &[DiffLine<'_>]) -> (usize, usize) {
    lines.iter().fold(
        (0_usize, 0_usize),
        |(added_count, removed_count), line| match line.kind {
            DiffLineKind::Addition => (added_count.saturating_add(1), removed_count),
            DiffLineKind::Deletion => (added_count, removed_count.saturating_add(1)),
            _ => (added_count, removed_count),
        },
    )
}

/// Split a diff content string into chunks that fit within `max_width`
/// characters. Returns at least one chunk (empty string if content is empty).
pub fn wrap_diff_content(content: &str, max_width: usize) -> Vec<&str> {
    if max_width == 0 {
        return vec![content];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_width {
            chunks.push(remaining);

            break;
        }

        let split_at = remaining
            .char_indices()
            .nth(max_width)
            .map_or(remaining.len(), |(idx, _)| idx);
        chunks.push(&remaining[..split_at]);
        remaining = &remaining[split_at..];
    }

    if chunks.is_empty() {
        chunks.push("");
    }

    chunks
}

const DEFAULT_REVIEW_COMMENT: &str = "Agent summary unavailable; review the highlighted changes.";
const MAX_AGENT_COMMENT_COUNT: usize = 4;
const MAX_REVIEW_HIGHLIGHT_COUNT: usize = 8;
const MAX_REVIEW_FALLBACK_COUNT: usize = 5;
const MAX_REVIEW_SNIPPET_WIDTH: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FocusedReviewHighlight {
    comment: &'static str,
    file_path: String,
    line_number: Option<u32>,
    order: usize,
    score: u16,
    sign: char,
    snippet: String,
}

/// Builds review markdown using concise agent comments and critical
/// diff highlights.
pub fn build_focused_review_text(diff: &str, summary: Option<&str>) -> String {
    let agent_comments = focused_review_agent_comments(summary);
    let highlights = focused_review_highlights(diff);

    let mut lines = vec![
        "## Review".to_string(),
        String::new(),
        "### Agent Comments".to_string(),
    ];

    lines.extend(
        agent_comments
            .into_iter()
            .map(|comment| format!("- {comment}")),
    );

    lines.push(String::new());
    lines.push("### Critical Diff Highlights".to_string());

    if highlights.is_empty() {
        lines.push("- No changes found in the current diff.".to_string());
    } else {
        lines.extend(highlights.iter().map(focused_review_highlight_markdown));
    }

    lines.push(String::new());
    lines.push("Press `d` for the full diff.".to_string());

    lines.join("\n")
}

/// Extracts concise one-line agent comments from session summary text.
///
/// Protocol summary headings are removed, but the content that follows those
/// headings is retained so user-facing notes such as canonical commit text
/// still appear in the focused review.
fn focused_review_agent_comments(summary: Option<&str>) -> Vec<String> {
    let summary_text = summary.unwrap_or_default().trim();
    let structured_summary_lines = serde_json::from_str::<AgentResponseSummary>(summary_text)
        .ok()
        .into_iter()
        .flat_map(|summary_payload| [summary_payload.turn, summary_payload.session])
        .collect::<Vec<_>>();
    let summary_lines = if structured_summary_lines.is_empty() {
        summary_text
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    } else {
        structured_summary_lines
    };
    let mut comments = Vec::new();

    for summary_line in summary_lines
        .into_iter()
        .flat_map(|line| line.lines().map(ToString::to_string).collect::<Vec<_>>())
    {
        let trimmed_line = summary_line.trim();

        if trimmed_line.is_empty() {
            continue;
        }

        let list_stripped = strip_markdown_list_prefix(trimmed_line);
        let heading_stripped = strip_markdown_heading_prefix(&list_stripped).to_string();

        if is_protocol_summary_heading(&heading_stripped) {
            continue;
        }

        if !heading_stripped.is_empty() {
            comments.push(heading_stripped);
        }

        if comments.len() >= MAX_AGENT_COMMENT_COUNT {
            break;
        }
    }

    if comments.is_empty() {
        comments.push(DEFAULT_REVIEW_COMMENT.to_string());
    }

    comments
}

/// Returns whether one normalized summary line is a protocol summary heading.
fn is_protocol_summary_heading(line: &str) -> bool {
    matches!(
        line,
        "Change Summary" | "Current Turn" | "Session Changes" | "Summary" | "Commit"
    )
}

/// Returns scored review highlights from unified diff text.
fn focused_review_highlights(diff: &str) -> Vec<FocusedReviewHighlight> {
    let mut highlights = Vec::new();
    let mut fallback_highlights = Vec::new();
    let mut current_file = "unknown".to_string();
    let mut old_line = 0_u32;
    let mut new_line = 0_u32;
    let mut order = 0_usize;

    for raw_line in diff.lines() {
        if let Some(file_path) = parse_diff_file_path(raw_line) {
            current_file = file_path;

            continue;
        }

        if let Some((old_start, _, new_start, _)) = parse_hunk_header(raw_line) {
            old_line = old_start;
            new_line = new_start;

            continue;
        }

        if raw_line.starts_with("index ")
            || raw_line.starts_with("--- ")
            || raw_line.starts_with("+++ ")
        {
            continue;
        }

        if let Some(content) = raw_line.strip_prefix('+') {
            let line_number = Some(new_line);
            new_line = new_line.saturating_add(1);

            if let Some(highlight) =
                focused_review_highlight(&current_file, line_number, '+', content, order)
            {
                highlights.push(highlight);
            } else if let Some(fallback_highlight) =
                focused_review_fallback_highlight(&current_file, line_number, '+', content, order)
            {
                fallback_highlights.push(fallback_highlight);
            }

            order = order.saturating_add(1);

            continue;
        }

        if let Some(content) = raw_line.strip_prefix('-') {
            let line_number = Some(old_line);
            old_line = old_line.saturating_add(1);

            if let Some(highlight) =
                focused_review_highlight(&current_file, line_number, '-', content, order)
            {
                highlights.push(highlight);
            } else if let Some(fallback_highlight) =
                focused_review_fallback_highlight(&current_file, line_number, '-', content, order)
            {
                fallback_highlights.push(fallback_highlight);
            }

            order = order.saturating_add(1);

            continue;
        }

        if !raw_line.starts_with('\\') {
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        }
    }

    if highlights.is_empty() {
        fallback_highlights.truncate(MAX_REVIEW_FALLBACK_COUNT);

        return fallback_highlights;
    }

    highlights.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.order.cmp(&right.order))
    });
    highlights.truncate(MAX_REVIEW_HIGHLIGHT_COUNT);
    highlights.sort_by_key(|highlight| highlight.order);

    highlights
}

/// Builds one markdown list item for a review highlight.
fn focused_review_highlight_markdown(highlight: &FocusedReviewHighlight) -> String {
    let location = highlight
        .line_number
        .map_or_else(|| "?".to_string(), |line_number| line_number.to_string());

    format!(
        "- `{}`:{} {} `{}` — {}",
        highlight.file_path, location, highlight.sign, highlight.snippet, highlight.comment
    )
}

/// Creates a scored highlight when a change matches high-signal criticality
/// heuristics.
fn focused_review_highlight(
    file_path: &str,
    line_number: Option<u32>,
    sign: char,
    content: &str,
    order: usize,
) -> Option<FocusedReviewHighlight> {
    const DEFAULT_COMMENT: &str = "Behavior changed.";
    const RUNTIME_COMMENT: &str = "Runtime safety or error handling changed.";
    const SECURITY_COMMENT: &str = "Authorization or security-sensitive logic changed.";
    const DATABASE_COMMENT: &str = "Database behavior or schema logic changed.";
    const PROCESS_COMMENT: &str = "External command execution path changed.";
    const CONFIG_COMMENT: &str = "Build or runtime configuration changed.";

    let normalized_content = content.to_lowercase();
    let normalized_path = file_path.to_lowercase();
    let mut score = 0_u16;
    let mut comment = DEFAULT_COMMENT;
    let mut matched_runtime = false;

    if contains_any(
        &normalized_content,
        &["unsafe", "unwrap(", "expect(", "panic!("],
    ) {
        score = score.saturating_add(5);
        comment = RUNTIME_COMMENT;
        matched_runtime = true;
    }

    if contains_any(
        &normalized_content,
        &[
            "auth",
            "permission",
            "token",
            "secret",
            "password",
            "admin",
            "role",
            "acl",
        ],
    ) || contains_any(&normalized_path, &["auth", "permission", "security"])
    {
        score = score.saturating_add(4);
        if !matched_runtime {
            comment = SECURITY_COMMENT;
        }
    }

    if contains_any(
        &normalized_content,
        &[
            "select ", "insert ", "update ", "delete ", "drop ", "alter ",
        ],
    ) || contains_any(&normalized_path, &["migration", ".sql"])
    {
        score = score.saturating_add(4);
        if !matched_runtime {
            comment = DATABASE_COMMENT;
        }
    }

    if contains_any(
        &normalized_content,
        &["command", "shell", "process", "exec(", "spawn(", "system("],
    ) {
        score = score.saturating_add(3);
        if !matched_runtime {
            comment = PROCESS_COMMENT;
        }
    }

    if contains_any(
        &normalized_path,
        &[
            "cargo.toml",
            ".github/workflows",
            "dockerfile",
            ".yaml",
            ".yml",
            ".toml",
        ],
    ) {
        score = score.saturating_add(2);
        if !matched_runtime {
            comment = CONFIG_COMMENT;
        }
    }

    if score == 0 {
        return None;
    }

    Some(FocusedReviewHighlight {
        comment,
        file_path: file_path.to_string(),
        line_number,
        order,
        score,
        sign,
        snippet: focused_review_snippet(content),
    })
}

/// Creates an unscored fallback highlight when no criticality heuristic
/// matches.
fn focused_review_fallback_highlight(
    file_path: &str,
    line_number: Option<u32>,
    sign: char,
    content: &str,
    order: usize,
) -> Option<FocusedReviewHighlight> {
    let snippet = focused_review_snippet(content);
    if snippet.is_empty() {
        return None;
    }

    Some(FocusedReviewHighlight {
        comment: "General code change; inspect full diff for context.",
        file_path: file_path.to_string(),
        line_number,
        order,
        score: 0,
        sign,
        snippet,
    })
}

/// Parses the destination file path from a `diff --git` header line.
fn parse_diff_file_path(line: &str) -> Option<String> {
    let suffix = line.strip_prefix("diff --git a/")?;
    let (_, rhs) = suffix.split_once(" b/")?;

    Some(rhs.to_string())
}

/// Returns a clean one-line snippet for review output.
fn focused_review_snippet(content: &str) -> String {
    let collapsed = content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if collapsed.is_empty() {
        return String::new();
    }

    let char_count = collapsed.chars().count();
    if char_count <= MAX_REVIEW_SNIPPET_WIDTH {
        return collapsed;
    }

    let truncated = collapsed
        .chars()
        .take(MAX_REVIEW_SNIPPET_WIDTH.saturating_sub(3))
        .collect::<String>();

    format!("{truncated}...")
}

/// Returns whether `text` contains any token from `needles`.
fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// Removes common markdown bullet prefixes from a summary line.
fn strip_markdown_list_prefix(line: &str) -> &str {
    line.trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim_start_matches("+ ")
}

/// Removes leading markdown heading markers from a summary line.
fn strip_markdown_heading_prefix(line: &str) -> &str {
    line.trim_start_matches('#').trim_start()
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    if let Some((start, count)) = range.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((range.parse().ok()?, 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hunk_header_basic() {
        // Arrange
        let line = "@@ -10,5 +20,7 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((10, 5, 20, 7)));
    }

    #[test]
    fn test_parse_hunk_header_no_count() {
        // Arrange
        let line = "@@ -1 +1 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_with_context() {
        // Arrange
        let line = "@@ -100,3 +200,4 @@ fn main() {";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((100, 3, 200, 4)));
    }

    #[test]
    fn test_parse_hunk_header_invalid() {
        // Arrange & Act & Assert
        assert_eq!(parse_hunk_header("not a hunk"), None);
        assert_eq!(parse_hunk_header("@@@ invalid @@@"), None);
    }

    #[test]
    fn test_parse_diff_lines_full() {
        // Arrange
        let diff = "\
diff --git a/file.rs b/file.rs
index abc..def 100644
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 9);

        assert_eq!(lines[0].kind, DiffLineKind::FileHeader);
        assert_eq!(lines[0].content, "diff --git a/file.rs b/file.rs");
        assert_eq!(lines[0].old_line, None);

        assert_eq!(lines[4].kind, DiffLineKind::HunkHeader);
        assert_eq!(lines[4].old_line, None);

        assert_eq!(lines[5].kind, DiffLineKind::Context);
        assert_eq!(lines[5].content, "line1");
        assert_eq!(lines[5].old_line, Some(1));
        assert_eq!(lines[5].new_line, Some(1));

        assert_eq!(lines[6].kind, DiffLineKind::Addition);
        assert_eq!(lines[6].content, "added");
        assert_eq!(lines[6].old_line, None);
        assert_eq!(lines[6].new_line, Some(2));

        assert_eq!(lines[7].kind, DiffLineKind::Context);
        assert_eq!(lines[7].content, "line2");
        assert_eq!(lines[7].old_line, Some(2));
        assert_eq!(lines[7].new_line, Some(3));

        assert_eq!(lines[8].kind, DiffLineKind::Deletion);
        assert_eq!(lines[8].content, "removed");
        assert_eq!(lines[8].old_line, Some(3));
        assert_eq!(lines[8].new_line, None);
    }

    #[test]
    fn test_parse_diff_lines_empty() {
        // Arrange
        let diff = "";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn test_max_diff_line_number() {
        // Arrange
        let diff = "\
@@ -95,3 +100,4 @@
 context
+added
 context2
-removed";
        let lines = parse_diff_lines(diff);

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 102);
    }

    #[test]
    fn test_max_diff_line_number_empty() {
        // Arrange
        let lines: Vec<DiffLine<'_>> = Vec::new();

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 0);
    }

    #[test]
    fn test_diff_line_change_totals() {
        // Arrange
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";
        let lines = parse_diff_lines(diff);

        // Act
        let totals = diff_line_change_totals(&lines);

        // Assert
        assert_eq!(totals, (1, 1));
    }

    #[test]
    fn test_diff_line_change_totals_ignores_headers() {
        // Arrange
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs";
        let lines = parse_diff_lines(diff);

        // Act
        let totals = diff_line_change_totals(&lines);

        // Assert
        assert_eq!(totals, (0, 0));
    }

    #[test]
    fn test_wrap_diff_content_fits() {
        // Arrange
        let content = "short line";

        // Act
        let chunks = wrap_diff_content(content, 80);

        // Assert
        assert_eq!(chunks, vec!["short line"]);
    }

    #[test]
    fn test_wrap_diff_content_wraps() {
        // Arrange
        let content = "abcdefghij";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn test_wrap_diff_content_empty() {
        // Arrange & Act
        let chunks = wrap_diff_content("", 10);

        // Assert
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn test_wrap_diff_content_exact() {
        // Arrange
        let content = "abcd";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd"]);
    }

    #[test]
    fn test_build_focused_review_text_includes_summary_and_critical_highlights() {
        // Arrange
        let diff = "\
diff --git a/src/auth.rs b/src/auth.rs
@@ -8,1 +8,1 @@
-let can_merge = false;
+let can_merge = user.role == \"admin\";
@@ -20,1 +20,1 @@
-let value = maybe_value.unwrap();
+let value = maybe_value.expect(\"missing value\");";
        let summary = Some("Tighten merge access\n- Add role guard");

        // Act
        let focused_review = build_focused_review_text(diff, summary);

        // Assert
        assert!(focused_review.contains("## Review"));
        assert!(focused_review.contains("- Tighten merge access"));
        assert!(focused_review.contains("Authorization or security-sensitive logic changed."));
        assert!(focused_review.contains("Runtime safety or error handling changed."));
        assert!(focused_review.contains("src/auth.rs"));
    }

    #[test]
    fn test_build_focused_review_text_uses_fallback_when_summary_and_critical_hits_missing() {
        // Arrange
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
@@ -1,1 +1,1 @@
-let old_value = 1;
+let new_value = 2;";

        // Act
        let focused_review = build_focused_review_text(diff, None);

        // Assert
        assert!(focused_review.contains(DEFAULT_REVIEW_COMMENT));
        assert!(focused_review.contains("General code change; inspect full diff for context."));
        assert!(focused_review.contains("src/main.rs"));
    }

    #[test]
    fn test_build_focused_review_text_skips_protocol_summary_headings() {
        // Arrange
        let summary = Some(
            "## Change Summary\n### Current Turn\n- Added protocol summary fields.\n\n### Session \
             Changes\n- Session output renders summary markdown separately.\n\n# Summary\n\n# \
             Commit\n- Canonical commit note",
        );

        // Act
        let focused_review = build_focused_review_text("", summary);

        // Assert
        assert!(focused_review.contains("- Added protocol summary fields."));
        assert!(focused_review.contains("- Session output renders summary markdown separately."));
        assert!(focused_review.contains("- Canonical commit note"));
        assert!(!focused_review.contains("- Change Summary"));
        assert!(!focused_review.contains("- Current Turn"));
        assert!(!focused_review.contains("- Session Changes"));
        assert!(!focused_review.contains("- Summary"));
        assert!(!focused_review.contains("- Commit"));
    }

    #[test]
    fn test_build_focused_review_text_truncates_comments_at_max_count() {
        // Arrange — 5 content lines exceed MAX_AGENT_COMMENT_COUNT (3),
        // verify only the first 3 survive and the rest are dropped.
        let summary = Some(
            "- First comment\n- Second comment\n- Third comment\n- Fourth comment\n- Fifth comment",
        );

        // Act
        let focused_review = build_focused_review_text("", summary);

        // Assert — first three kept in order
        assert!(focused_review.contains("- First comment"));
        assert!(focused_review.contains("- Second comment"));
        assert!(focused_review.contains("- Third comment"));
        // fourth and fifth truncated
        assert!(!focused_review.contains("Fourth comment"));
        assert!(!focused_review.contains("Fifth comment"));
    }

    #[test]
    fn test_build_focused_review_text_parses_structured_summary_json() {
        // Arrange
        let summary = Some(
            "{\"turn\":\"- Added protocol summary fields.\",\"session\":\"- Session output \
             renders summary markdown separately.\"}",
        );

        // Act
        let focused_review = build_focused_review_text("", summary);

        // Assert
        assert!(focused_review.contains("- Added protocol summary fields."));
        assert!(focused_review.contains("- Session output renders summary markdown separately."));
    }

    #[test]
    fn test_build_focused_review_text_handles_empty_diff() {
        // Arrange
        let summary = Some("Keep behavior stable");

        // Act
        let focused_review = build_focused_review_text("", summary);

        // Assert
        assert!(focused_review.contains("- Keep behavior stable"));
        assert!(focused_review.contains("No changes found in the current diff."));
    }
}
