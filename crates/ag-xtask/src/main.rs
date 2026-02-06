use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, exit};
use std::{env, fs};

#[cfg(test)]
use mockall::{automock, predicate::*};
use tracing::error;

fn main() {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = env::args().collect();
    if let Err(err) = run(&args, &RealCommandRunner) {
        error!("{err}");
        exit(1);
    }
}

#[cfg_attr(test, automock)]
trait CommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<Output>;
}

struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<Output> {
        command.output()
    }
}

fn run(_args: &[String], runner: &dyn CommandRunner) -> Result<(), String> {
    let tracked_files = get_tracked_files(runner)?;
    let agents_files = get_agents_files(runner)?;

    let mut success = true;
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
        if let Some(start) = line.find('[') {
            if let Some(end) = line[start..].find(']') {
                let name = &line[start + 1..start + end];
                indexed_files.push(name.to_string());
            }
        }
    }

    Some(indexed_files)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

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
            "crates/ag-cli/Cargo.toml".to_string(),
        ];

        // Act
        let entries = get_local_entries(directory, &tracked_files);

        // Assert
        assert_eq!(entries, vec!["Cargo.toml", "crates/", "src/"]);
    }

    #[test]
    fn test_get_local_entries_windows_paths() {
        // Arrange
        let directory = Path::new("crates/ag-cli");
        let tracked_files = vec![
            "crates\\ag-cli\\Cargo.toml".to_string(),
            "crates\\ag-cli\\src\\main.rs".to_string(),
        ];

        // Act
        let entries = get_local_entries(directory, &tracked_files);

        // Assert
        assert_eq!(entries, vec!["Cargo.toml", "src/"]);
    }

    #[test]
    fn test_get_local_entries_subdir() {
        // Arrange
        let directory = Path::new("crates/ag-cli");
        let tracked_files = vec![
            "Cargo.toml".to_string(),
            "crates/ag-cli/Cargo.toml".to_string(),
            "crates/ag-cli/src/main.rs".to_string(),
            "crates/ag-cli/AGENTS.md".to_string(),
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
    fn test_run_failure() {
        // Arrange
        let mut runner = MockCommandRunner::new();
        runner
            .expect_run()
            .times(1)
            .returning(|_| Ok(mock_output(1, "", "error"))); // Fail first command

        // Act
        let result = run(&[], &runner);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_run_success() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        // Create a valid AGENTS.md with file1 indexed
        fs::write(
            &agents_path,
            "## Directory Index\n- [file1](file1) - desc\n",
        )
        .expect("Failed to write AGENTS.md");

        let file1_path = dir.path().join("file1");

        let mut runner = MockCommandRunner::new();

        // We need to capture the paths to use in the closures
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
                // ls-files **/AGENTS.md AGENTS.md
                args.len() > 1
            })
            .returning(move |_| Ok(mock_output(0, &agents_files_output, "")));

        // Act
        let result = run(&[], &runner);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_missing_entries() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let agents_path = dir.path().join("AGENTS.md");
        // Empty index
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
        let result = run(&[], &runner);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn test_real_command_runner() {
        let runner = RealCommandRunner;
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let result = runner.run(&mut cmd);
        assert!(result.is_ok());
    }
}
