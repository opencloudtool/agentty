//! Session model, permission, and response settings.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::test_support;
use testty::assertion;
use testty::region::Region;

use super::fixture::{
    E2eResult, seed_auto_address_review_mode, seed_project_settings, seed_review_ready_session,
    seed_review_ready_session_on_sessions_tab,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// Visible confirmation emitted only when Codex receives unrestricted Auto
/// Edit policies at both app-server request boundaries.
const CODEX_AUTO_EDIT_POLICY_CONFIRMED_TEXT: &str = "Codex Auto Edit unrestricted policy applied.";

/// Installs a Codex app-server stub that verifies Auto Edit policy payloads.
fn seed_codex_auto_edit_policy_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let codex_path = env.stub_bin.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi

extract_id() {
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

thread_policy_matches=false
while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*)
            case "$request" in
                *'"approvalPolicy":"never"'*'"sandbox":"danger-full-access"'*)
                    thread_policy_matches=true
                    ;;
            esac
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"thread":{"id":"policy-thread"}}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            answer='Codex policy test title.'
            case "$request" in
                *'Generate a concise, commit-style title'*)
                    ;;
                *'Verify Codex Auto Edit permissions'*)
                    answer='Codex Auto Edit policy mismatch.'
                    case "$request" in
                        *'"approvalPolicy":"never"'*'"sandboxPolicy":{"type":"dangerFullAccess"}'*)
                            if [ "$thread_policy_matches" = true ]; then
                                answer='Codex Auto Edit unrestricted policy applied.'
                            fi
                            ;;
                    esac
                    ;;
            esac
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"turn":{"id":"policy-turn"}}}\n' "$request_id"
            printf '%s\n' '{"method":"turn/started","params":{"turn":{"id":"policy-turn"}}}'
            printf '{"method":"item/completed","params":{"threadId":"policy-thread","turnId":"policy-turn","item":{"type":"agentMessage","id":"policy-answer","text":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"review_comment_outcomes\\":[],\\"subtasks\\":[],\\"verification_verdicts\\":[]}","phase":"final_answer"}}}\n' "$answer"
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"policy-thread","turn":{"id":"policy-turn","status":"completed","items":[]}}}'
            ;;
    esac
done
"#;
    std::fs::write(&codex_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "codex"),
            ("DefaultSmartModel", "gpt-5.6-sol"),
        ],
    )
}

/// Seeds a review-ready session with Detailed response style selected so the
/// semantic run and GIF replay remain idempotent.
fn seed_detailed_response_style_session(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_on_sessions_tab(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_response_style(
                "review-shortcut-0001",
                agentty::domain::agent::ResponseStyle::Detailed,
            )
            .await
    })?;

    Ok(())
}

/// Seeds one review-ready session with a worktree-local personality.
fn seed_session_personality(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let session_folder =
        test_support::session_folder(&env.agentty_root.join("wt"), "review-shortcut-0001");
    let personality_directory = session_folder
        .join(".agents")
        .join("agents")
        .join("reviewer");
    std::fs::create_dir_all(&personality_directory)?;
    std::fs::write(
        personality_directory.join("agent.md"),
        "---\nid: reviewer\nname: Code Reviewer\ndescription: Reviews code carefully\nrole: \
         delegation-target\nenabled: true\n---\nReview every change for correctness.",
    )?;

    Ok(())
}

/// Verify that slash-command filtering can match text contained inside a
/// command name, not only command prefixes.
#[test]
fn model_slash_command_contains_match_is_visible() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("model_slash_command_contains_match")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("/")
                    .write_text("o")
                    .wait_for_text("/model", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "model_slash_command_contains_match",
                        "`/model` appears for contained slash-command input",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "/model", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Choose an agent and model for this session.",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify `/mode` selects a session permission mode from the composer.
#[test]
fn session_permission_mode_selection() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_permission_mode_selection")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_auto_address_review_mode)
        .zola(
            "Switch session mode",
            "Choose auto-edit, auto-address, or read-only from the composer.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("] · Normal ·", 5000)
                    .wait_for_text("Shift+Tab: switch mode", 5000)
                    .capture_labeled(
                        "initial_permission_mode",
                        "Prompt title shows the current session permission mode",
                    )
                    .write_text("/mode")
                    .wait_for_text("Choose editing permissions", 3000)
                    .press_key("Enter")
                    .wait_for_text("Auto Edit + Auto Address Comments", 3000)
                    .press_key("Enter")
                    .wait_for_text("Auto Edit + Auto Address Comments", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "permission_mode_selected",
                        "The selected permission mode appears in the composer title",
                    )
            },
            |frame, report| {
                let initial_frame = common::frame_from_capture(&report.captures[0]);
                let initial_full = Region::full(initial_frame.cols(), initial_frame.rows());
                assertion::assert_text_in_region(
                    &initial_frame,
                    "] · Normal · Auto Edit + Auto Address Comments",
                    &initial_full,
                );
                assertion::assert_text_in_region(
                    &initial_frame,
                    "Shift+Tab: switch mode",
                    &initial_full,
                );
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "] · Normal · Auto Edit + Auto Address Comments",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify a submitted Codex Auto Edit turn receives unrestricted app-server
/// policies and completes visibly through the real session runtime boundary.
#[test]
fn codex_auto_edit_uses_unrestricted_app_server_policy() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("codex_auto_edit_unrestricted_policy")
        .with_git()
        .setup(seed_codex_auto_edit_policy_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("· Normal · Auto Edit", 5000)
                    .write_text("Verify Codex Auto Edit permissions")
                    .press_key("Enter")
                    .wait_for_text(CODEX_AUTO_EDIT_POLICY_CONFIRMED_TEXT, 30000)
                    .capture_labeled(
                        "codex_auto_edit_unrestricted_policy",
                        "Codex completes with unrestricted Auto Edit permissions",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    CODEX_AUTO_EDIT_POLICY_CONFIRMED_TEXT,
                    &full,
                );
                assertion::assert_not_visible(frame, "Codex Auto Edit policy mismatch.");
            },
        )?;

    Ok(())
}

/// Verify that moving up from the first slash-command option wraps selection
/// to the final visible option.
#[test]
fn slash_command_selection_wraps_from_first_to_last() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("slash_command_selection_wraps")
        .with_git()
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("/")
                    .wait_for_text("Slash Command", 3000)
                    .press_key("Up")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "slash_selection_wrapped",
                        "Up wraps slash selection from the first option to the last",
                    )
            },
            |frame, _report| {
                let slash_menu_title = frame
                    .find_text("Slash Command")
                    .into_iter()
                    .next()
                    .expect("slash-command menu title should render");
                let slash_menu_left_col = (0..=slash_menu_title.rect.col)
                    .rev()
                    .find(|column| frame.cell_text(slash_menu_title.rect.row, *column) == "╭")
                    .expect("slash-command menu should have a left border");
                let option_rows = (slash_menu_title.rect.row + 1..frame.rows())
                    .take_while(|row| frame.cell_text(*row, slash_menu_left_col) == "│")
                    .collect::<Vec<_>>();
                let last_option_row = option_rows
                    .last()
                    .copied()
                    .expect("slash-command menu should render at least one option");

                assert_eq!(
                    frame.cell_text(last_option_row, slash_menu_left_col + 1),
                    ">",
                    "expected the final visible slash command to be selected after wrapping up"
                );
            },
        )?;

    Ok(())
}

/// Verify `/speed` exposes normal and fast modes, reflects the selection, and
/// drops both fast mode and its speed display when `/model` switches to a
/// provider without a speed control.
#[test]
fn session_speed_mode_selection() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_speed_mode_selection")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("/")
                    .write_text("speed")
                    .wait_for_text("/speed", 3000)
                    .press_key("Enter")
                    .wait_for_text("/speed Mode", 3000)
                    .capture_labeled(
                        "speed_mode_picker",
                        "Speed picker offers normal and fast modes",
                    )
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("· Fast", 5000)
                    .wait_for_text("Reasoning: high  Speed: Fast", 5000)
                    .capture_labeled(
                        "fast_mode_selected",
                        "Session header and composer reflect fast mode",
                    )
                    .press_key("/")
                    .write_text("model")
                    .wait_for_text("/model", 3000)
                    .press_key("Enter")
                    .wait_for_text("/model Agent", 3000)
                    .press_key("Enter")
                    .wait_for_text("gemini-3.1-pro-preview", 3000)
                    .press_key("Enter")
                    .wait_for_text(
                        "Model: gemini-3.1-pro-preview  Reasoning: high  Tokens:",
                        5000,
                    )
                    .capture_labeled(
                        "incompatible_model_normalizes_speed",
                        "Switching to Gemini drops fast mode and its speed display",
                    )
            },
            |frame, report| {
                let picker_frame = common::frame_from_capture(&report.captures[0]);
                let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
                assertion::assert_text_in_region(&picker_frame, "Normal", &picker_full);
                assertion::assert_text_in_region(&picker_frame, "Fast", &picker_full);

                let fast_frame = common::frame_from_capture(&report.captures[1]);
                let fast_full = Region::full(fast_frame.cols(), fast_frame.rows());
                assertion::assert_text_in_region(&fast_frame, "· Fast", &fast_full);
                assertion::assert_text_in_region(
                    &fast_frame,
                    "Reasoning: high  Speed: Fast",
                    &fast_full,
                );

                // Gemini has no speed control, so the header runs straight from
                // reasoning to tokens and the composer drops its speed status.
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Model: gemini-3.1-pro-preview  Reasoning: high  Tokens:",
                    &full,
                );
                assertion::assert_not_visible(frame, "· Fast");
            },
        )?;

    Ok(())
}

/// Verify `/style` exposes response-detail choices and persists the selected
/// style visibly in the current session.
#[test]
fn test_session_response_style() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_response_style")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_detailed_response_style_session)
        .zola(
            "Session response style",
            "Choose concise, balanced, or detailed responses for each session.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("· Detailed · Normal · Auto Edit", 5000)
                    .write_text("/style")
                    .wait_for_text("/style", 3000)
                    .press_key("Enter")
                    .wait_for_text("/style Response style", 3000)
                    .wait_for_text("Concise", 3000)
                    .wait_for_text("Detailed", 3000)
                    .capture_labeled(
                        "response_style_picker",
                        "Response style picker explains all three choices",
                    )
                    .press_key("Enter")
                    .wait_for_text("· Detailed · Normal · Auto Edit", 5000)
                    .wait_for_text("Style: Detailed", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "detailed_style_selected",
                        "Header and composer show the detailed response style",
                    )
            },
            |frame, report| {
                let picker_frame = common::frame_from_capture(&report.captures[0]);
                let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
                assertion::assert_text_in_region(&picker_frame, "Concise", &picker_full);
                assertion::assert_text_in_region(&picker_frame, "Balanced", &picker_full);
                assertion::assert_text_in_region(&picker_frame, "Detailed", &picker_full);
                assertion::assert_text_in_region(
                    &picker_frame,
                    "Thorough decisions, trade-offs, effects, and verification.",
                    &picker_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "· Detailed · Normal · Auto Edit", &full);
                assertion::assert_text_in_region(frame, "Style: Detailed", &full);
            },
        )?;

    Ok(())
}

/// Verify `/personality` lists worktree-local agent definitions and persists
/// the selected profile with visible transcript feedback.
#[test]
fn test_session_personality() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_personality")
        .with_git()
        .setup(seed_session_personality)
        .zola(
            "Session personality",
            "Choose a worktree-local agent personality for a session.",
            41,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("/")
                    .write_text("personality")
                    .wait_for_text("Slash Command", 3000)
                    .wait_for_text("List: .agents/agents/.", 3000)
                    .capture_labeled(
                        "personality_slash_command",
                        "Personality source directory appears in the slash-command menu",
                    )
                    .press_key("Enter")
                    .wait_for_text("None (default)", 3000)
                    .wait_for_text("Code Reviewer", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "personality_picker",
                        "Worktree personalities appear in the session picker",
                    )
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Personality set to", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "personality_selected",
                        "Selected personality is confirmed in the transcript",
                    )
            },
            |frame, report| {
                let command_frame = common::frame_from_capture(&report.captures[0]);
                let command_full = Region::full(command_frame.cols(), command_frame.rows());
                assertion::assert_text_in_region(
                    &command_frame,
                    "List: .agents/agents/.",
                    &command_full,
                );

                let picker_frame = common::frame_from_capture(&report.captures[1]);
                let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
                assertion::assert_text_in_region(&picker_frame, "None (default)", &picker_full);
                assertion::assert_text_in_region(&picker_frame, "Code Reviewer", &picker_full);
                assertion::assert_text_in_region(
                    &picker_frame,
                    "Reviews code carefully",
                    &picker_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Personality set to Code Reviewer.", &full);
            },
        )?;

    Ok(())
}
