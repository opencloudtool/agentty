//! Session lifecycle and prompt E2E tests.
//!
//! Tests cover session creation via `a` key, opening sessions with `Enter`,
//! list navigation with `j`/`k`, deletion with confirmation, prompt input
//! basics (typing, multiline via Alt+Enter, cancel via Esc), and returning
//! to the session list from session view.

use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::BuilderEnv;

/// Verify that the Sessions tab shows an empty-state message when no
/// sessions exist.
///
/// Starts agentty in a fresh temp directory (no database, no sessions),
/// switches to the Sessions tab, and asserts that the placeholder text
/// is visible.
#[test]
fn session_list_empty_state() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("session_empty")
        .compose(&common::wait_for_agentty_startup())
        // Navigate to Sessions tab.
        .compose(&common::switch_to_tab("Sessions"))
        .capture_labeled("sessions_tab", "Sessions tab with no sessions");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(&scenario, &report, &env, "session_list_empty_state");
}

/// Verify that pressing `a` on the Sessions tab creates a session and
/// opens prompt mode with the submit footer.
#[test]
fn session_creation_opens_prompt_mode() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("session_creation")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .capture_labeled("prompt_mode", "Prompt mode after pressing a");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — prompt-mode footer is visible with submit and cancel hints.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "Enter: submit", &full);
    assertion::assert_text_in_region(&frame, "Esc: cancel", &full);

    common::save_feature_gif(
        &scenario,
        &report,
        &env,
        "session_creation_opens_prompt_mode",
    );
}

/// Verify that pressing `Esc` in an empty prompt for a new non-draft
/// session deletes it and returns to the empty Sessions list.
#[test]
fn session_prompt_cancel_returns_to_empty_list() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("prompt_cancel")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .press_key("Esc")
        .wait_for_stable_frame(300, 5000)
        .capture_labeled("back_to_list", "Sessions list after cancel");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(
        &scenario,
        &report,
        &env,
        "session_prompt_cancel_returns_to_empty_list",
    );
}

/// Verify that pressing `Enter` on a session opens the session view and
/// pressing `q` returns to the session list.
#[test]
fn session_open_and_return_to_list() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("open_and_return")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::create_session_and_return_to_list())
        // Open the session with Enter.
        .press_key("Enter")
        // Use fixed sleep because the agent may produce continuous output.
        .sleep(std::time::Duration::from_secs(2))
        .capture_labeled("session_view", "Session view after Enter")
        // Return to list with q.
        .press_key("q")
        .sleep(std::time::Duration::from_secs(1))
        .capture_labeled("back_to_list", "Sessions list after q");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — intermediate capture shows session view with back action.
    let session_view_frame = common::frame_from_capture(&report.captures[0]);
    let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
    assertion::assert_text_in_region(&session_view_frame, "q: back", &view_full);

    // Assert — final frame is back on the list with the session visible.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "test", &full);

    common::save_feature_gif(&scenario, &report, &env, "session_open_and_return_to_list");
}

/// Verify that `j` and `k` navigate the session list when multiple
/// sessions exist.
#[test]
fn session_list_jk_navigation() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("jk_navigation")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::create_session_with_prompt_and_return_to_list(
            "alpha",
        ))
        .compose(&common::create_session_with_prompt_and_return_to_list(
            "beta",
        ))
        .capture_labeled("two_sessions", "Two sessions in list")
        // Navigate down with j, open the selection, and capture the session.
        .press_key("j")
        .wait_for_stable_frame(300, 3000)
        .press_key("Enter")
        .sleep(std::time::Duration::from_secs(2))
        .capture_labeled("opened_after_j", "Session opened after pressing j")
        .press_key("q")
        .sleep(std::time::Duration::from_secs(1))
        // Navigate back up with k, open the selection, and capture it.
        .press_key("k")
        .wait_for_stable_frame(300, 3000)
        .press_key("Enter")
        .sleep(std::time::Duration::from_secs(2))
        .capture_labeled("opened_after_k", "Session opened after pressing k");

    // Act
    let (_frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — both sessions are visible in the list capture.
    let initial_frame = common::frame_from_capture(&report.captures[0]);
    let opened_after_j_frame = common::frame_from_capture(&report.captures[1]);
    let opened_after_k_frame = common::frame_from_capture(&report.captures[2]);

    let initial_matches = initial_frame.find_text("alpha");
    let initial_beta_matches = initial_frame.find_text("beta");
    let opened_after_j_text = opened_after_j_frame.text_in_region(&Region::full(
        opened_after_j_frame.cols(),
        opened_after_j_frame.rows(),
    ));
    let opened_after_k_text = opened_after_k_frame.text_in_region(&Region::full(
        opened_after_k_frame.cols(),
        opened_after_k_frame.rows(),
    ));

    assert!(
        !initial_matches.is_empty(),
        "Expected at least 1 'alpha' match in initial frame, found {}",
        initial_matches.len()
    );
    assert!(
        !initial_beta_matches.is_empty(),
        "Expected at least 1 'beta' match in initial frame, found {}",
        initial_beta_matches.len()
    );

    // Assert — `j` and `k` navigate to different sessions when opening them.
    assert_eq!(
        opened_after_j_text.contains("alpha") || opened_after_j_text.contains("beta"),
        true,
        "Expected the session opened after j to contain either alpha or beta"
    );
    assert_eq!(
        opened_after_k_text.contains("alpha") || opened_after_k_text.contains("beta"),
        true,
        "Expected the session opened after k to contain either alpha or beta"
    );
    assert_ne!(
        opened_after_j_text.contains("alpha"),
        opened_after_k_text.contains("alpha"),
        "Expected j and k to open different sessions"
    );

    common::save_feature_gif(&scenario, &report, &env, "session_list_jk_navigation");
}

/// Verify that pressing `d` on a selected session opens a delete
/// confirmation dialog and pressing `y` deletes the session.
#[test]
fn session_delete_with_confirmation() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("delete_confirmation")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::create_session_and_return_to_list())
        // Press d to open delete confirmation.
        .press_key("d")
        .wait_for_stable_frame(300, 3000)
        .capture_labeled("confirm_dialog", "Delete confirmation dialog")
        // Press y to confirm deletion.
        .press_key("y")
        .wait_for_stable_frame(500, 10000)
        .capture_labeled("after_delete", "Sessions list after deletion");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — confirmation dialog is visible in intermediate capture.
    let dialog_frame = common::frame_from_capture(&report.captures[0]);
    let dialog_full = Region::full(dialog_frame.cols(), dialog_frame.rows());
    assertion::assert_text_in_region(&dialog_frame, "Confirm Delete", &dialog_full);

    // Assert — session is deleted, empty state visible.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(&scenario, &report, &env, "session_delete_with_confirmation");
}

/// Verify that typed text appears in the prompt input.
#[test]
fn prompt_typing_shows_text() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("prompt_typing")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .write_text("hello world")
        .wait_for_text("hello world", 3000)
        .capture_labeled("typed_text", "Prompt input with typed text");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — typed text is visible in the prompt.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "hello world", &full);

    common::save_feature_gif(&scenario, &report, &env, "prompt_typing_shows_text");
}

/// Verify that Alt+Enter inserts a newline in the prompt input,
/// producing multiline content.
///
/// Alt+Enter is sent as ESC (0x1b) followed by CR (0x0d) which crossterm
/// interprets as `KeyCode::Enter` with `KeyModifiers::ALT`.
#[test]
fn prompt_multiline_via_alt_enter() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");
    env.init_git().expect("failed to init git");

    let scenario = Scenario::new("prompt_multiline")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .write_text("first line")
        .wait_for_text("first line", 3000)
        // Alt+Enter: ESC (0x1b) followed by CR (0x0d).
        .write_text("\x1b\r")
        .wait_for_stable_frame(300, 3000)
        .write_text("second line")
        .wait_for_text("second line", 3000)
        .capture_labeled("multiline", "Prompt input with multiline text");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert — both lines are visible in the prompt.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "first line", &full);
    assertion::assert_text_in_region(&frame, "second line", &full);

    common::save_feature_gif(&scenario, &report, &env, "prompt_multiline_via_alt_enter");
}
