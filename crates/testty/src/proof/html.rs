//! HTML report proof backend.
//!
//! [`HtmlBackend`] generates a self-contained HTML file with embedded
//! base64-encoded frame images, step-by-step narrative, diff summaries,
//! and assertion results. The output can be opened in any browser or
//! uploaded as a CI artifact.

use std::fmt::Write;
use std::io::Cursor;
use std::path::Path;

use base64::Engine;
use image::ImageFormat;

use super::backend::ProofBackend;
use super::report::{ProofCapture, ProofError, ProofReport};
use crate::frame::TerminalFrame;
use crate::renderer;

/// Renders a proof report as a self-contained HTML file.
///
/// Frame images are base64-encoded and inlined as `<img>` tags. Diff
/// summaries and assertion results are displayed alongside each step.
pub struct HtmlBackend;

impl ProofBackend for HtmlBackend {
    /// Render the proof report as self-contained HTML.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if rendering or writing fails.
    fn render(&self, report: &ProofReport, output: &Path) -> Result<(), ProofError> {
        let html = build_html(report)?;
        std::fs::write(output, html)?;

        Ok(())
    }
}

/// Build the complete HTML document from a proof report.
fn build_html(report: &ProofReport) -> Result<String, ProofError> {
    let mut html = String::with_capacity(8192);

    write_html_header(&mut html, &report.scenario_name);

    for (index, capture) in report.captures.iter().enumerate() {
        let step_number = index + 1;
        let diff_summary = if index > 0 && index - 1 < report.diffs.len() {
            Some(report.diffs[index - 1].summary())
        } else {
            None
        };

        write_step_card(&mut html, step_number, capture, diff_summary.as_deref())?;
    }

    write_html_footer(&mut html, report.captures.len());

    Ok(html)
}

/// Write the HTML document header with inline CSS.
fn write_html_header(html: &mut String, scenario_name: &str) {
    let escaped_name = escape_html(scenario_name);

    let _ = write!(
        html,
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Proof Report: {escaped_name}</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; background: #1a1a2e; color: #e0e0e0; margin: 0; padding: 20px; }}
h1 {{ color: #00d4ff; border-bottom: 2px solid #00d4ff; padding-bottom: 10px; }}
.step-card {{ background: #16213e; border: 1px solid #0f3460; border-radius: 8px; margin: 20px 0; padding: 20px; }}
.step-header {{ display: flex; align-items: center; gap: 12px; margin-bottom: 15px; }}
.step-number {{ background: #00d4ff; color: #1a1a2e; font-weight: bold; padding: 4px 12px; border-radius: 4px; font-size: 14px; }}
.step-label {{ color: #e94560; font-weight: bold; font-size: 16px; }}
.step-desc {{ color: #a0a0a0; font-size: 14px; }}
.frame-img {{ max-width: 100%; border: 1px solid #0f3460; border-radius: 4px; }}
.diff-section {{ background: #0d1b2a; padding: 12px; border-radius: 4px; margin-top: 12px; font-size: 13px; }}
.diff-section h4 {{ color: #ffd700; margin: 0 0 8px 0; }}
.diff-item {{ color: #b0b0b0; padding: 2px 0; }}
.assertions {{ margin-top: 12px; }}
.assertion {{ padding: 4px 0; font-size: 13px; }}
.pass {{ color: #00e676; }}
.pass::before {{ content: "\2713 "; }}
.fail {{ color: #ff5252; }}
.fail::before {{ content: "\2717 "; }}
.footer {{ text-align: center; color: #606060; margin-top: 30px; padding-top: 15px; border-top: 1px solid #0f3460; font-size: 12px; }}
.terminal-info {{ color: #606060; font-size: 12px; margin-bottom: 8px; }}
</style>
</head>
<body>
<h1>Proof Report: {escaped_name}</h1>
"#
    );
}

/// Write a single step card with frame image, diff, and assertions.
fn write_step_card(
    html: &mut String,
    step_number: usize,
    capture: &ProofCapture,
    diff_summary: Option<&[String]>,
) -> Result<(), ProofError> {
    let _ = writeln!(html, "<div class=\"step-card\">");
    let _ = writeln!(html, "<div class=\"step-header\">");
    let _ = writeln!(
        html,
        "<span class=\"step-number\">Step {step_number}</span>"
    );
    let _ = writeln!(
        html,
        "<span class=\"step-label\">[{}]</span>",
        escape_html(&capture.label)
    );
    let _ = writeln!(
        html,
        "<span class=\"step-desc\">{}</span>",
        escape_html(&capture.description)
    );
    let _ = writeln!(html, "</div>");

    // Terminal dimensions.
    let _ = writeln!(
        html,
        "<div class=\"terminal-info\">Terminal: {}x{}</div>",
        capture.cols, capture.rows
    );

    // Rendered frame image as base64.
    let base64_image = render_capture_to_base64(capture)?;
    let _ = writeln!(
        html,
        "<img class=\"frame-img\" src=\"data:image/png;base64,{base64_image}\" alt=\"Step \
         {step_number}: {}\" />",
        escape_html(&capture.label)
    );

    // Diff summary.
    if let Some(summaries) = diff_summary
        && !summaries.is_empty()
    {
        let _ = writeln!(html, "<div class=\"diff-section\">");
        let _ = writeln!(html, "<h4>Changes from previous step</h4>");
        for summary_line in summaries {
            let _ = writeln!(
                html,
                "<div class=\"diff-item\">{}</div>",
                escape_html(summary_line)
            );
        }
        let _ = writeln!(html, "</div>");
    }

    // Assertion results.
    if !capture.assertions.is_empty() {
        let _ = writeln!(html, "<div class=\"assertions\">");
        for assertion in &capture.assertions {
            let css_class = if assertion.passed { "pass" } else { "fail" };
            let _ = writeln!(
                html,
                "<div class=\"assertion {css_class}\">{}</div>",
                escape_html(&assertion.description)
            );
        }
        let _ = writeln!(html, "</div>");
    }

    let _ = writeln!(html, "</div>");

    Ok(())
}

/// Write the HTML document footer.
fn write_html_footer(html: &mut String, capture_count: usize) {
    let _ = write!(
        html,
        r#"<div class="footer">Total captures: {capture_count} | Generated by testty</div>
</body>
</html>
"#
    );
}

/// Render a capture's frame to a base64-encoded PNG string.
fn render_capture_to_base64(capture: &ProofCapture) -> Result<String, ProofError> {
    let frame = TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes);
    let image = renderer::render_to_image(&frame);

    let mut png_bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, ImageFormat::Png)
        .map_err(|err| ProofError::Format(err.to_string()))?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes.into_inner());

    Ok(encoded)
}

/// Escape special HTML characters.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_contains_step_cards() {
        // Arrange
        let frame = TerminalFrame::new(20, 3, b"Hello HTML");
        let mut report = ProofReport::new("html_test");
        report.add_capture("init", "Initial state", &frame);
        report.add_capture("done", "Final state", &frame);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert!(html.contains("Step 1"));
        assert!(html.contains("Step 2"));
        assert!(html.contains("[init]"));
        assert!(html.contains("[done]"));
        assert!(html.contains("Initial state"));
        assert!(html.contains("Final state"));
    }

    #[test]
    fn html_contains_embedded_images() {
        // Arrange
        let frame = TerminalFrame::new(10, 2, b"Img");
        let mut report = ProofReport::new("image_test");
        report.add_capture("snap", "Snapshot", &frame);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert!(html.contains("data:image/png;base64,"));
    }

    #[test]
    fn html_contains_assertion_results() {
        // Arrange
        let frame = TerminalFrame::new(20, 3, b"Test");
        let mut report = ProofReport::new("assert_html");
        report.add_capture("check", "Check", &frame);
        report.add_assertion("check", true, "text visible");
        report.add_assertion("check", false, "color match");

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert!(html.contains("class=\"assertion pass\""));
        assert!(html.contains("class=\"assertion fail\""));
        assert!(html.contains("text visible"));
        assert!(html.contains("color match"));
    }

    #[test]
    fn html_contains_diff_summaries() {
        // Arrange
        let frame_a = TerminalFrame::new(20, 3, b"Before");
        let frame_b = TerminalFrame::new(20, 3, b"After!");
        let mut report = ProofReport::new("diff_html");
        report.add_capture("before", "Before", &frame_a);
        report.add_capture("after", "After", &frame_b);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert
        assert!(html.contains("Changes from previous step"));
    }

    #[test]
    fn html_is_self_contained() {
        // Arrange
        let frame = TerminalFrame::new(10, 2, b"SC");
        let mut report = ProofReport::new("self_contained");
        report.add_capture("snap", "Snapshot", &frame);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — has doctype, head with style, body, and closing tags.
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<style>"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn html_backend_writes_file() {
        // Arrange
        let frame = TerminalFrame::new(10, 2, b"File");
        let mut report = ProofReport::new("file_test");
        report.add_capture("snap", "Snapshot", &frame);

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = temp_dir.path().join("report.html");

        // Act
        let backend = HtmlBackend;
        backend
            .render(&report, &output_path)
            .expect("render should succeed");

        // Assert
        assert!(output_path.exists());
        let content = std::fs::read_to_string(&output_path).expect("failed to read");
        assert!(content.contains("Proof Report: file_test"));
    }

    #[test]
    fn escape_html_handles_special_chars() {
        // Arrange / Act / Assert
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a&b"), "a&amp;b");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn html_escapes_scenario_name_in_header() {
        // Arrange
        let frame = TerminalFrame::new(10, 2, b"X");
        let mut report = ProofReport::new("<script>alert('xss')</script>");
        report.add_capture("snap", "Snapshot", &frame);

        // Act
        let html = build_html(&report).expect("should build HTML");

        // Assert — raw script tag must not appear.
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
