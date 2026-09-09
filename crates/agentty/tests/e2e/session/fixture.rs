//! Shared session fixtures and bounded Git command helpers.

use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::domain::agent::ReasoningLevel;
use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use agentty::domain::session_message::SessionMessageKind;
use agentty::test_support;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Executor};

use crate::common;
use crate::common::{BuilderEnv, SessionSeed};

pub(super) type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Stable id for the seeded running session used by stop-turn tests.
const RUNNING_STOP_SESSION_ID: &str = "running-stop-0001";

/// Focused-review output emitted when the prompt carries both the saved
/// decision and the instruction to honor it.
pub(super) const RESOLVED_DECISION_REVIEW_TEXT: &str = "Resolved session decision honored.";

/// Stable policy phrase the focused-review stub expects in the prompt.
///
/// This intentionally matches only the durable concept instead of one full
/// template sentence so harmless copy edits do not masquerade as behavior
/// regressions.
const REVIEW_DECISION_CONTEXT_PROMPT_MARKER: &str = "session chat history as decision context";

/// Accepted tradeoff saved in the transcript and expected by the review stub.
const RESOLVED_DECISION_HISTORY_TEXT: &str =
    "Understood. Retaining the println call is an accepted tradeoff for the demo output.";

/// Diagnostic emitted when the focused-review prompt omits its policy marker.
pub(super) const MISSING_DECISION_CONTEXT_POLICY_TEXT: &str =
    "Focused review prompt omitted decision-context guidance.";

/// Diagnostic emitted when the focused-review prompt omits saved chat context.
pub(super) const MISSING_RESOLVED_DECISION_HISTORY_TEXT: &str =
    "Focused review prompt omitted the resolved session decision.";

/// Wall-clock budget for one seeded git invocation before it is killed.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_mins(1);

/// Poll interval used while waiting for a seeded git invocation to exit.
const GIT_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Installs a deterministic Claude stub for stable-context title generation
/// and resumed review turns.
pub(super) fn seed_session_title_candidate_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
case "$prompt" in
  *"Generate a concise, commit-style title"*)
    case "$prompt" in
      *'\<latest_request> Improve session title generation. \</latest_request>'*)
        answer=''
        ;;
      *'\<original_request> Improve session title generation. \</original_request>'*'\<latest_request> Also reject punctuation-only copies. \</latest_request>'*)
        answer='Stabilize session title generation'
        ;;
      *)
        answer='Assess project quality'
        ;;
    esac
    ;;
  *"Also reject punctuation-only copies."*)
    answer='Follow-up complete. No files were changed.'
    ;;
  *"Improve session title generation."*)
    answer='The session title workflow is ready for a focused follow-up.'
    ;;
  *)
    answer='Got it. What would you like me to do?'
    ;;
esac
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '{"type":"result","subtype":"success","result":"{\\"answer\\":\\"%s\\",\\"questions\\":[],\\"review_comment_outcomes\\":[]}","usage":{"input_tokens":5,"output_tokens":9}}\n' "$answer"
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

/// Seeds one review-ready session plus its default source branch and
/// propagates setup errors to the caller.
pub(super) fn seed_review_with_resolved_decision(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::UserPrompt,
                "Keep the println call; its output is required by the demo contract.",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::AssistantAnswer,
                RESOLVED_DECISION_HISTORY_TEXT,
            )
            .await
    })?;

    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
case "$prompt" in
  *"{REVIEW_DECISION_CONTEXT_PROMPT_MARKER}"*)
    case "$prompt" in
      *"{RESOLVED_DECISION_HISTORY_TEXT}"*)
        answer='{RESOLVED_DECISION_REVIEW_TEXT}'
        ;;
      *)
        answer='{MISSING_RESOLVED_DECISION_HISTORY_TEXT}'
        ;;
    esac
    ;;
  *)
    answer='{MISSING_DECISION_CONTEXT_POLICY_TEXT}'
    ;;
esac
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '{{"type":"result","subtype":"success","result":"{{\\"project_impact\\":[\\"%s\\"],\\"suggestions\\":[]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}\n' "$answer"
"#,
    );
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(
        env,
        &[
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", "claude-haiku-4-5-20251001"),
        ],
    )
}

/// Seeds one review-ready session plus its default source branch and
/// propagates setup errors to the caller.
pub(super) fn seed_review_ready_session(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular("review-shortcut-0001", "gpt-5.6-sol", "main", "Review")
            .with_title("Review-ready session shortcuts"),
    )?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_diff_stats(12, 3, true, "review-shortcut-0001", "M")
            .await
    })?;

    run_git(&env.workdir, &["branch", "wt/review-s"])?;
    std::fs::create_dir_all(env.agentty_root.join("wt").join("review-s"))?;

    Ok(())
}

/// Seeds a review-ready session and opens the Sessions tab on startup.
pub(super) fn seed_review_ready_session_on_sessions_tab(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        test_support::persist_active_tab_for_test(&database, agentty::app::Tab::Sessions).await
    })?;

    Ok(())
}

/// Seeds the review-ready feature session with automatic addressing already
/// selected so semantic execution and GIF replay remain idempotent.
pub(super) fn seed_auto_address_review_mode(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_on_sessions_tab(env)?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_permission_mode(
                "review-shortcut-0001",
                agentty::domain::permission::PermissionMode::AutoEditAddressComments,
            )
            .await
    })?;

    Ok(())
}

/// Seeds the rebase transcript fixture with a configurable pre-rebase delay.
pub(super) fn seed_rebase_transcript_session_with_delay(
    env: &BuilderEnv,
    delay_seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_linked_review_worktree_with_diff(env)?;
    std::fs::write(env.workdir.join("base-update.txt"), "new base commit\n")?;
    run_git(&env.workdir, &["add", "base-update.txt"])?;
    run_git(&env.workdir, &["commit", "-m", "advance base branch"])?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::UserPrompt,
                "keep completed transcript stable",
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::AssistantAnswer,
                "Completed answer before rebase.",
            )
            .await
    })?;

    let pre_rebase_hook = env.workdir.join(".git").join("hooks").join("pre-rebase");
    std::fs::write(
        &pre_rebase_hook,
        format!("#!/bin/sh\nsleep {delay_seconds}\n"),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&pre_rebase_hook, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Starts a feature recording on the Sessions tab without replay-time tab
/// persistence changing the scenario's first action.
pub(super) fn seed_sessions_tab(env: &BuilderEnv) -> E2eResult {
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        test_support::persist_active_tab_for_test(&database, agentty::app::Tab::Sessions).await
    })?;

    Ok(())
}

/// Answer returned through Claude's schema-validated result field.
pub(super) const CLAUDE_STRUCTURED_RESPONSE_TEXT: &str = "Claude structured response rendered";

/// Installs a Claude stub that returns its final protocol reply through
/// `structured_output`, matching current Claude Code schema-validated turns.
pub(super) fn seed_claude_structured_output_project(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"StructuredOutput","input":{{"answer":"{CLAUDE_STRUCTURED_RESPONSE_TEXT}"}}}}]}}}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"","structured_output":{{"answer":"{CLAUDE_STRUCTURED_RESPONSE_TEXT}","questions":[]}},"usage":{{"input_tokens":8,"output_tokens":6}}}}'
"#
    );
    std::fs::write(&claude_path, script)?;

    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    seed_project_settings(env, &[("DefaultSmartModel", "claude-haiku-4-5-20251001")])?;

    Ok(())
}

/// Seeds one running session so `Ctrl+c` can exercise the turn-stop path
/// without needing a live agent backend.
pub(super) fn seed_running_stop_session(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(RUNNING_STOP_SESSION_ID, "gpt-5.6-sol", "main", "InProgress")
            .with_title("Running session stop"),
    )?;

    // Match `session_folder()` so the seeded row has the worktree path the
    // runtime expects for this session id.
    let worktree_name = &RUNNING_STOP_SESSION_ID[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_reasoning_level(RUNNING_STOP_SESSION_ID, ReasoningLevel::High)
            .await
    })?;

    Ok(())
}

/// Persists project-scoped settings so feature tests route model selections
/// to deterministic backends even when additional real CLIs exist on `PATH`.
pub(super) fn seed_project_settings(
    env: &BuilderEnv,
    settings: &[(&str, &str)],
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let canonical_workdir = env.workdir.canonicalize()?;
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
        drop(database);

        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        for (setting_name, setting_value) in settings {
            let query = sqlx::query!(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value
",
                project_id,
                setting_name,
                setting_value
            );
            connection.execute(query).await?;
        }
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    Ok(())
}

/// Seeds one review-ready session with a linked review request.
pub(super) fn seed_review_ready_session_with_review_request(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;
    seed_github_review_request_stub(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        let review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/review-s".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Review-ready session shortcuts".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        };

        database
            .reviews()
            .update_session_review_request("review-shortcut-0001", Some(review_request.clone()))
            .await
    })?;

    Ok(())
}

/// Seeds the review session folder with a real git diff and GitHub remote so
/// diff mode and background review-comment sync can run without live services.
pub(super) fn seed_review_worktree_with_diff(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_clean_review_worktree(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;

    Ok(())
}

/// Seeds the review session as a real linked worktree so submitting its line
/// comment can exercise the next agent turn without an isolation warning.
pub(super) fn seed_linked_review_worktree_with_diff(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::remove_dir(&session_worktree)?;
    let session_worktree_path = session_worktree
        .to_str()
        .ok_or("session worktree path must be valid UTF-8")?;
    run_git(
        &env.workdir,
        &["worktree", "add", session_worktree_path, "wt/review-s"],
    )?;
    std::fs::create_dir_all(session_worktree.join("src"))?;
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "add main"])?;

    Ok(())
}

/// Seeds the review session folder as a clean Git worktree.
pub(super) fn seed_clean_review_worktree(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::create_dir_all(session_worktree.join("src"))?;
    run_git(&session_worktree, &["init", "-b", "main"])?;
    run_git(
        &session_worktree,
        &["config", "user.email", "test@test.com"],
    )?;
    run_git(&session_worktree, &["config", "user.name", "Test"])?;
    std::fs::write(session_worktree.join("src/main.rs"), "fn main() {\n}\n")?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "init"])?;
    run_git(
        &session_worktree,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ],
    )?;

    Ok(())
}

/// Runs one git command in `working_directory`, returning an error with stderr
/// detail when git fails and discarding its stdout.
pub(super) fn run_git(
    working_directory: &Path,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    run_git_stdout(working_directory, args)?;

    Ok(())
}

/// Runs one git command in `working_directory` and returns trimmed stdout.
///
/// The child is killed and reported as an error once `GIT_COMMAND_TIMEOUT`
/// elapses so a stalled git process fails the test instead of hanging CI.
pub(super) fn run_git_stdout(
    working_directory: &Path,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(working_directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Drain both pipes from reader threads so a chatty git command cannot fill
    // a pipe buffer and block while the timeout loop polls for exit.
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("git stdout pipe missing"))?;
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("git stderr pipe missing"))?;
    let stdout_reader = thread::spawn(move || {
        let mut stdout = Vec::new();
        child_stdout.read_to_end(&mut stdout).map(|_| stdout)
    });
    let stderr_reader = thread::spawn(move || {
        let mut stderr = Vec::new();
        child_stderr.read_to_end(&mut stderr).map(|_| stderr)
    });

    let deadline = Instant::now() + GIT_COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }

        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;

            return Err(std::io::Error::other(format!(
                "git {} timed out after {GIT_COMMAND_TIMEOUT:?}",
                args.join(" ")
            ))
            .into());
        }

        thread::sleep(GIT_COMMAND_POLL_INTERVAL);
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("git stderr reader panicked"))??;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&stderr)
        ))
        .into());
    }

    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

/// Installs a deterministic `gh` stub that returns review-request status.
fn seed_github_review_request_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"addPullRequestReviewThreadReply"*)
    printf '%s\n' '{"data":{"addPullRequestReviewThreadReply":{"comment":{"id":"reply-1"}}}}'
    ;;
  *"resolveReviewThread"*)
    printf '%s\n' '{"data":{"resolveReviewThread":{"thread":{"id":"thread-inline","isResolved":true}}}}'
    ;;
  *"reviewThreads(first:"*)
    cat <<'JSON'
[{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"id":"thread-inline","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":2,"path":"src/main.rs","startLine":1,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"alice"},"body":"<!-- hidden reviewer note --><p>Please <strong>explain</strong> why this review output is needed.<br>Use <code>stdout</code> context.</p>"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-file","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":null,"path":"src/main.rs","startLine":null,"subjectType":"FILE","comments":{"nodes":[{"author":{"login":"bob"},"body":"Please review the whole file."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-outdated","diffSide":"RIGHT","isOutdated":true,"isResolved":false,"line":2,"path":"old.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"erin"},"body":"This comment refers to an earlier diff."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-resolved","diffSide":"RIGHT","isOutdated":false,"isResolved":true,"line":3,"path":"src/main.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"dana"},"body":"This thread is complete."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-resolved-outdated","diffSide":"LEFT","isOutdated":true,"isResolved":true,"line":4,"path":"ro.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"frank"},"body":"This resolved thread refers to an earlier diff."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]
JSON
    ;;
  *"comments(first:"*)
    cat <<'JSON'
[{"data":{"repository":{"pullRequest":{"comments":{"nodes":[{"author":{"login":"carol"},"body":"Thanks for documenting the behavior."}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]
JSON
    ;;
  *"pr view"*)
    printf '%s\n' '{"number":42,"title":"Review-ready session shortcuts","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/review-s","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Persists the `Sessions` tab as the startup tab.
///
/// `Tab` is the composer focus toggle under test, so the scenario cannot spend
/// a `Tab` press on tab navigation: the seeded startup tab keeps every `Tab` in
/// the scenario meaningful, and keeps the PTY proof and the VHS replay (which
/// share this database) starting from the same tab.
pub(super) fn seed_sessions_startup_tab(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        // Opening the database applies the migrations that create `setting`.
        common::open_database(env).await?;

        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        let query = sqlx::query!(
            r"
INSERT INTO setting (name, value) VALUES ('ActiveTab', 'Sessions')
ON CONFLICT(name) DO UPDATE SET value = excluded.value
"
        );
        connection.execute(query).await?;
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    Ok(())
}
