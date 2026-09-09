//! Prompt editing, focus, paste, and file lookup.

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{E2eResult, seed_sessions_startup_tab};
use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

/// Draft text typed into the composer by the chat-focus toggle test.
const PROMPT_FOCUS_DRAFT_TEXT: &str = "Draft kept while reading chat";

/// Seeds one draft-session lookup target file into the temporary project.
fn seed_draft_at_lookup_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(
        env.workdir.join("draft_lookup_target.txt"),
        "draft lookup target\n",
    )?;

    Ok(())
}

/// Seeds two unmaterialized stacked drafts whose nearest materialized ancestor
/// contains a new file that is absent from the project checkout.
fn seed_nested_stacked_at_lookup_session(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let ancestor_session_id = "atparent-0001";
    let parent_session_id = "atmiddle-0001";
    let child_session_id = "atchildx-0001";
    common::seed_session(
        env,
        SessionSeed::regular(ancestor_session_id, "gpt-5.6-sol", "main", "Review")
            .with_title("Ancestor with lookup file"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            parent_session_id,
            "gpt-5.6-sol",
            "wt/atparent",
            "Draft",
            ancestor_session_id,
        )
        .with_title("Unmaterialized middle draft"),
    )?;
    common::seed_session(
        env,
        SessionSeed::stacked_draft(
            child_session_id,
            "gpt-5.6-sol",
            "wt/atmiddle",
            "Draft",
            parent_session_id,
        )
        .with_title("Nested lookup child"),
    )?;

    let parent_worktree = env.agentty_root.join("wt").join("atparent");
    std::fs::create_dir_all(&parent_worktree)?;
    std::fs::write(
        parent_worktree.join("ancestor_lookup_target.txt"),
        "ancestor lookup target\n",
    )?;

    Ok(())
}

/// Verify that `Tab` moves focus from the prompt composer to the chat
/// transcript, and that `q` returns to the sessions list while preserving the
/// typed draft for reopening.
#[test]
fn session_prompt_chat_focus_toggle() -> E2eResult {
    // Arrange
    FeatureTest::new("session_prompt_chat_focus")
        .with_git()
        .setup(seed_sessions_startup_tab)
        .zola(
            "Read the chat while composing",
            "Press Tab to read the chat, then return to Sessions and reopen the composer without \
             losing the typed draft.",
            50,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("new session", 5000)
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text(PROMPT_FOCUS_DRAFT_TEXT)
                    .wait_for_text(PROMPT_FOCUS_DRAFT_TEXT, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("composer_focused", "The composer accepts the typed draft")
                    .press_key("Tab")
                    .wait_for_text("j/k: scroll", 5000)
                    .press_key("j")
                    .press_key("k")
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "chat_focused",
                        "Tab focuses the chat transcript for scrolling",
                    )
                    .press_key("q")
                    .wait_for_text("new session", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "sessions_list",
                        "Q returns from the focused chat to the sessions list",
                    )
                    .press_key("Enter")
                    .wait_for_text("Enter: send", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "restored_composer",
                        "Reopening the session restores the typed draft",
                    )
            },
            |frame, report| {
                // Assert
                let chat_focused_frame = common::frame_from_capture(&report.captures[1]);
                let chat_focused_full =
                    Region::full(chat_focused_frame.cols(), chat_focused_frame.rows());
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "Tab: focus",
                    &chat_focused_full,
                );
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "j/k: scroll",
                    &chat_focused_full,
                );
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "q: sessions",
                    &chat_focused_full,
                );
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    "d: diff",
                    &chat_focused_full,
                );
                // Chat focus exposes no cancel shortcut, so the composer draft
                // cannot be lost while scrolling.
                assertion::assert_not_visible(&chat_focused_frame, "Ctrl+C");
                // Scroll keys pressed in chat focus must not reach the draft.
                assertion::assert_text_in_region(
                    &chat_focused_frame,
                    PROMPT_FOCUS_DRAFT_TEXT,
                    &chat_focused_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: send", &full);
                assertion::assert_text_in_region(frame, PROMPT_FOCUS_DRAFT_TEXT, &full);
                assertion::assert_not_visible(frame, "j/k: scroll");
                assertion::assert_not_visible(frame, "q: sessions");

                // This session's worktree name comes from a fresh UUID, so the
                // footer reads differently on every run. Pin the harness
                // redaction against the footer agentty actually paints: without
                // a match, the frame hash moves every run and the committed GIF
                // is re-recorded for a UI that never changed.
                let redacted = common::session_worktree_redaction().apply(&frame.all_text());
                let placeholder = format!(
                    "{}{}",
                    common::SESSION_WORKTREE_PREFIX,
                    common::SESSION_WORKTREE_PLACEHOLDER,
                );

                assert!(
                    redacted.contains(&placeholder),
                    "the session worktree hash must be redacted out of the footer, \
                     got:\n{redacted}",
                );

                // The footer must paint the worktree path home-collapsed. An
                // absolute temp path is truncated differently per platform
                // (macOS temp roots are far longer than Linux's `/tmp`), which
                // would make the committed freshness hash unreproducible on CI.
                assert!(
                    redacted.contains("~/.agentty/wt/"),
                    "the footer must paint the worktree path home-collapsed so frames hash \
                     identically on every platform, got:\n{redacted}",
                );
            },
        )?;

    Ok(())
}

/// Verify that prompt image paste reports unavailable clipboard backends
/// inline.
#[test]
fn prompt_image_paste_unavailable_shows_inline_error() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_image_paste_unavailable")
        .with_git()
        .env("AGENTTY_DISABLE_CLIPBOARD", "1")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("ctrl+v")
                    .wait_for_text("Paste Image Error", 5000)
                    .capture_labeled(
                        "paste_error",
                        "Prompt mode showing unavailable clipboard paste error",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Paste Image Error", &full);
                assertion::assert_text_in_region(frame, "Clipboard is unavailable", &full);
                assertion::assert_not_visible(frame, "[Image #1]");
            },
        )?;

    Ok(())
}

/// Verify that pasted-image shortcuts work directly from a draft session view
/// by opening the draft composer and routing through prompt image paste.
#[test]
fn draft_session_view_paste_image_opens_composer() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_view_paste_image")
        .with_git()
        .env("AGENTTY_DISABLE_CLIPBOARD", "1")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 5000)
                    .write_text("Draft with image")
                    .press_key("Enter")
                    .wait_for_text("Enter: add draft", 5000)
                    .capture_labeled(
                        "draft_view",
                        "Draft-session view showing direct image paste action",
                    )
                    .press_key("ctrl+v")
                    .wait_for_text("Paste Image Error", 5000)
                    .capture_labeled(
                        "draft_paste_error",
                        "Draft composer opened from view-mode image paste shortcut",
                    )
            },
            |frame, report| {
                let draft_view_frame = common::frame_from_capture(&report.captures[0]);
                let draft_view_full =
                    Region::full(draft_view_frame.cols(), draft_view_frame.rows());
                assertion::assert_text_in_region(
                    &draft_view_frame,
                    "Ctrl+V/Alt+V: paste image",
                    &draft_view_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Paste Image Error", &full);
                assertion::assert_text_in_region(frame, "Clipboard is unavailable", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
                assertion::assert_not_visible(frame, "[Image #1]");
            },
        )?;

    Ok(())
}

/// Verify that pressing `Esc` in an empty prompt for a new non-draft
/// session deletes it and returns to the empty Sessions list.
#[test]
fn session_prompt_cancel_returns_to_empty_list() -> E2eResult {
    // Arrange
    FeatureTest::new("prompt_cancel")
        .with_git()
        .zola(
            "Prompt cancel",
            "Cancel prompt input with Esc to return to the session view.",
            120,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("prompt_open", "Prompt mode opened")
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_list", "Sessions list after cancel")
            },
            |frame, report| {
                // Assert
                let prompt_frame = common::frame_from_capture(&report.captures[0]);
                let prompt_full = Region::full(prompt_frame.cols(), prompt_frame.rows());
                assertion::assert_text_in_region(
                    &prompt_frame,
                    "Tab: focus | Enter: send",
                    &prompt_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that draft-session prompt mode can open `@` file lookup suggestions
/// before the deferred worktree exists.
#[test]
fn draft_session_at_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_at_lookup")
        .with_git()
        .setup(seed_draft_at_lookup_project)
        .zola(
            "Draft session @ lookup",
            "Browse project files with `@` before a draft session materializes its worktree.",
            121,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Down")
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 3000)
                    .viewing_pause_ms(1000)
                    .write_text("@draft_lookup")
                    .wait_for_text("draft_lookup_target.txt", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_at_lookup",
                        "Draft-session prompt mode with an active @ lookup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "draft_lookup_target.txt", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
            },
        )?;

    Ok(())
}

/// Verify that a nested unmaterialized stacked draft resolves `@` suggestions
/// from its nearest materialized ancestor worktree.
#[test]
fn stacked_session_at_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("stacked_session_at_lookup")
        .with_git()
        .setup(seed_nested_stacked_at_lookup_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Nested lookup child", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: add draft", 3000)
                    .press_key("Enter")
                    .wait_for_text("Enter: stage draft", 3000)
                    .write_text("@ancestor_lookup")
                    .wait_for_text("ancestor_lookup_target.txt", 5000)
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "ancestor_lookup_target.txt", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
            },
        )?;

    Ok(())
}

/// Verify that typed text appears in the prompt input.
#[test]
fn prompt_typing_shows_text() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_typing")
        .with_git()
        .zola(
            "Prompt typing",
            "Type text into the prompt input and see it appear in real time.",
            115,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("empty_prompt", "Empty prompt input")
                    .write_text("hello world")
                    .wait_for_text("hello world", 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("typed_text", "Prompt input with typed text")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "hello world", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `Backspace` after deleting all prompt text leaves the
/// empty prompt open.
#[test]
fn prompt_backspace_on_empty_input() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_empty_backspace").with_git().run(
        |scenario| {
            scenario
                .compose(&common::wait_for_agentty_startup())
                .compose(&common::switch_to_tab("Sessions"))
                .press_key("a")
                .press_key("Enter")
                .wait_for_stable_frame(300, 5000)
                .write_text("bug")
                .wait_for_text("bug", 3000)
                .press_key("Backspace")
                .press_key("Backspace")
                .press_key("Backspace")
                .press_key("Backspace")
                .wait_for_text("Type your message", 3000)
                .capture_labeled("empty_prompt", "Empty prompt after one extra Backspace")
        },
        |frame, _report| {
            let full = Region::full(frame.cols(), frame.rows());
            assertion::assert_text_in_region(frame, "Type your message", &full);
            assertion::assert_text_in_region(frame, "Enter: send", &full);
        },
    )?;

    Ok(())
}

/// Verify that Alt+Enter inserts a newline in the prompt input,
/// producing multiline content.
///
/// Alt+Enter is sent as ESC (0x1b) followed by CR (0x0d) which crossterm
/// interprets as `KeyCode::Enter` with `KeyModifiers::ALT`.
#[test]
fn prompt_multiline_via_alt_enter() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_multiline")
        .with_git()
        .zola(
            "Multiline prompt",
            "Insert newlines with Alt+Enter to compose multiline prompts.",
            125,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .write_text("first line")
                    .wait_for_text("first line", 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("first_line", "First line typed")
                    // Alt+Enter: ESC (0x1b) followed by CR (0x0d).
                    .write_text("\x1b\r")
                    .wait_for_stable_frame(300, 3000)
                    .write_text("second line")
                    .wait_for_text("second line", 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("multiline", "Multiline prompt with both lines")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "first line", &full);
                assertion::assert_text_in_region(frame, "second line", &full);
            },
        )?;

    Ok(())
}

/// Verify bracketed paste keeps leading indentation after prompt submission.
#[test]
fn prompt_paste_indentation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_paste_indentation")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .write_text("\x1b[200~    indented prompt\x1b[201~")
                    .wait_for_text("indented prompt", 3000)
                    .press_key("Enter")
                    .wait_for_text("q: back", 10000)
                    .capture_labeled(
                        "submitted_indentation",
                        "Submitted pasted prompt retaining leading indentation",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let text = frame.text_in_region(&full);

                assert!(text.contains(" ›     indented prompt"));
            },
        )?;

    Ok(())
}

/// Verify that CSI-u `Shift+Enter` inserts a newline in the prompt input.
#[test]
fn prompt_multiline_via_csi_u_shift_enter() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock();
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    let scenario = Scenario::new("prompt_multiline_shift_enter")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .press_key("Enter")
        .wait_for_stable_frame(300, 5000)
        .write_text("first line")
        .wait_for_text("first line", 3000)
        .write_text("\x1b[13;2u")
        .wait_for_stable_frame(300, 3000)
        .write_text("second line")
        .wait_for_text("second line", 3000);

    // Act
    let frame = scenario.run(env.builder())?;

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "first line", &full);
    assertion::assert_text_in_region(&frame, "second line", &full);
    assertion::assert_not_visible(&frame, "first linesecond line");

    Ok(())
}
