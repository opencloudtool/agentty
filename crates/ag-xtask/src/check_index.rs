use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// Runs repository-wide `AGENTS.md` directory index validation.
pub(crate) fn run() -> Result<(), String> {
    run_with_runner(&RealCommandRunner)
}

fn run_with_runner(runner: &dyn CommandRunner) -> Result<(), String> {
    let tracked_files = get_tracked_files(runner)?;
    let agents_files = get_agents_files(runner)?;

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
        return Err(
            "Index check failed. Some files are missing from AGENTS.md indexes.".to_string(),
        );
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

    let Some(indexed_files) = parse_index(&content) else {
        error!(
            "Error: No '## Directory Index' section in {}",
            agents_path.display()
        );
        return false;
    };

    let mut missing = Vec::new();
    for entry in all_entries {
        if !indexed_files.contains(&entry) {
            missing.push(entry);
        }
    }

    if missing.is_empty() {
        return true;
    }

    error!(
        "Error: {} is missing entries: {}",
        agents_path.display(),
        missing.join(", ")
    );

    false
}

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
            } else if rel_path != "AGENTS.md" && rel_path != "CLAUDE.md" && rel_path != "GEMINI.md"
            {
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

/// Parses the `Directory Index` section of an `AGENTS.md` file.
///
/// The parser collects markdown link destinations from index bullets so
/// formatter changes to link text do not affect validation.
fn parse_index(content: &str) -> Option<Vec<String>> {
    let index_header = "## Directory Index";
    let header_pos = content.find(index_header)?;

    let index_section = &content[header_pos..];
    let mut indexed_files = Vec::new();
    let mut first_line = true;

    for line in index_section.split('\n') {
        if !first_line && line.starts_with("##") {
            break;
        }
        first_line = false;

        if let Some(destination) = parse_index_entry_destination(line) {
            indexed_files.push(destination);
        }
    }

    Some(indexed_files)
}

/// Parses a markdown index bullet and returns the link destination.
///
/// The destination is the canonical path used for index validation because
/// the link text may be escaped or formatted (for example, `\_index.md` or
/// `` `_index.md` ``) by markdown formatters.
fn parse_index_entry_destination(line: &str) -> Option<String> {
    let link_start = line.find('[')?;
    let destination_start_offset = line[link_start..].find("](")?;
    let destination_start = link_start + destination_start_offset + 2;
    let destination_end_offset = line[destination_start..].find(')')?;
    let destination_end = destination_start + destination_end_offset;
    let destination = line[destination_start..destination_end].trim();

    if destination.is_empty() {
        return None;
    }

    Some(destination.to_string())
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
                       Desc\n\n## Next Section";

        // Act
        let files = parse_index(content).expect("Failed to parse index");

        // Assert
        assert_eq!(files, vec!["file1", "dir1/"]);
    }

    #[test]
    fn test_parse_index_prefers_destination_over_escaped_link_text() {
        // Arrange
        let content = "## Directory Index\n- [\\_index.md](_index.md) - Homepage content.\n";

        // Act
        let files = parse_index(content).expect("Failed to parse index");

        // Assert
        assert_eq!(files, vec!["_index.md"]);
    }

    #[test]
    fn test_parse_index_prefers_destination_over_code_formatted_link_text() {
        // Arrange
        let content = "## Directory Index\n- [`_index.md`](_index.md) - Homepage content.\n";

        // Act
        let files = parse_index(content).expect("Failed to parse index");

        // Assert
        assert_eq!(files, vec!["_index.md"]);
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
}
