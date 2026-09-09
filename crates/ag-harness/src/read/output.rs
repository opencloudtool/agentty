use std::{fmt, io};

use serde::Serialize;
use thiserror::Error;

use crate::schema_contract;
use crate::tool::MAX_TOOL_RESULT_BYTES;

/// Bounded text returned by one successful `read` execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadOutput {
    pub(super) content: String,
    pub(super) end_line: Option<u64>,
    pub(super) next_offset: Option<u64>,
    pub(super) path: String,
    pub(super) start_line: u64,
    pub(super) truncated: bool,
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
