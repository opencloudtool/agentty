use std::path::PathBuf;
use std::sync::Arc;

use super::command::{LocalRepositoryCommandRunner, RepositoryCommandRunner};
use super::{InspectionError, ReadError, ReadOutput};
use crate::file_system::FileSystem;
#[cfg(test)]
use crate::repository::test_git_executable;
use crate::tool::{ReadAction, ReadArguments, ReadSide};

pub(super) const DEFAULT_RESULT_LINES: u64 = 200;
pub(super) const DEFAULT_REVIEW_BASE: &str = "main";
pub(super) const MAX_READ_BYTES: usize = 50 * 1024;
pub(super) const MAX_READ_LINES: u64 = 2_000;
pub(super) const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_UNTRACKED_DIFF_FILES: usize = 100;

/// Bounded built-in repository inspector.
pub(crate) struct ReadTool {
    pub(super) command_runner: Arc<dyn RepositoryCommandRunner>,
    pub(super) file_system: Arc<dyn FileSystem>,
    pub(super) repository_root: PathBuf,
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
    pub(super) fn new(file_system: Arc<dyn FileSystem>, repository_root: PathBuf) -> Self {
        Self::with_git(file_system, repository_root, test_git_executable())
    }

    #[cfg(test)]
    pub(super) fn with_command_runner(
        mut self,
        command_runner: Arc<dyn RepositoryCommandRunner>,
    ) -> Self {
        self.command_runner = command_runner;

        self
    }

    pub(crate) async fn execute(&self, arguments: &ReadArguments) -> Result<ReadOutput, ReadError> {
        self.execute_file(arguments, arguments.path()).await
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
}
