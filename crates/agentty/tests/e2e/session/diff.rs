//! Diff navigation, previews, and line comments.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::test_support;
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{
    E2eResult, run_git, seed_clean_review_worktree, seed_linked_review_worktree_with_diff,
    seed_project_settings, seed_review_ready_session,
    seed_review_ready_session_with_review_request, seed_review_worktree_with_diff,
    seed_sessions_startup_tab,
};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Stable id for the seeded binary-only diff session.
const BINARY_DIFF_SESSION_ID: &str = "binary-diff-0001";

/// Seeds one review-ready session whose latest diff refresh found no changes.
fn seed_clean_review_ready_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_clean_review_worktree(env)?;

    let runtime = common::seed_runtime()?;

    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_diff_stats(0, 0, false, "review-shortcut-0001", "XS")
            .await
    })?;

    Ok(())
}

/// Seeds a linked review worktree whose diff replaces one existing source
/// line, yielding adjacent old- and new-side rows.
fn seed_linked_review_worktree_with_replacement_diff(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(env.workdir.join("src"))?;
    std::fs::write(
        env.workdir.join("src/main.rs"),
        "fn main() {\n    println!(\"before\");\n}\n",
    )?;
    run_git(&env.workdir, &["add", "."])?;
    run_git(&env.workdir, &["commit", "-m", "add initial main"])?;
    seed_review_ready_session(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    std::fs::remove_dir(&session_worktree)?;
    let session_worktree_path = session_worktree
        .to_str()
        .ok_or("session worktree path must be valid UTF-8")?;
    run_git(
        &env.workdir,
        &["worktree", "add", session_worktree_path, "wt/review-s"],
    )?;
    std::fs::write(
        session_worktree.join("src/main.rs"),
        "fn main() {\n    println!(\"after\");\n}\n",
    )?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "replace main"])?;

    Ok(())
}

/// Installs a deterministic Codex app-server stub for the submitted line
/// comment turn.
fn seed_line_comment_codex_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let codex_path = env.stub_bin.join("codex");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.146.0\n'; exit 0; fi

extract_id() {
    printf '%s\n' "$1" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'
}

while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{}}\n' "$request_id"
            ;;
        *'"method":"thread/start"'*|*'"method":"thread/resume"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"thread":{"id":"line-comment-thread"}}}\n' "$request_id"
            ;;
        *'"method":"turn/start"'*)
            request_id=$(extract_id "$request")
            printf '{"id":"%s","result":{"turn":{"id":"line-comment-turn"}}}\n' "$request_id"
            printf '%s\n' '{"method":"turn/started","params":{"turn":{"id":"line-comment-turn"}}}'
            sleep 1
            printf '%s\n' '{"method":"item/completed","params":{"threadId":"line-comment-thread","turnId":"line-comment-turn","item":{"type":"agentMessage","id":"line-comment-answer","text":"{\"answer\":\"Line comment received.\",\"questions\":[]}","phase":"final_answer"}}}'
            printf '%s\n' '{"method":"turn/completed","params":{"threadId":"line-comment-thread","turn":{"id":"line-comment-turn","status":"completed","items":[]}}}'
            ;;
    esac
done
"#;
    std::fs::write(&codex_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&codex_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Installs a deterministic review provider so the automatic post-turn review
/// reaches a visible terminal state before the final feature capture.
fn seed_line_comment_review_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
printf '%s\n' '{"type":"system","subtype":"init"}'
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"project_impact\":[\"No review findings.\"],\"suggestions\":[]}","usage":{"input_tokens":5,"output_tokens":9}}'
"#;
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

/// Installs a tmux stub that edits the clean review worktree when opened.
fn install_worktree_edit_tmux_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let edited_file = env
        .agentty_root
        .join("wt")
        .join("review-s")
        .join("src/main.rs")
        .to_string_lossy()
        .replace('\'', "'\\''");
    let tmux_path = env.stub_bin.join("tmux");
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "new-window" ]; then
  printf '%s\n' 'external worktree edit' > '{edited_file}'
  printf '%s\n' '@42'
fi
"#
    );
    std::fs::write(&tmux_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&tmux_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Adds a changed file below a single-child folder chain for compact-tree
/// rendering coverage.
fn seed_review_session_with_compact_diff_tree(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    let nested_directory = session_worktree.join("src/app/session");
    let nested_file = nested_directory.join("handler.rs");
    std::fs::create_dir_all(&nested_directory)?;
    std::fs::write(&nested_file, "fn handle() {\n}\n")?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "add nested handler"])?;
    std::fs::write(
        &nested_file,
        "fn handle() {\n    println!(\"review\");\n}\n",
    )?;

    Ok(())
}

/// Seeds a clean review session whose worktree-open action creates an edit.
fn seed_clean_review_session_with_worktree_edit(env: &BuilderEnv) -> E2eResult {
    seed_clean_review_ready_session(env)?;
    install_worktree_edit_tmux_stub(env)
}

/// Seeds a failing review diff whose external driver stays busy long enough
/// to prove that the TUI accepts cancellation before Git completes.
fn seed_slow_review_diff(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    let slow_diff_driver = session_worktree.join("slow-diff.sh");
    std::fs::write(&slow_diff_driver, "#!/bin/sh\nsleep 3\nexit 1\n")?;
    #[cfg(unix)]
    std::fs::set_permissions(&slow_diff_driver, std::fs::Permissions::from_mode(0o750))?;
    let slow_diff_driver = slow_diff_driver
        .to_str()
        .ok_or("slow diff driver path must be valid UTF-8")?;
    run_git(
        &session_worktree,
        &["config", "diff.external", slow_diff_driver],
    )
}

/// Seeds enough changed lines to demonstrate right-pane cursor navigation and
/// viewport scrolling without a live agent backend.
fn seed_scrollable_diff_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;
    seed_review_worktree_with_diff(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    let changed_lines = (0..80)
        .map(|line_index| format!("    println!(\"changed line {line_index:02}\");"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        session_worktree.join("src/main.rs"),
        format!("fn main() {{\n{changed_lines}\n}}\n"),
    )?;

    Ok(())
}

/// Seeds a review-ready worktree whose only change is previewable markdown.
fn seed_markdown_diff_preview(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let session_worktree = env.agentty_root.join("wt").join("review-s");
    run_git(&session_worktree, &["init", "-b", "main"])?;
    run_git(
        &session_worktree,
        &["config", "user.email", "test@test.com"],
    )?;
    run_git(&session_worktree, &["config", "user.name", "Test"])?;
    std::fs::create_dir_all(session_worktree.join("docs"))?;
    std::fs::write(session_worktree.join("docs/日本.md"), "# Before\n")?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "init"])?;
    std::fs::write(
        session_worktree.join("docs/日本.md"),
        concat!(
            "# Rendered Markdown Preview\n\n",
            "| Mode | Output |\n| --- | --- |\n| Preview | Markdown |\n\n",
            "```mermaid\ngraph TD\nSource[Source] --> Preview[Preview]\n```\n",
        ),
    )?;

    Ok(())
}

/// Seeds a review-ready session whose only worktree change is binary and
/// whose persisted diff presence remains conservatively unknown.
fn seed_binary_diff_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    common::seed_session(
        env,
        SessionSeed::regular(BINARY_DIFF_SESSION_ID, "gpt-5.6-sol", "main", "Review")
            .with_title("Binary diff session"),
    )?;

    let session_worktree =
        test_support::session_folder(&env.agentty_root.join("wt"), BINARY_DIFF_SESSION_ID);
    std::fs::create_dir_all(&session_worktree)?;
    run_git(&session_worktree, &["init", "-b", "main"])?;
    run_git(
        &session_worktree,
        &["config", "user.email", "test@test.com"],
    )?;
    run_git(&session_worktree, &["config", "user.name", "Test"])?;
    std::fs::write(session_worktree.join("asset.bin"), [0_u8, 1, 2, 3])?;
    run_git(&session_worktree, &["add", "."])?;
    run_git(&session_worktree, &["commit", "-m", "init"])?;
    std::fs::write(session_worktree.join("asset.bin"), [0_u8, 4, 5, 6, 7])?;

    let runtime = common::seed_runtime()?;
    runtime.block_on(async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .mark_session_diff_unknown(BINARY_DIFF_SESSION_ID)
            .await
    })?;

    Ok(())
}

/// Verify that opening the diff page from a review-ready session shows the
/// selected file's local changes and change totals.
#[test]
fn diff_preview_opens_from_session() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_preview")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("diff_preview", "Diff preview after pressing d")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assert_diff_file_tree_change_totals(frame);
                assertion::assert_text_in_region(frame, "println!(\"review\")", &full);
                assertion::assert_text_in_region(frame, "j/k: select file", &full);
            },
        )?;

    Ok(())
}

/// Verify that Diff mode collapses uninterrupted folder chains so the Files
/// sidebar shows more changed paths at once.
#[test]
fn test_compact_diff_tree() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("compact_diff_tree")
        .with_git()
        .setup(seed_review_session_with_compact_diff_tree)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("app/session/", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "compact_diff_tree",
                        "Diff tree with a compact single-child folder chain",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let file_tree = Region::new(0, 0, frame.cols() / 5, frame.rows());

                assertion::assert_text_in_region(frame, "app/session/", &full);
                assertion::assert_text_in_region(frame, "handler.rs", &full);
                assertion::assert_text_in_region(frame, "src/a", &file_tree);
                assertion::assert_text_in_region(frame, "han", &file_tree);
                let root_match = frame
                    .find_text_in_region("src/a", &file_tree)
                    .into_iter()
                    .next()
                    .expect("compact tree should render its root path");
                let nested_match = frame
                    .find_text_in_region("han", &file_tree)
                    .into_iter()
                    .next()
                    .expect("compact tree should render its nested path");
                assert_eq!(root_match.rect.col, 2);
                assert_eq!(nested_match.rect.col, 4);
            },
        )?;

    Ok(())
}

/// Verify that a known-clean session neither advertises nor opens Diff mode.
#[test]
fn test_clean_session_hides_diff_action() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("clean_session_hides_diff_action")
        .with_git()
        .setup(seed_clean_review_ready_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("d")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "clean_session_view",
                        "Clean session remains in chat after pressing d",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(frame, "Review-ready session shortcuts", &full);
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "d: diff");
                assertion::assert_not_visible(frame, "Loading diff...");
            },
        )?;

    Ok(())
}

/// Verify opening a clean writable worktree makes subsequent edits inspectable.
#[test]
fn test_worktree_open_reenables_diff_action() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("worktree_open_reenables_diff_action")
        .env("TMUX", "/tmp/tmux-agentty-test/default,1,0")
        .with_git()
        .setup(seed_clean_review_session_with_worktree_edit)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("Enter: reply", 5000)
                    .capture_labeled("clean_session", "Clean session hides the diff action")
                    .press_key("o")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("d")
                    .wait_for_text("external worktree edit", 5000)
                    .capture_labeled(
                        "external_edit_diff",
                        "Diff opened after editing through the writable worktree",
                    )
            },
            |frame, report| {
                let clean_frame = common::frame_from_capture(&report.captures[0]);
                assertion::assert_not_visible(&clean_frame, "d: diff");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "external worktree edit", &full);
                assertion::assert_text_in_region(frame, "q/Esc: back", &full);
            },
        )?;

    Ok(())
}

/// Verify that a slow full diff leaves redraw and input handling responsive.
#[test]
fn test_slow_diff_loading_remains_cancelable() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("slow_diff_loading_remains_cancelable")
        .with_git()
        .setup(seed_slow_review_diff)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("Loading diff...", 2000)
                    .capture_labeled(
                        "diff_loading",
                        "Slow Git diff shows a responsive loading page",
                    )
                    .press_key("q")
                    .wait_for_text("Review-ready session shortcuts", 2000)
                    .capture_labeled(
                        "diff_loading_canceled",
                        "Cancel returns before the slow Git diff completes",
                    )
                    .press_key("d")
                    .wait_for_text("Unable to load diff:", 5000)
                    .capture_labeled(
                        "diff_loading_failed",
                        "Git failure returns to the session with a diagnostic",
                    )
            },
            |frame, report| {
                let loading_frame = common::frame_from_capture(&report.captures[0]);
                let loading_full = Region::full(loading_frame.cols(), loading_frame.rows());
                assertion::assert_text_in_region(&loading_frame, "Loading diff...", &loading_full);
                assertion::assert_not_visible(&loading_frame, "No files");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Review-ready session shortcuts", &full);
                assertion::assert_text_in_region(frame, "Unable to load diff:", &full);
                assertion::assert_not_visible(frame, "Loading diff...");
                assertion::assert_not_visible(frame, "No files");
            },
        )?;

    Ok(())
}

/// Verify that the right-hand patch scrolls while Files remains focused, then
/// `l` moves focus into changed-line navigation without resetting the viewport.
#[test]
fn test_diff_changed_line_navigation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_changed_line_navigation")
        .with_git()
        .setup(seed_scrollable_diff_session)
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000);
                let scenario = (0..70).fold(scenario, |scenario, _| scenario.press_key("Down"));
                scenario
                    .wait_for_text("changed line 70", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "diff_file_scroll",
                        "Selected file scrolled without leaving Files focus",
                    )
                    .press_key("l")
                    .wait_for_text("Esc/Left: files", 5000)
                    .wait_for_text("changed line 70", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "diff_changed_line_navigation",
                        "Changed-line cursor entered at the retained file position",
                    )
            },
            |frame, report| {
                let file_focus_frame = common::frame_from_capture(&report.captures[0]);
                let file_focus_full =
                    Region::full(file_focus_frame.cols(), file_focus_frame.rows());
                let full = Region::full(frame.cols(), frame.rows());

                assertion::assert_text_in_region(
                    &file_focus_frame,
                    "changed line 70",
                    &file_focus_full,
                );
                assertion::assert_text_in_region(
                    &file_focus_frame,
                    "j/k: select file",
                    &file_focus_full,
                );
                assertion::assert_text_in_region(frame, "changed line 70", &full);
                assertion::assert_text_in_region(frame, "Esc/Left: files", &full);
                assertion::assert_text_in_region(frame, "j/k: select row", &full);
                let file_row = frame
                    .find_text("src/")
                    .first()
                    .expect("selected file's parent folder should remain visible")
                    .rect
                    .row
                    .saturating_add(1);
                let aligned_changed_line_row = frame
                    .find_text("changed line 65")
                    .first()
                    .expect("aligned changed line should remain visible")
                    .rect
                    .row;
                assert_eq!(aligned_changed_line_row, file_row);
            },
        )?;

    Ok(())
}

/// Verify that file and changed-line comments survive navigation until one
/// batch is submitted as the next session turn.
#[test]
fn test_diff_comment_file_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_comment_file_lookup")
        .with_git()
        .zola(
            "Look up files in diff comments",
            "Reference repository files while writing diff feedback.",
            48,
        )
        .setup(|env| {
            seed_review_ready_session(env)?;
            seed_linked_review_worktree_with_diff(env)?;
            seed_line_comment_codex_stub(env)?;
            seed_line_comment_review_stub(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_text("Enter/l: open", 5000)
                    .write_text("C")
                    .wait_for_text("File comment", 5000)
                    .write_text("See @src/ma")
                    .wait_for_text("Tab/Enter: select", 5000)
                    .capture_labeled(
                        "comment_lookup",
                        "File lookup beside the active comment input",
                    )
                    .press_key("Tab")
                    .write_text("for context")
                    .wait_for_text("See @src/main.rs for context", 5000)
                    .capture_labeled(
                        "lookup_selected",
                        "File reference inserted without leaving the comment editor",
                    )
            },
            |frame, report| {
                let lookup = common::frame_from_capture(&report.captures[0]);
                let content =
                    Region::new(lookup.cols() / 5, 0, lookup.cols() * 4 / 5, lookup.rows());
                assertion::assert_text_in_region(&lookup, "src/main.rs", &content);
                assertion::assert_text_in_region(&lookup, "Files (", &content);
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "See @src/main.rs for context", &full);
            },
        )?;

    Ok(())
}

#[test]
fn test_diff_line_comments() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_line_comments")
        .with_git()
        .zola(
            "Comment on files and changed lines",
            "Keep file and inline diff comments across screens, then submit them together.",
            47,
        )
        .setup(|env| {
            seed_review_ready_session(env)?;
            seed_linked_review_worktree_with_diff(env)?;
            seed_line_comment_codex_stub(env)?;
            seed_line_comment_review_stub(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(diff_line_comments_scenario, assert_diff_line_comments)?;

    Ok(())
}

/// Builds the file/inline comment persistence and submission journey.
fn diff_line_comments_scenario(scenario: Scenario) -> Scenario {
    // Ctrl+M emits Enter's carriage return consistently in PTY and VHS.
    const ENTER_KEY: &str = "Ctrl+m";

    let scenario = scenario
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::open_selected_session_view())
        .press_key("d")
        .wait_for_text("main.rs", 5000)
        .press_key("j")
        .wait_for_stable_frame(200, 3000)
        .wait_for_text("Enter/l: open", 5000)
        .write_text("C")
        .wait_for_text("File comment", 5000);

    enter_multiline_diff_file_comment(scenario)
        .wait_for_stable_frame(300, 3000)
        .press_key("Esc")
        .wait_for_text("Add regression coverage.", 3000)
        .capture_labeled(
            "whole_file_comment",
            "Multiline whole-file feedback appears above the selected patch",
        )
        .press_key("j")
        .wait_for_text("Enter: comment", 5000)
        .press_key(ENTER_KEY)
        .wait_for_text("New line 1", 5000)
        .write_text("Explain the entry point.")
        .wait_for_text("Explain the entry point.|", 3000)
        .press_key(ENTER_KEY)
        .press_key("k")
        .press_key("j")
        .wait_for_stable_frame(300, 5000)
        .capture_labeled(
            "selected_inline_comment",
            "Completed inline comment selected for editing",
        )
        .press_key(ENTER_KEY)
        .wait_for_text("Explain the entry point.|", 3000)
        .write_text(" Updated.")
        .press_key(ENTER_KEY)
        .press_key("j")
        .press_key(ENTER_KEY)
        .write_text("Why print review?")
        .wait_for_text("Why print review?|", 3000)
        .press_key(ENTER_KEY)
        .wait_for_text("Why print review?", 3000)
        .wait_for_stable_frame(1000, 5000)
        .capture_labeled(
            "inline_line_comments",
            "Multiple comments remain visible inside the diff",
        )
        .viewing_pause_ms(1500)
        .press_key("q")
        .wait_for_text("Enter: reply", 5000)
        .press_key("d")
        .wait_for_text("main.rs", 5000)
        .press_key("j")
        .wait_for_text("Explain the entry point. Updated.", 5000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled(
            "restored_line_comments",
            "Diff comments survive a round trip through session chat",
        )
        .wait_for_text("s: submit comments", 3000)
        .press_key("s")
        .wait_for_text("File comments:", 5000)
        .wait_for_text("src/main.rs: Review the whole file.", 5000)
        .wait_for_text("| Check the tests too.", 5000)
        .wait_for_text("| Add regression coverage.", 5000)
        .wait_for_text("Line comments:", 5000)
        .wait_for_text(
            "src/main.rs:1 [new]: Explain the entry point. Updated.",
            5000,
        )
        .wait_for_text("src/main.rs:2 [new]: Why print review?", 5000)
        .wait_for_text("Ctrl+c: stop", 5000)
        .wait_for_text("Line comment received.", 5000)
        .wait_for_text("Enter: reply", 5000)
        .wait_for_text("[Commit] No changes to commit.", 5000)
        .wait_for_text("No review findings.", 5000)
        .write_text("G")
        .wait_for_stable_frame(1000, 5000)
        .viewing_pause_ms(1500)
        .capture_labeled(
            "line_comment_submitted",
            "Line comment submitted in the next session turn",
        )
        .press_key("d")
        .wait_for_text("main.rs", 5000)
        .press_key("j")
        .wait_for_text("Shift+C: comment", 5000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled(
            "submitted_comments_purged",
            "Submitted comments no longer appear in the next diff view",
        )
        .press_key("q")
        .wait_for_text("Line comment received.", 5000)
}

/// Enters enough file-comment rows to exercise completed-editor expansion.
fn enter_multiline_diff_file_comment(scenario: Scenario) -> Scenario {
    scenario
        .write_text("Review the whole file.")
        .press_key("Ctrl+j")
        .write_text("Check the tests too.")
        .press_key("Ctrl+j")
        .write_text("Verify the docs.")
        .press_key("Ctrl+j")
        .write_text("Keep the public API stable.")
        .press_key("Ctrl+j")
        .write_text("Handle errors explicitly.")
        .press_key("Ctrl+j")
        .write_text("Add regression coverage.")
}

/// Verifies inline comment selection, editing, and next-turn submission.
fn assert_diff_line_comments(frame: &TerminalFrame, report: &ProofReport) {
    let file_comment_frame = common::frame_from_capture(&report.captures[0]);
    let file_comment_full = Region::full(file_comment_frame.cols(), file_comment_frame.rows());
    assertion::assert_text_in_region(&file_comment_frame, "File comment", &file_comment_full);
    assertion::assert_text_in_region(
        &file_comment_frame,
        "Add regression coverage.",
        &file_comment_full,
    );

    let selected_comment_frame = common::frame_from_capture(&report.captures[1]);
    let selected_comment_full =
        Region::full(selected_comment_frame.cols(), selected_comment_frame.rows());
    assertion::assert_text_in_region(
        &selected_comment_frame,
        "New line 1",
        &selected_comment_full,
    );
    assertion::assert_text_in_region(
        &selected_comment_frame,
        "Explain the entry point.",
        &selected_comment_full,
    );

    let diff_frame = common::frame_from_capture(&report.captures[2]);
    let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
    assertion::assert_text_in_region(&diff_frame, "Explain the entry point. Updated.", &diff_full);
    assertion::assert_text_in_region(&diff_frame, "Why print review?", &diff_full);

    let restored_frame = common::frame_from_capture(&report.captures[3]);
    let restored_full = Region::full(restored_frame.cols(), restored_frame.rows());
    assertion::assert_text_in_region(
        &restored_frame,
        "Explain the entry point. Updated.",
        &restored_full,
    );

    let purged_frame = common::frame_from_capture(&report.captures[5]);
    assertion::assert_not_visible(&purged_frame, "Explain the entry point. Updated.");
    assertion::assert_not_visible(&purged_frame, "Why print review?");
    assertion::assert_not_visible(&purged_frame, "s: submit comments");

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(frame, "Line comment received.", &full);
}

/// Verify that `Shift+V` selects a changed-row range for one inline comment.
#[test]
fn test_diff_row_selection_comments() -> E2eResult {
    // Arrange — Ctrl+M emits Enter's carriage return consistently in PTY and
    // VHS.
    const ENTER_KEY: &str = "Ctrl+m";

    // Act, Assert
    FeatureTest::new("diff_row_selection_comments")
        .with_git()
        .setup(|env| {
            seed_linked_review_worktree_with_replacement_diff(env)?;
            seed_sessions_startup_tab(env)
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key(ENTER_KEY)
                    .wait_for_text("Enter: comment", 5000)
                    .write_text("V")
                    .wait_for_text("Esc: cancel", 5000)
                    .press_key("j")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "selected_comment_rows",
                        "Visual line selection highlights a changed-row range",
                    )
                    .write_text("C")
                    .wait_for_text("File comment", 5000)
                    .write_text("Review the selected file.")
                    .press_key(ENTER_KEY)
                    .wait_for_text("Review the selected file.", 3000)
                    .capture_labeled(
                        "file_comment_from_row_selection",
                        "Whole-file feedback replaces the visual row selection",
                    )
                    .write_text("V")
                    .press_key("j")
                    .press_key(ENTER_KEY)
                    .wait_for_text("Old line 2 · New line 2", 5000)
                    .write_text("Explain these lines.")
                    .wait_for_text("Explain these lines.|", 3000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "editing_row_range_comment",
                        "Selected rows stay highlighted while entering the comment",
                    )
                    .press_key(ENTER_KEY)
                    .wait_for_text("Explain these lines.", 3000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "row_range_comment",
                        "The completed comment keeps its source range highlighted",
                    )
            },
            |frame, report| {
                let selection_frame = common::frame_from_capture(&report.captures[0]);
                let selection_full = Region::full(selection_frame.cols(), selection_frame.rows());
                assertion::assert_text_in_region(&selection_frame, "Esc: cancel", &selection_full);
                assertion::assert_text_in_region(&selection_frame, "Shift+C", &selection_full);

                let file_comment_frame = common::frame_from_capture(&report.captures[1]);
                let file_comment_full =
                    Region::full(file_comment_frame.cols(), file_comment_frame.rows());
                assertion::assert_text_in_region(
                    &file_comment_frame,
                    "File comment",
                    &file_comment_full,
                );
                assertion::assert_text_in_region(
                    &file_comment_frame,
                    "Review the selected file.",
                    &file_comment_full,
                );
                assertion::assert_not_visible(&file_comment_frame, "Esc: cancel");

                let editor_frame = common::frame_from_capture(&report.captures[2]);
                let editor_full = Region::full(editor_frame.cols(), editor_frame.rows());
                assertion::assert_text_in_region(
                    &editor_frame,
                    "Old line 2 · New line 2",
                    &editor_full,
                );
                assertion::assert_text_in_region(
                    &editor_frame,
                    "Explain these lines.|",
                    &editor_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Old line 2 · New line 2", &full);
                assertion::assert_text_in_region(frame, "Explain these lines.", &full);
            },
        )?;

    Ok(())
}

/// Verify that `p` toggles a changed markdown file between raw diff and a
/// rendered markdown/mermaid preview.
#[test]
fn test_markdown_diff_preview() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("markdown_diff_preview")
        .with_git()
        .zola(
            "Rendered markdown diff preview",
            "Preview changed markdown and Mermaid diagrams directly from the diff view.",
            46,
        )
        .setup(seed_markdown_diff_preview)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("d")
                    .wait_for_text("# Rendered Markdown Preview", 5000)
                    .press_key("j")
                    .press_key("p")
                    .wait_for_text("Preview — docs/日本.md", 5000)
                    .wait_for_text("Rendered Markdown Preview", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "markdown_preview",
                        "Rendered markdown and Mermaid diagram in the diff view",
                    )
                    .press_key("p")
                    .wait_for_text("# Rendered Markdown Preview", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("raw_diff", "Raw diff restored after toggling preview off")
            },
            |frame, report| {
                let preview_frame = common::frame_from_capture(&report.captures[0]);
                let preview_full = Region::full(preview_frame.cols(), preview_frame.rows());
                assertion::assert_text_in_region(
                    &preview_frame,
                    "Preview — docs/日本.md",
                    &preview_full,
                );
                assertion::assert_text_in_region(
                    &preview_frame,
                    "Rendered Markdown Preview",
                    &preview_full,
                );
                assertion::assert_text_in_region(&preview_frame, "Source", &preview_full);
                assertion::assert_text_in_region(&preview_frame, "Preview", &preview_full);
                let preview_text = preview_frame.text_in_region(&preview_full);
                assert!(preview_text.contains('┌'));
                assert!(preview_text.contains('▼'));
                assertion::assert_not_visible(&preview_frame, "graph TD");

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "# Rendered Markdown Preview", &full);
                assertion::assert_text_in_region(frame, "p: preview", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `d` while the chat transcript is focused in the reply
/// composer opens the diff preview, and that leaving it restores the composer
/// with the typed draft intact.
#[test]
fn diff_preview_opens_from_prompt_chat_focus() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("diff_preview_from_prompt")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("follow up draft")
                    .wait_for_text("follow up draft", 3000)
                    .press_key("Tab")
                    .wait_for_text("d: diff", 5000)
                    .capture_labeled(
                        "prompt_chat_focused",
                        "Chat transcript focused in the reply composer",
                    )
                    .press_key("d")
                    .wait_for_text("j/k: select file", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "diff_from_prompt",
                        "Diff preview opened from the reply composer",
                    )
                    .press_key("Esc")
                    .wait_for_text("follow up draft", 5000)
                    .viewing_pause_ms(1000)
            },
            |frame, report| {
                let focused_frame = common::frame_from_capture(&report.captures[0]);
                let focused_full = Region::full(focused_frame.cols(), focused_frame.rows());
                assertion::assert_text_in_region(&focused_frame, "d: diff", &focused_full);

                let diff_frame = common::frame_from_capture(&report.captures[1]);
                let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                assert_diff_file_tree_change_totals(&diff_frame);
                assertion::assert_text_in_region(&diff_frame, "println!(\"review\")", &diff_full);
                assertion::assert_text_in_region(&diff_frame, "j/k: select file", &diff_full);

                // Leaving the diff restores the composer with the draft intact.
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "follow up draft", &full);
            },
        )?;

    Ok(())
}

/// Verifies the compact Files panel keeps both per-row totals and the expected
/// left-aligned hierarchy.
fn assert_diff_file_tree_change_totals(frame: &TerminalFrame) {
    let file_tree = Region::new(0, 0, frame.cols() / 5, frame.rows());

    assertion::assert_text_in_region(frame, "src/", &file_tree);
    assertion::assert_text_in_region(frame, "mai", &file_tree);
    let root_matches = frame.find_text_in_region("src/", &file_tree);
    let file_matches = frame.find_text_in_region("mai", &file_tree);
    let root_match = &root_matches[0];
    let file_match = &file_matches[0];
    assert_eq!(root_match.rect.col, 2);
    assert_eq!(file_match.rect.col, 4);

    let text = frame.text_in_region(&file_tree);
    assert_eq!(
        text.matches("+1/-0").count(),
        2,
        "expected change totals on both file-tree rows:\n{text}"
    );
}

/// Verify binary-only changes retain the chat-focus diff hint and open in the
/// diff preview even though their added/deleted line totals are zero.
#[test]
fn binary_diff_preview_opens_from_prompt_chat_focus() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("binary_diff_preview_from_prompt")
        .with_git()
        .setup(seed_binary_diff_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .press_key("Tab")
                    .wait_for_text("d: diff", 5000)
                    .capture_labeled(
                        "binary_diff_chat_focus",
                        "Binary-only diff remains available from chat focus",
                    )
                    .press_key("d")
                    .wait_for_text("asset.bin", 5000)
                    .wait_for_text("Binary files", 5000)
            },
            |frame, report| {
                let focused_frame = common::frame_from_capture(&report.captures[0]);
                let focused_full = Region::full(focused_frame.cols(), focused_frame.rows());
                assertion::assert_text_in_region(&focused_frame, "d: diff", &focused_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "asset.bin", &full);
                assertion::assert_text_in_region(frame, "Binary files", &full);
            },
        )?;

    Ok(())
}
