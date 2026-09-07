use std::fmt;
use std::future::Future;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader};
use tokio::process::Command;

use crate::file_system::FileSystem;
#[cfg(test)]
use crate::repository::test_git_executable;
use crate::schema_contract;
use crate::tool::{MAX_TOOL_RESULT_BYTES, ReadAction, ReadArguments, ReadSide};

const DEFAULT_RESULT_LINES: u64 = 200;
const DEFAULT_REVIEW_BASE: &str = "main";
const MAX_COMMAND_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const MAX_READ_BYTES: usize = 50 * 1024;
const MAX_READ_LINES: u64 = 2_000;
const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;
const MAX_UNTRACKED_DIFF_FILES: usize = 100;
const REPOSITORY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct RepositoryCommandOutput {
    code: Option<i32>,
    stderr: Vec<u8>,
    stdout: Vec<u8>,
    truncated: bool,
}

impl RepositoryCommandOutput {
    fn retain_complete_records(mut self, delimiter: u8) -> Self {
        if self.truncated {
            let retained = self
                .stdout
                .iter()
                .rposition(|byte| *byte == delimiter)
                .map_or(0, |index| index + 1);
            self.stdout.truncate(retained);
        }

        self
    }
}

#[derive(Debug)]
struct BoundedStreamOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
trait RepositoryCommandRunner: Send + Sync {
    async fn run(&self, root: &Path, arguments: &[String]) -> io::Result<RepositoryCommandOutput>;

    async fn run_large(
        &self,
        root: &Path,
        arguments: &[String],
    ) -> io::Result<RepositoryCommandOutput>;
}

struct LocalRepositoryCommandRunner {
    git_executable: PathBuf,
}

impl LocalRepositoryCommandRunner {
    fn new(git_executable: PathBuf) -> Self {
        Self { git_executable }
    }
}

#[async_trait]
impl RepositoryCommandRunner for LocalRepositoryCommandRunner {
    async fn run(&self, root: &Path, arguments: &[String]) -> io::Result<RepositoryCommandOutput> {
        self.run_bounded(root, arguments, MAX_READ_BYTES).await
    }

    async fn run_large(
        &self,
        root: &Path,
        arguments: &[String],
    ) -> io::Result<RepositoryCommandOutput> {
        self.run_bounded(root, arguments, MAX_SCAN_BYTES).await
    }
}

impl LocalRepositoryCommandRunner {
    async fn run_bounded(
        &self,
        root: &Path,
        arguments: &[String],
        stdout_limit: usize,
    ) -> io::Result<RepositoryCommandOutput> {
        let verification = self
            .run_git_bounded(
                &self.git_executable,
                root,
                &["rev-parse".to_string(), "--show-toplevel".to_string()],
                MAX_COMMAND_DIAGNOSTIC_BYTES,
            )
            .await?;
        Self::verify_repository_root(root, verification).await?;

        self.run_git_bounded(&self.git_executable, root, arguments, stdout_limit)
            .await
    }

    async fn run_git_bounded(
        &self,
        git_executable: &Path,
        root: &Path,
        arguments: &[String],
        stdout_limit: usize,
    ) -> io::Result<RepositoryCommandOutput> {
        let mut command = Command::new(git_executable);
        command
            .env_clear()
            .arg("--no-pager")
            .args(["-c", "core.fsmonitor=false"])
            .args(arguments)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or(io::Error::other("git stdout was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(io::Error::other("git stderr was unavailable"))?;
        let operation = async move {
            let stdout = Self::read_bounded(stdout, stdout_limit);
            let stderr = Self::read_bounded(stderr, MAX_COMMAND_DIAGNOSTIC_BYTES);
            let (stdout, stderr) = tokio::join!(stdout, stderr);
            let (stdout, stderr) = (stdout?, stderr?);
            let status = child.wait().await?;

            Ok(RepositoryCommandOutput {
                code: status.code(),
                stderr: stderr.bytes,
                stdout: stdout.bytes,
                truncated: stdout.truncated,
            })
        };

        Self::with_timeout(REPOSITORY_COMMAND_TIMEOUT, operation).await
    }

    async fn with_timeout<T>(
        timeout: Duration,
        operation: impl Future<Output = io::Result<T>>,
    ) -> io::Result<T> {
        tokio::time::timeout(timeout, operation)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "git inspection timed out"))?
    }

    async fn verify_repository_root(
        root: &Path,
        output: RepositoryCommandOutput,
    ) -> io::Result<()> {
        if output.truncated || output.code != Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "configured repository root is not inside a Git worktree",
            ));
        }
        let top_level = std::str::from_utf8(&output.stdout)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Git root is not UTF-8"))?
            .trim_end_matches(['\n', '\r']);
        let canonical_root = tokio::fs::canonicalize(root).await?;
        let canonical_top_level = tokio::fs::canonicalize(top_level).await?;
        if !canonical_root.starts_with(&canonical_top_level) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "configured repository root is outside the selected Git worktree",
            ));
        }

        Ok(())
    }

    async fn read_bounded(
        mut reader: impl AsyncRead + Unpin,
        limit: usize,
    ) -> io::Result<BoundedStreamOutput> {
        let mut bytes = Vec::new();
        let mut buffer = vec![0_u8; 8 * 1024];
        let mut truncated = false;
        loop {
            let bytes_read = reader.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            let retained = bytes_read.min(limit.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..retained]);
            truncated |= retained < bytes_read;
        }

        Ok(BoundedStreamOutput { bytes, truncated })
    }
}

#[derive(Serialize)]
struct InspectionOutput<T> {
    action: &'static str,
    result: T,
    truncated: bool,
}

/// Bounded text returned by one successful `read` execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadOutput {
    content: String,
    end_line: Option<u64>,
    next_offset: Option<u64>,
    path: String,
    start_line: u64,
    truncated: bool,
}

impl ReadOutput {
    /// Returns the selected text with line endings normalized to `\n`.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the final included one-based line, or `None` for empty output.
    pub fn end_line(&self) -> Option<u64> {
        self.end_line
    }

    /// Returns the next one-based line to request when output was truncated.
    pub fn next_offset(&self) -> Option<u64> {
        self.next_offset
    }

    /// Returns the repository-relative path that was read.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the requested one-based starting line.
    pub fn start_line(&self) -> u64 {
        self.start_line
    }

    /// Returns whether additional file content follows this result.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub(crate) fn to_tool_result(&self) -> Result<String, ReadError> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() <= MAX_TOOL_RESULT_BYTES {
            return Ok(encoded);
        }

        let line_ends = self
            .content
            .match_indices('\n')
            .map(|(index, _)| index)
            .chain((!self.content.is_empty()).then_some(self.content.len()))
            .collect::<Vec<_>>();
        let mut fitting_lines = 0_usize;
        let mut candidate_lines = line_ends.len();
        while fitting_lines < candidate_lines {
            let midpoint = fitting_lines + (candidate_lines - fitting_lines).div_ceil(2);
            let candidate = self.with_line_prefix(&line_ends, midpoint);
            if serde_json::to_string(&candidate)?.len() <= MAX_TOOL_RESULT_BYTES {
                fitting_lines = midpoint;
            } else {
                candidate_lines = midpoint - 1;
            }
        }
        if fitting_lines == 0 {
            return Err(ReadError::LineTooLong {
                line: self.start_line,
                path: self.path.clone(),
            });
        }

        serde_json::to_string(&self.with_line_prefix(&line_ends, fitting_lines))
            .map_err(ReadError::from)
    }

    fn with_line_prefix(&self, line_ends: &[usize], lines: usize) -> Self {
        let content_end = line_ends[lines - 1];
        let lines = u64::try_from(lines).unwrap_or(u64::MAX);
        let end_line = self.start_line.checked_add(lines.saturating_sub(1));
        let next_offset = self.start_line.checked_add(lines);

        Self {
            content: self.content[..content_end].to_string(),
            end_line,
            next_offset,
            path: self.path.clone(),
            start_line: self.start_line,
            truncated: true,
        }
    }
}

/// Failure while safely executing one repository-relative read.
#[derive(Debug, Error)]
pub enum ReadError {
    /// The repository root could not be resolved.
    #[error("failed to resolve repository root: {source}")]
    RepositoryRoot {
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested path could not be resolved.
    #[error("failed to resolve read path `{path}`: {source}")]
    ResolvePath {
        /// Repository-relative requested path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The canonical requested path escapes the canonical repository root.
    #[error("read path `{path}` resolves outside the repository")]
    OutsideRepository {
        /// Repository-relative requested path.
        path: String,
    },
    /// The requested file could not be opened.
    #[error("failed to open read path `{path}`: {source}")]
    Open {
        /// Repository-relative requested path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested file could not be read.
    #[error("failed to read `{path}`: {source}")]
    Read {
        /// Repository-relative requested path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested line does not exist.
    #[error("read offset {offset} is beyond the end of `{path}`")]
    OffsetBeyondEnd {
        /// Requested one-based line.
        offset: u64,
        /// Repository-relative requested path.
        path: String,
    },
    /// A line cannot fit in the bounded tool result.
    #[error("line {line} in `{path}` exceeds the read size limit")]
    LineTooLong {
        /// One-based line whose content exceeded the cap.
        line: u64,
        /// Repository-relative requested path.
        path: String,
    },
    /// File content is not valid UTF-8 text.
    #[error("line {line} in `{path}` is not valid UTF-8")]
    InvalidUtf8 {
        /// One-based invalid line.
        line: u64,
        /// Repository-relative requested path.
        path: String,
    },
    /// The read consumed its bounded file-scan allowance.
    #[error("read of `{path}` exceeds the scan limit of {limit} bytes")]
    ScanLimitExceeded {
        /// Maximum file bytes one read may consume.
        limit: usize,
        /// Repository-relative requested path.
        path: String,
    },
    /// The successful result could not be encoded for the model.
    #[error("failed to encode read result: {0}")]
    Encode(#[from] serde_json::Error),
}

impl ReadError {
    pub(crate) fn is_model_correctable(&self) -> bool {
        !matches!(self, Self::RepositoryRoot { .. } | Self::Encode(_))
    }

    pub(crate) fn to_tool_result(&self, path: &str) -> Result<String, serde_json::Error> {
        rejected_tool_result(self, path)
    }
}

#[derive(Debug, Error)]
pub(crate) enum InspectionError {
    #[error("repository inspection failed: {source}")]
    RepositoryCommand {
        #[source]
        source: io::Error,
    },
    #[error("repository inspection was rejected: {detail}")]
    RepositoryCommandRejected { detail: String },
    #[error("repository inspection returned non-UTF-8 output")]
    InvalidUtf8,
    #[error(transparent)]
    Read(#[from] ReadError),
}

impl InspectionError {
    pub(crate) fn is_model_correctable(&self) -> bool {
        match self {
            Self::RepositoryCommand { .. } => false,
            Self::Read(error) => error.is_model_correctable(),
            Self::RepositoryCommandRejected { .. } | Self::InvalidUtf8 => true,
        }
    }

    pub(crate) fn to_tool_result(&self, path: &str) -> Result<String, serde_json::Error> {
        rejected_tool_result(self, path)
    }

    pub(crate) fn into_read_error(self, path: String) -> ReadError {
        match self {
            Self::RepositoryCommand { source } => ReadError::Read { path, source },
            Self::RepositoryCommandRejected { detail } => ReadError::Open {
                path,
                source: io::Error::other(detail),
            },
            Self::InvalidUtf8 => ReadError::InvalidUtf8 { line: 1, path },
            Self::Read(error) => error,
        }
    }
}

pub(crate) fn invalid_arguments_tool_result(
    error: &str,
    path: &str,
) -> Result<String, serde_json::Error> {
    rejected_tool_result(error, path)
}

fn rejected_tool_result(error: impl fmt::Display, path: &str) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct RejectedRead<'a> {
        error: String,
        path: &'a str,
        status: &'static str,
    }

    serde_json::to_string(&RejectedRead {
        error: schema_contract::bounded_diagnostic(error),
        path,
        status: "rejected",
    })
}

/// Bounded built-in repository inspector.
pub(crate) struct ReadTool {
    command_runner: Arc<dyn RepositoryCommandRunner>,
    file_system: Arc<dyn FileSystem>,
    repository_root: PathBuf,
}

impl ReadTool {
    /// Creates a repository reader backed by an injected filesystem.
    pub(crate) fn with_git(
        file_system: Arc<dyn FileSystem>,
        repository_root: PathBuf,
        git_executable: PathBuf,
    ) -> Self {
        Self {
            command_runner: Arc::new(LocalRepositoryCommandRunner::new(git_executable)),
            file_system,
            repository_root,
        }
    }

    #[cfg(test)]
    fn new(file_system: Arc<dyn FileSystem>, repository_root: PathBuf) -> Self {
        Self::with_git(file_system, repository_root, test_git_executable())
    }

    #[cfg(test)]
    fn with_command_runner(mut self, command_runner: Arc<dyn RepositoryCommandRunner>) -> Self {
        self.command_runner = command_runner;

        self
    }

    pub(crate) async fn execute(&self, arguments: &ReadArguments) -> Result<ReadOutput, ReadError> {
        self.execute_file(arguments, arguments.path()).await
    }

    async fn execute_file(
        &self,
        arguments: &ReadArguments,
        requested_path: &str,
    ) -> Result<ReadOutput, ReadError> {
        let root = self
            .file_system
            .canonicalize(&self.repository_root)
            .await
            .map_err(|source| ReadError::RepositoryRoot { source })?;
        let path = requested_path.to_string();
        let candidate = root.join(Path::new(&path));
        let canonical_path = self
            .file_system
            .canonicalize(&candidate)
            .await
            .map_err(|source| ReadError::ResolvePath {
                path: path.clone(),
                source,
            })?;
        if !canonical_path.starts_with(&root) || canonical_path == root {
            return Err(ReadError::OutsideRepository { path });
        }
        let file = self
            .file_system
            .open_beneath(&root, Path::new(&path))
            .await
            .map_err(|source| ReadError::Open {
                path: path.clone(),
                source,
            })?;

        Self::read(file, arguments, path, false).await
    }

    pub(crate) async fn execute_inspection(
        &self,
        arguments: &ReadArguments,
    ) -> Result<(String, String), InspectionError> {
        match arguments.action() {
            ReadAction::Diff => self.diff(arguments).await,
            ReadAction::File => {
                let output = self.execute_file(arguments, arguments.path()).await?;
                let summary = output.path().to_string();

                Ok((output.to_tool_result()?, summary))
            }
            ReadAction::List => self.list(arguments).await,
            ReadAction::Search => {
                self.search(arguments, arguments.query().unwrap_or_default())
                    .await
            }
            ReadAction::Show => {
                self.show(
                    arguments,
                    arguments.path(),
                    arguments.side().unwrap_or(ReadSide::Head),
                )
                .await
            }
        }
    }

    async fn list(&self, arguments: &ReadArguments) -> Result<(String, String), InspectionError> {
        let mut command = vec![
            "ls-files".to_string(),
            "--cached".to_string(),
            "--others".to_string(),
            "--exclude-standard".to_string(),
            "-z".to_string(),
            "--".to_string(),
        ];
        command.push(arguments.path_filter().unwrap_or(".").to_string());
        let output = self
            .run_command(command, &[0])
            .await?
            .retain_complete_records(0);
        let paths = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                std::str::from_utf8(path)
                    .map(str::to_string)
                    .map_err(|_| InspectionError::InvalidUtf8)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (paths, limited) = Self::limit_items(paths, arguments.limit());
        let truncated = limited || output.truncated;
        let summary = arguments.path_filter().unwrap_or(".").to_string();

        Ok((
            Self::bounded_items_result("list", &paths, truncated),
            summary,
        ))
    }

    async fn search(
        &self,
        arguments: &ReadArguments,
        query: &str,
    ) -> Result<(String, String), InspectionError> {
        let mut command = vec![
            "grep".to_string(),
            "--untracked".to_string(),
            "-n".to_string(),
            "-I".to_string(),
            "-F".to_string(),
            "-e".to_string(),
            query.to_string(),
            "--".to_string(),
        ];
        command.push(arguments.path_filter().unwrap_or(".").to_string());
        let output = self
            .run_command(command, &[0, 1])
            .await?
            .retain_complete_records(b'\n');
        let text = std::str::from_utf8(&output.stdout).map_err(|_| InspectionError::InvalidUtf8)?;
        let matches = text.lines().map(str::to_string).collect::<Vec<_>>();
        let (matches, limited) = Self::limit_items(matches, arguments.limit());
        let result = Self::bounded_items_result("search", &matches, limited || output.truncated);

        Ok((result, query.to_string()))
    }

    async fn diff(&self, arguments: &ReadArguments) -> Result<(String, String), InspectionError> {
        let base = DEFAULT_REVIEW_BASE;
        let root = self.repository_root().await?;
        let mut command = vec![
            "diff".to_string(),
            "--no-ext-diff".to_string(),
            "--no-textconv".to_string(),
            "--relative".to_string(),
            "--unified=20".to_string(),
            base.to_string(),
            "--".to_string(),
        ];
        command.push(arguments.path_filter().unwrap_or(".").to_string());
        let output = self
            .run_command_at(&root, command, &[0])
            .await?
            .retain_complete_records(b'\n');
        let (mut text, mut truncated) = Self::bounded_inspection_text(output)?;
        if !truncated {
            let mut untracked_command = vec![
                "ls-files".to_string(),
                "--others".to_string(),
                "--exclude-standard".to_string(),
                "-z".to_string(),
                "--".to_string(),
            ];
            untracked_command.push(arguments.path_filter().unwrap_or(".").to_string());
            let untracked_output = self
                .run_command_at(&root, untracked_command, &[0])
                .await?
                .retain_complete_records(0);
            truncated = untracked_output.truncated;
            let untracked_paths = untracked_output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| {
                    std::str::from_utf8(path)
                        .map(str::to_string)
                        .map_err(|_| InspectionError::InvalidUtf8)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if untracked_paths.len() > MAX_UNTRACKED_DIFF_FILES {
                truncated = true;
            }
            if !truncated {
                for path in untracked_paths.into_iter().take(MAX_UNTRACKED_DIFF_FILES) {
                    let command = vec![
                        "diff".to_string(),
                        "--no-index".to_string(),
                        "--no-ext-diff".to_string(),
                        "--no-textconv".to_string(),
                        "--unified=20".to_string(),
                        "--".to_string(),
                        "/dev/null".to_string(),
                        path,
                    ];
                    let output = self
                        .run_command_at(&root, command, &[0, 1])
                        .await?
                        .retain_complete_records(b'\n');
                    let (addition, addition_truncated) = Self::bounded_inspection_text(output)?;
                    let append_truncated = Self::append_bounded_diff(&mut text, &addition);
                    truncated |= addition_truncated || append_truncated;
                    if truncated {
                        break;
                    }
                }
            }
        }
        Ok((
            Self::bounded_text_result("diff", &text, truncated)?,
            base.to_string(),
        ))
    }

    fn bounded_items_result(action: &'static str, items: &[String], truncated: bool) -> String {
        let encoded = Self::encode_items_result(action, items, truncated);
        if encoded.len() <= MAX_TOOL_RESULT_BYTES {
            return encoded;
        }

        let mut fitting_items = 0_usize;
        let mut candidate_items = items.len();
        while fitting_items < candidate_items {
            let midpoint = fitting_items + (candidate_items - fitting_items).div_ceil(2);
            let candidate = Self::encode_items_result(action, &items[..midpoint], true);
            if candidate.len() <= MAX_TOOL_RESULT_BYTES {
                fitting_items = midpoint;
            } else {
                candidate_items = midpoint - 1;
            }
        }

        Self::encode_items_result(action, &items[..fitting_items], true)
    }

    fn encode_items_result(action: &'static str, items: &[String], truncated: bool) -> String {
        serde_json::json!({
            "action": action,
            "result": items,
            "truncated": truncated,
        })
        .to_string()
    }

    fn bounded_text_result(
        action: &'static str,
        text: &str,
        truncated: bool,
    ) -> Result<String, ReadError> {
        let result = InspectionOutput {
            action,
            result: &text,
            truncated,
        };
        let encoded = serde_json::to_string(&result)?;
        if encoded.len() <= MAX_TOOL_RESULT_BYTES {
            return Ok(encoded);
        }

        let boundaries = text
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .collect::<Vec<_>>();
        let mut fitting_boundary = 0_usize;
        let mut candidate_boundary = boundaries.len() - 1;
        while fitting_boundary < candidate_boundary {
            let midpoint = fitting_boundary + (candidate_boundary - fitting_boundary).div_ceil(2);
            let candidate = InspectionOutput {
                action,
                result: &text[..boundaries[midpoint]],
                truncated: true,
            };
            if serde_json::to_string(&candidate)?.len() <= MAX_TOOL_RESULT_BYTES {
                fitting_boundary = midpoint;
            } else {
                candidate_boundary = midpoint - 1;
            }
        }
        let result = InspectionOutput {
            action,
            result: &text[..boundaries[fitting_boundary]],
            truncated: true,
        };

        serde_json::to_string(&result).map_err(ReadError::from)
    }

    fn bounded_inspection_text(
        output: RepositoryCommandOutput,
    ) -> Result<(String, bool), InspectionError> {
        let mut text =
            String::from_utf8(output.stdout).map_err(|_| InspectionError::InvalidUtf8)?;
        let truncated = output.truncated || text.len() > MAX_READ_BYTES;
        if truncated {
            let mut boundary = MAX_READ_BYTES.min(text.len());
            while !text.is_char_boundary(boundary) {
                boundary -= 1;
            }
            text.truncate(boundary);
        }

        Ok((text, truncated))
    }

    fn append_bounded_diff(diff: &mut String, addition: &str) -> bool {
        let separator = usize::from(!diff.is_empty() && !diff.ends_with('\n'));
        let available = MAX_READ_BYTES.saturating_sub(diff.len());
        if separator + addition.len() <= available {
            if separator > 0 {
                diff.push('\n');
            }
            diff.push_str(addition);

            return false;
        }
        if separator > 0 && available > 0 {
            diff.push('\n');
        }
        let available = MAX_READ_BYTES.saturating_sub(diff.len());
        let mut boundary = available.min(addition.len());
        while !addition.is_char_boundary(boundary) {
            boundary -= 1;
        }
        diff.push_str(&addition[..boundary]);

        true
    }

    async fn show(
        &self,
        arguments: &ReadArguments,
        path: &str,
        side: ReadSide,
    ) -> Result<(String, String), InspectionError> {
        let revision = match side {
            ReadSide::Base => DEFAULT_REVIEW_BASE,
            ReadSide::Head => "HEAD",
        };
        let root = self.repository_root().await?;
        let prefix = self.repository_prefix(&root).await?;
        let command = vec![
            "cat-file".to_string(),
            "blob".to_string(),
            format!("{revision}:{prefix}{path}"),
        ];
        let output = self.run_large_command_at(&root, command, &[0]).await?;
        let output = Self::read(
            Box::new(Cursor::new(output.stdout)),
            arguments,
            path.to_string(),
            output.truncated,
        )
        .await?;

        Ok((output.to_tool_result()?, format!("{revision}:{path}")))
    }

    async fn repository_prefix(&self, root: &Path) -> Result<String, InspectionError> {
        let output = self
            .run_command_at(
                root,
                vec!["rev-parse".to_string(), "--show-prefix".to_string()],
                &[0],
            )
            .await?;
        if output.truncated {
            return Err(InspectionError::RepositoryCommandRejected {
                detail: "Git returned a truncated repository prefix".to_string(),
            });
        }
        let prefix = std::str::from_utf8(&output.stdout)
            .map_err(|_| InspectionError::InvalidUtf8)?
            .trim_end_matches(['\n', '\r']);
        if prefix.starts_with('/')
            || prefix.contains(['\0', '\n', '\r', '\\'])
            || prefix.split('/').any(|part| part == "..")
        {
            return Err(InspectionError::RepositoryCommandRejected {
                detail: "Git returned an invalid repository prefix".to_string(),
            });
        }

        Ok(prefix.to_string())
    }

    async fn run_command(
        &self,
        arguments: Vec<String>,
        accepted_codes: &[i32],
    ) -> Result<RepositoryCommandOutput, InspectionError> {
        let root = self.repository_root().await?;

        self.run_command_at(&root, arguments, accepted_codes).await
    }

    async fn repository_root(&self) -> Result<PathBuf, InspectionError> {
        self.file_system
            .canonicalize(&self.repository_root)
            .await
            .map_err(|source| ReadError::RepositoryRoot { source }.into())
    }

    async fn run_command_at(
        &self,
        root: &Path,
        arguments: Vec<String>,
        accepted_codes: &[i32],
    ) -> Result<RepositoryCommandOutput, InspectionError> {
        let output = self
            .command_runner
            .run(root, &arguments)
            .await
            .map_err(|source| InspectionError::RepositoryCommand { source })?;
        Self::validate_command_output(output, accepted_codes)
    }

    async fn run_large_command_at(
        &self,
        root: &Path,
        arguments: Vec<String>,
        accepted_codes: &[i32],
    ) -> Result<RepositoryCommandOutput, InspectionError> {
        let output = self
            .command_runner
            .run_large(root, &arguments)
            .await
            .map_err(|source| InspectionError::RepositoryCommand { source })?;
        Self::validate_command_output(output, accepted_codes)
    }

    fn validate_command_output(
        output: RepositoryCommandOutput,
        accepted_codes: &[i32],
    ) -> Result<RepositoryCommandOutput, InspectionError> {
        if !output
            .code
            .is_some_and(|code| accepted_codes.contains(&code))
        {
            let detail = String::from_utf8_lossy(&output.stderr);

            return Err(InspectionError::RepositoryCommandRejected {
                detail: schema_contract::bounded_diagnostic(detail.trim()),
            });
        }

        Ok(output)
    }

    fn limit_items<T>(mut items: Vec<T>, requested: Option<u64>) -> (Vec<T>, bool) {
        let limit = requested
            .unwrap_or(DEFAULT_RESULT_LINES)
            .min(MAX_READ_LINES);
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let truncated = items.len() > limit;
        items.truncate(limit);

        (items, truncated)
    }

    async fn read(
        file: Box<dyn AsyncRead + Send + Unpin>,
        arguments: &ReadArguments,
        path: String,
        source_truncated: bool,
    ) -> Result<ReadOutput, ReadError> {
        let start_line = arguments.offset().unwrap_or(1);
        let requested_lines = arguments.limit().unwrap_or(MAX_READ_LINES);
        let selected_lines = requested_lines.min(MAX_READ_LINES);
        let file: Box<dyn AsyncRead + Send + Unpin> =
            Box::new(file.take((MAX_SCAN_BYTES + 1) as u64));
        let mut reader = BufReader::new(file);
        let mut remaining_scan_bytes = MAX_SCAN_BYTES;

        Self::skip_to_line(
            &mut reader,
            start_line,
            &path,
            source_truncated,
            &mut remaining_scan_bytes,
        )
        .await?;

        let mut content = String::new();
        let mut current_line = start_line;
        let mut lines_read = 0_u64;
        let mut next_offset = None;
        while lines_read < selected_lines {
            let Some(line) =
                Self::next_line(&mut reader, current_line, &path, &mut remaining_scan_bytes)
                    .await?
            else {
                if source_truncated {
                    return Err(ReadError::ScanLimitExceeded {
                        limit: MAX_SCAN_BYTES,
                        path,
                    });
                }
                break;
            };
            let line = Self::decode_line(line, current_line, &path)?;
            let separator_bytes = usize::from(lines_read > 0);
            if content
                .len()
                .checked_add(separator_bytes)
                .and_then(|bytes| bytes.checked_add(line.len()))
                .is_none_or(|bytes| bytes > MAX_READ_BYTES)
            {
                next_offset = Some(current_line);
                break;
            }
            if separator_bytes > 0 {
                content.push('\n');
            }
            content.push_str(&line);
            lines_read += 1;
            current_line += 1;
        }

        if next_offset.is_none()
            && lines_read == selected_lines
            && (source_truncated || Self::has_more(&mut reader, &path).await?)
        {
            next_offset = Some(current_line);
        }
        if lines_read == 0 && start_line > 1 {
            return Err(ReadError::OffsetBeyondEnd {
                offset: start_line,
                path,
            });
        }
        let end_line = lines_read
            .checked_sub(1)
            .and_then(|additional_lines| start_line.checked_add(additional_lines));

        Ok(ReadOutput {
            content,
            end_line,
            next_offset,
            path,
            start_line,
            truncated: next_offset.is_some(),
        })
    }

    async fn skip_to_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        start_line: u64,
        path: &str,
        source_truncated: bool,
        remaining_scan_bytes: &mut usize,
    ) -> Result<(), ReadError> {
        let mut current_line = 1_u64;
        while current_line < start_line {
            if !Self::skip_line(reader, path, remaining_scan_bytes).await? {
                return Err(if source_truncated {
                    ReadError::ScanLimitExceeded {
                        limit: MAX_SCAN_BYTES,
                        path: path.to_string(),
                    }
                } else {
                    ReadError::OffsetBeyondEnd {
                        offset: start_line,
                        path: path.to_string(),
                    }
                });
            }
            current_line += 1;
        }

        Ok(())
    }

    async fn next_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        line: u64,
        path: &str,
        remaining_scan_bytes: &mut usize,
    ) -> Result<Option<Vec<u8>>, ReadError> {
        let mut bytes = Vec::new();
        let mut limited = (&mut *reader).take((MAX_READ_BYTES + 3) as u64);
        let bytes_read = limited
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|source| ReadError::Read {
                path: path.to_string(),
                source,
            })?;
        if bytes_read == 0 {
            return Ok(None);
        }
        Self::consume_scan_budget(remaining_scan_bytes, bytes.len(), path)?;
        let line_content_bytes = if let Some(line) = bytes.strip_suffix(b"\n") {
            line.strip_suffix(b"\r").unwrap_or(line)
        } else {
            &bytes
        };
        if line_content_bytes.len() > MAX_READ_BYTES {
            return Err(ReadError::LineTooLong {
                line,
                path: path.to_string(),
            });
        }

        Ok(Some(bytes))
    }

    async fn skip_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        path: &str,
        remaining_scan_bytes: &mut usize,
    ) -> Result<bool, ReadError> {
        let mut saw_bytes = false;
        loop {
            let (bytes_to_consume, reached_newline) = {
                let bytes = reader.fill_buf().await.map_err(|source| ReadError::Read {
                    path: path.to_string(),
                    source,
                })?;
                if bytes.is_empty() {
                    return Ok(saw_bytes);
                }
                saw_bytes = true;

                bytes
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or((bytes.len(), false), |index| (index + 1, true))
            };
            Self::consume_scan_budget(remaining_scan_bytes, bytes_to_consume, path)?;
            reader.consume(bytes_to_consume);
            if reached_newline {
                return Ok(true);
            }
        }
    }

    fn consume_scan_budget(
        remaining_scan_bytes: &mut usize,
        bytes: usize,
        path: &str,
    ) -> Result<(), ReadError> {
        if bytes > *remaining_scan_bytes {
            return Err(ReadError::ScanLimitExceeded {
                limit: MAX_SCAN_BYTES,
                path: path.to_string(),
            });
        }
        *remaining_scan_bytes -= bytes;

        Ok(())
    }

    async fn has_more(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        path: &str,
    ) -> Result<bool, ReadError> {
        reader
            .fill_buf()
            .await
            .map(|bytes| !bytes.is_empty())
            .map_err(|source| ReadError::Read {
                path: path.to_string(),
                source,
            })
    }

    fn decode_line(mut line: Vec<u8>, line_number: u64, path: &str) -> Result<String, ReadError> {
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }

        String::from_utf8(line).map_err(|_| ReadError::InvalidUtf8 {
            line: line_number,
            path: path.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use mockall::Sequence;
    use serde_json::{Value, json};
    use tokio::io::ReadBuf;

    use super::*;
    use crate::file_system::{LocalFileSystem, MockFileSystem};

    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("broken stream")))
        }
    }

    struct ContentThenFailReader {
        content: Option<Vec<u8>>,
    }

    impl AsyncRead for ContentThenFailReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let Some(content) = self.content.take() else {
                return Poll::Ready(Err(io::Error::other("broken continuation probe")));
            };
            buffer.put_slice(&content);

            Poll::Ready(Ok(()))
        }
    }

    fn arguments(mut value: serde_json::Value) -> ReadArguments {
        value
            .as_object_mut()
            .expect("read argument fixture should be an object")
            .insert("action".to_string(), serde_json::json!("file"));

        serde_json::from_value(value).expect("read arguments should be valid")
    }

    fn file_system(content: impl Into<Vec<u8>>) -> Arc<MockFileSystem> {
        file_system_reader(Box::new(Cursor::new(content.into())))
    }

    fn file_system_reader(reader: Box<dyn AsyncRead + Send + Unpin>) -> Arc<MockFileSystem> {
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .withf(|path| path == Path::new("repo"))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .withf(|path| path == Path::new("/repo/input.txt"))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/input.txt")));
        file_system
            .expect_open_beneath()
            .withf(|root, path| root == Path::new("/repo") && path == Path::new("input.txt"))
            .times(1)
            .return_once(move |_, _| Ok(reader));

        Arc::new(file_system)
    }

    fn inspection_file_system() -> Arc<MockFileSystem> {
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .withf(|path| path == Path::new("repo"))
            .times(1)
            .returning(|_| Ok(PathBuf::from("/repo")));

        Arc::new(file_system)
    }

    fn command_output(code: i32, stdout: impl Into<Vec<u8>>) -> RepositoryCommandOutput {
        RepositoryCommandOutput {
            code: Some(code),
            stderr: Vec::new(),
            stdout: stdout.into(),
            truncated: false,
        }
    }

    fn truncated_command_output(code: i32, stdout: impl Into<Vec<u8>>) -> RepositoryCommandOutput {
        RepositoryCommandOutput {
            code: Some(code),
            stderr: Vec::new(),
            stdout: stdout.into(),
            truncated: true,
        }
    }

    #[test]
    fn complete_record_retention_keeps_a_delimiter_at_the_capture_boundary() {
        // Arrange
        let output = truncated_command_output(0, b"complete\n");

        // Act
        let output = output.retain_complete_records(b'\n');

        // Assert
        assert_eq!(output.stdout, b"complete\n");
        assert!(output.truncated);
    }

    #[test]
    fn public_read_error_contract_remains_exhaustive() {
        // Arrange
        let error = ReadError::OutsideRepository {
            path: "outside.rs".to_string(),
        };

        // Act
        match &error {
            ReadError::RepositoryRoot { .. }
            | ReadError::ResolvePath { .. }
            | ReadError::OutsideRepository { .. }
            | ReadError::Open { .. }
            | ReadError::Read { .. }
            | ReadError::OffsetBeyondEnd { .. }
            | ReadError::LineTooLong { .. }
            | ReadError::InvalidUtf8 { .. }
            | ReadError::ScanLimitExceeded { .. }
            | ReadError::Encode(_) => {}
        }

        // Assert
        assert!(matches!(error, ReadError::OutsideRepository { .. }));
    }

    #[test]
    fn private_inspection_errors_map_to_compatible_read_errors() {
        // Arrange
        let errors = [
            InspectionError::RepositoryCommand {
                source: io::Error::other("command"),
            },
            InspectionError::RepositoryCommandRejected {
                detail: "rejected".to_string(),
            },
            InspectionError::InvalidUtf8,
            InspectionError::Read(ReadError::OutsideRepository {
                path: "original.rs".to_string(),
            }),
        ];

        // Act
        let command_is_correctable = errors[0].is_model_correctable();
        let errors = errors
            .into_iter()
            .map(|error| error.into_read_error("inspection".to_string()))
            .collect::<Vec<_>>();

        // Assert
        assert!(!command_is_correctable);
        assert!(matches!(&errors[0], ReadError::Read { path, .. } if path == "inspection"));
        assert!(matches!(&errors[1], ReadError::Open { path, .. } if path == "inspection"));
        assert!(matches!(
            &errors[2],
            ReadError::InvalidUtf8 { line: 1, path } if path == "inspection"
        ));
        assert!(matches!(
            &errors[3],
            ReadError::OutsideRepository { path } if path == "original.rs"
        ));
    }

    #[tokio::test]
    async fn dispatches_worktree_file_through_read_action() {
        // Arrange
        let tool = ReadTool::new(file_system("first\nsecond\n"), PathBuf::from("repo"));
        let arguments = arguments(json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let (result, summary) = tool
            .execute_inspection(&arguments)
            .await
            .expect("file action should succeed");
        let result: Value = serde_json::from_str(&result).expect("file result should be JSON");

        // Assert
        assert_eq!(summary, "input.txt");
        assert_eq!(result["content"], "first");
        assert_eq!(result["next_offset"], 2);
    }

    #[tokio::test]
    async fn lists_bounded_repository_paths_with_one_read_action() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "ls-files",
                            "--cached",
                            "--others",
                            "--exclude-standard",
                            "-z",
                            "--",
                            "crates",
                        ]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, b"crates/a.rs\0crates/b.rs\0")));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "list",
            "path": "crates",
            "limit": 1
        }))
        .expect("list arguments should be valid");

        // Act
        let (result, summary) = tool
            .execute_inspection(&arguments)
            .await
            .expect("list inspection should succeed");
        let result: Value = serde_json::from_str(&result).expect("list result should be JSON");

        // Assert
        assert_eq!(summary, "crates");
        assert_eq!(result["action"], "list");
        assert_eq!(result["result"], json!(["crates/a.rs"]));
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn searches_literal_repository_text_and_accepts_no_matches() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "grep",
                            "--untracked",
                            "-n",
                            "-I",
                            "-F",
                            "-e",
                            "ReadTool",
                            "--",
                            "crates",
                        ]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(1, Vec::new())));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "search",
            "query": "ReadTool",
            "path": "crates"
        }))
        .expect("search arguments should be valid");

        // Act
        let (result, summary) = tool
            .execute_inspection(&arguments)
            .await
            .expect("empty search should succeed");
        let result: Value = serde_json::from_str(&result).expect("search result should be JSON");

        // Assert
        assert_eq!(summary, "ReadTool");
        assert_eq!(result["result"], json!([]));
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn propagates_command_truncation_for_list_and_search() {
        // Arrange
        let mut list_runner = MockRepositoryCommandRunner::new();
        list_runner
            .expect_run()
            .times(1)
            .returning(|_, _| Ok(truncated_command_output(0, b"src/lib.rs\0partial-\xc3")));
        let list_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(list_runner));
        let list_arguments = serde_json::from_value(json!({ "action": "list" }))
            .expect("list arguments should be valid");
        let mut search_runner = MockRepositoryCommandRunner::new();
        search_runner.expect_run().times(1).returning(|_, _| {
            Ok(truncated_command_output(
                0,
                b"src/lib.rs:1:hit\npartial-\xc3",
            ))
        });
        let search_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(search_runner));
        let search_arguments = serde_json::from_value(json!({
            "action": "search",
            "query": "hit"
        }))
        .expect("search arguments should be valid");

        // Act
        let (list_result, _) = list_tool
            .execute_inspection(&list_arguments)
            .await
            .expect("truncated list should return its retained paths");
        let (search_result, _) = search_tool
            .execute_inspection(&search_arguments)
            .await
            .expect("truncated search should return its retained matches");
        let list_result: Value =
            serde_json::from_str(&list_result).expect("list result should be JSON");
        let search_result: Value =
            serde_json::from_str(&search_result).expect("search result should be JSON");

        // Assert
        assert_eq!(list_result["result"], json!(["src/lib.rs"]));
        assert_eq!(list_result["truncated"], true);
        assert_eq!(search_result["result"], json!(["src/lib.rs:1:hit"]));
        assert_eq!(search_result["truncated"], true);
    }

    #[tokio::test]
    async fn reads_host_bound_diff_with_path_filter() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        let mut sequence = Sequence::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "diff",
                            "--no-ext-diff",
                            "--no-textconv",
                            "--relative",
                            "--unified=20",
                            "main",
                            "--",
                            "crates/ag-harness",
                        ]
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, "diff --git a/file b/file\n")));
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "ls-files",
                            "--others",
                            "--exclude-standard",
                            "-z",
                            "--",
                            "crates/ag-harness",
                        ]
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "diff",
            "path": "crates/ag-harness"
        }))
        .expect("diff arguments should be valid");

        // Act
        let (result, summary) = tool
            .execute_inspection(&arguments)
            .await
            .expect("diff inspection should succeed");
        let result: Value = serde_json::from_str(&result).expect("diff result should be JSON");

        // Assert
        assert_eq!(summary, "main");
        assert_eq!(result["result"], "diff --git a/file b/file\n");
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn includes_untracked_files_in_host_bound_diff() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        let mut sequence = Sequence::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "diff",
                            "--no-ext-diff",
                            "--no-textconv",
                            "--relative",
                            "--unified=20",
                            "main",
                            "--",
                            ".",
                        ]
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, "tracked\n")));
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "ls-files",
                            "--others",
                            "--exclude-standard",
                            "-z",
                            "--",
                            ".",
                        ]
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, b"new.rs\0")));
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo")
                    && arguments
                        == [
                            "diff",
                            "--no-index",
                            "--no-ext-diff",
                            "--no-textconv",
                            "--unified=20",
                            "--",
                            "/dev/null",
                            "new.rs",
                        ]
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(1, "untracked\n")));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({"action": "diff"}))
            .expect("diff arguments should be valid");

        // Act
        let (result, _) = tool
            .execute_inspection(&arguments)
            .await
            .expect("diff inspection should succeed");
        let result: Value = serde_json::from_str(&result).expect("diff result should be JSON");

        // Assert
        assert_eq!(result["result"], "tracked\nuntracked\n");
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn bounds_large_diffs_and_untracked_path_discovery() {
        // Arrange
        let mut large_diff_runner = MockRepositoryCommandRunner::new();
        large_diff_runner
            .expect_run()
            .times(1)
            .returning(|_, _| Ok(truncated_command_output(0, b"complete\npartial-\xc3")));
        let large_diff_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(large_diff_runner));
        let mut untracked_runner = MockRepositoryCommandRunner::new();
        let mut sequence = Sequence::new();
        untracked_runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        let untracked_paths = (0..=MAX_UNTRACKED_DIFF_FILES)
            .flat_map(|index| format!("file-{index}.rs\0").into_bytes())
            .collect::<Vec<_>>();
        untracked_runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .return_once(move |_, _| Ok(command_output(0, untracked_paths)));
        let untracked_tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(untracked_runner));
        let arguments = serde_json::from_value(json!({"action": "diff"}))
            .expect("diff arguments should be valid");

        // Act
        let (large_result, _) = large_diff_tool
            .execute_inspection(&arguments)
            .await
            .expect("large diff should be bounded");
        let (untracked_result, _) = untracked_tool
            .execute_inspection(&arguments)
            .await
            .expect("large untracked set should be bounded");
        let large_result: Value =
            serde_json::from_str(&large_result).expect("large diff result should be JSON");
        let untracked_result: Value =
            serde_json::from_str(&untracked_result).expect("untracked result should be JSON");

        // Assert
        assert_eq!(large_result["result"], "complete\n");
        assert_eq!(large_result["truncated"], true);
        assert_eq!(untracked_result["result"], "");
        assert_eq!(untracked_result["truncated"], true);
    }

    #[tokio::test]
    async fn stops_untracked_diff_collection_after_a_truncated_patch() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        let mut sequence = Sequence::new();
        runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, b"large.rs\0ignored.rs\0")));
        runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(truncated_command_output(1, b"complete\npartial-\xc3")));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({"action": "diff"}))
            .expect("diff arguments should be valid");

        // Act
        let (result, _) = tool
            .execute_inspection(&arguments)
            .await
            .expect("truncated untracked diff should be bounded");
        let result: Value = serde_json::from_str(&result).expect("diff result should be JSON");

        // Assert
        assert_eq!(result["result"], "complete\n");
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn bounded_diff_helpers_preserve_utf8_and_separate_patches() {
        // Arrange
        let mut oversized = "x".repeat(MAX_READ_BYTES - 1);
        oversized.push('é');
        let mut joined = "tracked".to_string();
        let mut nearly_full = "x".repeat(MAX_READ_BYTES - 2);
        let addition = "éé";

        // Act
        let (bounded, truncated) =
            ReadTool::bounded_inspection_text(command_output(0, oversized.into_bytes()))
                .expect("UTF-8 diff should remain valid");
        let joined_truncated = ReadTool::append_bounded_diff(&mut joined, "untracked");
        let full_truncated = ReadTool::append_bounded_diff(&mut nearly_full, addition);

        // Assert
        assert_eq!(bounded.len(), MAX_READ_BYTES - 1);
        assert!(truncated);
        assert_eq!(joined, "tracked\nuntracked");
        assert!(!joined_truncated);
        assert_eq!(nearly_full.len(), MAX_READ_BYTES - 1);
        assert!(nearly_full.ends_with('\n'));
        assert!(full_truncated);
    }

    #[test]
    fn bounds_escaping_heavy_inspection_results_after_json_encoding() {
        // Arrange
        let items = (0..1_000).map(|_| "\u{1}".repeat(100)).collect::<Vec<_>>();
        let text = "\u{1}".repeat(MAX_READ_BYTES);

        // Act
        let items = ReadTool::bounded_items_result("list", &items, false);
        let text =
            ReadTool::bounded_text_result("diff", &text, false).expect("text result should encode");
        let items_value: Value = serde_json::from_str(&items).expect("items should be JSON");
        let text_value: Value = serde_json::from_str(&text).expect("text should be JSON");

        // Assert
        assert!(items.len() <= MAX_TOOL_RESULT_BYTES);
        assert!(text.len() <= MAX_TOOL_RESULT_BYTES);
        assert_eq!(items_value["truncated"], true);
        assert_eq!(text_value["truncated"], true);
    }

    #[tokio::test]
    async fn shows_selected_lines_from_base_revision() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        runner
            .expect_run_large()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["cat-file", "blob", "main:src/lib.rs"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, "one\ntwo\nthree\n")));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "base",
            "path": "src/lib.rs",
            "offset": 2,
            "limit": 1
        }))
        .expect("show arguments should be valid");

        // Act
        let (result, summary) = tool
            .execute_inspection(&arguments)
            .await
            .expect("show inspection should succeed");
        let result: Value = serde_json::from_str(&result).expect("show result should be JSON");

        // Assert
        assert_eq!(summary, "main:src/lib.rs");
        assert_eq!(result["content"], "two");
        assert_eq!(result["start_line"], 2);
        assert_eq!(result["end_line"], 2);
        assert_eq!(result["next_offset"], 3);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn shows_head_revision_and_rejects_an_offset_beyond_end() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        runner
            .expect_run_large()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["cat-file", "blob", "HEAD:src/lib.rs"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, "one\n")));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "src/lib.rs",
            "offset": 2
        }))
        .expect("show arguments should be valid");

        // Act
        let error = tool
            .execute_inspection(&arguments)
            .await
            .expect_err("offset beyond the revision file should fail");

        // Assert
        assert!(matches!(
            error,
            InspectionError::Read(ReadError::OffsetBeyondEnd { offset: 2, path })
                if path == "src/lib.rs"
        ));
    }

    #[tokio::test]
    async fn shows_revision_file_beyond_normal_command_capture_limit() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        runner
            .expect_run_large()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["cat-file", "blob", "HEAD:large.txt"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, "123456789\n".repeat(6_000))));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "large.txt",
            "offset": 5500,
            "limit": 1
        }))
        .expect("show arguments should be valid");

        // Act
        let (result, _) = tool
            .execute_inspection(&arguments)
            .await
            .expect("a later revision-file page should be readable");
        let result: Value = serde_json::from_str(&result).expect("show result should be JSON");

        // Assert
        assert_eq!(result["content"], "123456789");
        assert_eq!(result["start_line"], 5500);
        assert_eq!(result["end_line"], 5500);
        assert_eq!(result["next_offset"], 5501);
    }

    #[tokio::test]
    async fn reports_scan_limit_when_revision_page_exceeds_large_capture() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        runner.expect_run_large().times(1).returning(|_, _| {
            let mut source = "x\n".repeat(MAX_SCAN_BYTES / 2 + 1).into_bytes();
            source.truncate(MAX_SCAN_BYTES);

            Ok(truncated_command_output(0, source))
        });
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "very-large.txt",
            "offset": u64::try_from(MAX_SCAN_BYTES / 2).unwrap_or(u64::MAX) + 2,
            "limit": 1
        }))
        .expect("large show arguments should be valid");

        // Act
        let error = tool
            .execute_inspection(&arguments)
            .await
            .expect_err("paging beyond a truncated capture should fail safely");

        // Assert
        assert!(matches!(
            error,
            InspectionError::Read(ReadError::ScanLimitExceeded { limit, path })
                if limit == MAX_SCAN_BYTES && path == "very-large.txt"
        ));
    }

    #[tokio::test]
    async fn reports_scan_limit_when_revision_capture_ends_during_page() {
        // Arrange
        let arguments = arguments(json!({
            "path": "large.txt",
            "limit": 2
        }));

        // Act
        let error = ReadTool::read(
            Box::new(Cursor::new(b"one\n")),
            &arguments,
            "large.txt".to_string(),
            true,
        )
        .await
        .expect_err("truncated revision capture should not look complete");

        // Assert
        assert!(matches!(
            error,
            ReadError::ScanLimitExceeded { limit, path }
                if limit == MAX_SCAN_BYTES && path == "large.txt"
        ));
    }

    #[tokio::test]
    async fn show_scopes_tree_path_to_configured_subdirectory_root() {
        // Arrange
        let tool = ReadTool::new(
            Arc::new(LocalFileSystem),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        );
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "Cargo.toml",
            "limit": 12
        }))
        .expect("show arguments should be valid");

        // Act
        let (result, summary) = tool
            .execute_inspection(&arguments)
            .await
            .expect("subdirectory-root show should succeed");
        let result: Value = serde_json::from_str(&result).expect("show result should be JSON");

        // Assert
        assert_eq!(summary, "HEAD:Cargo.toml");
        assert!(
            result["content"]
                .as_str()
                .is_some_and(|content| content.contains("name = \"ag-harness\""))
        );
        assert!(
            result["content"]
                .as_str()
                .is_none_or(|content| !content.contains("[workspace]"))
        );
    }

    #[tokio::test]
    async fn rejects_invalid_git_prefix_before_reading_revision_file() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
            })
            .times(1)
            .returning(|_, _| Ok(command_output(0, "../\n")));
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "Cargo.toml"
        }))
        .expect("show arguments should be valid");

        // Act
        let error = tool
            .execute_inspection(&arguments)
            .await
            .expect_err("invalid Git prefix should be rejected");

        // Assert
        assert!(matches!(
            error,
            InspectionError::RepositoryCommandRejected { detail }
                if detail == "Git returned an invalid repository prefix"
        ));
    }

    #[tokio::test]
    async fn rejects_truncated_git_prefix_before_reading_revision_file() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner
            .expect_run()
            .withf(|root, arguments| {
                root == Path::new("/repo") && arguments == ["rev-parse", "--show-prefix"]
            })
            .times(1)
            .returning(|_, _| Ok(truncated_command_output(0, b"crates/ag-harness/")));
        runner.expect_run_large().times(0);
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "Cargo.toml"
        }))
        .expect("show arguments should be valid");

        // Act
        let error = tool
            .execute_inspection(&arguments)
            .await
            .expect_err("truncated Git prefix should be rejected");

        // Assert
        assert!(matches!(
            error,
            InspectionError::RepositoryCommandRejected { detail }
                if detail == "Git returned a truncated repository prefix"
        ));
    }

    #[tokio::test]
    async fn truncated_repository_command_still_rejects_failed_status() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        runner.expect_run().times(1).returning(|_, _| {
            Ok(RepositoryCommandOutput {
                code: Some(2),
                stderr: b"invalid revision".to_vec(),
                stdout: vec![b'x'; MAX_READ_BYTES],
                truncated: true,
            })
        });
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({"action": "list"}))
            .expect("list arguments should be valid");

        // Act
        let error = tool
            .execute_inspection(&arguments)
            .await
            .expect_err("rejected Git command should fail");

        // Assert
        assert!(matches!(
            error,
            InspectionError::RepositoryCommandRejected { detail } if detail == "invalid revision"
        ));
    }

    #[tokio::test]
    async fn large_repository_command_rejection_returns_bounded_diagnostic() {
        // Arrange
        let mut runner = MockRepositoryCommandRunner::new();
        let mut sequence = Sequence::new();
        runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(command_output(0, Vec::new())));
        runner
            .expect_run_large()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| {
                Ok(RepositoryCommandOutput {
                    code: Some(2),
                    stderr: b"invalid object".to_vec(),
                    stdout: Vec::new(),
                    truncated: false,
                })
            });
        let tool = ReadTool::new(inspection_file_system(), PathBuf::from("repo"))
            .with_command_runner(Arc::new(runner));
        let arguments = serde_json::from_value(json!({
            "action": "show",
            "side": "head",
            "path": "missing.rs"
        }))
        .expect("show arguments should be valid");

        // Act
        let error = tool
            .execute_inspection(&arguments)
            .await
            .expect_err("rejected Git object read should fail");

        // Assert
        assert!(matches!(
            error,
            InspectionError::RepositoryCommandRejected { detail } if detail == "invalid object"
        ));
    }

    #[tokio::test]
    async fn local_repository_runner_executes_bounded_read_only_git_command() {
        // Arrange
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let arguments = [
            "ls-files".to_string(),
            "--".to_string(),
            "Cargo.toml".to_string(),
        ];

        // Act
        let output = LocalRepositoryCommandRunner::new(test_git_executable())
            .run(root, &arguments)
            .await
            .expect("read-only Git command should run");

        // Assert
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, b"Cargo.toml\n");
        assert!(!output.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn local_repository_runner_ignores_untrusted_process_configuration() {
        // Arrange
        let test_executable = std::env::current_exe().expect("test executable should be available");
        let fake_directory = tempfile::Builder::new()
            .prefix("ag-harness-fake-git-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .expect("fake Git directory should be created beneath the inspected repository");
        let fake_git = fake_directory
            .path()
            .join(format!("git{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&fake_git, "#!/bin/sh\nexit 97\n")
            .expect("fake Git executable should be created");
        let mut permissions = std::fs::metadata(&fake_git)
            .expect("fake Git metadata should be available")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_git, permissions)
            .expect("fake Git executable permissions should be installed");
        let inherited_path = std::env::var_os("PATH").expect("test PATH should be configured");
        let git_executable = test_git_executable();
        let path = std::env::join_paths(
            [PathBuf::from("."), fake_directory.path().to_path_buf()]
                .into_iter()
                .chain(std::env::split_paths(&inherited_path)),
        )
        .expect("test PATH should be valid");

        // Act
        let output = std::process::Command::new(test_executable)
            .args([
                "--ignored",
                "--exact",
                "read::tests::local_repository_runner_environment_subprocess",
            ])
            .env("GIT_DIR", "missing-git-dir")
            .env("GIT_WORK_TREE", "/")
            .env("GIT_INDEX_FILE", "missing-index")
            .env("AG_HARNESS_TEST_GIT", git_executable)
            .env("PATH", path)
            .output()
            .expect("isolated Git environment test should run");
        let standard_error = String::from_utf8_lossy(&output.stderr);

        // Assert
        assert!(
            output.status.success(),
            "isolated Git environment test failed: {standard_error}"
        );
    }

    #[tokio::test]
    #[ignore = "run by local_repository_runner_ignores_untrusted_process_configuration"]
    async fn local_repository_runner_environment_subprocess() {
        // Arrange
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let arguments = ["rev-parse".to_string(), "--show-prefix".to_string()];
        let git_executable = std::env::var_os("AG_HARNESS_TEST_GIT")
            .map(PathBuf::from)
            .expect("trusted test Git executable should be configured");

        // Act
        let output = LocalRepositoryCommandRunner::new(git_executable)
            .run(root, &arguments)
            .await
            .expect("sanitized Git inspection should run");

        // Assert
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, b"crates/ag-harness/\n");
    }

    #[tokio::test]
    async fn repository_verification_rejects_root_outside_selected_worktree() {
        // Arrange
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let unrelated_root = root
            .parent()
            .expect("crate directory should have a parent")
            .join("ag-agent");
        let output = command_output(0, format!("{}\n", unrelated_root.display()));

        // Act
        let error = LocalRepositoryCommandRunner::verify_repository_root(root, output)
            .await
            .expect_err("outside worktree root should be rejected");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn repository_verification_rejects_failed_discovery() {
        // Arrange
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let output = command_output(1, Vec::new());

        // Act
        let error = LocalRepositoryCommandRunner::verify_repository_root(root, output)
            .await
            .expect_err("failed Git discovery should be rejected");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn treats_repository_path_filters_as_literal() {
        // Arrange
        let tool = ReadTool::new(
            Arc::new(LocalFileSystem),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        );
        let arguments = serde_json::from_value(json!({
            "action": "list",
            "path": ":(top)Cargo.toml"
        }))
        .expect("literal list arguments should be valid");

        // Act
        let (result, _) = tool
            .execute_inspection(&arguments)
            .await
            .expect("literal path inspection should succeed");
        let result: Value = serde_json::from_str(&result).expect("list result should be JSON");

        // Assert
        assert_eq!(result["result"], json!([]));
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn local_repository_runner_bounds_large_git_output() {
        // Arrange
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let arguments = [
            "cat-file".to_string(),
            "blob".to_string(),
            "HEAD:Cargo.lock".to_string(),
        ];

        // Act
        let output = LocalRepositoryCommandRunner::new(test_git_executable())
            .run(root, &arguments)
            .await
            .expect("large read-only Git command should be bounded");

        // Assert
        assert_eq!(output.stdout.len(), MAX_READ_BYTES);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn local_repository_runner_reports_timeout_and_stream_failures() {
        // Arrange
        let stalled = std::future::pending::<io::Result<()>>();

        // Act
        let timeout = LocalRepositoryCommandRunner::with_timeout(Duration::ZERO, stalled).await;
        let stream_error = LocalRepositoryCommandRunner::read_bounded(FailingReader, 1).await;

        // Assert
        assert_eq!(
            timeout.expect_err("stalled command should time out").kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            stream_error
                .expect_err("failing stream should be reported")
                .kind(),
            io::ErrorKind::Other
        );
    }

    #[tokio::test]
    async fn bounded_stream_reader_drains_bytes_after_retention_limit() {
        // Arrange
        let content = b"retained-and-drained".to_vec();
        let mut reader = Cursor::new(content.clone());

        // Act
        let output = LocalRepositoryCommandRunner::read_bounded(&mut reader, 8)
            .await
            .expect("bounded stream should be readable");

        // Assert
        assert_eq!(output.bytes, b"retained");
        assert!(output.truncated);
        assert_eq!(reader.position(), content.len() as u64);
    }

    #[tokio::test]
    async fn reads_requested_lines_and_reports_continuation() {
        // Arrange
        let tool = ReadTool::new(file_system("one\r\ntwo\nthree\nfour\n"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2,
            "limit": 2
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("bounded read should succeed");

        // Assert
        assert_eq!(output.content(), "two\nthree");
        assert_eq!(output.path(), "input.txt");
        assert_eq!(output.start_line(), 2);
        assert_eq!(output.end_line(), Some(3));
        assert_eq!(output.next_offset(), Some(4));
        assert!(output.truncated());
        assert_eq!(
            output.to_tool_result().expect("output should serialize"),
            r#"{"content":"two\nthree","end_line":3,"next_offset":4,"path":"input.txt","start_line":2,"truncated":true}"#
        );
    }

    #[tokio::test]
    async fn bounds_serialized_read_result_with_escaping_heavy_content() {
        // Arrange
        let content = format!("{}\n", "\u{1}".repeat(100)).repeat(480);
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("raw read should succeed");
        let result = output
            .to_tool_result()
            .expect("encoded read should be bounded");
        let result_value: Value =
            serde_json::from_str(&result).expect("bounded read result should be JSON");

        // Assert
        assert!(result.len() <= MAX_TOOL_RESULT_BYTES);
        assert_eq!(result_value["truncated"], true);
        assert!(
            result_value["next_offset"]
                .as_u64()
                .is_some_and(|offset| offset > 1)
        );
    }

    #[tokio::test]
    async fn rejects_one_escaping_heavy_line_that_cannot_fit_encoded_result() {
        // Arrange
        let tool = ReadTool::new(file_system("\u{1}".repeat(20_000)), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("raw line should fit the read limit");
        let error = output
            .to_tool_result()
            .expect_err("encoded line should be rejected without aborting the turn");

        // Assert
        assert!(matches!(
            error,
            ReadError::LineTooLong { line: 1, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn reads_empty_file_without_truncation() {
        // Arrange
        let tool = ReadTool::new(file_system(Vec::new()), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("empty file should be readable");

        // Assert
        assert_eq!(output.content(), "");
        assert_eq!(output.end_line(), None);
        assert_eq!(output.next_offset(), None);
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn preserves_leading_and_consecutive_blank_lines() {
        // Arrange
        let tool = ReadTool::new(file_system("\n\nvalue\n\n"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 4
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("blank lines should be preserved");

        // Assert
        assert_eq!(output.content(), "\n\nvalue\n");
        assert_eq!(output.start_line(), 1);
        assert_eq!(output.end_line(), Some(4));
        assert_eq!(output.next_offset(), None);
    }

    #[tokio::test]
    async fn reads_to_exact_end_without_truncation() {
        // Arrange
        let tool = ReadTool::new(file_system("one\ntwo"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 2
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("complete bounded read should succeed");

        // Assert
        assert_eq!(output.content(), "one\ntwo");
        assert_eq!(output.end_line(), Some(2));
        assert_eq!(output.next_offset(), None);
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn caps_requested_line_count() {
        // Arrange
        let line_count =
            usize::try_from(MAX_READ_LINES + 1).expect("read line limit should fit the platform");
        let content = "line\n".repeat(line_count);
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": u64::MAX
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("line-bounded read should succeed");

        // Assert
        assert_eq!(output.end_line(), Some(MAX_READ_LINES));
        assert_eq!(output.next_offset(), Some(MAX_READ_LINES + 1));
        assert!(output.truncated());
    }

    #[tokio::test]
    async fn bounds_output_by_bytes() {
        // Arrange
        let first_line = "a".repeat(MAX_READ_BYTES - 1);
        let content = format!("{first_line}\nsecond\n");
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("byte-bounded read should succeed");

        // Assert
        assert_eq!(output.content(), first_line);
        assert_eq!(output.next_offset(), Some(2));
        assert!(output.truncated());
    }

    #[tokio::test]
    async fn accepts_exact_byte_limit_before_lf() {
        // Arrange
        let expected = "x".repeat(MAX_READ_BYTES);
        let tool = ReadTool::new(file_system(format!("{expected}\n")), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("line at the normalized byte limit should succeed");

        // Assert
        assert_eq!(output.content(), expected);
        assert_eq!(output.end_line(), Some(1));
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn accepts_exact_byte_limit_before_crlf() {
        // Arrange
        let expected = "x".repeat(MAX_READ_BYTES);
        let tool = ReadTool::new(file_system(format!("{expected}\r\n")), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("CRLF line at the normalized byte limit should succeed");

        // Assert
        assert_eq!(output.content(), expected);
        assert_eq!(output.end_line(), Some(1));
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn does_not_validate_unrequested_oversized_line() {
        // Arrange
        let content = format!("one\n{}", "x".repeat(MAX_READ_BYTES + 1));
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("unrequested line should only be probed for presence");

        // Assert
        assert_eq!(output.content(), "one");
        assert_eq!(output.end_line(), Some(1));
        assert_eq!(output.next_offset(), Some(2));
        assert!(output.truncated());
    }

    #[tokio::test]
    async fn skips_unrequested_oversized_prefix_line() {
        // Arrange
        let content = format!("{}\nvalue\n", "x".repeat(MAX_READ_BYTES + 1));
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2,
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("unrequested prefix line should be discarded");

        // Assert
        assert_eq!(output.content(), "value");
        assert_eq!(output.start_line(), 2);
        assert_eq!(output.end_line(), Some(2));
        assert_eq!(output.next_offset(), None);
    }

    #[tokio::test]
    async fn rejects_reads_that_exceed_scan_budget() {
        // Arrange
        let tool = ReadTool::new(file_system(vec![b'x'; MAX_SCAN_BYTES + 1]), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("prefix scan beyond the byte budget should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::ScanLimitExceeded { limit, path }
                if limit == MAX_SCAN_BYTES && path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn reports_continuation_probe_failure() {
        // Arrange
        let reader = ContentThenFailReader {
            content: Some(b"one\n".to_vec()),
        };
        let tool = ReadTool::new(file_system_reader(Box::new(reader)), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("failed continuation probe should fail the read");

        // Assert
        assert!(matches!(
            error,
            ReadError::Read { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::Other
        ));
    }

    #[tokio::test]
    async fn reports_failure_while_skipping_prefix() {
        // Arrange
        let tool = ReadTool::new(file_system_reader(Box::new(FailingReader)), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("failed prefix discard should fail the read");

        // Assert
        assert!(matches!(
            error,
            ReadError::Read { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::Other
        ));
    }

    #[tokio::test]
    async fn rejects_offset_beyond_end() {
        // Arrange
        let tool = ReadTool::new(file_system("one\n"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 3
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("out-of-range offset should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::OffsetBeyondEnd { offset: 3, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_offset_after_unterminated_final_line() {
        // Arrange
        let tool = ReadTool::new(file_system("one"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("offset after an unterminated final line should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::OffsetBeyondEnd { offset: 2, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_line_without_unbounded_read() {
        // Arrange
        let tool = ReadTool::new(file_system(vec![b'x'; MAX_READ_BYTES + 1]), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("oversized line should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::LineTooLong { line: 1, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_utf8() {
        // Arrange
        let tool = ReadTool::new(file_system(vec![0xff, b'\n']), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("invalid UTF-8 should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::InvalidUtf8 { line: 1, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_path_that_resolves_outside_repository() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/outside/input.txt")));
        file_system.expect_open_beneath().times(0);
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("escaping canonical path should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::OutsideRepository { path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_path_that_resolves_to_repository_root() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(2)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system.expect_open_beneath().times(0);
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("repository directory should not be readable as a file");

        // Assert
        assert!(matches!(
            error,
            ReadError::OutsideRepository { path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn reports_path_resolution_failure() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing file")));
        file_system.expect_open_beneath().times(0);
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("missing file should fail path resolution");

        // Assert
        assert!(matches!(
            error,
            ReadError::ResolvePath { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::NotFound
        ));
    }

    #[tokio::test]
    async fn reports_file_open_failure() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/input.txt")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "permission denied",
                ))
            });
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("unopenable file should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::Open { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[tokio::test]
    async fn reports_file_read_failure() {
        // Arrange
        let tool = ReadTool::new(file_system_reader(Box::new(FailingReader)), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("broken stream should fail the read");

        // Assert
        assert!(matches!(
            error,
            ReadError::Read { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::Other
        ));
    }
}
