//! Forge review comments and resolution.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::domain::session_message::SessionMessageKind;
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::region::Region;

use super::fixture::{
    E2eResult, run_git, seed_review_ready_session_with_review_request, seed_sessions_startup_tab,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

/// User-authored prompt retained in composer history after review resolution.
const REVIEW_HISTORY_PROMPT_TEXT: &str = "Explain the review status loader";

/// Seeds one unresolved thread whose latest comment is a prior Agentty reply.
fn seed_addressed_review_comment(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;
    seed_sessions_startup_tab(env)?;

    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"reviewThreads(first:"*)
    cat <<'JSON'
[{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"id":"thread-addressed","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":2,"path":"src/main.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"alice"},"body":"Please explain this output.","viewerDidAuthor":false},{"author":{"login":"agentty-bot"},"body":"No change is needed.\n\n<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000 -->","viewerDidAuthor":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"thread-reviewer-marker","diffSide":"RIGHT","isOutdated":false,"isResolved":false,"line":2,"path":"src/reviewer.rs","startLine":null,"subjectType":"LINE","comments":{"nodes":[{"author":{"login":"mallory"},"body":"Still needs work.\n\n<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000 -->","viewerDidAuthor":false}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]
JSON
    ;;
  *"comments(first:"*)
    printf '%s\n' '[{"data":{"repository":{"pullRequest":{"comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]'
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

/// Seeds the linked-review fixture with a delayed Claude turn so the feature
/// scenario can observe the review-resolution loader in progress.
fn seed_review_comment_agent_resolution(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::remove_dir_all(&session_worktree)?;
    let session_worktree_path = session_worktree.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            session_worktree_path.as_str(),
            "wt/review-s",
        ],
    )?;
    run_git(
        &session_worktree,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/agentty-xyz/agentty.git",
        ],
    )?;
    std::fs::create_dir_all(session_worktree.join("src"))?;
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"review\");\n}\n",
    )?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_model("review-shortcut-0001", "claude-haiku-4-5-20251001")
            .await?;
        database
            .sessions()
            .append_session_message(
                "review-shortcut-0001",
                SessionMessageKind::UserPrompt,
                REVIEW_HISTORY_PROMPT_TEXT,
            )
            .await
    })?;

    let claude_path = env.stub_bin.join("claude");
    std::fs::write(
        &claude_path,
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
sleep 10
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Processed the selected review threads."}]}}'
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"answer\":\"Processed the selected review threads.\",\"questions\":[],\"review_comment_outcomes\":[{\"reply\":\"Added the explanation.\",\"resolution\":\"fixed\",\"thread_id\":\"thread-inline\"},{\"reply\":\"The whole-file change is not needed because the scoped update covers the request.\",\"resolution\":\"fixed\",\"thread_id\":\"thread-file\"}]}","usage":{"input_tokens":5,"output_tokens":9}}'
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Seeds a two-thread review batch whose agent response omits one required
/// outcome so the UI can prove partial forge updates are rejected visibly.
fn seed_incomplete_review_comment_outcomes(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_comment_agent_resolution(env)?;

    let claude_path = env.stub_bin.join("claude");
    std::fs::write(
        &claude_path,
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Processed only one selected review thread."}]}}'
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"answer\":\"Processed only one selected review thread.\",\"questions\":[],\"review_comment_outcomes\":[{\"reply\":\"Added the explanation.\",\"resolution\":\"fixed\",\"thread_id\":\"thread-inline\"}]}","usage":{"input_tokens":5,"output_tokens":9}}'
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Verify linked review comments share one workspace with changed files and
/// the selected thread's current diff context.
#[test]
fn test_session_review_comments() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_review_comments")
        .with_git()
        .with_terminal_size(160, 60)
        .zola(
            "Unified diff review comments",
            "Browse changed files and linked review comments in one diff workspace.",
            44,
        )
        .setup(|env| {
            seed_review_ready_session_with_review_request(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("Please explain why this review output is needed.", 5000)
                    .wait_for_text("Use stdout context.", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "inline_review_comment",
                        "Multiline review comment with its attached code context",
                    )
                    .press_key("j")
                    .wait_for_text(
                        "This file-level comment is not attached to a code line.",
                        5000,
                    )
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "file_review_comment",
                        "File-level review comment without a synthetic code anchor",
                    )
                    .press_key("j")
                    .wait_for_text("Original code context unavailable.", 5000)
                    .wait_for_text("Space: select", 5000)
                    .press_key("Space")
                    .wait_for_text("[x]", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "outdated_review_comment",
                        "Outdated comment without misleading current diff context",
                    )
                    .press_key("f")
                    .wait_for_text("c: comments", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                let inline_frame = common::frame_from_capture(&report.captures[0]);
                assert_inline_review_comment(&inline_frame);

                let file_frame = common::frame_from_capture(&report.captures[1]);
                let file_full = Region::full(file_frame.cols(), file_frame.rows());
                assertion::assert_text_in_region(&file_frame, "file  ·  1 comments", &file_full);
                assertion::assert_text_in_region(
                    &file_frame,
                    "This file-level comment is not attached to a code line.",
                    &file_full,
                );
                assertion::assert_text_in_region(
                    &file_frame,
                    "Please review the whole file.",
                    &file_full,
                );
                assertion::assert_not_visible(&file_frame, "println!(\"review\")");

                let outdated_frame = common::frame_from_capture(&report.captures[2]);
                assert_outdated_review_comment(&outdated_frame);

                let files_focus_region = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Files", &files_focus_region);
                assertion::assert_not_visible(frame, "Space: select");
                assertion::assert_not_visible(frame, "Enter: submit");
            },
        )?;

    Ok(())
}

/// Verify that `Esc` returns review-comment focus to Files without leaving
/// Diff mode.
#[test]
fn test_review_comments_escape_focuses_files() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_comments_escape_focuses_files")
        .with_git()
        .with_terminal_size(160, 60)
        .setup(|env| {
            seed_review_ready_session_with_review_request(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("Space: select", 5000)
                    .wait_for_text("q: back", 5000)
                    .wait_for_text("f/Esc: files", 5000)
                    .press_key("Esc")
                    .wait_for_text("c: comments", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, "Files", &full);
                assertion::assert_text_in_region(frame, "c: comments", &full);
                assertion::assert_not_visible(frame, "Space: select");
            },
        )?;

    Ok(())
}

/// Verify an unchanged unresolved thread cannot be submitted again after an
/// Agentty reply, while remaining visible for reviewer follow-up.
#[test]
fn test_review_comment_addressed_guard() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_comment_addressed_guard")
        .with_git()
        .with_terminal_size(160, 60)
        .setup(seed_addressed_review_comment)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("addressed", 5000)
                    .press_key("Space")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "agentty_addressed",
                        "Agentty-authored marker disables unchanged feedback",
                    )
                    .press_key("j")
                    .wait_for_text("Space: select", 5000)
                    .press_key("Space")
                    .wait_for_text("[x]", 5000)
                    .wait_for_stable_frame(300, 5000)
            },
            |frame, report| {
                let addressed_frame = common::frame_from_capture(&report.captures[0]);
                let addressed_full = Region::full(addressed_frame.cols(), addressed_frame.rows());
                assertion::assert_text_in_region(
                    &addressed_frame,
                    "unresolved  ·  addressed",
                    &addressed_full,
                );
                assertion::assert_text_in_region(
                    &addressed_frame,
                    "No change is needed.",
                    &addressed_full,
                );
                assertion::assert_not_visible(&addressed_frame, "[x]");
                assertion::assert_not_visible(&addressed_frame, "Space: select");
                assertion::assert_not_visible(&addressed_frame, "Enter: submit");

                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, "Still needs work.", &full);
                assertion::assert_text_in_region(frame, "[x]", &full);
                assertion::assert_text_in_region(frame, "Selected 1", &full);
                assertion::assert_text_in_region(frame, "Space: select", &full);
                assertion::assert_text_in_region(frame, "Enter: submit", &full);
            },
        )?;

    Ok(())
}

/// Verifies grouped row placement and inline code-context rendering.
fn assert_inline_review_comment(frame: &TerminalFrame) {
    let full = Region::full(frame.cols(), frame.rows());
    let page_text = frame.text_in_region(&full);

    assertion::assert_text_in_region(frame, "Comments (6)", &full);
    assertion::assert_text_in_region(frame, "Files", &full);
    assertion::assert_text_in_region(frame, "Unresolved", &full);
    assertion::assert_text_in_region(frame, "Outdated", &full);
    assertion::assert_text_in_region(frame, "Resolved", &full);
    assertion::assert_text_in_region(frame, "Standalone", &full);
    assertion::assert_text_in_region(frame, "src/main.rs:1-2", &full);
    assertion::assert_text_in_region(frame, "Conversation", &full);
    assertion::assert_text_in_region(
        frame,
        "Please explain why this review output is needed.",
        &full,
    );
    assertion::assert_text_in_region(frame, "Use stdout context.", &full);
    assertion::assert_not_visible(frame, "hidden reviewer note");
    assertion::assert_not_visible(frame, "<strong>");
    assertion::assert_text_in_region(frame, "Code context", &full);
    assertion::assert_text_in_region(frame, "println!(\"review\")", &full);
    assert!(
        matches!(
            (
                page_text.find("Code context"),
                page_text.find("Conversation"),
                page_text.find("Outdated"),
                page_text.find("old.rs:2"),
                page_text.find("Resolved"),
                page_text.find("ro.rs:4"),
            ),
            (
                Some(code_context_index),
                Some(conversation_index),
                Some(outdated_group_index),
                Some(outdated_thread_index),
                Some(resolved_group_index),
                Some(resolved_outdated_thread_index),
            ) if code_context_index < conversation_index
                && outdated_group_index < outdated_thread_index
                && outdated_thread_index < resolved_group_index
                && resolved_group_index < resolved_outdated_thread_index
        ),
        "unexpected review comment layout:\n{page_text}"
    );
}

/// Verifies that an outdated thread stays actionable without stale context.
fn assert_outdated_review_comment(frame: &TerminalFrame) {
    let full = Region::full(frame.cols(), frame.rows());

    assertion::assert_text_in_region(frame, "unresolved  ·  outdated", &full);
    assertion::assert_text_in_region(frame, "Original code context unavailable.", &full);
    assertion::assert_text_in_region(frame, "This comment refers to an earlier diff.", &full);
    assertion::assert_text_in_region(frame, "[x]", &full);
    assertion::assert_not_visible(frame, "println!(\"review\")");
    assertion::assert_text_in_region(frame, "Space: select", &full);
    assertion::assert_text_in_region(frame, "j/k: select comment", &full);
}

/// Verify actionable review threads can be selected and submitted to the
/// active session agent as one evaluation batch.
#[test]
fn session_review_comment_agent_resolution() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_review_comment_agent_resolution")
        .with_git()
        .with_terminal_size(160, 60)
        .zola(
            "Batch review comment selection",
            "Select linked review comments, then submit one agent evaluation batch.",
            45,
        )
        .setup(seed_review_comment_agent_resolution)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("Space: select", 5000)
                    .press_key("Space")
                    .wait_for_text("[x]", 5000)
                    .press_key("j")
                    .press_key("Space")
                    .wait_for_text("Selected 2", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_comment_batch",
                        "Comments selected before batch evaluation",
                    )
                    .press_key("Enter")
                    .wait_for_text("Resolving 2 review comments...", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "agent_review_comment_resolution",
                        "Selected comments submitted to the session agent",
                    )
                    .wait_for_text("Processed the selected review threads.", 15000)
                    .wait_for_text("Enter: reply", 15000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Up")
                    .wait_for_text(REVIEW_HISTORY_PROMPT_TEXT, 5000)
                    .capture_labeled(
                        "review_comment_prompt_history",
                        "Composer history retains only user-authored prompts",
                    )
            },
            |frame, report| {
                let selection_frame = common::frame_from_capture(&report.captures[0]);
                let selection_full = Region::full(selection_frame.cols(), selection_frame.rows());
                assertion::assert_text_in_region(&selection_frame, "[x]", &selection_full);
                assertion::assert_text_in_region(&selection_frame, "Selected 2", &selection_full);
                let loader_frame = common::frame_from_capture(&report.captures[1]);
                let loader_full = Region::full(loader_frame.cols(), loader_frame.rows());
                assertion::assert_text_in_region(
                    &loader_frame,
                    "Resolving 2 review comments...",
                    &loader_full,
                );
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, REVIEW_HISTORY_PROMPT_TEXT, &full);
                assertion::assert_not_visible(
                    frame,
                    "Evaluate the following selected forge review comments",
                );
                assertion::assert_not_visible(frame, "Thread ID: thread-inline");
                assertion::assert_not_visible(frame, "Requested action:");
            },
        )?;

    Ok(())
}

/// Verify an incomplete structured outcome batch produces a visible warning
/// and does not silently apply only the reported thread.
#[test]
fn test_review_comment_incomplete_outcomes() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_comment_incomplete_outcomes")
        .with_git()
        .with_terminal_size(160, 60)
        .setup(seed_incomplete_review_comment_outcomes)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_text("c: comments", 5000)
                    .press_key("c")
                    .wait_for_text("Space: select", 5000)
                    .press_key("Space")
                    .press_key("j")
                    .press_key("Space")
                    .wait_for_text("Selected 2", 5000)
                    .press_key("Enter")
                    .wait_for_text("Processed only one selected review thread.", 10000)
                    .wait_for_text("exactly one valid outcome for 1 of 2", 10000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "exactly one valid outcome for 1 of 2",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "No review replies were posted or threads",
                    &full,
                );
                assertion::assert_text_in_region(
                    frame,
                    "resolved. Reopen review comments to retry",
                    &full,
                );
            },
        )?;

    Ok(())
}
