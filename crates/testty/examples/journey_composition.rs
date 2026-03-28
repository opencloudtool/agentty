//! Showcase: Composable journeys and scenario building.
//!
//! Demonstrates how to build reusable [`Journey`] blocks, compose them
//! into [`Scenario`] instances, and build scenarios from raw steps.
//!
//! Run with: `cargo run --example journey_composition -p testty`

#![allow(clippy::print_stdout)]

use testty::journey::Journey;
use testty::scenario::Scenario;
use testty::step::Step;

fn main() {
    println!("=== Testty Journey Composition Showcase ===\n");

    // --- Part 1: Building reusable journeys ---
    println!("--- Part 1: Reusable Journey Building Blocks ---\n");

    let startup = Journey::wait_for_startup(300, 5000);
    println!(
        "  Journey '{}': {} step(s) — {}",
        startup.name,
        startup.steps.len(),
        startup.description.as_deref().unwrap_or("(no description)")
    );

    let navigate_settings = Journey::navigate_with_key("Tab", "Settings", 3000);
    println!(
        "  Journey '{}': {} step(s) — {}",
        navigate_settings.name,
        navigate_settings.steps.len(),
        navigate_settings
            .description
            .as_deref()
            .unwrap_or("(no description)")
    );

    let type_search = Journey::type_and_confirm("hello world");
    println!(
        "  Journey '{}': {} step(s) — {}",
        type_search.name,
        type_search.steps.len(),
        type_search
            .description
            .as_deref()
            .unwrap_or("(no description)")
    );

    let dismiss_dialog = Journey::press_and_wait("Escape", 200);
    println!(
        "  Journey '{}': {} step(s) — {}",
        dismiss_dialog.name,
        dismiss_dialog.steps.len(),
        dismiss_dialog
            .description
            .as_deref()
            .unwrap_or("(no description)")
    );

    let snapshot = Journey::capture_labeled("final_state", "Application final state");
    println!(
        "  Journey '{}': {} step(s) — {}",
        snapshot.name,
        snapshot.steps.len(),
        snapshot
            .description
            .as_deref()
            .unwrap_or("(no description)")
    );

    // --- Part 2: Composing scenarios from journeys ---
    println!("\n--- Part 2: Scenario Composition ---\n");

    let quick_scenario = Scenario::new("smoke_startup")
        .compose(&startup)
        .capture_labeled("launched", "App reached stable state");

    println!(
        "  Scenario '{}': {} steps",
        quick_scenario.name,
        quick_scenario.steps.len(),
    );

    let nav_scenario = Scenario::new("settings_navigation")
        .compose(&startup)
        .compose(&navigate_settings)
        .capture_labeled("settings_visible", "Settings tab is active")
        .compose(&dismiss_dialog);

    println!(
        "  Scenario '{}': {} steps",
        nav_scenario.name,
        nav_scenario.steps.len(),
    );

    let full_scenario = Scenario::new("full_workflow")
        .compose(&startup)
        .compose(&navigate_settings)
        .capture_labeled("settings", "Navigated to settings")
        .compose(&type_search)
        .capture_labeled("searched", "Typed and confirmed search")
        .compose(&dismiss_dialog)
        .compose(&snapshot);

    println!(
        "  Scenario '{}': {} steps",
        full_scenario.name,
        full_scenario.steps.len(),
    );

    // --- Part 3: Build a scenario from raw steps ---
    println!("\n--- Part 3: Raw Step Building ---\n");

    let manual_scenario = Scenario::new("manual_test")
        .step(Step::wait_for_stable_frame(200, 3000))
        .step(Step::write_text("ls -la"))
        .step(Step::press_key("Enter"))
        .step(Step::wait_for_text("total", 5000))
        .step(Step::capture_labeled(
            "listing",
            "Directory listing visible",
        ));

    println!(
        "  Scenario '{}': {} steps (built from raw steps)",
        manual_scenario.name,
        manual_scenario.steps.len(),
    );

    println!("\n=== Journey composition showcase complete! ===");
}
