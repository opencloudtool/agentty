use std::io::{self, Cursor};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use mockall::Sequence;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, ReadBuf};

use super::command::{
    LocalRepositoryCommandRunner, MockRepositoryCommandRunner, RepositoryCommandOutput,
    RepositoryCommandRunner,
};
use super::runtime::{MAX_READ_BYTES, MAX_READ_LINES, MAX_SCAN_BYTES, MAX_UNTRACKED_DIFF_FILES};
use super::*;
use crate::file_system::{LocalFileSystem, MockFileSystem};
use crate::repository::test_git_executable;
use crate::tool::{MAX_TOOL_RESULT_BYTES, ReadArguments};

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
    let arguments =
        serde_json::from_value(json!({"action": "diff"})).expect("diff arguments should be valid");

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
    let arguments =
        serde_json::from_value(json!({"action": "diff"})).expect("diff arguments should be valid");

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
    let arguments =
        serde_json::from_value(json!({"action": "diff"})).expect("diff arguments should be valid");

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
    let arguments =
        serde_json::from_value(json!({"action": "list"})).expect("list arguments should be valid");

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
