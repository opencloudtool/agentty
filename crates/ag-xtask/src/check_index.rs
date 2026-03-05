use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use tracing::error;

#[cfg_attr(test, mockall::automock)]
trait CommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<Output>;
}

struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<Output> {
        command.output()
    }
}

const OPTIONAL_INDEX_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "GEMINI.md"];

/// Runs repository-wide `AGENTS.md` directory index validation.
pub(crate) fn run() -> Result<(), String> {
    run_with_runner(&RealCommandRunner)
}

fn run_with_runner(runner: &dyn CommandRunner) -> Result<(), String> {
    let tracked_files = get_tracked_files(runner)?
        .into_iter()
        .filter(|path| !is_ignored_index_path(path))
        .collect::<Vec<_>>();
    let agents_files = get_agents_files(runner)?
        .into_iter()
        .filter(|path| !is_ignored_index_path(&normalize_path(path)))
        .collect::<Vec<_>>();

    let mut success = true;
    let missing_agents_files = missing_agents_files(&tracked_files, &agents_files);
    if !missing_agents_files.is_empty() {
        for missing_agents_file in &missing_agents_files {
            error!(
                "Error: missing {}. Add a local AGENTS.md to index this directory.",
                missing_agents_file.display()
            );
        }
        success = false;
    }

    for agents_path in agents_files {
        if !process_directory(&agents_path, &tracked_files) {
            success = false;
        }
    }

    if !success {
        return Err("Index check failed. Some AGENTS.md indexes are outdated.".to_string());
    }

    Ok(())
}

fn missing_agents_files(tracked_files: &[String], agents_files: &[PathBuf]) -> Vec<PathBuf> {
    let relative_tracked_files = tracked_files
        .iter()
        .filter(|path| is_relative_git_path(path))
        .cloned()
        .collect::<Vec<_>>();
    if relative_tracked_files.is_empty() {
        return Vec::new();
    }

    let existing_agents_paths = agents_files
        .iter()
        .filter_map(|path| path.to_str())
        .map(std::string::ToString::to_string)
        .collect::<BTreeSet<_>>();

    let directories = directories_with_indexable_entries(&relative_tracked_files);
    let mut missing_files = Vec::new();
    for directory in directories {
        let expected_agents_file = if directory == Path::new(".") {
            PathBuf::from("AGENTS.md")
        } else {
            directory.join("AGENTS.md")
        };
        if !existing_agents_paths.contains(&normalize_path(&expected_agents_file)) {
            missing_files.push(expected_agents_file);
        }
    }

    missing_files
}

fn directories_with_indexable_entries(tracked_files: &[String]) -> BTreeSet<PathBuf> {
    let mut directories = BTreeSet::new();
    directories.insert(PathBuf::from("."));

    for tracked_file in tracked_files {
        let normalized_file_path = normalize_path(Path::new(tracked_file));
        let path_segments = normalized_file_path.split('/').collect::<Vec<_>>();
        if path_segments.len() <= 1 {
            continue;
        }

        let mut directory = String::new();
        for segment in path_segments.iter().take(path_segments.len() - 1) {
            if directory.is_empty() {
                directory.push_str(segment);
            } else {
                directory.push('/');
                directory.push_str(segment);
            }
            directories.insert(PathBuf::from(&directory));
        }
    }

    directories
        .into_iter()
        .filter(|directory| !get_local_entries(directory.as_path(), tracked_files).is_empty())
        .collect()
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_relative_git_path(path: &str) -> bool {
    let path_bytes = path.as_bytes();
    let has_windows_drive_prefix = path_bytes.len() >= 3
        && path_bytes[1] == b':'
        && (path_bytes[2] == b'\\' || path_bytes[2] == b'/');

    !path.starts_with('/') && !has_windows_drive_prefix
}

/// Returns whether `path` should be excluded from directory-index checks.
///
/// `pre-commit` can create a transient `.tmp-home/` workspace while running
/// hooks. Those files are not part of repository-owned documentation and must
/// not require `AGENTS.md` indexing.
fn is_ignored_index_path(path: &str) -> bool {
    path == ".tmp-home" || path.starts_with(".tmp-home/")
}

/// Returns whether `entry` is optional in `Directory Index` sections.
///
/// These files are generated or symlinked alongside `AGENTS.md` and may be
/// documented for discoverability, but they are not required for indexing
/// correctness.
fn is_optional_index_entry(entry: &str) -> bool {
    OPTIONAL_INDEX_FILES.contains(&entry)
}

fn get_tracked_files(runner: &dyn CommandRunner) -> Result<Vec<String>, String> {
    let output = runner
        .run(Command::new("git").arg("ls-files"))
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    Ok(stdout.lines().map(ToString::to_string).collect())
}

fn get_agents_files(runner: &dyn CommandRunner) -> Result<Vec<PathBuf>, String> {
    let output = runner
        .run(Command::new("git").args(["ls-files", "**/AGENTS.md", "AGENTS.md"]))
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    Ok(stdout.lines().map(PathBuf::from).collect())
}

/// Validates one `AGENTS.md` file against the tracked entries in its directory.
///
/// The check reports missing, stale, duplicate, and mislabeled index entries.
fn process_directory(agents_path: &Path, tracked_files: &[String]) -> bool {
    let directory = agents_path.parent().unwrap_or_else(|| Path::new("."));
    let all_entries = get_local_entries(directory, tracked_files);

    let content = match fs::read_to_string(agents_path) {
        Ok(content) => content,
        Err(err) => {
            error!("Error reading {}: {}", agents_path.display(), err);
            return false;
        }
    };

    let Some(index_entries) = parse_index(&content) else {
        error!(
            "Error: No '## Directory Index' section in {}",
            agents_path.display()
        );
        return false;
    };

    let indexed_destinations = index_entries
        .iter()
        .map(|entry| entry.destination.clone())
        .collect::<Vec<_>>();
    let indexed_set = indexed_destinations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let local_set = all_entries.iter().cloned().collect::<BTreeSet<_>>();

    let missing = local_set
        .difference(&indexed_set)
        .cloned()
        .collect::<Vec<_>>();
    let stale = indexed_set
        .difference(&local_set)
        .filter(|entry| !is_optional_index_entry(entry.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let duplicate = duplicate_index_entries(&indexed_destinations);
    let mismatched_labels = index_entries
        .iter()
        .filter(|entry| entry.label != entry.destination)
        .map(|entry| format!("{} -> {}", entry.label, entry.destination))
        .collect::<Vec<_>>();

    if missing.is_empty()
        && stale.is_empty()
        && duplicate.is_empty()
        && mismatched_labels.is_empty()
    {
        return true;
    }

    if !missing.is_empty() {
        error!(
            "Error: {} is missing entries: {}",
            agents_path.display(),
            missing.join(", ")
        );
    }

    if !stale.is_empty() {
        error!(
            "Error: {} has stale entries: {}",
            agents_path.display(),
            stale.join(", ")
        );
    }

    if !duplicate.is_empty() {
        error!(
            "Error: {} has duplicate entries: {}",
            agents_path.display(),
            duplicate.join(", ")
        );
    }

    if !mismatched_labels.is_empty() {
        error!(
            "Error: {} has mismatched link labels: {}",
            agents_path.display(),
            mismatched_labels.join(", ")
        );
    }

    false
}

/// Returns duplicate index entries in sorted order.
fn duplicate_index_entries(indexed_destinations: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut duplicate = BTreeSet::new();

    for indexed_destination in indexed_destinations {
        if !seen.insert(indexed_destination.clone()) {
            duplicate.insert(indexed_destination.clone());
        }
    }

    duplicate.into_iter().collect()
}

/// Collects direct child files/directories for one indexed directory path.
///
/// Agent instruction files are excluded because they are optional index
/// entries.
fn get_local_entries(directory: &Path, tracked_files: &[String]) -> Vec<String> {
    let mut local_files = BTreeSet::new();
    let mut local_dirs = BTreeSet::new();

    let prefix = if directory == Path::new(".") || directory.to_string_lossy().is_empty() {
        String::new()
    } else {
        let mut path_str = directory.to_string_lossy().to_string();
        path_str = path_str.replace('\\', "/");
        if !path_str.ends_with('/') {
            path_str.push('/');
        }
        path_str
    };

    for file_path in tracked_files {
        let f_normalized = file_path.replace('\\', "/");
        if f_normalized.starts_with(&prefix) || prefix.is_empty() {
            let rel_path = if prefix.is_empty() {
                &f_normalized
            } else {
                &f_normalized[prefix.len()..]
            };

            if rel_path.is_empty() {
                continue;
            }

            if let Some(slash_idx) = rel_path.find('/') {
                local_dirs.insert(rel_path[..slash_idx].to_string());
            } else if !is_optional_index_entry(rel_path) {
                local_files.insert(rel_path.to_string());
            }
        }
    }

    let mut all_entries = Vec::new();
    for dir_name in local_dirs {
        all_entries.push(format!("{dir_name}/"));
    }
    for file_name in local_files {
        all_entries.push(file_name);
    }
    all_entries.sort();

    all_entries
}

/// Parsed `Directory Index` link metadata from one entry line.
#[derive(Debug, PartialEq)]
struct IndexEntry {
    destination: String,
    label: String,
}

/// Temporary parser state for one in-progress markdown link.
#[derive(Debug)]
struct LinkCapture {
    destination: String,
    label: String,
}

impl LinkCapture {
    /// Creates a new link capture from a parsed markdown destination.
    fn new(destination: &str) -> Self {
        Self {
            destination: destination.to_string(),
            label: String::new(),
        }
    }

    /// Converts parser state into a finalized index entry.
    fn into_index_entry(self) -> IndexEntry {
        IndexEntry {
            destination: self.destination,
            label: self.label,
        }
    }
}

/// Parses the `Directory Index` section of an `AGENTS.md` file.
///
/// The parser uses markdown events to collect entry destinations and labels,
/// which allows validation to detect stale or duplicate index references.
fn parse_index(content: &str) -> Option<Vec<IndexEntry>> {
    collect_directory_index_entries(content)
}

/// Collects markdown link entries from the `Directory Index` section.
///
/// The section starts at an `H2` heading with text equal to `Directory Index`
/// and ends when the next `H1` or `H2` heading starts.
fn collect_directory_index_entries(content: &str) -> Option<Vec<IndexEntry>> {
    let mut indexed_entries = Vec::new();
    let mut is_in_directory_index = false;
    let mut current_heading_level = None;
    let mut current_heading_text = String::new();
    let mut current_link = None;

    for event in Parser::new(content) {
        if let Event::Start(Tag::Heading { level, .. }) = &event {
            if is_in_directory_index && is_section_terminator(*level) {
                break;
            }

            current_heading_level = Some(*level);
            current_heading_text.clear();

            continue;
        }

        if let Some(level) = current_heading_level {
            append_heading_text(&mut current_heading_text, &event);

            if let Event::End(TagEnd::Heading(end_level)) = &event {
                if *end_level == level
                    && level == HeadingLevel::H2
                    && current_heading_text.trim() == "Directory Index"
                {
                    is_in_directory_index = true;
                }

                if *end_level == level {
                    current_heading_level = None;
                    current_heading_text.clear();
                }
            }

            continue;
        }

        if !is_in_directory_index {
            continue;
        }

        match &event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                let destination = dest_url.trim();
                if destination.is_empty() {
                    continue;
                }

                current_link = Some(LinkCapture::new(destination));
            }
            Event::End(TagEnd::Link) => {
                if let Some(link_capture) = current_link.take() {
                    indexed_entries.push(link_capture.into_index_entry());
                }
            }
            _ => {
                append_link_label_text(&mut current_link, &event);
            }
        }
    }

    if !is_in_directory_index {
        return None;
    }

    Some(indexed_entries)
}

/// Returns whether a heading level starts a new top-level index section.
fn is_section_terminator(level: HeadingLevel) -> bool {
    level == HeadingLevel::H1 || level == HeadingLevel::H2
}

/// Appends heading text events to a single normalized heading buffer.
fn append_heading_text(current_heading_text: &mut String, event: &Event<'_>) {
    match event {
        Event::Text(text) | Event::Code(text) => current_heading_text.push_str(text),
        Event::SoftBreak | Event::HardBreak => current_heading_text.push(' '),
        _ => {}
    }
}

/// Appends markdown link-label text while a `Directory Index` link is active.
fn append_link_label_text(current_link: &mut Option<LinkCapture>, event: &Event<'_>) {
    if let Some(link_capture) = current_link {
        match event {
            Event::Text(text) | Event::Code(text) => link_capture.label.push_str(text),
            Event::SoftBreak | Event::HardBreak => link_capture.label.push(' '),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Command, ExitStatus, Output};

    use tempfile::tempdir;

    use super::*;

    fn mock_output(status: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn test_get_local_entries_root() {
        // Arrange
        let directory = Path::new(".");
        let tracked_files = vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "AGENTS.md".to_string(),
            "crates/agentty/Cargo.toml".to_string(),
        ];

        // Act
        let entries = get_local_entries(directory, &tracked_files);

        // Assert
        assert_eq!(entries, vec!["Cargo.toml", "crates/", "src/"]);
    }

    #[test]
    fn test_get_local_entries_windows_paths() {
        // Arrange
        let directory = Path::new("crates/agentty");
        let tracked_files = vec![
            "crates\\agentty\\Cargo.toml".to_string(),
            "crates\\agentty\\src\\main.rs".to_string(),
        ];

        // Act
        let entries = get_local_entries(directory, &tracked_files);

        // Assert
        assert_eq!(entries, vec!["Cargo.toml", "src/"]);
    }

    #[test]
    fn test_get_local_entries_subdir() {
        // Arrange
        let directory = Path::new("crates/agentty");
        let tracked_files = vec![
            "Cargo.toml".to_string(),
            "crates/agentty/Cargo.toml".to_string(),
            "crates/agentty/src/main.rs".to_string(),
            "crates/agentty/AGENTS.md".to_string(),
        ];

        // Act
        let entries = get_local_entries(directory, &tracked_files);

        // Assert
        assert_eq!(entries, vec!["Cargo.toml", "src/"]);
    }

    #[test]
    fn test_parse_index_basic() {
        // Arrange
        let content = "# Title\n\n## Directory Index\n- [file1](file1) - Desc\n- [dir1/](dir1/) - \
                       Desc\n\n## Next Section\n- [ignored](ignored) - Desc";

        // Act
        let entries = parse_index(content).expect("Failed to parse index");

        // Assert
        assert_eq!(
            entries,
            vec![
                IndexEntry {
                    destination: "file1".to_string(),
                    label: "file1".to_string(),
                },
                IndexEntry {
                    destination: "dir1/".to_string(),
                    label: "dir1/".to_string(),
                },
            ]
        );
    }

    #[test]
    fn test_parse_index_prefers_destination_over_escaped_link_text() {
        // Arrange
        let content = "## Directory Index\n- [\\_index.md](_index.md) - Homepage content.\n";

        // Act
        let entries = parse_index(content).expect("Failed to parse index");

        // Assert
        assert_eq!(
            entries,
            vec![IndexEntry {
                destination: "_index.md".to_string(),
                label: "_index.md".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_index_prefers_destination_over_code_formatted_link_text() {
        // Arrange
        let content = "## Directory Index\n- [`_index.md`](_index.md) - Homepage content.\n";

        // Act
        let entries = parse_index(content).expect("Failed to parse index");

        // Assert
        assert_eq!(
            entries,
            vec![IndexEntry {
                destination: "_index.md".to_string(),
                label: "_index.md".to_string(),
            }]
        );
    }

    #[test]
    fn test_process_directory_missing_header() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(&agents_path, "# No Header").expect("Failed to write AGENTS.md");
        let tracked_files = vec![];

        // Act
        let result = process_directory(&agents_path, &tracked_files);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_process_directory_io_error() {
        // Arrange
        let agents_path = Path::new("non_existent_agents_md");
        let tracked_files = vec![];

        // Act
        let result = process_directory(agents_path, &tracked_files);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_process_directory_fails_on_stale_entries() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "## Directory Index\n- [file1](file1) - desc\n- [stale](stale) - desc\n",
        )
        .expect("Failed to write AGENTS.md");
        let tracked_files = vec![
            agents_path.to_string_lossy().to_string(),
            dir.path().join("file1").to_string_lossy().to_string(),
        ];

        // Act
        let result = process_directory(&agents_path, &tracked_files);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_process_directory_allows_optional_entries() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "## Directory Index\n- [file1](file1) - desc\n- [AGENTS.md](AGENTS.md) - desc\n- \
             [CLAUDE.md](CLAUDE.md) - desc\n- [GEMINI.md](GEMINI.md) - desc\n",
        )
        .expect("Failed to write AGENTS.md");
        let tracked_files = vec![
            agents_path.to_string_lossy().to_string(),
            dir.path().join("file1").to_string_lossy().to_string(),
        ];

        // Act
        let result = process_directory(&agents_path, &tracked_files);

        // Assert
        assert!(result);
    }

    #[test]
    fn test_process_directory_fails_on_duplicate_entries() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "## Directory Index\n- [file1](file1) - desc\n- [file1 duplicate](file1) - desc\n",
        )
        .expect("Failed to write AGENTS.md");
        let tracked_files = vec![
            agents_path.to_string_lossy().to_string(),
            dir.path().join("file1").to_string_lossy().to_string(),
        ];

        // Act
        let result = process_directory(&agents_path, &tracked_files);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_process_directory_fails_on_mismatched_labels() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "## Directory Index\n- [Renamed file](file1) - desc\n",
        )
        .expect("Failed to write AGENTS.md");
        let tracked_files = vec![
            agents_path.to_string_lossy().to_string(),
            dir.path().join("file1").to_string_lossy().to_string(),
        ];

        // Act
        let result = process_directory(&agents_path, &tracked_files);

        // Assert
        assert!(!result);
    }

    #[test]
    fn test_get_tracked_files_error() {
        // Arrange
        let mut runner = MockCommandRunner::new();
        runner
            .expect_run()
            .returning(|_| Ok(mock_output(1, "", "error")));

        // Act
        let result = get_tracked_files(&runner);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_get_agents_files_error() {
        // Arrange
        let mut runner = MockCommandRunner::new();
        runner
            .expect_run()
            .returning(|_| Ok(mock_output(1, "", "error")));

        // Act
        let result = get_agents_files(&runner);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_run_with_runner_failure() {
        // Arrange
        let mut runner = MockCommandRunner::new();
        runner
            .expect_run()
            .times(1)
            .returning(|_| Ok(mock_output(1, "", "error")));

        // Act
        let result = run_with_runner(&runner);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_run_with_runner_success() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "## Directory Index\n- [file1](file1) - desc\n",
        )
        .expect("Failed to write AGENTS.md");

        let file1_path = dir.path().join("file1");

        let mut runner = MockCommandRunner::new();

        let tracked_files_output = format!(
            "{}\n{}",
            file1_path.to_string_lossy(),
            agents_path.to_string_lossy()
        );
        let agents_files_output = agents_path.to_string_lossy().to_string();

        runner
            .expect_run()
            .withf(|cmd| {
                let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
                args.len() == 1 && args[0] == "ls-files"
            })
            .returning(move |_| Ok(mock_output(0, &tracked_files_output, "")));

        runner
            .expect_run()
            .withf(|cmd| {
                let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
                args.len() > 1
            })
            .returning(move |_| Ok(mock_output(0, &agents_files_output, "")));

        // Act
        let result = run_with_runner(&runner);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_runner_missing_entries() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(&agents_path, "## Directory Index\n").expect("Failed to write AGENTS.md");

        let file1_path = dir.path().join("file1");
        let mut runner = MockCommandRunner::new();

        let tracked_files_output = format!(
            "{}\n{}",
            file1_path.to_string_lossy(),
            agents_path.to_string_lossy()
        );
        let agents_files_output = agents_path.to_string_lossy().to_string();

        runner
            .expect_run()
            .withf(|cmd| {
                let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
                args.len() == 1 && args[0] == "ls-files"
            })
            .returning(move |_| Ok(mock_output(0, &tracked_files_output, "")));

        runner
            .expect_run()
            .withf(|cmd| {
                let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
                args.len() > 1
            })
            .returning(move |_| Ok(mock_output(0, &agents_files_output, "")));

        // Act
        let result = run_with_runner(&runner);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_real_command_runner() {
        // Arrange
        let runner = RealCommandRunner;
        let mut cmd = Command::new("echo");
        cmd.arg("hello");

        // Act
        let result = CommandRunner::run(&runner, &mut cmd);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_missing_agents_files_reports_nested_directory() {
        // Arrange
        let tracked_files = vec![
            "AGENTS.md".to_string(),
            "src/AGENTS.md".to_string(),
            "src/runtime/mode.rs".to_string(),
        ];
        let agents_files = vec![PathBuf::from("AGENTS.md"), PathBuf::from("src/AGENTS.md")];

        // Act
        let missing_files = missing_agents_files(&tracked_files, &agents_files);

        // Assert
        assert_eq!(missing_files, vec![PathBuf::from("src/runtime/AGENTS.md")]);
    }

    #[test]
    fn test_missing_agents_files_ignores_directories_without_indexable_entries() {
        // Arrange
        let tracked_files = vec![
            "AGENTS.md".to_string(),
            "docs/AGENTS.md".to_string(),
            "docs/CLAUDE.md".to_string(),
            "docs/GEMINI.md".to_string(),
        ];
        let agents_files = vec![PathBuf::from("AGENTS.md"), PathBuf::from("docs/AGENTS.md")];

        // Act
        let missing_files = missing_agents_files(&tracked_files, &agents_files);

        // Assert
        assert!(missing_files.is_empty());
    }

    #[test]
    fn test_is_ignored_index_path_matches_tmp_home_entries() {
        // Arrange
        let root_path = ".tmp-home";
        let nested_path = ".tmp-home/.claude/debug/output.txt";
        let normal_path = "crates/agentty/src/main.rs";

        // Act
        let root_ignored = is_ignored_index_path(root_path);
        let nested_ignored = is_ignored_index_path(nested_path);
        let normal_ignored = is_ignored_index_path(normal_path);

        // Assert
        assert!(root_ignored);
        assert!(nested_ignored);
        assert!(!normal_ignored);
    }

    #[test]
    fn test_run_with_runner_ignores_tmp_home_paths() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "## Directory Index\n- [file1](file1) - desc\n",
        )
        .expect("Failed to write AGENTS.md");
        let file1_path = dir.path().join("file1");

        let tracked_files_output = format!(
            "{}\n{}\n.tmp-home/.claude/debug/log.txt",
            file1_path.to_string_lossy(),
            agents_path.to_string_lossy()
        );
        let agents_files_output = agents_path.to_string_lossy().to_string();

        let mut runner = MockCommandRunner::new();
        runner
            .expect_run()
            .withf(|cmd| {
                let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
                args.len() == 1 && args[0] == "ls-files"
            })
            .returning(move |_| Ok(mock_output(0, &tracked_files_output, "")));
        runner
            .expect_run()
            .withf(|cmd| {
                let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
                args.len() > 1
            })
            .returning(move |_| Ok(mock_output(0, &agents_files_output, "")));

        // Act
        let result = run_with_runner(&runner);

        // Assert
        assert!(result.is_ok());
    }
}
