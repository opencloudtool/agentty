//! Approved session orchestration waves.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use testty::assertion;
use testty::frame::TerminalFrame;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{E2eResult, seed_project_settings};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Installs a prompt-aware Claude stub for the full orchestration feature
/// journey: plan, approval, concurrent child completion, and roll-up.
fn install_orchestration_claude_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
input=$(cat)
case "$input" in
  *"Generate a concise, commit-style title"*)
    result='{\"answer\":\"Coordinate parallel work\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[]}'
    ;;
  *"The user or coordinator message follows:"*"Implement the protocol review suggestions"*)
    result='{\"answer\":\"I will continue the protocol worker with the review findings.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[{\"task_key\":\"protocol\",\"title\":\"Protocol worker\",\"prompt\":\"Implement the protocol findings on the same worker branch.\",\"touched_areas\":[\"crates/ag-protocol/\"],\"acceptance_criteria\":[\"Protocol review findings are implemented and checked\"]}]}'
    ;;
  *"Orchestration verification gate"*)
    result='{\"answer\":\"All workers finished. Review and merge protocol before UI.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[],\"verification_verdicts\":[{\"reason\":\"Protocol criteria pass\",\"task_key\":\"protocol\",\"verdict\":\"pass\"},{\"reason\":\"UI criteria pass\",\"task_key\":\"ui\",\"verdict\":\"pass\"}]}'
    ;;
  *"The user or coordinator message follows:"*"Continue protocol beyond its expected areas"*)
    result='{\"answer\":\"I will route that feedback to the existing worker.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[{\"task_key\":\"protocol\",\"title\":\"Protocol worker\",\"prompt\":\"Continue protocol beyond its expected areas.\",\"touched_areas\":[\"docs/\"],\"acceptance_criteria\":[\"Apply the requested feedback\"]}]}'
    ;;
  *"The user or coordinator message follows:"*"Build protocol and UI in parallel"*)
    result='{\"answer\":\"I propose independent protocol and UI workers, merged in that order.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[{\"task_key\":\"protocol\",\"title\":\"Protocol worker\",\"prompt\":\"Implement the protocol slice.\",\"touched_areas\":[\"crates/shared/\"],\"acceptance_criteria\":[\"Protocol worker completes\"]},{\"task_key\":\"ui\",\"title\":\"UI worker\",\"prompt\":\"Implement the UI slice.\",\"touched_areas\":[\"crates/shared/\"],\"acceptance_criteria\":[\"UI worker completes\"]}]}'
    ;;
  *"Implement the protocol findings on the same worker branch"*)
    sleep 4
    result='{\"answer\":\"Protocol review suggestions implemented. Continued the existing worker and checked the findings.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[]}'
    ;;
  *"Continue protocol beyond its expected areas"*"Expected touched areas (planning references): [\"docs/\"]"*)
    sleep 4
    result='{\"answer\":\"Protocol feedback implemented beyond the expected areas and planning references.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[]}'
    ;;
  *"Task key: protocol"*)
    sleep 4
    result='{\"answer\":\"Protocol worker completed. Implemented and checked the protocol slice.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[]}'
    ;;
  *"Task key: ui"*)
    sleep 4
    result='{\"answer\":\"UI worker completed. Implemented and checked the UI slice.\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[]}'
    ;;
  *)
    result='{\"answer\":\"Ready\",\"questions\":[],\"review_comment_outcomes\":[],\"subtasks\":[]}'
    ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"result","subtype":"success","result":"%s","usage":{"input_tokens":5,"output_tokens":9}}\n' "$result"
"#;

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Verify the orchestrator proposes a durable plan for approval, fans out
/// children, reports live status, and submits a final roll-up.
#[test]
fn session_orchestration_runs_approved_parallel_wave() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_orchestration")
        .with_git()
        .setup(install_orchestration_claude_stub)
        .zola(
            "Parallel orchestration",
            "Approve an independent plan, watch workers run, and review the roll-up.",
            35,
        )
        .run(build_orchestration_scenario, |frame, report| {
            // Assert
            let picker_frame = common::frame_from_capture(&report.captures[0]);
            let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
            assertion::assert_text_in_region(&picker_frame, "Orchestrator", &picker_full);
            assertion::assert_text_in_region(&picker_frame, "[Preview] Plan workers", &picker_full);

            let approval_frame = common::frame_from_capture(&report.captures[1]);
            let approval_full = Region::full(approval_frame.cols(), approval_frame.rows());
            assertion::assert_text_in_region(
                &approval_frame,
                "Phase: AwaitingApproval",
                &approval_full,
            );
            assertion::assert_text_in_region(
                &approval_frame,
                "a approve  Enter discuss/revise",
                &approval_full,
            );

            let status_frame = common::frame_from_capture(&report.captures[2]);
            let status_full = Region::full(status_frame.cols(), status_frame.rows());
            assertion::assert_text_in_region(&status_frame, "Phase: Running", &status_full);
            assertion::assert_text_in_region(
                &status_frame,
                "Protocol worker [protocol]: running",
                &status_full,
            );
            assertion::assert_text_in_region(
                &status_frame,
                "UI worker [ui]: running",
                &status_full,
            );
            assertion::assert_match_count(&status_frame, "Phase: Running", 1);
            let protocol_status = status_frame.find_text("Protocol worker [protocol]: running");
            let ui_status = status_frame.find_text("UI worker [ui]: running");
            assert_ne!(protocol_status[0].rect.row, ui_status[0].rect.row);

            let list_frame = common::frame_from_capture(&report.captures[3]);
            assert_running_orchestration_session_list(&list_frame);
            assert_orchestration_rollup_and_references(frame, report);
        })?;

    Ok(())
}

fn build_orchestration_scenario(scenario: Scenario) -> Scenario {
    scenario
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_text("Orchestrator", 5000)
        .capture_labeled("orchestrator_picker", "Choose an orchestrator session")
        .press_key("Down")
        .press_key("Down")
        .press_key("Enter")
        .wait_for_text("Tab: focus | Enter: send", 5000)
        .write_text("Build protocol and UI in parallel")
        .press_key("Enter")
        .wait_for_text("Phase: AwaitingApproval", 30000)
        .capture_labeled(
            "plan_approval",
            "Review the independent plan before fan-out",
        )
        .press_key("a")
        .wait_for_text("Phase: Running", 10000)
        .wait_for_text("Protocol worker [protocol]: running", 10000)
        .wait_for_text("UI worker [ui]: running", 10000)
        .capture_labeled("live_status", "Monitor workers on the campaign board")
        .press_key("q")
        .wait_for_text("Phase: Running", 10000)
        .capture_labeled(
            "orchestration_sessions",
            "Workers stay grouped with their controller",
        )
        .wait_for_text("Phase: AwaitingIntegration", 30000)
        .wait_for_stable_frame(300, 5000)
        .press_key("Enter")
        .wait_for_text(
            "All workers finished. Review and merge protocol before UI.",
            5000,
        )
        .capture_labeled(
            "orchestration_rollup",
            "Review worker results and merge order",
        )
        .press_key("a")
        .wait_for_text("Integration Approach", 5000)
        .capture_labeled(
            "integration_approach",
            "Choose local merges or review requests",
        )
        .press_key("Escape")
        .wait_for_stable_frame(300, 5000)
        .press_key("Enter")
        .wait_for_text("Tab: focus | Enter: send", 5000)
        .write_text("Implement the protocol review suggestions")
        .press_key("Enter")
        .wait_for_text("Protocol worker [protocol]: continuing", 30000)
        .capture_labeled(
            "orchestration_continuation",
            "Continue a completed worker from orchestrator chat",
        )
        .wait_for_text("Phase: AwaitingIntegration", 30000)
        .capture_labeled(
            "orchestration_reverification",
            "Review the continued worker after reverification",
        )
        .press_key("Enter")
        .wait_for_text("Tab: focus | Enter: send", 5000)
        .write_text("Continue protocol beyond its expected areas")
        .press_key("Enter")
        .wait_for_text("Protocol worker [protocol]: continuing", 30000)
        .capture_labeled(
            "orchestration_reference_areas",
            "Continue work beyond its planning references",
        )
        .wait_for_text("Phase: AwaitingIntegration", 30000)
}

fn assert_orchestration_rollup_and_references(frame: &TerminalFrame, report: &ProofReport) {
    let rollup_frame = common::frame_from_capture(&report.captures[4]);
    let rollup_full = Region::full(rollup_frame.cols(), rollup_frame.rows());
    assertion::assert_text_in_region(&rollup_frame, "Phase: AwaitingIntegration", &rollup_full);
    assertion::assert_text_in_region(
        &rollup_frame,
        "All workers finished. Review and merge protocol before UI.",
        &rollup_full,
    );
    assertion::assert_text_in_region(
        &rollup_frame,
        "Protocol worker [protocol]: awaiting integration",
        &rollup_full,
    );
    assertion::assert_text_in_region(
        &rollup_frame,
        "within expected areas; verified",
        &rollup_full,
    );
    assertion::assert_not_visible(&rollup_frame, "d: diff");

    let approach_frame = common::frame_from_capture(&report.captures[5]);
    let approach_full = Region::full(approach_frame.cols(), approach_frame.rows());
    assertion::assert_text_in_region(&approach_frame, "Integration Approach", &approach_full);
    assertion::assert_text_in_region(&approach_frame, "Local merges", &approach_full);
    assertion::assert_text_in_region(&approach_frame, "Review requests", &approach_full);

    let continuation_frame = common::frame_from_capture(&report.captures[6]);
    let continuation_full = Region::full(continuation_frame.cols(), continuation_frame.rows());
    assertion::assert_text_in_region(
        &continuation_frame,
        "Protocol worker [protocol]: continuing",
        &continuation_full,
    );

    let reverification_frame = common::frame_from_capture(&report.captures[7]);
    let reverification_full =
        Region::full(reverification_frame.cols(), reverification_frame.rows());
    assertion::assert_text_in_region(
        &reverification_frame,
        "Phase: AwaitingIntegration",
        &reverification_full,
    );
    assertion::assert_text_in_region(
        &reverification_frame,
        "Protocol worker [protocol]: awaiting integration",
        &reverification_full,
    );

    let reference_frame = common::frame_from_capture(&report.captures[8]);
    let reference_full = Region::full(reference_frame.cols(), reference_frame.rows());
    assertion::assert_text_in_region(
        &reference_frame,
        "Protocol worker [protocol]: continuing",
        &reference_full,
    );

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(frame, "Phase: AwaitingIntegration", &full);
    assertion::assert_not_visible(frame, "Question 1/1");
}

/// Verifies that multiline campaign progress preserves the title column in
/// the grouped Sessions table.
fn assert_running_orchestration_session_list(frame: &TerminalFrame) {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, "Phase: Running", &full);
    assertion::assert_text_in_region(frame, "ACTIVE", &full);
    assertion::assert_match_count(frame, "[XS]", 3);
}
