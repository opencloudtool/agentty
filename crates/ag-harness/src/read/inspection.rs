use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::command::RepositoryCommandOutput;
use super::runtime::{
    DEFAULT_RESULT_LINES, DEFAULT_REVIEW_BASE, MAX_READ_BYTES, MAX_READ_LINES,
    MAX_UNTRACKED_DIFF_FILES, ReadTool,
};
use super::{InspectionError, ReadError};
use crate::schema_contract;
use crate::tool::{MAX_TOOL_RESULT_BYTES, ReadArguments, ReadSide};

#[derive(Serialize)]
struct InspectionOutput<T> {
    action: &'static str,
    result: T,
    truncated: bool,
}

impl ReadTool {
    pub(super) async fn list(
        &self,
        arguments: &ReadArguments,
    ) -> Result<(String, String), InspectionError> {
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

    pub(super) async fn search(
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

    pub(super) async fn diff(
        &self,
        arguments: &ReadArguments,
    ) -> Result<(String, String), InspectionError> {
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

    pub(super) fn bounded_items_result(
        action: &'static str,
        items: &[String],
        truncated: bool,
    ) -> String {
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

    pub(super) fn bounded_text_result(
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

    pub(super) fn bounded_inspection_text(
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

    pub(super) fn append_bounded_diff(diff: &mut String, addition: &str) -> bool {
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

    pub(super) async fn show(
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
}
