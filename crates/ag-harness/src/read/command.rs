use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

use super::runtime::{MAX_READ_BYTES, MAX_SCAN_BYTES};

const MAX_COMMAND_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const REPOSITORY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) struct RepositoryCommandOutput {
    pub(super) code: Option<i32>,
    pub(super) stderr: Vec<u8>,
    pub(super) stdout: Vec<u8>,
    pub(super) truncated: bool,
}

impl RepositoryCommandOutput {
    pub(super) fn retain_complete_records(mut self, delimiter: u8) -> Self {
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
pub(super) struct BoundedStreamOutput {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(super) trait RepositoryCommandRunner: Send + Sync {
    async fn run(&self, root: &Path, arguments: &[String]) -> io::Result<RepositoryCommandOutput>;

    async fn run_large(
        &self,
        root: &Path,
        arguments: &[String],
    ) -> io::Result<RepositoryCommandOutput>;
}

pub(super) struct LocalRepositoryCommandRunner {
    git_executable: PathBuf,
}

impl LocalRepositoryCommandRunner {
    pub(super) fn new(git_executable: PathBuf) -> Self {
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

    pub(super) async fn with_timeout<T>(
        timeout: Duration,
        operation: impl Future<Output = io::Result<T>>,
    ) -> io::Result<T> {
        tokio::time::timeout(timeout, operation)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "git inspection timed out"))?
    }

    pub(super) async fn verify_repository_root(
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

    pub(super) async fn read_bounded(
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
