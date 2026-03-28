//! Showcase: Full proof pipeline from captured frames to multi-format output.
//!
//! Demonstrates building a [`ProofReport`] from simulated terminal frames,
//! attaching assertions, and rendering the report through all four proof
//! backends: frame-text, PNG strip, animated GIF, and self-contained HTML.
//!
//! Run with: `cargo run --example proof_pipeline -p testty`

#![allow(clippy::print_stdout, clippy::expect_used)]

use std::path::{Path, PathBuf};

use testty::frame::TerminalFrame;
use testty::proof::frame_text::FrameTextBackend;
use testty::proof::gif::GifBackend;
use testty::proof::html::HtmlBackend;
use testty::proof::report::ProofReport;
use testty::proof::strip::ScreenshotStripBackend;

fn main() {
    let output_path = output_dir();
    std::fs::create_dir_all(&output_path).expect("failed to create output dir");

    // Simulate a three-step user journey through a TUI application.
    let frame_startup = TerminalFrame::new(
        60,
        10,
        b"Welcome to MyApp v1.0\n\nPress Enter to continue...",
    );
    let frame_menu = TerminalFrame::new(
        60,
        10,
        b"Main Menu\n\n  [1] Dashboard\n  [2] Settings\n  [3] Quit",
    );
    let frame_dashboard = TerminalFrame::new(
        60,
        10,
        b"Dashboard\n\n  CPU: 42%   Memory: 1.2 GB\n  Uptime: 3h 14m",
    );

    // Build the proof report with labeled captures.
    let mut report = ProofReport::new("myapp_startup_journey");
    report.add_capture(
        "startup",
        "Application launched and shows welcome screen",
        &frame_startup,
    );
    report.add_capture(
        "main_menu",
        "User pressed Enter, main menu appeared",
        &frame_menu,
    );
    report.add_capture(
        "dashboard",
        "User selected Dashboard view",
        &frame_dashboard,
    );

    // Attach assertions to specific captures by label.
    report.add_assertion("startup", true, "Welcome screen is visible");
    report.add_assertion("dashboard", true, "Dashboard heading is visible");
    report.add_assertion("dashboard", true, "CPU usage is displayed");
    report.add_assertion(
        "dashboard",
        false,
        "Memory should show under 1 GB (found 1.2 GB)",
    );

    // Render through all four backends.
    println!("=== Testty Proof Pipeline Showcase ===\n");

    // 1. Frame-text backend (annotated plain text).
    let text_path = output_path.join("proof.txt");
    report
        .save(&FrameTextBackend, &text_path)
        .expect("frame-text render failed");
    println!("Frame-text proof written to: {}", text_path.display());
    print_file_preview(&text_path, 20);

    // 2. Screenshot strip backend (vertical PNG).
    let strip_path = output_path.join("proof_strip.png");
    report
        .save(&ScreenshotStripBackend, &strip_path)
        .expect("strip render failed");
    let strip_size = std::fs::metadata(&strip_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    println!(
        "\nPNG strip written to: {} ({} bytes)",
        strip_path.display(),
        strip_size
    );

    // 3. Animated GIF backend.
    let gif_path = output_path.join("proof.gif");
    let gif_backend = GifBackend::new();
    report
        .save(&gif_backend, &gif_path)
        .expect("GIF render failed");
    let gif_size = std::fs::metadata(&gif_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    println!(
        "Animated GIF written to: {} ({} bytes)",
        gif_path.display(),
        gif_size
    );

    // 4. Self-contained HTML backend.
    let html_path = output_path.join("proof.html");
    report
        .save(&HtmlBackend, &html_path)
        .expect("HTML render failed");
    let html_size = std::fs::metadata(&html_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    println!(
        "HTML report written to: {} ({} bytes)",
        html_path.display(),
        html_size
    );

    println!(
        "\n=== All four proof formats generated in {} ===",
        output_path.display()
    );
}

/// Resolve the output directory for proof artifacts.
///
/// Uses the first CLI argument if provided, otherwise defaults to
/// `./testty_proof_output/` so generated files persist after the
/// example exits.
fn output_dir() -> PathBuf {
    std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("testty_proof_output"), PathBuf::from)
}

/// Print the first `max_lines` of a text file as a preview.
fn print_file_preview(path: &Path, max_lines: usize) {
    let content = std::fs::read_to_string(path).expect("failed to read file");
    println!();
    for (index, line) in content.lines().enumerate() {
        if index >= max_lines {
            println!("  ... ({} more lines)", content.lines().count() - max_lines);
            break;
        }
        println!("  {line}");
    }
}
