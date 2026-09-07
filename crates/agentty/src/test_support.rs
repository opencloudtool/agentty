//! Hidden support APIs for tests.
//!
//! These helpers intentionally live outside the production-facing module
//! surface so tests can share canonical naming and render-buffer rules without
//! widening app APIs.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::{Instant, SystemTime};

#[cfg(test)]
use ag_agent::{AppServerClient, MockAppServerClient, StaticAgentAvailabilityProbe};
#[cfg(test)]
use ag_git as git;
use ratatui::buffer::{Buffer, Cell};
#[cfg(test)]
use tracing::field::{Field, Visit};
#[cfg(test)]
use tracing::subscriber::{Interest, Subscriber};
#[cfg(test)]
use tracing::{Event, Level, Metadata, span};

use crate::app;
#[cfg(test)]
use crate::app::{App, SessionManager, SessionState};
use crate::db::{Database, DbError};
use crate::domain::agent::ReasoningLevel;
#[cfg(test)]
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
#[cfg(test)]
use crate::domain::question::QuestionItem;
#[cfg(test)]
use crate::domain::selection::SelectionState;
#[cfg(test)]
use crate::domain::session::{
    ReviewRequest, Session, SessionHandles, SessionId, SessionRole, SessionSize, SessionStats,
    Status,
};
#[cfg(test)]
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::setting::SettingName;
#[cfg(test)]
use crate::domain::transient_message::TransientMessageStore;
#[cfg(test)]
use crate::infra::project_discovery::MockProjectDiscoveryClient;
#[cfg(test)]
/// Subscriber that enables tracing fields while unit tests exercise warning
/// paths under source coverage.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestSubscriber;

#[cfg(test)]
struct TestVisitor;

#[cfg(test)]
impl Visit for TestVisitor {
    fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
        let _rendered = format!("{value:?}");
    }
}

#[cfg(test)]
impl Subscriber for TestSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &span::Attributes<'_>) -> span::Id {
        span.record(&mut TestVisitor);

        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, values: &span::Record<'_>) {
        values.record(&mut TestVisitor);
    }

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        event.record(&mut TestVisitor);
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}

    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(Level::TRACE.into())
    }
}

/// Returns the canonical session folder path for integration-test fixtures.
pub fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    app::session::session_folder(base, session_id)
}

/// Persists the active project id for integration-test database setup.
pub async fn persist_active_project_id_for_test(
    database: &Database,
    project_id: i64,
) -> Result<(), DbError> {
    sqlx::query!(
        r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        SettingName::ActiveProjectId.as_str(),
        project_id.to_string()
    )
    .execute(database.pool())
    .await?;

    Ok(())
}

/// Persists the active list tab for integration-test database setup.
pub async fn persist_active_tab_for_test(
    database: &Database,
    tab: app::Tab,
) -> Result<(), DbError> {
    sqlx::query!(
        r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        SettingName::ActiveTab.as_str(),
        tab.as_str()
    )
    .execute(database.pool())
    .await?;

    Ok(())
}

/// Persists the three project role reasoning defaults for integration-test
/// database setup using canonical `SettingName` keys.
pub async fn persist_project_reasoning_levels_for_test(
    database: &Database,
    project_id: i64,
    smart_reasoning_level: ReasoningLevel,
    fast_reasoning_level: ReasoningLevel,
    review_reasoning_level: ReasoningLevel,
) -> Result<(), DbError> {
    database
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (
                    SettingName::DefaultSmartReasoningLevel,
                    smart_reasoning_level.as_str().to_string(),
                ),
                (
                    SettingName::DefaultFastReasoningLevel,
                    fast_reasoning_level.as_str().to_string(),
                ),
                (
                    SettingName::DefaultReviewReasoningLevel,
                    review_reasoning_level.as_str().to_string(),
                ),
            ],
        )
        .await
}

/// Deterministic [`crate::infra::clock::Clock`] implementation for unit-test
/// fixtures.
#[cfg(test)]
pub(crate) struct FixedClock {
    instant: Instant,
    system_time: SystemTime,
}

#[cfg(test)]
impl FixedClock {
    /// Creates a fixed clock pinned to the given monotonic and system times.
    pub(crate) fn new(instant: Instant, system_time: SystemTime) -> Self {
        Self {
            instant,
            system_time,
        }
    }

    /// Creates a fixed clock whose wall time is Unix epoch and whose instant
    /// starts at construction time.
    pub(crate) fn unix_epoch() -> Self {
        Self::new(Instant::now(), SystemTime::UNIX_EPOCH)
    }
}

#[cfg(test)]
impl crate::infra::clock::Clock for FixedClock {
    fn now_instant(&self) -> Instant {
        self.instant
    }

    fn now_system_time(&self) -> SystemTime {
        self.system_time
    }
}

/// Chainable builder that produces deterministic [`Session`] values for unit
/// tests.
#[cfg(test)]
pub(crate) struct SessionFixtureBuilder {
    session: Session,
}

/// Builds a typed transcript containing one assistant answer.
#[cfg(test)]
pub(crate) fn assistant_transcript(content: impl AsRef<str>) -> SessionTranscript {
    SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        content.as_ref(),
    )])
}

#[cfg(test)]
impl SessionFixtureBuilder {
    /// Creates a builder seeded with minimal deterministic defaults that match
    /// the common session snapshot used across app, runtime, and UI tests.
    pub(crate) fn new() -> Self {
        Self {
            session: Session {
                agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
                base_branch: "main".to_string(),
                created_at: 0,
                draft_attachments: Vec::new(),
                folder: PathBuf::new(),
                follow_up_tasks: Vec::new(),
                id: SessionId::from("session-id"),
                in_progress_started_at: None,
                in_progress_total_seconds: 0,
                is_draft: false,
                controller_session_id: None,
                orchestration_progress: None,
                role: SessionRole::default(),
                parent_session_id: None,
                permission_mode: crate::domain::permission::PermissionMode::default(),
                personality_id: None,
                project_name: "project".to_string(),
                prompt: String::new(),
                queued_messages: Vec::new(),
                reasoning_level_override: None,
                response_style: crate::domain::agent::ResponseStyle::default(),
                published_upstream_ref: None,
                questions: Vec::new(),
                review_request: None,
                size: SessionSize::Xs,
                speed_mode: crate::domain::agent::SpeedMode::default(),
                stats: SessionStats::default(),
                status: Status::Review,
                title: None,
                transcript: None,
                updated_at: 0,
                transient_messages: TransientMessageStore::default(),
            },
        }
    }

    /// Overrides the selected agent.
    pub(crate) fn agent(mut self, agent: AgentSelection) -> Self {
        self.session.agent = agent;

        self
    }

    /// Overrides the draft flag.
    pub(crate) fn draft(mut self, is_draft: bool) -> Self {
        self.session.is_draft = is_draft;

        self
    }

    /// Overrides the worktree folder.
    pub(crate) fn folder(mut self, folder: PathBuf) -> Self {
        self.session.folder = folder;

        self
    }

    /// Overrides the stable session identifier.
    pub(crate) fn id(mut self, id: impl Into<SessionId>) -> Self {
        self.session.id = id.into();

        self
    }

    /// Overrides the agent model while preserving the current agent kind.
    pub(crate) fn model(mut self, model: AgentModel) -> Self {
        self.session.agent = AgentSelection::new(self.session.agent.kind(), model);

        self
    }

    /// Overrides the captured transcript using already formatted text.
    pub(crate) fn transcript(mut self, transcript: impl Into<String>) -> Self {
        self.session.transcript = Some(assistant_transcript(transcript.into()));

        self
    }

    /// Overrides the optional stacked-session parent identifier.
    pub(crate) fn parent_session_id(mut self, parent_session_id: Option<SessionId>) -> Self {
        self.session.parent_session_id = parent_session_id;

        self
    }

    /// Overrides the project name.
    pub(crate) fn project_name(mut self, project_name: impl Into<String>) -> Self {
        self.session.project_name = project_name.into();

        self
    }

    /// Overrides the user prompt text.
    pub(crate) fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.session.prompt = prompt.into();

        self
    }

    /// Overrides the pending clarification questions.
    pub(crate) fn questions(mut self, questions: Vec<QuestionItem>) -> Self {
        self.session.questions = questions;

        self
    }

    /// Overrides the session-scoped reasoning level override.
    pub(crate) fn reasoning_level_override(
        mut self,
        reasoning_level_override: Option<ReasoningLevel>,
    ) -> Self {
        self.session.reasoning_level_override = reasoning_level_override;

        self
    }

    /// Overrides the persisted forge review request.
    pub(crate) fn review_request(mut self, review_request: Option<ReviewRequest>) -> Self {
        self.session.review_request = review_request;

        self
    }

    /// Overrides the session's orchestration role.
    pub(crate) fn role(mut self, role: SessionRole) -> Self {
        self.session.role = role;

        self
    }

    /// Overrides the lifecycle status.
    pub(crate) fn status(mut self, status: Status) -> Self {
        self.session.status = status;

        self
    }

    /// Overrides the optional explicit session title.
    pub(crate) fn title(mut self, title: Option<String>) -> Self {
        self.session.title = title;

        self
    }

    /// Consumes the builder and returns the fully populated fixture.
    pub(crate) fn build(self) -> Session {
        self.session
    }
}

/// Builds a minimal session fixture with the given identifier and status.
#[cfg(test)]
pub(crate) fn session_fixture(session_id: &str, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .status(status)
        .folder(PathBuf::from("/tmp/test"))
        .build()
}

/// Builds a session fixture whose title matches its identifier.
#[cfg(test)]
pub(crate) fn titled_session_fixture(session_id: &str, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .status(status)
        .title(Some(session_id.to_string()))
        .build()
}

/// Builds a review-state session fixture rooted at the given folder.
#[cfg(test)]
pub(crate) fn session_fixture_with_folder(session_folder: PathBuf) -> Session {
    SessionFixtureBuilder::new()
        .id("session-1")
        .folder(session_folder)
        .project_name("test-project")
        .prompt("test prompt")
        .build()
}

/// Returns a mock app-server client wrapped in `Arc` for test injection.
#[cfg(test)]
pub(crate) fn mock_app_server() -> Arc<dyn AppServerClient> {
    Arc::new(MockAppServerClient::new())
}

/// Builds one client bundle with a caller-provided agent availability
/// snapshot.
#[cfg(test)]
pub(crate) fn test_app_clients_with_available_agent_kinds(
    available_agent_kinds: Vec<AgentKind>,
) -> app::AppClients {
    let mut project_discovery_client = MockProjectDiscoveryClient::new();
    project_discovery_client
        .expect_discover_home_project_paths()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));

    app::AppClients::new()
        .with_agent_availability_probe(Arc::new(StaticAgentAvailabilityProbe {
            available_agent_kinds,
        }))
        .with_project_discovery_client(Arc::new(project_discovery_client))
}

/// Builds one client bundle with deterministic agent availability for test
/// app startup.
#[cfg(test)]
pub(crate) fn test_app_clients() -> app::AppClients {
    test_app_clients_with_available_agent_kinds(AgentKind::ALL.to_vec())
}

/// Builds one client bundle with deterministic agent availability and a mock
/// app-server override.
#[cfg(test)]
pub(crate) fn test_app_clients_with_mock_app_server() -> app::AppClients {
    test_app_clients().with_app_server_client_override(mock_app_server())
}

/// Builds one app rooted at a retained temporary directory using the given
/// clients.
#[cfg(test)]
pub(crate) async fn new_test_app_with_clients(
    clients: app::AppClients,
) -> (App, tempfile::TempDir) {
    let base_dir = tempfile::tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let app = App::new_with_clients(base_path.clone(), base_path, None, database, clients)
        .await
        .expect("failed to build app");

    (app, base_dir)
}

/// Builds one app rooted at a retained temporary directory.
#[cfg(test)]
pub(crate) async fn new_test_app() -> (App, tempfile::TempDir) {
    new_test_app_with_clients(test_app_clients()).await
}

/// Builds one app rooted at a retained temporary directory with a mocked tmux
/// boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_test_app_with_mock_tmux_client() -> (App, tempfile::TempDir) {
    new_test_app_with_tmux_client(Arc::new(crate::infra::tmux::MockTmuxClient::new())).await
}

/// Builds one app rooted at a retained temporary directory with an injected
/// tmux boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_test_app_with_tmux_client(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> (App, tempfile::TempDir) {
    let clients = test_app_clients_with_mock_app_server().with_tmux_client(tmux_client);

    new_test_app_with_clients(clients).await
}

/// Builds one app with an injected tmux boundary, then intentionally drops
/// the temporary directory guard before returning.
#[cfg(test)]
pub(crate) async fn new_test_app_with_tmux_client_without_retained_base_dir(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> App {
    let (app, _base_dir) = new_test_app_with_tmux_client(tmux_client).await;

    app
}

/// Builds one app and intentionally drops the temporary directory guard before
/// returning, matching tests that only need in-memory state.
#[cfg(test)]
pub(crate) async fn new_test_app_without_retained_base_dir() -> App {
    let (app, _base_dir) = new_test_app().await;

    app
}

/// Initializes a minimal git repository for retained-tempdir app fixtures.
///
/// Every git invocation is checked for success because a silently failing
/// setup command leaves a commit-less repository behind. Host git settings
/// such as `commit.gpgsign`, `core.hooksPath`, or `init.templateDir` can break
/// the initial commit, and an unchecked failure only resurfaces much later as
/// an opaque session-creation panic inside an unrelated test.
#[cfg(test)]
pub(crate) fn setup_test_git_repo(path: &Path) {
    run_fixture_git_command(path, &["init"]);
    run_fixture_git_command(path, &["config", "user.name", "Test"]);
    run_fixture_git_command(path, &["config", "user.email", "test@test.com"]);

    std::fs::write(path.join("README.md"), "test").expect("write failed");

    run_fixture_git_command(path, &["add", "."]);
    run_fixture_git_command(path, &["commit", "-m", "Initial commit"]);
    run_fixture_git_command(path, &["branch", "-M", "main"]);
}

/// Runs one git command inside a fixture repository and panics with the
/// captured stderr when the command cannot spawn or exits non-zero.
///
/// Both panic messages are formatted before they are needed so the success
/// path executes every line in this helper.
#[cfg(test)]
fn run_fixture_git_command(path: &Path, args: &[&str]) {
    let command_label = format!("git {}", args.join(" "));
    let spawn_failure = format!("failed to run `{command_label}`");
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect(&spawn_failure);
    let exit_failure = format!(
        "`{command_label}` failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(output.status.success(), "{exit_failure}");
}

/// Builds one git-backed app rooted at a retained temporary directory using
/// the given clients.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_clients(
    clients: app::AppClients,
) -> (App, tempfile::TempDir) {
    let (app, base_dir, _pool) = new_git_test_app_with_clients_and_pool(clients).await;

    (app, base_dir)
}

/// Builds one git-backed app and exposes its shared database pool for tests
/// that need to inject a persistence failure after app construction.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_pool() -> (App, tempfile::TempDir, sqlx::SqlitePool) {
    new_git_test_app_with_clients_and_pool(test_app_clients()).await
}

/// Builds one git-backed app plus its shared database pool using the given
/// clients.
#[cfg(test)]
async fn new_git_test_app_with_clients_and_pool(
    clients: app::AppClients,
) -> (App, tempfile::TempDir, sqlx::SqlitePool) {
    let base_dir = tempfile::tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    setup_test_git_repo(base_dir.path());
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let pool = database.pool().clone();
    let app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build app");

    (app, base_dir, pool)
}

/// Drives foreground reducers until all tracked workspace setup has completed.
#[cfg(test)]
pub(crate) async fn finish_session_creation_tasks(app: &mut App) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !app.pending_session_creations.is_empty() {
            let event = app
                .next_app_event()
                .await
                .expect("workspace completion event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("workspace setup should complete");
}

/// Builds one git-backed app rooted at a retained temporary directory.
#[cfg(test)]
pub(crate) async fn new_git_test_app() -> (App, tempfile::TempDir) {
    new_git_test_app_with_clients(test_app_clients()).await
}

/// Builds one git-backed app rooted at a retained temporary directory with a
/// mocked tmux boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_mock_tmux_client() -> (App, tempfile::TempDir) {
    new_git_test_app_with_tmux_client(Arc::new(crate::infra::tmux::MockTmuxClient::new())).await
}

/// Builds one git-backed app rooted at a retained temporary directory with an
/// injected tmux boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_tmux_client(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> (App, tempfile::TempDir) {
    let clients = test_app_clients_with_mock_app_server().with_tmux_client(tmux_client);

    new_git_test_app_with_clients(clients).await
}

/// Builds a session manager fixture with the provided sessions and handles.
#[cfg(test)]
pub(crate) fn session_manager_with_handles(
    sessions: Vec<Session>,
    handles: std::collections::HashMap<SessionId, SessionHandles>,
) -> SessionManager {
    SessionManager::new(
        app::session::SessionDefaults {
            model: AgentKind::Antigravity.default_model(),
        },
        Arc::new(git::MockGitClient::new()),
        SessionState::new(
            handles,
            sessions,
            SelectionState::default(),
            Arc::new(FixedClock::unix_epoch()),
            0,
            0,
        ),
        Vec::new(),
    )
}

/// Builds a session manager fixture with the provided sessions and no runtime
/// handles.
#[cfg(test)]
pub(crate) fn session_manager_with_sessions(sessions: Vec<Session>) -> SessionManager {
    session_manager_with_handles(sessions, std::collections::HashMap::new())
}

/// Sets a session status in both the session snapshot and its live handles,
/// when either exists.
#[cfg(test)]
pub(crate) fn set_session_status_for_test(app: &mut App, session_id: &str, status: Status) {
    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = status;
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut current_status) = handles.status.lock()
    {
        *current_status = status;
    }
}

/// Returns the first rendered cell for a contiguous text match in a test
/// buffer.
pub fn rendered_text_start_cell<'a>(buffer: &'a Buffer, needle: &str) -> Option<&'a Cell> {
    rendered_text_start_cells(buffer, needle).into_iter().next()
}

/// Returns rendered start cells for every contiguous text match in a test
/// buffer.
pub fn rendered_text_start_cells<'a>(buffer: &'a Buffer, needle: &str) -> Vec<&'a Cell> {
    let width = usize::from(buffer.area.width.max(1));
    let needle_symbols = needle.chars().map(|character| character.to_string());
    let needle_symbols = needle_symbols.collect::<Vec<_>>();
    let content = buffer.content();
    let mut cells = Vec::new();

    for row_start in (0..content.len()).step_by(width) {
        let row_end = row_start + width.min(content.len().saturating_sub(row_start));
        let row = &content[row_start..row_end];

        for (index, window) in row.windows(needle_symbols.len()).enumerate() {
            let window_matches = window
                .iter()
                .zip(&needle_symbols)
                .all(|(cell, symbol)| cell.symbol() == symbol);

            if window_matches {
                cells.push(&row[index]);
            }
        }
    }

    cells
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    #[test]
    fn test_subscriber_records_span_and_event_fields() {
        // Arrange
        let subscriber = TestSubscriber;

        // Act
        let span_registered = tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "test_subscriber_span",
                recorded_value = tracing::field::Empty
            );
            span.record("recorded_value", "recorded");
            if let Some(span_id) = span.id() {
                span.follows_from(span_id);
            }
            {
                let _guard = span.enter();
                tracing::warn!(path = %Path::new("/workspace").display(), "test warning");
            }

            span.id().is_some()
        });

        // Assert
        assert!(span_registered);
    }

    #[tokio::test]
    async fn persist_settings_for_test_upserts_values() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");

        // Act
        persist_active_project_id_for_test(&database, 41)
            .await
            .expect("failed to persist initial active project");
        persist_active_project_id_for_test(&database, 42)
            .await
            .expect("failed to update active project");
        persist_active_tab_for_test(&database, app::Tab::Projects)
            .await
            .expect("failed to persist initial active tab");
        persist_active_tab_for_test(&database, app::Tab::Sessions)
            .await
            .expect("failed to update active tab");
        let project_id = database
            .projects()
            .upsert_project("/tmp/reasoning-defaults", Some("main".to_string()))
            .await
            .expect("failed to create project");
        persist_project_reasoning_levels_for_test(
            &database,
            project_id,
            ReasoningLevel::Medium,
            ReasoningLevel::Low,
            ReasoningLevel::XHigh,
        )
        .await
        .expect("failed to persist role reasoning levels");

        // Assert
        assert_eq!(
            database
                .settings()
                .load_active_project_id()
                .await
                .expect("failed to load active project"),
            Some(42)
        );
        assert_eq!(
            database
                .settings()
                .get_setting(SettingName::ActiveTab)
                .await
                .expect("failed to load active tab")
                .as_deref(),
            Some("Sessions")
        );
        for (setting_name, expected_level) in [
            (
                SettingName::DefaultSmartReasoningLevel,
                ReasoningLevel::Medium,
            ),
            (SettingName::DefaultFastReasoningLevel, ReasoningLevel::Low),
            (
                SettingName::DefaultReviewReasoningLevel,
                ReasoningLevel::XHigh,
            ),
        ] {
            assert_eq!(
                database
                    .settings()
                    .load_project_reasoning_level(project_id, setting_name)
                    .await
                    .expect("failed to load role reasoning level"),
                expected_level
            );
        }
    }

    #[test]
    fn rendered_text_start_cell_returns_first_match() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
        buffer.set_string(1, 0, "one", Style::default().fg(Color::Green));
        buffer.set_string(1, 1, "one", Style::default().fg(Color::Yellow));

        // Act
        let cell = rendered_text_start_cell(&buffer, "one").expect("text should render");

        // Assert
        assert_eq!(cell.fg, Color::Green);
    }

    #[test]
    fn rendered_text_start_cells_returns_all_matches() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
        buffer.set_string(1, 0, "same", Style::default().fg(Color::Green));
        buffer.set_string(1, 1, "same", Style::default().fg(Color::Yellow));

        // Act
        let cells = rendered_text_start_cells(&buffer, "same");
        let colors = cells.iter().map(|cell| cell.fg).collect::<Vec<_>>();

        // Assert
        assert_eq!(colors, vec![Color::Green, Color::Yellow]);
    }

    #[test]
    fn rendered_text_start_cell_returns_none_for_missing_text() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 1));
        buffer.set_string(1, 0, "present", Style::default());

        // Act
        let cell = rendered_text_start_cell(&buffer, "missing");

        // Assert
        assert!(cell.is_none());
    }
}
