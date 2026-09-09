//! Clarification questions and session refresh.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::test_support;
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::proof::report::ProofReport;
use testty::region::Region;

use super::fixture::{E2eResult, seed_project_settings};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Stable id for the unrelated row selected before the refresh regression
/// creates its question session.
const QUESTION_REFRESH_SELECTED_SESSION_ID: &str = "question-refresh-selected";

/// Initial prompt that must survive the title-generation refresh.
const QUESTION_REFRESH_INITIAL_PROMPT: &str = "Implement the initial task";

/// Generated title that must survive clarification submission.
const QUESTION_REFRESH_TITLE: &str = "Keep the original task title";

/// Final answer emitted after the clarification response resumes the worker.
const QUESTION_REFRESH_FINAL_ANSWER: &str = "Clarifications accepted.";

/// First clarification question shown when the seeded question session opens.
const FIRST_QUESTION_TEXT: &str = "Use the default target branch?";

/// Second clarification question that must be shown after resuming.
const SECOND_QUESTION_TEXT: &str = "Which tests should be added?";

/// Clarification question emitted by the delayed stub while help is open.
const RECONCILE_QUESTION_TEXT: &str = "Should I add a regression test?";

/// Seeds an unrelated selected row and a prompt-aware agent stub for the
/// question-refresh regression.
fn seed_question_refresh_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(
            QUESTION_REFRESH_SELECTED_SESSION_ID,
            "claude-haiku-4-5-20251001",
            "main",
            "Done",
        )
        .with_title("Other session"),
    )?;

    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
input=$(cat)
case "$input" in
  *"Generate a concise, commit-style title"*)
    sleep 2
    result='{{\"answer\":\"{QUESTION_REFRESH_TITLE}\",\"questions\":[],\"review_comment_outcomes\":[]}}'
    ;;
  *"Clarifications:"*)
    result='{{\"answer\":\"{QUESTION_REFRESH_FINAL_ANSWER}\",\"questions\":[],\"review_comment_outcomes\":[]}}'
    ;;
  *)
    result='{{\"answer\":\"Need two clarifications.\",\"questions\":[{{\"text\":\"{FIRST_QUESTION_TEXT}\",\"options\":[\"Yes\",\"No\"]}},{{\"text\":\"{SECOND_QUESTION_TEXT}\",\"options\":[\"Unit\",\"Integration\"]}}],\"review_comment_outcomes\":[]}}'
    ;;
esac
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$result\",\"usage\":{{\"input_tokens\":5,\"output_tokens\":9}}}}"
"#
    );
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
            ("DefaultFastAgent", "claude"),
            ("DefaultFastModel", "claude-haiku-4-5-20251001"),
        ],
    )?;

    Ok(())
}

/// Installs a Claude stub that emits one structured clarification question
/// after a delay, giving the scenario time to cover the active view with help.
fn install_delayed_question_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
sleep 2
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Need one clarification."}}]}}}}'
sleep 1
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"Need one clarification.\",\"questions\":[{{\"text\":\"{RECONCILE_QUESTION_TEXT}\",\"options\":[\"Yes\",\"No\"]}}]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#
    );

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Seeds clarification questions and a file lookup target without a live agent.
fn seed_question_at_lookup_session(env: &BuilderEnv) -> E2eResult {
    let session_id = "question-lookup-session";
    common::seed_session(
        env,
        SessionSeed::regular(session_id, "claude-haiku-4-5-20251001", "main", "Question")
            .with_title("File answer"),
    )?;
    let session_folder = test_support::session_folder(&env.agentty_root.join("wt"), session_id);
    std::fs::create_dir_all(&session_folder)?;
    std::fs::write(
        session_folder.join("answer_lookup_target.rs"),
        "// lookup target\n",
    )?;
    common::seed_runtime()?.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_questions(
                session_id,
                r#"[{"text":"Which file?","options":[]},{"text":"Anything else?","options":[]}]"#,
            )
            .await
    })?;

    Ok(())
}

/// Verify file lookup inserts into a clarification answer before submission.
#[test]
fn test_question_answer_at_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("question_answer_at_lookup")
        .with_git()
        .setup(seed_question_at_lookup_session)
        .zola(
            "File lookup in answers",
            "Reference repository files while answering an agent's clarification question.",
            51,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("File answer", 5000)
                    .press_key("Enter")
                    .wait_for_text("Question 1/2", 5000)
                    .write_text("@missing")
                    .wait_for_text("Tab/Enter: close @", 5000)
                    .capture_labeled("empty", "An empty lookup offers dismissal controls")
                    .press_key("Tab")
                    .press_key("ctrl+w")
                    .write_text("@answer_lookup")
                    .wait_for_text("answer_lookup_target.rs", 5000)
                    .wait_for_text("Tab/Enter: select", 5000)
                    .write_text("\x1b[13;2u")
                    .wait_for_text("Enter: send", 5000)
                    .capture_labeled("newline", "Shift+Enter inserts a newline during lookup")
                    .write_text("@answer_lookup")
                    .wait_for_text("answer_lookup_target.rs", 5000)
                    .press_key("Left")
                    .wait_for_text("Tab/Enter: select", 5000)
                    .capture_labeled("lookup", "Repository file suggestions in the answer input")
                    .press_key("Enter")
                    .wait_for_text("@answer_lookup_target.rs", 5000)
                    .write_text("please")
                    .wait_for_text("@answer_lookup_target.rs please", 5000)
                    .capture_labeled(
                        "inserted",
                        "Enter inserts the file and keeps the answer editable",
                    )
                    .press_key("Enter")
                    .wait_for_text("Question 2/2", 5000)
            },
            |frame, report| {
                let empty_frame = common::frame_from_capture(&report.captures[0]);
                assertion::assert_not_visible(&empty_frame, "Tab/Enter: select");
                assertion::assert_not_visible(&empty_frame, "Up/Down: navigate");
                let newline_frame = common::frame_from_capture(&report.captures[1]);
                let newline_area = Region::full(newline_frame.cols(), newline_frame.rows());
                assertion::assert_text_in_region(&newline_frame, "@answer_lookup", &newline_area);
                assertion::assert_not_visible(&newline_frame, "answer_lookup_target.rs");
                let inserted_frame = common::frame_from_capture(&report.captures[3]);
                let inserted_area = Region::full(inserted_frame.cols(), inserted_frame.rows());
                assertion::assert_text_in_region(&inserted_frame, "Question 1/2", &inserted_area);
                assertion::assert_text_in_region(
                    &inserted_frame,
                    "@answer_lookup_target.rs please",
                    &inserted_area,
                );
                assertion::assert_not_visible(&inserted_frame, "Tab/Enter: select");
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Question 2/2", &full);
            },
        )
}

/// Verify a short terminal hides lookup controls when no result row fits.
#[test]
fn test_question_answer_at_lookup_clipped() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("question_answer_at_lookup_clipped")
        .with_git()
        .with_terminal_size(100, 10)
        .setup(seed_question_at_lookup_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Enter: send", 5000)
                    .write_text("@answer_lookup")
                    .wait_for_text("Esc: cancel @", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("clipped", "No selection hints without space for a result")
                    .press_key("Esc")
                    .wait_for_text("Enter: send", 5000)
            },
            |frame, report| {
                let clipped = common::frame_from_capture(&report.captures[0]);
                assertion::assert_not_visible(&clipped, "answer_lookup_target.rs");
                assertion::assert_not_visible(&clipped, "Tab/Enter: select");
                assertion::assert_not_visible(&clipped, "Up/Down: navigate");
                assertion::assert_not_visible(&clipped, "Tab/Enter: close @");
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "@answer_lookup", &full);
                assertion::assert_text_in_region(frame, "Enter: send", &full);
            },
        )
}

/// Verify that `Esc` leaves a clarification question active, then answering
/// one question, leaving with `q`, and reopening resumes at the next
/// unanswered question instead of restarting from the first.
#[test]
fn session_question_resume_after_leaving_to_list() -> E2eResult {
    // Arrange
    let agentty_root = Arc::new(Mutex::new(None::<PathBuf>));
    let setup_agentty_root = Arc::clone(&agentty_root);

    FeatureTest::new("session_question_resume")
        .with_git()
        .setup(move |env| {
            setup_agentty_root
                .lock()
                .expect("agentty root capture should remain available")
                .replace(env.agentty_root.clone());

            seed_question_refresh_project(env)
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Other session", 5000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text(QUESTION_REFRESH_INITIAL_PROMPT)
                    .wait_for_text(QUESTION_REFRESH_INITIAL_PROMPT, 3000)
                    .press_key("Enter")
                    .wait_for_text("Question 1/2", 30000)
                    .wait_for_text(QUESTION_REFRESH_TITLE, 30000)
                    .wait_for_text("Tab: focus", 5000)
                    .capture_labeled(
                        "answer_focused",
                        "Title refresh completes while question mode remains active",
                    )
                    .press_key("Escape")
                    .wait_for_text(
                        "Tab: focus | Enter: send | q: sessions | Ctrl+C: end turn",
                        5000,
                    )
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .press_key("ctrl+c")
                    .capture_labeled(
                        "chat_focused",
                        "Chat focus keeps Ctrl+C from ending the question turn",
                    )
                    .press_key("Tab")
                    .wait_for_text("Enter: send", 5000)
                    .press_key("Enter")
                    .wait_for_text("Question 2/2", 5000)
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .press_key("Enter")
                    .wait_for_text("Question 2/2", 5000)
                    .capture_labeled(
                        "resumed_second_question",
                        "Reopening the session resumes at the second question",
                    )
                    .press_key("Enter")
                    .wait_for_text(QUESTION_REFRESH_FINAL_ANSWER, 30000)
                    .capture_labeled(
                        "title_preserved",
                        "Clarification submission preserves title and initial prompt",
                    )
            },
            move |frame, report| {
                // Assert
                assert_question_refresh_result(frame, report);
                let agentty_root = agentty_root
                    .lock()
                    .expect("agentty root capture should remain available")
                    .clone()
                    .expect("feature setup should capture the agentty root");
                let (persisted_title, persisted_prompt) =
                    load_question_refresh_metadata(&agentty_root)
                        .expect("question metadata should remain persisted");

                assert_eq!(persisted_title.as_deref(), Some(QUESTION_REFRESH_TITLE));
                assert_eq!(persisted_prompt, QUESTION_REFRESH_INITIAL_PROMPT);
            },
        )?;

    Ok(())
}

/// Asserts question progress and refreshed-title output for the
/// question-refresh feature journey.
fn assert_question_refresh_result(frame: &TerminalFrame, report: &ProofReport) {
    let answer_focused_frame = common::frame_from_capture(&report.captures[0]);
    let answer_focused_full =
        Region::full(answer_focused_frame.cols(), answer_focused_frame.rows());
    assertion::assert_text_in_region(
        &answer_focused_frame,
        "Tab: focus | Enter: send | q: sessions | Ctrl+C: end turn",
        &answer_focused_full,
    );
    assertion::assert_text_in_region(
        &answer_focused_frame,
        QUESTION_REFRESH_TITLE,
        &answer_focused_full,
    );

    let chat_focused_frame = common::frame_from_capture(&report.captures[1]);
    let chat_focused_full = Region::full(chat_focused_frame.cols(), chat_focused_frame.rows());
    assertion::assert_text_in_region(
        &chat_focused_frame,
        "Tab: focus | j/k: scroll | q: sessions",
        &chat_focused_full,
    );
    assertion::assert_not_visible(&chat_focused_frame, "d: diff");
    assertion::assert_not_visible(&chat_focused_frame, "Ctrl+C");
    assertion::assert_text_in_region(&chat_focused_frame, "Question 1/2", &chat_focused_full);
    assertion::assert_text_in_region(&chat_focused_frame, FIRST_QUESTION_TEXT, &chat_focused_full);

    let resumed_frame = common::frame_from_capture(&report.captures[2]);
    let resumed_full = Region::full(resumed_frame.cols(), resumed_frame.rows());
    assertion::assert_text_in_region(&resumed_frame, "Question 2/2", &resumed_full);
    assertion::assert_text_in_region(&resumed_frame, SECOND_QUESTION_TEXT, &resumed_full);
    assertion::assert_not_visible(&resumed_frame, FIRST_QUESTION_TEXT);

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(frame, QUESTION_REFRESH_TITLE, &full);
    assertion::assert_text_in_region(frame, QUESTION_REFRESH_FINAL_ANSWER, &full);
}

/// Loads the newly created question session's title and prompt from the
/// feature-test database.
fn load_question_refresh_metadata(
    agentty_root: &Path,
) -> Result<(Option<String>, String), Box<dyn std::error::Error>> {
    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = Database::open(&agentty_root.join(DB_DIR).join(DB_FILE))
            .await
            .map_err(|error| std::io::Error::other(format!("feature database: {error}")))?;
        let selected_session = database
            .sessions()
            .load_session(QUESTION_REFRESH_SELECTED_SESSION_ID)
            .await
            .map_err(|error| std::io::Error::other(format!("selected session: {error}")))?
            .ok_or_else(|| std::io::Error::other("selected session does not exist"))?;
        let project_id = selected_session
            .project_id
            .ok_or_else(|| std::io::Error::other("selected session has no project"))?;
        let question_session_id = database
            .sessions()
            .load_sessions_for_project(project_id)
            .await
            .map_err(|error| std::io::Error::other(format!("project sessions: {error}")))?
            .into_iter()
            .find(|session| session.id != QUESTION_REFRESH_SELECTED_SESSION_ID)
            .map(|session| session.id)
            .ok_or_else(|| std::io::Error::other("new question session is not listed"))?;
        let question_session = database
            .sessions()
            .load_session(&question_session_id)
            .await
            .map_err(|error| std::io::Error::other(format!("question session: {error}")))?
            .ok_or_else(|| std::io::Error::other("question session does not exist"))?;

        Result::<_, Box<dyn std::error::Error>>::Ok((
            question_session.title,
            question_session.prompt,
        ))
    })
}

/// Verify that an already-open session view enters question mode after a
/// structured question arrives while the help overlay hides the live
/// transition.
#[test]
fn session_question_reconcile_after_help_overlay() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_question_reconcile")
        .with_git()
        .setup(install_delayed_question_claude_stub)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Need clarification")
                    .wait_for_text("Need clarification", 3000)
                    .press_key("Enter")
                    .wait_for_text("q: back", 5000)
                    .press_key("?")
                    .wait_for_text("Keybindings", 5000)
                    .viewing_pause_ms(4500)
                    .capture_labeled(
                        "help_during_completion",
                        "Help overlay remains open while the turn completes",
                    )
                    .press_key("Escape")
                    .wait_for_text("Question 1/1", 30000)
                    .capture_labeled(
                        "reconciled_question",
                        "Question panel appears without reopening the session",
                    )
            },
            |frame, report| {
                let help_frame = common::frame_from_capture(&report.captures[0]);
                let help_full = Region::full(help_frame.cols(), help_frame.rows());
                assertion::assert_text_in_region(&help_frame, "Keybindings", &help_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Question 1/1", &full);
                assertion::assert_text_in_region(frame, RECONCILE_QUESTION_TEXT, &full);
                assertion::assert_text_in_region(frame, "Yes", &full);
                assertion::assert_text_in_region(frame, "No", &full);
                assertion::assert_not_visible(frame, "Keybindings");
            },
        )?;

    Ok(())
}
