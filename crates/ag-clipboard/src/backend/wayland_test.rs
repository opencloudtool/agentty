use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use mockall::predicate;

use super::*;

#[test]
fn test_run_successful_reports_backend_failure_for_unsuccessful_command() {
    // Arrange
    let mut runner = MockWaylandCommandRunner::new();
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--list-types"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: false,
                stderr: b"compositor rejected request\n".to_vec(),
                stdout: Vec::new(),
            })
        });
    let clipboard = WaylandClipboard::with_runner(Box::new(runner));

    // Act
    let result = clipboard.run_successful(
        WL_PASTE_LIST_TYPES_ARGS,
        "failed to list Wayland clipboard types",
    );

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason == "failed to list Wayland clipboard types: compositor rejected request"
    ));
}

#[test]
fn test_read_text_prefers_utf8_plain_text_payload() {
    // Arrange
    let mut runner = MockWaylandCommandRunner::new();
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--list-types"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"text/html\ntext/plain;charset=utf-8\n".to_vec(),
            })
        });
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(
                args,
                &["--no-newline", "--type", "text/plain;charset=utf-8"],
            )
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"/tmp/image.png".to_vec(),
            })
        });
    let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

    // Act
    let text = clipboard
        .read_text()
        .expect("mocked text payload should decode");

    // Assert
    assert_eq!(text, "/tmp/image.png");
}

#[test]
fn test_read_text_reports_backend_failure_for_invalid_utf8() {
    // Arrange
    let mut runner = MockWaylandCommandRunner::new();
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--list-types"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"text/plain;charset=utf-8\n".to_vec(),
            })
        });
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(
                args,
                &["--no-newline", "--type", "text/plain;charset=utf-8"],
            )
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: vec![0xFF],
            })
        });
    let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

    // Act
    let result = clipboard.read_text();

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to decode Wayland clipboard text as UTF-8")
    ));
}

#[test]
fn test_read_file_list_reads_wayland_uri_list_payload() {
    // Arrange
    let mut runner = MockWaylandCommandRunner::new();
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--list-types"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"text/uri-list\ntext/plain;charset=utf-8\n".to_vec(),
            })
        });
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--no-newline", "--type", "text/uri-list"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"file:///tmp/image%201.png\r\n".to_vec(),
            })
        });
    let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

    // Act
    let paths = clipboard
        .read_file_list()
        .expect("mocked URI list should parse");

    // Assert
    assert_eq!(paths, vec![PathBuf::from("/tmp/image 1.png")]);
}

#[test]
fn test_read_image_rgba_reads_wayland_png_payload() {
    // Arrange
    let mut runner = MockWaylandCommandRunner::new();
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--list-types"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"image/png\ntext/plain;charset=utf-8\n".to_vec(),
            })
        });
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--no-newline", "--type", "image/png"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: test_png_bytes(),
            })
        });
    let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

    // Act
    let image_data = clipboard
        .read_image_rgba()
        .expect("mocked PNG payload should decode");

    // Assert
    assert_eq!(image_data.width, 1);
    assert_eq!(image_data.height, 1);
    assert_eq!(image_data.rgba_bytes, vec![255, 0, 0, 255]);
}

#[test]
fn test_read_image_rgba_reports_content_unavailable_without_png_mime() {
    // Arrange
    let mut runner = MockWaylandCommandRunner::new();
    runner
        .expect_run()
        .once()
        .with(predicate::function(|args: &[String]| {
            args_match(args, &["--list-types"])
        }))
        .returning(|_| {
            Ok(WaylandCommandOutput {
                status_success: true,
                stderr: Vec::new(),
                stdout: b"text/plain;charset=utf-8\n".to_vec(),
            })
        });
    let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

    // Act
    let result = clipboard.read_image_rgba();

    // Assert
    assert!(matches!(result, Err(ClipboardError::ContentUnavailable)));
}

#[test]
fn test_parse_mime_types_trims_blank_lines() {
    // Arrange
    let stdout = b"\n image/png \n\ntext/plain;charset=utf-8\n";

    // Act
    let mime_types = WaylandClipboard::parse_mime_types(stdout);

    // Assert
    assert_eq!(
        mime_types,
        vec![
            "image/png".to_string(),
            "text/plain;charset=utf-8".to_string()
        ]
    );
}

#[test]
fn test_wl_paste_failure_reason_uses_stderr_when_present() {
    // Arrange
    let stderr = b"compositor does not support data-control\n";

    // Act
    let reason =
        WaylandClipboard::wl_paste_failure_reason("failed to list Wayland clipboard types", stderr);

    // Assert
    assert_eq!(
        reason,
        "failed to list Wayland clipboard types: compositor does not support data-control"
    );
}

#[test]
fn test_wl_paste_failure_reason_describes_empty_stderr() {
    // Arrange
    let stderr = b"";

    // Act
    let reason =
        WaylandClipboard::wl_paste_failure_reason("failed to list Wayland clipboard types", stderr);

    // Assert
    assert_eq!(
        reason,
        "failed to list Wayland clipboard types: `wl-paste` exited unsuccessfully"
    );
}

fn test_png_bytes() -> Vec<u8> {
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG should encode");

    png_bytes
}

fn args_match(args: &[String], expected: &[&str]) -> bool {
    args.iter().map(String::as_str).eq(expected.iter().copied())
}
