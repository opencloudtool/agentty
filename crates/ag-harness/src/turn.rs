//! Public turn outcomes, observable activity, and terminal errors.

use std::fmt;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use crate::lifecycle::{ModelResponseType, TurnErrorType};
use crate::model::{CompletionMetadata, ModelError};
use crate::read::ReadError;
use crate::tool::ReadAction;
use crate::write::WriteError;

/// Successful model turn paired with observable execution activity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnOutcome {
    output: Value,
    report: TurnReport,
}

impl TurnOutcome {
    /// Returns the locally validated structured model output.
    pub fn output(&self) -> &Value {
        &self.output
    }

    /// Returns sanitized timing, model, and tool activity for the turn.
    pub fn report(&self) -> &TurnReport {
        &self.report
    }

    /// Consumes the outcome and returns its validated output.
    pub fn into_output(self) -> Value {
        self.output
    }

    pub(crate) fn new(output: Value, report: TurnReport) -> Self {
        Self { output, report }
    }
}

/// Observable, content-free activity from one successful model turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnReport {
    duration: Duration,
    model_requests: Vec<ModelRequestActivity>,
    tool_calls: Vec<ToolActivity>,
}

impl TurnReport {
    /// Returns the complete elapsed turn time.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns one entry for every provider request made during the turn.
    pub fn model_requests(&self) -> &[ModelRequestActivity] {
        &self.model_requests
    }

    /// Returns successful repository tool activity without file contents.
    pub fn tool_calls(&self) -> &[ToolActivity] {
        &self.tool_calls
    }

    pub(crate) fn new(
        duration: Duration,
        model_requests: Vec<ModelRequestActivity>,
        tool_calls: Vec<ToolActivity>,
    ) -> Self {
        Self {
            duration,
            model_requests,
            tool_calls,
        }
    }
}

/// Observable facts about one provider request in a successful turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequestActivity {
    completion: Option<CompletionMetadata>,
    duration: Duration,
    response_type: ModelResponseType,
}

impl ModelRequestActivity {
    /// Returns sanitized provider completion metadata, when available.
    pub fn completion(&self) -> Option<&CompletionMetadata> {
        self.completion.as_ref()
    }

    /// Returns the elapsed provider-request time.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns whether the request produced output, a tool call, or a rejected
    /// native continuation that the harness replayed.
    pub fn response_type(&self) -> ModelResponseType {
        self.response_type
    }

    pub(crate) fn new(
        completion: Option<CompletionMetadata>,
        duration: Duration,
        response_type: ModelResponseType,
    ) -> Self {
        Self {
            completion,
            duration,
            response_type,
        }
    }
}

/// Sanitized details about one built-in tool operation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolActivity {
    /// A bounded repository file read.
    Read {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Final included one-based line, when the file was nonempty.
        end_line: Option<u64>,
        /// Repository-relative path that was read.
        path: String,
        /// Requested one-based starting line.
        start_line: u64,
        /// Whether additional file content followed the result.
        truncated: bool,
    },
    /// A read-only repository inspection other than a worktree file read.
    ReadInspection {
        /// Selected inspection action.
        action: ReadAction,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Bounded path, query, or revision summary.
        summary: String,
    },
    /// A model-correctable repository inspection rejection returned to the
    /// model.
    ReadInspectionRejected {
        /// Selected inspection action.
        action: ReadAction,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Bounded path, query, or revision summary.
        summary: String,
    },
    /// A model-correctable repository read rejection returned to the model.
    ReadRejected {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was rejected.
        path: String,
    },
    /// A repository file write.
    Write {
        /// Number of bytes in the resulting file.
        bytes_written: usize,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was written.
        path: String,
    },
    /// A model-correctable repository write rejection returned to the model.
    WriteRejected {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was rejected.
        path: String,
    },
}

impl ToolActivity {
    /// Returns the elapsed tool-execution time.
    pub fn duration(&self) -> Duration {
        match self {
            Self::Read { duration, .. }
            | Self::ReadInspection { duration, .. }
            | Self::ReadInspectionRejected { duration, .. }
            | Self::ReadRejected { duration, .. }
            | Self::Write { duration, .. }
            | Self::WriteRejected { duration, .. } => *duration,
        }
    }

    /// Returns the bounded built-in tool name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Read { .. }
            | Self::ReadInspection { .. }
            | Self::ReadInspectionRejected { .. }
            | Self::ReadRejected { .. } => "read",
            Self::Write { .. } | Self::WriteRejected { .. } => "write",
        }
    }

    /// Returns the repository-relative target or bounded inspection summary.
    pub fn path(&self) -> &str {
        match self {
            Self::Read { path, .. }
            | Self::ReadRejected { path, .. }
            | Self::Write { path, .. }
            | Self::WriteRejected { path, .. } => path,
            Self::ReadInspection { summary, .. } | Self::ReadInspectionRejected { summary, .. } => {
                summary
            }
        }
    }
}

impl fmt::Display for ToolActivity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read {
                duration,
                end_line,
                path,
                start_line,
                truncated,
            } => {
                let path = sanitize_report_text(path);
                let lines = end_line.map_or_else(
                    || format!("line {start_line}"),
                    |end_line| format!("lines {start_line}-{end_line}"),
                );
                let continuation = if *truncated { ", truncated" } else { "" };

                write!(
                    formatter,
                    "read {path} ({lines}{continuation}; {})",
                    format_report_duration(*duration)
                )
            }
            Self::ReadInspection {
                action,
                duration,
                summary,
            } => write!(
                formatter,
                "read {} {} (completed; {})",
                action.as_str(),
                sanitize_report_text(summary),
                format_report_duration(*duration)
            ),
            Self::ReadInspectionRejected {
                action,
                duration,
                summary,
            } => write!(
                formatter,
                "read {} {} (rejected; {})",
                action.as_str(),
                sanitize_report_text(summary),
                format_report_duration(*duration)
            ),
            Self::ReadRejected { duration, path } => write!(
                formatter,
                "read {} (rejected; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
            Self::Write {
                bytes_written,
                duration,
                path,
            } => write!(
                formatter,
                "write {} ({bytes_written} bytes; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
            Self::WriteRejected { duration, path } => write!(
                formatter,
                "write {} (rejected; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
        }
    }
}

/// Failure returned by a complete harness turn.
#[derive(Debug, Error)]
pub enum TurnError {
    /// Provider request, response decoding, or terminal validation failed.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The model requested a tool unavailable under the configured policy.
    #[error("tool `{name}` is denied by policy")]
    ToolDenied {
        /// Denied native function name.
        name: String,
    },
    /// A repository read failed.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// Repository-scoped tools were enabled without a repository root.
    #[error("repository root is required when a repository tool is allowed")]
    RepositoryRequired,
    /// A repository write failed.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// The model exceeded the bounded number of calls in one turn.
    #[error("model exceeded the per-turn tool call limit of {limit}")]
    ToolCallLimit {
        /// Configured maximum calls.
        limit: usize,
    },
}

impl TurnError {
    /// Returns the stable lifecycle classification for this failure.
    pub fn error_type(&self) -> TurnErrorType {
        match self {
            Self::Model(error) => TurnErrorType::Model(error.error_type()),
            Self::ToolDenied { .. } => TurnErrorType::ToolDenied,
            Self::Read(_) | Self::Write(_) => TurnErrorType::Tool,
            Self::RepositoryRequired => TurnErrorType::RepositoryRequired,
            Self::ToolCallLimit { .. } => TurnErrorType::ToolCallLimit,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ResumeFailure {
    #[error("native provider continuation failed: {source}")]
    Native {
        #[source]
        source: ModelError,
    },
    #[error("native provider continuation was unavailable and history replay failed: {source}")]
    Replay {
        #[source]
        source: ModelError,
    },
}

impl ResumeFailure {
    pub(crate) fn into_model_error(self) -> ModelError {
        let source = match &self {
            Self::Native { source } | Self::Replay { source } => source,
        };
        if !matches!(source, ModelError::Request(_)) {
            return match self {
                Self::Native { source } | Self::Replay { source } => source,
            };
        }
        let error_type = source.error_type();
        let http_status = source.http_status();

        ModelError::classified_request(error_type, http_status, Box::new(self))
    }
}

pub(crate) fn sanitized_completion_metadata(metadata: &CompletionMetadata) -> CompletionMetadata {
    CompletionMetadata::new(
        sanitize_report_text(metadata.finish_reason()),
        metadata.response_id().map(sanitize_report_text),
        metadata.response_model().map(sanitize_report_text),
        metadata.system_fingerprint().map(sanitize_report_text),
        metadata.usage().copied(),
    )
}

pub(crate) fn sanitize_report_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn format_report_duration(duration: Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::json;

    use super::*;
    use crate::model::{CompletionUsage, ModelErrorType};

    #[test]
    fn outcome_exposes_output_and_report() {
        // Arrange
        let completion = CompletionMetadata::new(
            "stop".to_string(),
            None,
            None,
            None,
            Some(CompletionUsage::new(None, None, None, None, None, Some(1))),
        );
        let model_request = ModelRequestActivity::new(
            Some(completion.clone()),
            Duration::from_millis(2),
            ModelResponseType::Output,
        );
        let tool_call = ToolActivity::Write {
            bytes_written: 2,
            duration: Duration::from_millis(1),
            path: "output.txt".to_string(),
        };
        let output = json!({"summary": "done"});
        let outcome = TurnOutcome::new(
            output.clone(),
            TurnReport::new(
                Duration::from_millis(3),
                vec![model_request],
                vec![tool_call],
            ),
        );

        // Act and Assert
        assert_eq!(outcome.output(), &output);
        assert_eq!(outcome.report().duration(), Duration::from_millis(3));
        assert_eq!(outcome.report().model_requests().len(), 1);
        assert_eq!(outcome.report().tool_calls().len(), 1);
        let activity = &outcome.report().model_requests()[0];
        assert_eq!(activity.completion(), Some(&completion));
        assert_eq!(activity.duration(), Duration::from_millis(2));
        assert_eq!(activity.response_type(), ModelResponseType::Output);
        assert_eq!(outcome.into_output(), output);
    }

    #[test]
    fn tool_activity_display_formats_every_outcome_safely() {
        // Arrange
        let read = ToolActivity::Read {
            duration: Duration::ZERO,
            end_line: None,
            path: "empty\n\u{1b}]52;c;Y2xpcGJvYXJk\u{7}.txt".to_string(),
            start_line: 1,
            truncated: false,
        };
        let inspection = ToolActivity::ReadInspection {
            action: ReadAction::List,
            duration: Duration::from_millis(1),
            summary: ".".to_string(),
        };
        let rejected_inspection = ToolActivity::ReadInspectionRejected {
            action: ReadAction::Search,
            duration: Duration::from_millis(1),
            summary: "needle".to_string(),
        };
        let rejected_read = ToolActivity::ReadRejected {
            duration: Duration::from_millis(2),
            path: "missing.rs".to_string(),
        };
        let write = ToolActivity::Write {
            bytes_written: 4,
            duration: Duration::from_millis(3),
            path: "src/lib.rs".to_string(),
        };
        let rejected_write = ToolActivity::WriteRejected {
            duration: Duration::from_millis(4),
            path: "src/main.rs".to_string(),
        };

        // Act
        let displays = [
            read.to_string(),
            inspection.to_string(),
            rejected_inspection.to_string(),
            rejected_read.to_string(),
            write.to_string(),
            rejected_write.to_string(),
        ];

        // Assert
        assert_eq!(
            displays,
            [
                "read empty\u{fffd}\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}.txt (line 1; <1 ms)",
                "read list . (completed; 1 ms)",
                "read search needle (rejected; 1 ms)",
                "read missing.rs (rejected; 2 ms)",
                "write src/lib.rs (4 bytes; 3 ms)",
                "write src/main.rs (rejected; 4 ms)",
            ]
        );
        assert_eq!(inspection.duration(), Duration::from_millis(1));
        assert_eq!(inspection.path(), ".");
        assert_eq!(rejected_inspection.duration(), Duration::from_millis(1));
        assert_eq!(rejected_inspection.path(), "needle");
        assert_eq!(rejected_read.duration(), Duration::from_millis(2));
        assert_eq!(rejected_read.name(), "read");
        assert_eq!(rejected_read.path(), "missing.rs");
        assert_eq!(write.duration(), Duration::from_millis(3));
        assert_eq!(write.name(), "write");
        assert_eq!(write.path(), "src/lib.rs");
        assert_eq!(rejected_write.duration(), Duration::from_millis(4));
        assert_eq!(rejected_write.name(), "write");
        assert_eq!(rejected_write.path(), "src/main.rs");
    }

    #[test]
    fn resume_failure_preserves_request_context_and_http_status() {
        // Arrange
        let request_error = |status| {
            ModelError::classified_request(
                ModelErrorType::Provider,
                Some(status),
                io::Error::other("provider request failed").into(),
            )
        };
        let failures = [
            ResumeFailure::Native {
                source: request_error(429),
            },
            ResumeFailure::Replay {
                source: request_error(503),
            },
        ];

        // Act
        let errors = failures.map(ResumeFailure::into_model_error);

        // Assert
        assert_eq!(errors[0].http_status(), Some(429));
        assert_eq!(errors[1].http_status(), Some(503));
        assert!(
            errors[0]
                .to_string()
                .starts_with("model request failed: native provider continuation failed:")
        );
        assert!(errors[1].to_string().starts_with(
            "model request failed: native provider continuation was unavailable and history \
             replay failed:"
        ));
    }

    #[test]
    fn repository_required_error_has_stable_classification() {
        // Arrange
        let error = TurnError::RepositoryRequired;

        // Act
        let error_type = error.error_type();

        // Assert
        assert_eq!(error_type, TurnErrorType::RepositoryRequired);
    }
}
