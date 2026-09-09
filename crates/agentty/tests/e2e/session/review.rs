//! Focused review persistence and automatic review.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::db::{DB_DIR, DB_FILE, Database};
use testty::assertion;
use testty::region::Region;

use super::fixture::{
    E2eResult, MISSING_DECISION_CONTEXT_POLICY_TEXT, MISSING_RESOLVED_DECISION_HISTORY_TEXT,
    RESOLVED_DECISION_REVIEW_TEXT, seed_auto_address_review_mode,
    seed_linked_review_worktree_with_diff, seed_project_settings, seed_review_ready_session,
    seed_review_ready_session_on_sessions_tab, seed_review_with_resolved_decision,
    seed_review_worktree_with_diff,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Focused-review output emitted after Gemini starts without plan-mode flags.
const GEMINI_FOCUSED_REVIEW_TEXT: &str = "Gemini focused review completed without plan mode.";

/// Seeds a Codex focused review whose first direct review has an unknown field,
/// then returns a valid direct review for the schema-repair turn. Both turns
/// include a blank duplicate final item in `turn/completed`.
fn seed_codex_review_with_blank_completed_fallback(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let codex_path = env.stub_bin.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi

extract_id() {
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

turn_count=0
while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"thread":{"id":"review-thread"}}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            turn_count=$((turn_count + 1))
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"turn":{"id":"review-turn"}}}\n' "$request_id"
            printf '%s\n' '{"method":"turn/started","params":{"turn":{"id":"review-turn"}}}'
            case "$request" in
                *'"outputSchema":'*'"project_impact"'*)
                    if [ "$turn_count" -eq 1 ]; then
                        final_text='{\"project_impact\":[],\"suggestions\":[],\"summary\":\"extra\"}'
                    else
                        final_text='{\"project_impact\":[\"Final focused review result.\"],\"suggestions\":[]}'
                    fi
                    ;;
                *)
                    final_text='{\"project_impact\":[\"Codex did not receive the focused-review output schema.\"],\"suggestions\":[]}'
                    ;;
            esac
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"review-thread","turnId":"review-turn","item":{"type":"agentMessage","id":"commentary-item","text":"I will inspect the current code.","phase":"commentary"}}}'
            printf '{"method":"item/completed","params":{"threadId":"review-thread","turnId":"review-turn","item":{"type":"agentMessage","id":"final-item","text":"%s","phase":"final_answer"}}}\n' "$final_text"
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"review-thread","turn":{"id":"review-turn","status":"completed","items":[{"type":"agentMessage","id":"blank-final-item","text":"   ","phase":"final_answer"}]}}}'
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
            ("DefaultReviewAgent", "codex"),
            ("DefaultReviewModel", "gpt-5.6-sol"),
        ],
    )
}

/// Seeds a Gemini focused review whose ACP stub rejects plan-mode startup.
fn seed_gemini_focused_review_without_plan_mode(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let gemini_path = env.stub_bin.join("gemini");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'gemini 0.0.0-test\n'; exit 0; fi
answer='{GEMINI_FOCUSED_REVIEW_TEXT}'
for argument in "$@"; do
    if [ "$argument" = "--approval-mode" ] || [ "$argument" = "--sandbox" ]; then
        answer='Gemini focused review incorrectly used plan mode.'
    fi
done

extract_id() {{
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}}

while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            request_id=$(extract_id "$request")
            printf '{{"jsonrpc":"2.0","id":"%s","result":{{"protocolVersion":1}}}}\n' "$request_id"
            ;;
        *'"method":"session/new"'*)
            request_id=$(extract_id "$request")
            printf '{{"jsonrpc":"2.0","id":"%s","result":{{"sessionId":"review-session"}}}}\n' "$request_id"
            ;;
        *'"method":"session/prompt"'*)
            request_id=$(extract_id "$request")
            printf '{{"jsonrpc":"2.0","id":"%s","result":{{"response":"{{\\"project_impact\\":[\\"%s\\"],\\"suggestions\\":[]}}","usage":{{"inputTokens":5,"outputTokens":9}}}}}}\n' "$request_id" "$answer"
            ;;
    esac
done
"#,
    );
    std::fs::write(&gemini_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&gemini_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultReviewAgent", "gemini"),
            ("DefaultReviewModel", "gemini-3.1-pro-preview"),
        ],
    )
}

/// Seeds a real review worktree and deterministic providers for automatic
/// remediation lifecycle coverage.
fn seed_auto_address_review_lifecycle(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_auto_address_review_mode(env)?;
    seed_linked_review_worktree_with_diff(env)?;
    install_auto_address_review_lifecycle_stub(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_model("review-shortcut-0001", "claude-haiku-4-5-20251001")
            .await
    })?;

    Ok(())
}

/// Installs one prompt-aware Claude stub that exposes both automatic-review
/// stop conditions through stable transcript text. Coding turns change the
/// tracked fixture so each completed turn remains eligible for review.
fn install_auto_address_review_lifecycle_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi

state_dir=${0%/*}
review_count_file="$state_dir/auto-address-review-count"
if [ -f "$review_count_file" ]; then
  read review_count < "$review_count_file"
else
  review_count=0
fi
prompt=$(cat)

case "$prompt" in
  *"Review the Git diff for display in a terminal UI."*)
    review_count=$((review_count + 1))
    printf '%s\n' "$review_count" > "$review_count_file"
    case "$review_count" in
      1)
        result='{\"project_impact\":[\"First lifecycle review completed.\"],\"suggestions\":[{\"details\":\"Apply the first lifecycle suggestion.\",\"severity\":\"medium\"}]}'
        ;;
      2)
        result='{\"project_impact\":[\"No suggestions remain after one automatic remediation.\"],\"suggestions\":[]}'
        ;;
      3|4|5)
        result='{\"project_impact\":[\"Iteration-limit lifecycle review completed.\"],\"suggestions\":[{\"details\":\"Apply the next bounded lifecycle suggestion.\",\"severity\":\"medium\"}]}'
        ;;
      6)
        result='{\"project_impact\":[\"Three automatic remediation iterations completed.\"],\"suggestions\":[{\"details\":\"Fourth suggestion remains unapplied at the iteration limit.\",\"severity\":\"medium\"}]}'
        ;;
      *)
        result='{\"project_impact\":[\"Automatic remediation exceeded the iteration limit.\"],\"suggestions\":[]}'
        ;;
    esac
    ;;
  *"Verify the focused-review suggestions against the current code"*)
    printf '// Automatic remediation %s\n' "$review_count" >> src/main.rs
    result='{\"answer\":\"Automatic remediation turn completed.\",\"questions\":[]}'
    ;;
  *"Start the no-suggestions lifecycle"*)
    printf '// Start no-suggestions lifecycle\n' >> src/main.rs
    result='{\"answer\":\"No-suggestions lifecycle turn completed.\",\"questions\":[]}'
    ;;
  *"Start the iteration-limit lifecycle"*)
    printf '// Start iteration-limit lifecycle\n' >> src/main.rs
    result='{\"answer\":\"Iteration-limit lifecycle turn completed.\",\"questions\":[]}'
    ;;
  *)
    result='{\"answer\":\"Auto-address lifecycle utility response.\",\"questions\":[]}'
    ;;
esac

printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$result\",\"usage\":{\"input_tokens\":5,\"output_tokens\":9}}"
"#;
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", "claude-haiku-4-5-20251001"),
        ],
    )
}

/// Seeds one session that is already generating focused review output so
/// shortcut rendering can cover the transient `AgentReview` state.
fn seed_agent_review_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .projects()
            .upsert_project(
                &canonical_workdir.to_string_lossy(),
                Some("main".to_string()),
            )
            .await?;

        database
            .projects()
            .touch_project_last_opened(project_id)
            .await?;
        database
            .sessions()
            .insert_session(
                "agent-review-sync-0001",
                "gpt-5.6-sol",
                "main",
                "AgentReview",
                project_id,
            )
            .await?;
        database
            .sessions()
            .update_session_title("agent-review-sync-0001", "Agent review sync shortcut")
            .await?;
        database
            .sessions()
            .update_session_diff_stats(8, 2, true, "agent-review-sync-0001", "S")
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("agent-re"))?;

    Ok(())
}

/// Seeds one review-ready session with a focused review already persisted as
/// if Agentty had been restarted after review generation completed.
fn seed_review_ready_session_with_persisted_focused_review(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_focused_review(
                "review-shortcut-0001",
                Some(agentty::domain::review::FocusedReviewStatus::Ready),
                Some("42".to_string()),
                Some(
                    "## Review\n\n### Project Impact\n\n- Persisted focused review \
                     finding.\n\n### Suggestions\n\n- None."
                        .to_string(),
                ),
            )
            .await?;
        Ok::<(), ag_store::DbError>(())
    })?;

    Ok(())
}

/// Seeds one persisted focused review plus a second project so the review can
/// be restored after switching away from its owning project and back.
fn seed_cross_project_focused_review(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_persisted_focused_review(env)?;
    common::seed_mru_first_second_project(env)
}

/// Seeds two review-ready sessions with distinct persisted focused reviews so
/// switching away and back can verify cache-backed output restoration.
fn seed_sessions_with_persisted_focused_reviews(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    // The session list orders by `updated_at DESC, created_at DESC, id`, and
    // both timestamps have one-second resolution. Seed `second-review-0001`
    // first so `review-shortcut-0001` is row 0 under either outcome: when both
    // seeds land in the same second the `id` tiebreak selects it, and when a
    // second boundary falls between them its newer `updated_at` selects it.
    // Seeding in the other order makes row 0 depend on that boundary.
    common::seed_session(
        env,
        SessionSeed::regular("second-review-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Second persisted review"),
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_focused_review(
                "second-review-0001",
                Some(agentty::domain::review::FocusedReviewStatus::Ready),
                Some("84".to_string()),
                Some(
                    "## Review\n\n### Project Impact\n\n- Second persisted review finding.\n\n### \
                     Suggestions\n\n- None."
                        .to_string(),
                ),
            )
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("second-r"))?;

    seed_review_ready_session_with_persisted_focused_review(env)?;

    Ok(())
}

/// Verify that persisted focused review text is restored into the session
/// output panel after Agentty starts again.
#[test]
fn persisted_focused_review_survives_reload() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("persisted_focused_review")
        .with_terminal_size(100, 40)
        .with_git()
        .setup(seed_review_ready_session_with_persisted_focused_review)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "persisted_focused_review",
                        "Persisted focused review visible after startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Persisted focused review finding.", &full);
                let impact_header = frame
                    .find_text("Project Impact")
                    .into_iter()
                    .next()
                    .expect("project impact header should render");
                let impact_finding = frame
                    .find_text("Persisted focused review finding.")
                    .into_iter()
                    .next()
                    .expect("project impact finding should render");
                let suggestions_header = frame
                    .find_text("Suggestions")
                    .into_iter()
                    .next()
                    .expect("suggestions header should render");
                let empty_suggestion = frame
                    .find_text("- None.")
                    .into_iter()
                    .next()
                    .expect("empty suggestion should render");

                assert_eq!(impact_finding.rect.row, impact_header.rect.row + 1);
                assert_eq!(empty_suggestion.rect.row, suggestions_header.rect.row + 1);
                assertion::assert_not_visible(frame, "Change Summary");
                assertion::assert_not_visible(frame, "type \"/apply\" to verify and apply");
            },
        )?;

    Ok(())
}

/// Verify each session restores its own persisted focused review after users
/// switch between session views.
#[test]
fn focused_reviews_survive_session_switching() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_reviews_survive_session_switching")
        .with_git()
        .setup(seed_sessions_with_persisted_focused_reviews)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Persisted focused review finding.", 5000)
                    .capture_labeled("first_review", "First session focused review")
                    .press_key("q")
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Second persisted review finding.", 5000)
                    .capture_labeled("second_review", "Second session focused review")
                    .press_key("q")
                    .press_key("k")
                    .press_key("Enter")
                    .wait_for_text("Persisted focused review finding.", 5000)
                    .capture_labeled("restored_first_review", "Restored first focused review")
            },
            |frame, report| {
                assert_eq!(report.captures.len(), 3);
                let first_frame = common::frame_from_capture(&report.captures[0]);
                let second_frame = common::frame_from_capture(&report.captures[1]);
                let restored_first_frame = common::frame_from_capture(&report.captures[2]);
                let first_full = Region::full(first_frame.cols(), first_frame.rows());
                let second_full = Region::full(second_frame.cols(), second_frame.rows());
                let restored_first_full =
                    Region::full(restored_first_frame.cols(), restored_first_frame.rows());
                let final_full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(
                    &first_frame,
                    "Persisted focused review finding.",
                    &first_full,
                );
                assertion::assert_text_in_region(
                    &second_frame,
                    "Second persisted review finding.",
                    &second_full,
                );
                assertion::assert_text_in_region(
                    &restored_first_frame,
                    "Persisted focused review finding.",
                    &restored_first_full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Persisted focused review finding.",
                    &final_full,
                );
            },
        )?;

    Ok(())
}

/// Verify a persisted focused review remains available after users switch
/// away from its owning project and back.
#[test]
fn focused_review_survives_project_switching() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_review_survives_project_switching")
        .with_git()
        .setup(seed_cross_project_focused_review)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: alpha-project", 5000)
                    .press_key("p")
                    .wait_for_text("Switch project", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Project: test-project", 5000)
                    .wait_for_text("Review-ready session shortcuts", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .wait_for_text("Persisted focused review finding.", 5000)
                    .capture_labeled(
                        "restored_review",
                        "Focused review restored after project switching",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Persisted focused review finding.", &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_not_visible(frame, "Reviewing changes with");
            },
        )?;

    Ok(())
}

/// Verify focused review treats explanations and accepted tradeoffs from the
/// saved session chat as constraints instead of repeating resolved advice.
#[test]
fn focused_review_honors_resolved_session_decisions() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_review_resolved_decision")
        .with_git()
        .setup(seed_review_with_resolved_decision)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Keep the println call", 5000)
                    .press_key("f")
                    .wait_for_text("Suggestions", 30000)
                    .viewing_pause_ms(1000)
                    .capture_labeled(
                        "resolved_decision_honored",
                        "Focused review honors a decision resolved in session chat",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, RESOLVED_DECISION_REVIEW_TEXT, &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_text_in_region(frame, "- None", &full);
                assertion::assert_not_visible(frame, MISSING_DECISION_CONTEXT_POLICY_TEXT);
                assertion::assert_not_visible(frame, MISSING_RESOLVED_DECISION_HISTORY_TEXT);
            },
        )?;

    Ok(())
}

/// Verify Codex focused review uses its direct transport schema, repairs
/// unknown fields, and ignores a blank completion fallback.
#[test]
fn focused_review_ignores_blank_completed_fallback() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("focused_review_ignores_blank_completed_fallback")
        .with_git()
        .setup(seed_codex_review_with_blank_completed_fallback)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .press_key("f")
                    .wait_for_text("Final focused review result.", 30000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "final_review",
                        "Focused review preserves the nonblank final answer",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Final focused review result.", &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_not_visible(frame, "I will inspect the current code.");
                assertion::assert_not_visible(frame, "Reviewing changes with");
                assertion::assert_not_visible(
                    frame,
                    "Codex did not receive the focused-review output schema.",
                );
                assertion::assert_not_visible(frame, "Review assist unavailable");
            },
        )?;

    Ok(())
}

/// Verify Gemini focused review avoids the plan-mode bootstrap.
#[test]
fn gemini_focused_review_avoids_plan_mode_bootstrap() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("gemini_focused_review_avoids_plan_mode_bootstrap")
        .with_git()
        .setup(seed_gemini_focused_review_without_plan_mode)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .press_key("f")
                    .wait_for_text("Suggestions", 30000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "gemini_review",
                        "Gemini focused review completes without plan-mode startup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, GEMINI_FOCUSED_REVIEW_TEXT, &full);
                assertion::assert_text_in_region(frame, "Suggestions", &full);
                assertion::assert_not_visible(frame, "Reviewing changes with");
            },
        )?;

    Ok(())
}

/// Verify the `AgentReview` session footer keeps the sync shortcut visible so
/// users can start a rebase without waiting for focused review generation.
#[test]
fn agent_review_session_shows_sync_shortcut() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("agent_review_sync_shortcut")
        .with_git()
        .setup(seed_agent_review_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "agent_review_sync_shortcut",
                        "AgentReview session view with sync shortcut",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Agent review sync shortcut", &full);
                assertion::assert_text_in_region(frame, "r: sync", &full);
            },
        )?;

    Ok(())
}

/// Verify that typing `/apply` in a review-ready session keeps the command
/// text visible when no actionable focused-review cache is available.
#[test]
fn apply_slash_command_unavailable_without_review_cache() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("apply_slash_command_no_review")
        .with_git()
        .setup(seed_review_ready_session)
        .zola(
            "Apply slash command",
            "Type unavailable `/apply` in a review-ready session and keep the prompt intact.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("/")
                    .wait_for_text("/model", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "slash_commands_visible",
                        "Slash command suggestion list omits unavailable /apply",
                    )
                    .write_text("apply")
                    .wait_for_text("/apply", 3000)
                    .wait_for_stable_frame(300, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "apply_not_applied_without_review_cache",
                        "Session stays in prompt mode with `/apply` unchanged",
                    )
            },
            |frame, report| {
                let suggestion_frame = common::frame_from_capture(&report.captures[0]);
                let suggestion_full =
                    Region::full(suggestion_frame.cols(), suggestion_frame.rows());
                assertion::assert_text_in_region(&suggestion_frame, "/model", &suggestion_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "/apply", &full);
                let full_text = frame.text_in_region(&full);
                assert!(
                    !full_text.contains("Run a focused review first"),
                    "session without actionable review cache should not show apply guidance"
                );
            },
        )?;

    Ok(())
}

/// Verify `Shift+Tab` reaches automatic review addressing without changing the
/// draft.
#[test]
fn shift_tab_auto_address_mode() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("shift_tab_auto_address_mode")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_review_ready_session_on_sessions_tab)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("] · Normal · Auto Edit", 5000)
                    .write_text("Keep this draft")
                    .press_key("BackTab")
                    .wait_for_text("Auto Edit + Auto Address Comments", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "] · Normal · Auto Edit + Auto Address Comments",
                    &full,
                );
                assertion::assert_text_in_region(frame, "Keep this draft", &full);
            },
        )?;

    Ok(())
}

/// Verify `/mode` exposes and selects bounded focused-review automation.
#[test]
fn auto_address_review_mode() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("auto_address_review_mode")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_auto_address_review_mode)
        .zola(
            "Automatically address review suggestions",
            "Enable auto-edit and apply focused-review suggestions for up to three iterations.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("/mode")
                    .wait_for_text("Choose editing permissions", 3000)
                    .press_key("Enter")
                    .wait_for_text("Auto Edit + Auto Address Comments", 3000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "auto_address_mode_option",
                        "The mode picker explains bounded automatic review remediation",
                    )
                    .press_key("Enter")
                    .wait_for_text("Auto Edit + Auto Address Comments", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "auto_address_mode_selected",
                        "The composer shows automatic review addressing is enabled",
                    )
            },
            |frame, report| {
                let picker_frame = common::frame_from_capture(&report.captures[0]);
                let picker_full = Region::full(picker_frame.cols(), picker_frame.rows());
                assertion::assert_text_in_region(
                    &picker_frame,
                    "Auto Edit + Auto Address Comments",
                    &picker_full,
                );
                assertion::assert_text_in_region(
                    &picker_frame,
                    "address focused-review suggestions up to 3 times",
                    &picker_full,
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

/// Verify automatic focused-review remediation stops without suggestions and
/// after three iterations through the real session runtime.
#[test]
fn auto_address_review_lifecycle() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("auto_address_review_lifecycle")
        .with_git()
        .with_terminal_size(180, 24)
        .setup(seed_auto_address_review_lifecycle)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Start the no-suggestions lifecycle")
                    .press_key("Enter")
                    .wait_for_text(
                        "No suggestions remain after one automatic remediation.",
                        30000,
                    )
                    .wait_for_stable_frame(500, 5000)
                    .capture_labeled(
                        "auto_address_stops_without_suggestions",
                        "Automatic remediation stops when focused review returns no suggestions",
                    )
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Start the iteration-limit lifecycle")
                    .press_key("Enter")
                    .wait_for_text(
                        "Fourth suggestion remains unapplied at the iteration limit.",
                        30000,
                    )
                    .wait_for_stable_frame(1000, 5000)
                    .capture_labeled(
                        "auto_address_stops_at_iteration_limit",
                        "Automatic remediation stops after three iterations",
                    )
            },
            |frame, report| {
                let no_suggestions_frame = common::frame_from_capture(&report.captures[0]);
                let no_suggestions_full =
                    Region::full(no_suggestions_frame.cols(), no_suggestions_frame.rows());
                assertion::assert_text_in_region(
                    &no_suggestions_frame,
                    "No suggestions remain after one automatic remediation.",
                    &no_suggestions_full,
                );
                assertion::assert_text_in_region(
                    &no_suggestions_frame,
                    "- None",
                    &no_suggestions_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Three automatic remediation iterations completed.",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "Fourth suggestion remains unapplied at the iteration limit.",
                    &full,
                );
                assertion::assert_not_visible(
                    frame,
                    "Automatic remediation exceeded the iteration limit.",
                );
            },
        )?;

    Ok(())
}
