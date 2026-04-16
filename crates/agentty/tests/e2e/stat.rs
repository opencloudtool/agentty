//! Stats page E2E tests: heatmap and token table rendering.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

/// Verify that the Stats tab renders the activity heatmap and token stats
/// table.
///
/// Navigates to the Stats tab and asserts that both the heatmap title
/// and token stats table title are visible in the rendered frame.
#[test]
fn stats_tab_shows_heatmap_and_tokens() {
    // Arrange, Act, Assert
    FeatureTest::new("stats_content")
        .zola(
            "Stats tab",
            "View activity heatmap and per-session token usage statistics.",
            160,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .viewing_pause_ms(3000)
                    .capture_labeled("stats_tab", "Stats tab with heatmap and token table")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Activity Heatmap", &full);
                assertion::assert_text_in_region(frame, "TokenStats", &full);
            },
        );
}

/// Verify that the Stats tab shows the summary line with session
/// and token counts.
#[test]
fn stats_footer_shows_summary() {
    // Arrange, Act, Assert
    FeatureTest::new("stats_footer")
        .zola(
            "Stats footer",
            "Stats footer shows aggregate session and token counts.",
            162,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .viewing_pause_ms(3000)
                    .capture_labeled("stats_footer", "Stats tab footer with counts")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Sessions: 0 | Input: 0 | Output: 0",
                    &full,
                );
            },
        );
}

/// Verify that the Stats tab help overlay shows stats-specific keybindings.
#[test]
fn stats_help_shows_keybindings() {
    // Arrange, Act, Assert
    FeatureTest::new("stats_help")
        .zola(
            "Stats help",
            "Press ? on the Stats tab to see stats-specific keybindings.",
            164,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Stats"))
                    .viewing_pause_ms(2000)
                    .compose(&common::open_help_overlay())
                    .viewing_pause_ms(3000)
                    .capture_labeled("stats_help", "Help overlay on Stats tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Keybindings", &full);
                assertion::assert_text_in_region(frame, "quit", &full);
            },
        );
}
