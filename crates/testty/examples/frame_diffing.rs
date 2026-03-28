//! Showcase: Frame diffing engine for detecting terminal state changes.
//!
//! Demonstrates computing cell-level diffs between terminal frames,
//! extracting changed regions, and generating human-readable summaries.
//! The diff engine powers automatic change detection in proof reports.
//!
//! Run with: `cargo run --example frame_diffing -p testty`

#![allow(clippy::print_stdout)]

use testty::diff::{CellChange, FrameDiff};
use testty::frame::TerminalFrame;

fn main() {
    println!("=== Testty Frame Diffing Showcase ===\n");

    // --- Example 1: Identical frames ---
    println!("--- Example 1: Identical Frames ---");
    let frame_a = TerminalFrame::new(40, 5, b"Hello, World!\nStatus: OK");
    let frame_b = TerminalFrame::new(40, 5, b"Hello, World!\nStatus: OK");
    let diff = FrameDiff::compute(&frame_a, &frame_b);

    println!("  Identical: {}", diff.is_identical());
    println!("  Summary: {:?}", diff.summary());
    println!();

    // --- Example 2: Text content change ---
    println!("--- Example 2: Text Content Change ---");
    let before = TerminalFrame::new(40, 5, b"Counter: 0\nStatus: idle");
    let after = TerminalFrame::new(40, 5, b"Counter: 42\nStatus: running");
    let diff = FrameDiff::compute(&before, &after);

    println!("  Identical: {}", diff.is_identical());
    println!("  Summary: {:?}", diff.summary());

    let regions = diff.changed_regions();
    println!("  Changed regions: {}", regions.len());
    for region in &regions {
        println!(
            "    Row {}, cols {}..{}: {:?}",
            region.region.row,
            region.region.col,
            region.region.col + region.region.width,
            region.change_type,
        );
    }
    println!();

    // --- Example 3: Multi-line update simulating a dashboard refresh ---
    println!("--- Example 3: Dashboard Refresh ---");
    let dashboard_before = TerminalFrame::new(
        50,
        6,
        b"Dashboard\n  CPU: 23%\n  Mem: 512 MB\n  Disk: 45%\n  Net: 1.2 Mbps\nLast update: 10:30",
    );
    let dashboard_after = TerminalFrame::new(
        50,
        6,
        b"Dashboard\n  CPU: 67%\n  Mem: 1.1 GB\n  Disk: 45%\n  Net: 3.4 Mbps\nLast update: 10:31",
    );
    let diff = FrameDiff::compute(&dashboard_before, &dashboard_after);

    println!("  Summary: {:?}", diff.summary());

    let regions = diff.changed_regions();
    println!("  Changed regions: {}", regions.len());
    for region in &regions {
        println!(
            "    Row {}, cols {}..{}: {:?}",
            region.region.row,
            region.region.col,
            region.region.col + region.region.width,
            region.change_type,
        );
    }
    println!();

    // --- Example 4: Per-cell inspection ---
    println!("--- Example 4: Per-Cell Inspection ---");
    let line_before = TerminalFrame::new(10, 1, b"ABCDE");
    let line_after = TerminalFrame::new(10, 1, b"AbCdE");
    let diff = FrameDiff::compute(&line_before, &line_after);

    print!("  Cell changes: ");
    for col in 0..5 {
        let change = diff.cell_change(0, col);
        let marker = match change {
            Some(CellChange::Unchanged) | None => '.',
            Some(CellChange::TextChanged) => 'T',
            Some(CellChange::StyleChanged) => 'S',
            Some(CellChange::BothChanged) => 'B',
        };
        print!("{marker}");
    }
    println!("  (. = unchanged, T = text changed)");

    println!("\n=== Frame diffing showcase complete! ===");
}
