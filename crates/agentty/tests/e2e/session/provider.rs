//! Provider protocol, launch, and commit recovery.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{
    CLAUDE_STRUCTURED_RESPONSE_TEXT, E2eResult, run_git, seed_claude_structured_output_project,
    seed_project_settings,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Stable id for the Antigravity session whose replay exceeds the former argv
/// transport limit.
const ANTIGRAVITY_LARGE_REPLAY_SESSION_ID: &str = "antigravity-large-replay";

/// Creates a real worktree index lock after a stubbed agent edits a file.
fn seed_commit_index_lock_project(env: &BuilderEnv) -> E2eResult {
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
response='{"answer":"Preserve pending worktree change","questions":[],"review_comment_outcomes":[],"subtasks":[],"verification_verdicts":[]}'
printf '%s\n' "$GIT_OPTIONAL_LOCKS" > "$AGENTTY_TEST_EVIDENCE/optional-locks"
case "$prompt" in
  *"Generate the canonical session commit message"*)
    if [ "$AGENTTY_TEST_RELEASE_LOCK" = "1" ]; then
      lock_path=$(cat "$AGENTTY_TEST_EVIDENCE/lock-path") || exit 93
      (sleep 2; rm "$lock_path") </dev/null >/dev/null 2>&1 &
    fi
    ;;
  *"Review the Git diff for display in a terminal UI."*)
    response='{"project_impact":[],"suggestions":[]}'
    ;;
  *"Repair a failed git commit"*)
    printf 'unexpected assistance\n' > "$AGENTTY_TEST_EVIDENCE/assist"
    exit 91
    ;;
  *)
    printf 'pending change\n' > generated.txt
    lock_path=$(git rev-parse --git-path index.lock) || exit 92
    printf 'test lock\n' > "$lock_path"
    printf '%s\n' "$lock_path" > "$AGENTTY_TEST_EVIDENCE/lock-path"
    printf '%s/generated.txt\n' "$PWD" > "$AGENTTY_TEST_EVIDENCE/change-path"
    ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"StructuredOutput","input":%s}]}}\n' "$response"
printf '{"type":"result","subtype":"success","result":"","structured_output":%s,"usage":{"input_tokens":5,"output_tokens":9}}\n' "$response"
"#;
    let claude_path = env.stub_bin.join("claude");
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
    )
}

/// Seeds configured pre-commit validation without installing its Git hook.
fn seed_missing_pre_commit_hook_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(env.workdir.join(".pre-commit-config.yaml"), "repos: []\n")?;
    run_git(&env.workdir, &["add", ".pre-commit-config.yaml"])?;
    run_git(
        &env.workdir,
        &["commit", "-m", "configure pre-commit validation"],
    )?;
    run_git(
        &env.workdir,
        &["config", "core.hooksPath", ".missing-hooks"],
    )?;

    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
case "$prompt" in
  *"Generate the canonical session commit message"*)
    sleep 2
    ;;
  *)
    printf 'pending change\n' > generated.txt
    ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"StructuredOutput","input":{"answer":"Created pending worktree change","questions":[],"review_comment_outcomes":[],"subtasks":[],"verification_verdicts":[]}}]}}'
printf '%s\n' '{"type":"result","subtype":"success","result":"","structured_output":{"answer":"Created pending worktree change","questions":[],"review_comment_outcomes":[],"subtasks":[],"verification_verdicts":[]},"usage":{"input_tokens":5,"output_tokens":9}}'
"#;
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

/// Filler repeated through the stub's non-protocol payload.
const PROTOCOL_FAILURE_PAYLOAD_FILLER: &str = "not-json-filler";

/// Marker placed at the far end of the stub's non-protocol payload.
///
/// Neither this nor the filler may reach the chat: a protocol failure must
/// report why the payload was rejected without reproducing the payload.
const PROTOCOL_FAILURE_TAIL_MARKER: &str = "TAILOFPAYLOAD";

/// Installs a Claude stub whose final payload is not protocol JSON.
///
/// The payload is far longer than the excerpt budget and carries a marker at
/// each end, so the resulting transcript notice can be checked for a bounded
/// failure message instead of a raw provider dump.
fn seed_invalid_protocol_output_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = format!(
        "{} {PROTOCOL_FAILURE_TAIL_MARKER}",
        format!("{PROTOCOL_FAILURE_PAYLOAD_FILLER} ").repeat(40)
    );
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{payload}"}}'
"#
    );
    std::fs::write(&claude_path, script)?;

    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])?;

    Ok(())
}

/// Adds an Antigravity stub that validates persistent NDJSON prompt delivery.
fn seed_antigravity_stream_prompt_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("agy");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'agy 1.2.0\n'; exit 0; fi
output_format=''
input_format=''
schema=''
has_print=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --input-format)
      input_format=$2
      shift 2
      ;;
    --output-format)
      output_format=$2
      shift 2
      ;;
    --json-schema)
      schema=$2
      shift 2
      ;;
    --print)
      has_print=true
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
contract_error=''
if [ "$has_print" = true ]; then
  contract_error='Antigravity invocation did not satisfy the CLI contract: argv.'
elif [ "$input_format" != stream-json ]; then
  contract_error='Antigravity invocation did not satisfy the CLI contract: input.'
elif [ "$output_format" != stream-json ]; then
  contract_error='Antigravity invocation did not satisfy the CLI contract: output.'
elif ! printf '%s' "$schema" | grep -q '"required"' ||
     ! printf '%s' "$schema" | grep -q '"answer"'; then
  contract_error='Antigravity invocation did not satisfy the CLI contract: schema.'
fi
printf '%s\n' '{"event":"init","conversation_id":"stub-conversation","init":{"cwd":"stub"}}'
turn=0
while IFS= read -r prompt_event; do
  turn=$((turn + 1))
  if [ -n "$contract_error" ]; then
    answer=$contract_error
  elif ! printf '%s' "$prompt_event" | grep -q '"event":"user"'; then
    answer='Antigravity invocation did not satisfy the CLI contract: event.'
  elif printf '%s' "$prompt_event" | grep -q 'Hi from Agentty stdin'; then
    answer='Antigravity received the stdin prompt.'
  elif printf '%s' "$prompt_event" | grep -q 'Keep the native conversation'; then
    if [ "$turn" -eq 2 ]; then
      answer='Antigravity preserved the native conversation.'
    else
      answer='Antigravity invocation did not preserve the native conversation.'
    fi
  elif printf '%s' "$prompt_event" | grep -q 'Continue work'; then
    if printf '%s' "$prompt_event" | grep -q 'Session checkpoint' &&
       grep -q 'preserve the accepted middle decision' .agentty-replay-*/history.md; then
      answer='Antigravity accepted the large stdin replay.'
    else
      answer='Antigravity could not retrieve the omitted replay history.'
    fi
  else
    answer='Antigravity handled an Agentty utility prompt.'
  fi
  cumulative_input=$((turn * 7))
  cumulative_output=$((turn * 6))
  printf '{"event":"step_update","step_update":{"conversation_id":"stub-conversation","step_index":%s,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":7,"output_tokens":6}}}\n' "$turn"
  printf '{"event":"result","result":{"conversation_id":"stub-conversation","status":"SUCCESS","response":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"review_comment_outcomes\\":[]}","structured_output":{"answer":"%s","questions":[],"review_comment_outcomes":[]},"error":"","duration_seconds":0.1,"num_turns":%s,"usage":{"input_tokens":%s,"output_tokens":%s,"thinking_tokens":1,"cache_read_tokens":2,"total_tokens":16}}}\n' "$answer" "$answer" "$turn" "$cumulative_input" "$cumulative_output"
done
"#;
    std::fs::write(&stub_agent_path, script)?;

    #[cfg(unix)]
    std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "antigravity"),
            ("DefaultSmartModel", "gemini-3.1-pro-preview"),
        ],
    )?;

    Ok(())
}

/// Seeds a review-ready Antigravity session with a large replay transcript and
/// a valid worktree.
fn seed_antigravity_large_replay_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_antigravity_stream_prompt_project(env)?;
    common::seed_session(
        env,
        SessionSeed::regular(
            ANTIGRAVITY_LARGE_REPLAY_SESSION_ID,
            "gemini-3.1-pro-preview",
            "main",
            "Review",
        )
        .with_title("Large Antigravity replay"),
    )?;

    let runtime = common::seed_runtime()?;
    let large_answer = format!(
        "{}\npreserve the accepted middle decision\n{}",
        "x".repeat(24 * 1024),
        "x".repeat(24 * 1024)
    );
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                ANTIGRAVITY_LARGE_REPLAY_SESSION_ID,
                SessionMessageKind::UserPrompt,
                "Complete the initial task.",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                ANTIGRAVITY_LARGE_REPLAY_SESSION_ID,
                SessionMessageKind::AssistantAnswer,
                &large_answer,
            )
            .await?;
        database
            .sessions()
            .update_session_prompt(
                ANTIGRAVITY_LARGE_REPLAY_SESSION_ID,
                "Complete the initial task.",
            )
            .await
    })?;

    let session_worktree = test_support::session_folder(
        &env.agentty_root.join("wt"),
        ANTIGRAVITY_LARGE_REPLAY_SESSION_ID,
    );
    let session_worktree = session_worktree.to_string_lossy();
    run_git(
        &env.workdir,
        &["worktree", "add", "-b", "wt/antigrav", &session_worktree],
    )?;

    Ok(())
}

/// Installs a Claude stub that reports whether Agentty passed web-capable
/// Claude Code tools to the non-interactive session launch.
fn install_web_tool_reporting_claude_stub(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
case " $* " in
  *WebSearch*WebFetch*|*WebFetch*WebSearch*) answer='Claude web tools enabled' ;;
  *) answer='Claude web tools missing' ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"{\\"answer\\":\\"%s\\",\\"questions\\":[]}"}]}}\n' "$answer"
printf '{"type":"result","subtype":"success","result":"{\"answer\":\"%s\",\"questions\":[]}","usage":{"input_tokens":5,"output_tokens":9}}\n' "$answer"
"#;

    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])
}

/// Verify that provider output which fails protocol validation surfaces a
/// bounded failure notice instead of dumping the raw payload into the chat.
#[test]
fn test_session_invalid_protocol_output_is_bounded() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_invalid_protocol_output_is_bounded")
        .with_git()
        .setup(seed_invalid_protocol_output_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Return invalid protocol output")
                    .wait_for_text("Return invalid protocol output", 3000)
                    .press_key("Enter")
                    .wait_for_text("embedded_json_candidate", 60000)
                    .capture_labeled(
                        "invalid_protocol_output",
                        "Invalid provider output reports a failure without raw payload",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "embedded_json_candidate", &full);
                assertion::assert_not_visible(frame, PROTOCOL_FAILURE_TAIL_MARKER);
                assertion::assert_not_visible(frame, PROTOCOL_FAILURE_PAYLOAD_FILLER);
            },
        )?;

    Ok(())
}

/// Verify that schema-validated Claude results render their final answer
/// instead of leaving a transient structured-output tool event in chat.
#[test]
fn test_claude_structured_output_response() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("claude_structured_output_response")
        .with_git()
        .setup(seed_claude_structured_output_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Return a structured response")
                    .wait_for_text("Return a structured response", 3000)
                    .press_key("Enter")
                    .wait_for_text(CLAUDE_STRUCTURED_RESPONSE_TEXT, 30000)
                    .capture_labeled(
                        "structured_response",
                        "Claude schema-validated response renders in chat",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, CLAUDE_STRUCTURED_RESPONSE_TEXT, &full);
                assertion::assert_not_visible(
                    frame,
                    "Agent output did not match the required JSON schema",
                );
                assertion::assert_not_visible(frame, "Working: tool use");
            },
        )?;

    Ok(())
}

/// Verify Antigravity receives stdin prompts and retains one native process
/// across follow-up turns.
#[test]
fn test_session_antigravity_preserves_stream_context() -> E2eResult {
    // Arrange
    FeatureTest::new("session_antigravity_stream_context")
        .with_git()
        .setup(seed_antigravity_stream_prompt_project)
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Hi from Agentty stdin")
                    .wait_for_text("Hi from Agentty stdin", 3000)
                    .press_key("Enter")
                    .wait_for_text("Antigravity received the stdin prompt.", 60000)
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Keep the native conversation")
                    .press_key("Enter")
                    .wait_for_text("Antigravity preserved the native conversation.", 60000)
                    .capture_labeled(
                        "antigravity_response",
                        "Antigravity retains native context across stdin turns",
                    )
            },
            |frame, _report| {
                // Assert
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Antigravity preserved the native conversation.",
                    &full,
                );
                assertion::assert_not_visible(
                    frame,
                    "Antigravity invocation did not satisfy the CLI contract:",
                );
            },
        )?;

    Ok(())
}

/// Verify resumed transcripts larger than the former argv ceiling are sent
/// successfully through Antigravity stdin.
#[test]
fn test_session_antigravity_accepts_large_replay() -> E2eResult {
    // Arrange
    FeatureTest::new("session_antigravity_large_replay")
        .with_git()
        .setup(seed_antigravity_large_replay_project)
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("Continue work")
                    .wait_for_text("Continue work", 3000)
                    .press_key("Enter")
                    .wait_for_text("Antigravity accepted the large stdin replay.", 30000)
                    .capture_labeled(
                        "large_replay_response",
                        "Large Antigravity replay succeeds through stdin",
                    )
            },
            |frame, _report| {
                // Assert
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Antigravity accepted the large stdin replay.",
                    &full,
                );
                assertion::assert_not_visible(frame, "32768-byte");
            },
        )?;

    Ok(())
}

/// Verify that a persistent worktree index lock stops commit recovery without
/// invoking assistance or deleting either the lock or the pending changes.
#[test]
fn test_session_commit_index_lock() -> E2eResult {
    // Arrange
    let evidence = tempfile::tempdir()?;

    // Act / Assert
    FeatureTest::new("session_commit_index_lock")
        .with_git()
        .with_terminal_size(100, 40)
        .env("AGENTTY_TEST_EVIDENCE", evidence.path().to_string_lossy())
        .setup(seed_commit_index_lock_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Select session type", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Create a pending change")
                    .press_key("Enter")
                    .wait_for_text("Auto-commit blocked by a Git index lock", 30000)
                    .wait_for_text("Enter: reply", 5000)
                    .write_text("g")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "index_lock",
                        "Commit stops with index-lock recovery guidance",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Commit Error]", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Auto-commit blocked by a Git index lock",
                    &full,
                );
                assertion::assert_text_in_region(frame, "repository owner confirm it is", &full);
                assertion::assert_text_in_region(frame, "stale before removing it", &full);
                assertion::assert_not_visible(frame, "Committing...");
                assertion::assert_not_visible(frame, "[Commit Assist]");
                assert!(!evidence.path().join("assist").exists());
                for (record, expected) in [
                    ("lock-path", "test lock\n"),
                    ("change-path", "pending change\n"),
                ] {
                    let path = std::fs::read_to_string(evidence.path().join(record))
                        .expect("stub should record the fixture path");
                    let content = std::fs::read_to_string(path.trim())
                        .expect("auto-commit should preserve the fixture");
                    assert_eq!(content, expected);
                }
            },
        )?;

    Ok(())
}

/// Verify that auto-commit waits for a short-lived writer without assistance.
#[test]
fn test_session_commit_index_lock_recovers() -> E2eResult {
    // Arrange
    let evidence = tempfile::tempdir()?;

    // Act / Assert
    FeatureTest::new("session_commit_index_lock_recovers")
        .with_git()
        .with_terminal_size(100, 40)
        .env("AGENTTY_TEST_EVIDENCE", evidence.path().to_string_lossy())
        .env("AGENTTY_TEST_RELEASE_LOCK", "1")
        .env("GIT_OPTIONAL_LOCKS", "1")
        .setup(seed_commit_index_lock_project)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Select session type", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Create a pending change")
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 30000)
                    .write_text("g")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("commit_recovered", "Auto-commit waits for the index writer")
            },
            |frame, _report| {
                assertion::assert_not_visible(frame, "[Commit Error]");
                assertion::assert_not_visible(frame, "[Commit Assist]");
                assert!(!evidence.path().join("assist").exists());
                assert_eq!(
                    std::fs::read_to_string(evidence.path().join("optional-locks"))
                        .expect("agent should record the inherited Git environment"),
                    "0\n"
                );
                let change_path = std::fs::read_to_string(evidence.path().join("change-path"))
                    .expect("agent should record its changed file");
                let worktree = Path::new(change_path.trim())
                    .parent()
                    .expect("changed file should have a worktree");
                let committed = Command::new("git")
                    .args(["show", "HEAD:generated.txt"])
                    .current_dir(worktree)
                    .output()
                    .expect("committed file should be readable");
                assert!(committed.status.success());
                assert_eq!(committed.stdout, b"pending change\n");
            },
        )?;

    Ok(())
}

/// Verify that configured validation without an installed hook warns before
/// session selection and after a successful normal commit.
#[test]
fn test_session_pre_commit_hook_warning() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_pre_commit_hook_warning")
        .with_git()
        .setup(seed_missing_pre_commit_hook_project)
        .zola(
            "Pre-commit hook warning",
            "Warn about missing pre-commit hooks without blocking session creation.",
            32,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Pre-commit hook warning", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "missing_hook_warning",
                        "Session creation warns about the missing pre-commit hook",
                    )
                    .press_key("Enter")
                    .wait_for_text("Select session type", 5000)
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("Create one worktree change")
                    .wait_for_text("Create one worktree change", 3000)
                    .press_key("Enter")
                    .wait_for_text("Created pending worktree change", 30000)
                    .wait_for_text("Committing...", 10000)
                    .eventually(
                        Duration::from_secs(60),
                        Duration::from_millis(100),
                        |frame| assertion::match_not_visible(frame, "Committing..."),
                    )
                    .wait_for_text("Enter: reply", 5000)
                    .write_text("g")
                    .eventually(
                        Duration::from_secs(5),
                        Duration::from_millis(100),
                        |frame| {
                            let full = Region::full(frame.cols(), frame.rows());

                            assertion::match_text_in_region(frame, "[Commit Warning]", &full)
                        },
                    )
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "commit_warning",
                        "Auto-commit succeeds and prints the missing-hook warning",
                    )
            },
            |frame, report| {
                let warning_frame = common::frame_from_capture(&report.captures[0]);
                let warning_full = Region::full(warning_frame.cols(), warning_frame.rows());
                assertion::assert_text_in_region(
                    &warning_frame,
                    "Pre-commit hook warning",
                    &warning_full,
                );
                assertion::assert_text_in_region(
                    &warning_frame,
                    "not installed or executable.",
                    &warning_full,
                );
                assertion::assert_text_in_region(&warning_frame, "Install it", &warning_full);
                assertion::assert_text_in_region(
                    &warning_frame,
                    "become an error in a future release.",
                    &warning_full,
                );
                assertion::assert_text_in_region(&warning_frame, "prek install", &warning_full);
                assertion::assert_text_in_region(
                    &warning_frame,
                    "pre-commit install",
                    &warning_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "[Commit Warning]", &full);
                assertion::assert_text_in_region(frame, "Created pending worktree change", &full);
                assertion::assert_text_in_region(frame, "prek install", &full);
                assertion::assert_text_in_region(frame, "pre-commit install", &full);
            },
        )?;

    Ok(())
}

/// Verify that Claude sessions are launched with web-capable Claude Code
/// tools so current-information prompts do not require an interactive grant.
#[test]
fn claude_session_launch_allows_web_tools() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock();
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    install_web_tool_reporting_claude_stub(&env)?;

    let scenario = Scenario::new("claude_session_web_tools")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .press_key("Enter")
        .wait_for_stable_frame(300, 5000)
        .write_text("Use the web for current package docs")
        .wait_for_text("Use the web for current package docs", 3000)
        .press_key("Enter")
        .wait_for_text("Claude web tools enabled", 30000);

    // Act
    let frame = scenario.run(env.builder())?;

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Claude web tools enabled", &full);
    assertion::assert_not_visible(&frame, "Claude web tools missing");

    Ok(())
}
