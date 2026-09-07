use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use thiserror::Error;

use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::{
    LifecycleEmitter, LifecycleId, LifecycleObserver, ToolErrorType, ToolLifecycle, TurnErrorType,
    TurnLifecycle,
};
use crate::model::{
    CompletionMetadata, Model, ModelError, ModelMessage, ModelRequest, ModelResponse,
    ensure_unique_tool_call_ids,
};
use crate::policy::Policy;
use crate::read::{self, ReadError, ReadTool};
use crate::repository::Repository;
use crate::schema_contract::OutputSchema;
use crate::session::{AcquiredTurn, Database, LoadedSession, NewSession, SessionError};
use crate::tool::{
    ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments, ToolDefinition, WriteArguments,
};
use crate::write::{WriteError, WriteTool};

const DEFAULT_MAX_HISTORY_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_TOOL_CALLS: usize = 8;

/// Successful model turn paired with observable execution activity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnOutcome {
    output: Value,
    report: TurnReport,
}

impl TurnOutcome {
    /// Returns the locally validated structured model output.
    pub fn output(&self) -> &Value {
        &self.output
    }

    /// Returns sanitized timing, model, and tool activity for the turn.
    pub fn report(&self) -> &TurnReport {
        &self.report
    }

    /// Consumes the outcome and returns its validated output.
    pub fn into_output(self) -> Value {
        self.output
    }
}

/// Observable, content-free activity from one successful model turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnReport {
    duration: Duration,
    model_requests: Vec<ModelRequestActivity>,
    tool_calls: Vec<ToolActivity>,
}

impl TurnReport {
    /// Returns the complete elapsed turn time.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns one entry for every provider request made during the turn.
    pub fn model_requests(&self) -> &[ModelRequestActivity] {
        &self.model_requests
    }

    /// Returns successful repository tool activity without file contents.
    pub fn tool_calls(&self) -> &[ToolActivity] {
        &self.tool_calls
    }
}

/// Observable facts about one provider request in a successful turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequestActivity {
    completion: Option<CompletionMetadata>,
    duration: Duration,
    response_type: crate::lifecycle::ModelResponseType,
}

impl ModelRequestActivity {
    /// Returns sanitized provider completion metadata, when available.
    pub fn completion(&self) -> Option<&CompletionMetadata> {
        self.completion.as_ref()
    }

    /// Returns the elapsed provider-request time.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns whether the request produced output, a tool call, or a rejected
    /// native continuation that the harness replayed.
    pub fn response_type(&self) -> crate::lifecycle::ModelResponseType {
        self.response_type
    }
}

/// Sanitized details about one built-in tool operation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolActivity {
    /// A bounded repository file read.
    Read {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Final included one-based line, when the file was nonempty.
        end_line: Option<u64>,
        /// Repository-relative path that was read.
        path: String,
        /// Requested one-based starting line.
        start_line: u64,
        /// Whether additional file content followed the result.
        truncated: bool,
    },
    /// A read-only repository inspection other than a worktree file read.
    ReadInspection {
        /// Selected inspection action.
        action: crate::tool::ReadAction,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Bounded path, query, or revision summary.
        summary: String,
    },
    /// A model-correctable repository inspection rejection returned to the
    /// model.
    ReadInspectionRejected {
        /// Selected inspection action.
        action: crate::tool::ReadAction,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Bounded path, query, or revision summary.
        summary: String,
    },
    /// A model-correctable repository read rejection returned to the model.
    ReadRejected {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was rejected.
        path: String,
    },
    /// A repository file write.
    Write {
        /// Number of bytes in the resulting file.
        bytes_written: usize,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was written.
        path: String,
    },
    /// A model-correctable repository write rejection returned to the model.
    WriteRejected {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was rejected.
        path: String,
    },
}

impl ToolActivity {
    /// Returns the elapsed tool-execution time.
    pub fn duration(&self) -> Duration {
        match self {
            Self::Read { duration, .. }
            | Self::ReadInspection { duration, .. }
            | Self::ReadInspectionRejected { duration, .. }
            | Self::ReadRejected { duration, .. }
            | Self::Write { duration, .. }
            | Self::WriteRejected { duration, .. } => *duration,
        }
    }

    /// Returns the bounded built-in tool name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Read { .. }
            | Self::ReadInspection { .. }
            | Self::ReadInspectionRejected { .. }
            | Self::ReadRejected { .. } => "read",
            Self::Write { .. } | Self::WriteRejected { .. } => "write",
        }
    }

    /// Returns the repository-relative target or bounded inspection summary.
    pub fn path(&self) -> &str {
        match self {
            Self::Read { path, .. }
            | Self::ReadRejected { path, .. }
            | Self::Write { path, .. }
            | Self::WriteRejected { path, .. } => path,
            Self::ReadInspection { summary, .. } | Self::ReadInspectionRejected { summary, .. } => {
                summary
            }
        }
    }
}

impl fmt::Display for ToolActivity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read {
                duration,
                end_line,
                path,
                start_line,
                truncated,
            } => {
                let path = sanitize_report_text(path);
                let lines = end_line.map_or_else(
                    || format!("line {start_line}"),
                    |end_line| format!("lines {start_line}-{end_line}"),
                );
                let continuation = if *truncated { ", truncated" } else { "" };

                write!(
                    formatter,
                    "read {path} ({lines}{continuation}; {})",
                    format_report_duration(*duration)
                )
            }
            Self::ReadInspection {
                action,
                duration,
                summary,
            } => write!(
                formatter,
                "read {} {} (completed; {})",
                action.as_str(),
                sanitize_report_text(summary),
                format_report_duration(*duration)
            ),
            Self::ReadInspectionRejected {
                action,
                duration,
                summary,
            } => write!(
                formatter,
                "read {} {} (rejected; {})",
                action.as_str(),
                sanitize_report_text(summary),
                format_report_duration(*duration)
            ),
            Self::ReadRejected { duration, path } => write!(
                formatter,
                "read {} (rejected; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
            Self::Write {
                bytes_written,
                duration,
                path,
            } => write!(
                formatter,
                "write {} ({bytes_written} bytes; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
            Self::WriteRejected { duration, path } => write!(
                formatter,
                "write {} (rejected; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
        }
    }
}

/// Durable, resumable sequence of model turns.
pub struct Session<'a> {
    database: Database,
    harness: &'a Harness,
    history: SessionHistory,
    id: String,
    provider_session_id: Option<String>,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl Session<'_> {
    /// Returns the stable application-provided session identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Sends one prompt and durably records its lifecycle and messages.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the model turn or persistence operation
    /// fails.
    pub async fn send(&mut self, prompt: impl Into<String>) -> Result<TurnOutcome, SessionError> {
        let prompt = prompt.into();
        let AcquiredTurn {
            mut guard,
            provider_session_id,
            turn_position,
            turns,
        } = self.database.begin_turn(&self.id, &prompt).await?;
        self.history.replace(turns);
        self.provider_session_id = provider_session_id;
        let mut messages = self.history.messages();
        if let Some(system_prompt) = &self.system_prompt {
            messages.insert(0, ModelMessage::System(system_prompt.clone()));
        }
        let retained_messages = messages.len();
        let mut request = ModelRequest::with_history(messages, prompt, self.schema.clone());
        request.set_provider_session_id(self.provider_session_id.clone());
        let result = tokio::select! {
            biased;
            error = guard.ownership_failure() => {
                guard.mark_interrupted();

                return Err(error);
            }
            result = self.harness.run_request(request) => result,
        };
        let (outcome, mut messages, provider_session_id) = match result {
            Ok(result) => result,
            Err(error) => {
                self.provider_session_id = None;
                let persistence = self
                    .database
                    .fail_turn(&self.id, turn_position, &error)
                    .await;
                if let Err(persistence) = persistence {
                    guard.mark_interrupted();

                    return Err(SessionError::TurnPersistence {
                        turn: error,
                        persistence: Box::new(persistence),
                    });
                }
                guard.disarm();

                return Err(error.into());
            }
        };
        let turn = messages.split_off(retained_messages);
        let persistence = self
            .database
            .complete_turn(
                &self.id,
                turn_position,
                &turn[1..],
                provider_session_id.as_deref(),
            )
            .await;
        if let Err(error) = persistence {
            guard.mark_interrupted();

            return Err(error);
        }
        guard.disarm();
        self.provider_session_id = provider_session_id;
        self.history.push(turn);

        Ok(outcome)
    }
}

/// Builder for one durable session.
pub struct SessionBuilder<'a> {
    harness: &'a Harness,
    id: String,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl<'a> SessionBuilder<'a> {
    /// Adds a system prompt that is restored with the session.
    #[must_use]
    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }

    /// Creates the session in the configured database.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when storage is not configured, the identifier
    /// already exists, or SQLite cannot create the session.
    pub async fn create(self) -> Result<Session<'a>, SessionError> {
        self.harness.create_session(self).await
    }
}

pub(crate) struct SessionHistory {
    bytes: usize,
    max_bytes: usize,
    turns: VecDeque<Vec<ModelMessage>>,
}

impl SessionHistory {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            bytes: 0,
            max_bytes,
            turns: VecDeque::new(),
        }
    }

    pub(crate) fn messages(&self) -> Vec<ModelMessage> {
        self.turns
            .iter()
            .flat_map(|turn| turn.iter().cloned())
            .collect()
    }

    pub(crate) fn push(&mut self, turn: Vec<ModelMessage>) {
        self.bytes = self.bytes.saturating_add(retained_bytes(&turn));
        self.turns.push_back(turn);

        while self.bytes > self.max_bytes && !self.turns.is_empty() {
            let evicted_bytes = self
                .turns
                .pop_front()
                .map_or(self.bytes, |evicted| retained_bytes(&evicted));
            self.bytes = self.bytes.saturating_sub(evicted_bytes);
        }
    }

    fn replace(&mut self, turns: Vec<Vec<ModelMessage>>) {
        self.bytes = 0;
        self.turns.clear();
        for turn in turns {
            self.push(turn);
        }
    }
}

fn retained_bytes(messages: &[ModelMessage]) -> usize {
    messages.iter().fold(0, |bytes, message| {
        bytes.saturating_add(message.retained_bytes())
    })
}

/// Application-facing harness for one complete model turn.
///
/// A turn advertises policy-approved tools, executes validated native calls,
/// returns tool results to the model, and finishes with locally validated
/// structured output.
pub struct Harness {
    database_path: Option<PathBuf>,
    file_system: Arc<dyn FileSystem>,
    lifecycle: LifecycleEmitter,
    max_history_bytes: usize,
    max_tool_calls: usize,
    model: Arc<dyn Model>,
    policy: Policy,
    repository: Option<Repository>,
}

impl Harness {
    /// Creates a deny-by-default harness backed by the local filesystem.
    pub fn new(model: impl Model + 'static) -> Self {
        Self {
            database_path: None,
            file_system: Arc::new(LocalFileSystem),
            lifecycle: LifecycleEmitter::default(),
            max_history_bytes: DEFAULT_MAX_HISTORY_BYTES,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            model: Arc::new(model),
            policy: Policy::default(),
            repository: None,
        }
    }

    /// Configures the SQLite database used by durable sessions.
    #[must_use]
    pub fn database(mut self, path: impl Into<PathBuf>) -> Self {
        self.database_path = Some(path.into());

        self
    }

    /// Configures the validated repository root and Git executable used by
    /// tools.
    #[must_use]
    pub fn repository(mut self, repository: Repository) -> Self {
        self.repository = Some(repository);

        self
    }

    /// Enables one built-in tool for model requests.
    #[must_use]
    pub fn allow(mut self, tool: Tool) -> Self {
        self.policy.allow(tool);

        self
    }

    /// Replaces the local filesystem implementation.
    #[must_use]
    pub fn file_system(mut self, file_system: impl FileSystem + 'static) -> Self {
        self.file_system = Arc::new(file_system);

        self
    }

    /// Sends metadata-only turn, model, and tool events to `observer`.
    ///
    /// This observer owns model events for requests made through the harness.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.lifecycle = LifecycleEmitter::new(observer);

        self
    }

    /// Overrides the maximum number of native calls allowed in one turn.
    #[must_use]
    pub fn max_tool_calls(mut self, max_tool_calls: NonZeroUsize) -> Self {
        self.max_tool_calls = max_tool_calls.get();

        self
    }

    /// Overrides the retained chat-history payload budget.
    ///
    /// Complete oldest turns are evicted when the budget is exceeded, so
    /// native tool-call and tool-result messages are never split.
    #[must_use]
    pub fn max_history_bytes(mut self, max_history_bytes: NonZeroUsize) -> Self {
        self.max_history_bytes = max_history_bytes.get();

        self
    }

    /// Runs one prompt without creating durable session history.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when the model fails, requests a denied tool,
    /// exceeds the call limit, or a requested repository operation fails.
    pub async fn run_once(
        &self,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Result<TurnOutcome, TurnError> {
        let request = ModelRequest::new(prompt, schema);
        self.run_request(request)
            .await
            .map(|(outcome, _, _)| outcome)
    }

    /// Builds a new durable session whose responses must match `schema`.
    pub fn session(&self, id: impl Into<String>, schema: OutputSchema) -> SessionBuilder<'_> {
        SessionBuilder {
            harness: self,
            id: id.into(),
            schema,
            system_prompt: None,
        }
    }

    /// Resumes a durable session and restores its bounded completed history.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when storage is not configured, the session is
    /// missing, its model differs, or SQLite cannot load it.
    pub async fn resume(&self, id: &str) -> Result<Session<'_>, SessionError> {
        let database = self.open_database().await?;
        let loaded = database.load_session(id).await?;
        self.validate_session_model(id, &loaded)?;
        let mut history = SessionHistory::new(loaded.max_history_bytes);
        for turn in loaded.turns {
            history.push(turn);
        }

        Ok(Session {
            database,
            harness: self,
            history,
            id: id.to_string(),
            provider_session_id: loaded.provider_session_id,
            schema: loaded.schema,
            system_prompt: loaded.system_prompt,
        })
    }

    async fn create_session<'a>(
        &'a self,
        builder: SessionBuilder<'a>,
    ) -> Result<Session<'a>, SessionError> {
        let database = self.open_database().await?;
        let config = NewSession::new(builder.id, builder.schema)
            .with_optional_system_prompt(builder.system_prompt);
        database
            .create_session(&config, self.model.metadata(), self.max_history_bytes)
            .await?;

        Ok(Session {
            database,
            harness: self,
            history: SessionHistory::new(self.max_history_bytes),
            id: config.id().to_string(),
            provider_session_id: None,
            schema: config.schema().clone(),
            system_prompt: config.system_prompt().map(str::to_string),
        })
    }

    async fn open_database(&self) -> Result<Database, SessionError> {
        let path = self
            .database_path
            .as_deref()
            .ok_or(SessionError::StorageRequired)?;

        Database::open(path).await
    }

    fn validate_session_model(&self, id: &str, loaded: &LoadedSession) -> Result<(), SessionError> {
        let (Some(stored_provider), Some(stored_model)) = (&loaded.provider, &loaded.model) else {
            if loaded.provider.is_none() && loaded.model.is_none() {
                return Ok(());
            }

            return Err(SessionError::InvalidData {
                reason: format!("session `{id}` has incomplete model identity"),
            });
        };
        let metadata = self.model.metadata();
        let (actual_provider, actual_model) = metadata.as_ref().map_or(
            ("unavailable".to_string(), "unavailable".to_string()),
            |metadata| {
                (
                    metadata.provider().to_string(),
                    metadata.model().to_string(),
                )
            },
        );
        if actual_provider != *stored_provider || actual_model != *stored_model {
            return Err(SessionError::ModelMismatch {
                actual_model,
                actual_provider,
                id: id.to_string(),
                stored_model: stored_model.clone(),
                stored_provider: stored_provider.clone(),
            });
        }

        Ok(())
    }

    async fn run_request(
        &self,
        request: ModelRequest,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>, Option<String>), TurnError> {
        let turn = self.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let result = self.run_turn(request, turn_id).await;

        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(error) => turn.failed(error.error_type()),
            }
        }

        result
    }

    async fn run_turn(
        &self,
        request: ModelRequest,
        turn_id: Option<LifecycleId>,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>, Option<String>), TurnError> {
        let started_at = Instant::now();
        let (mut request, read_tool, write_tool) = self.prepare_request(request)?;
        let mut completed_tool_calls = 0_usize;
        let mut model_request_index = 0_u64;
        let mut model_requests = Vec::new();
        let mut tool_calls = Vec::new();

        loop {
            let (response, activities, provider_session_id, native_resume_rejected) = self
                .complete_model_request(&request, model_request_index, turn_id)
                .await?;
            if native_resume_rejected || provider_session_id.is_some() {
                request.set_provider_session_id(provider_session_id);
            }
            model_request_index = model_request_index
                .saturating_add(u64::try_from(activities.len()).unwrap_or(u64::MAX));
            model_requests.extend(activities);

            match response {
                ModelResponse::Output(output) => {
                    request.record_output(&output);
                    let report = TurnReport {
                        duration: started_at.elapsed(),
                        model_requests,
                        tool_calls,
                    };

                    let provider_session_id = request.provider_session_id().map(str::to_string);

                    return Ok((
                        TurnOutcome { output, report },
                        request.into_messages(),
                        provider_session_id,
                    ));
                }
                ModelResponse::ToolCall(call) => {
                    let (result, activity) = self
                        .execute_tool_call(
                            &call,
                            read_tool.as_ref(),
                            write_tool.as_ref(),
                            completed_tool_calls,
                            turn_id,
                        )
                        .await?;
                    request.record_tool_result(call, result);
                    tool_calls.push(activity);
                    completed_tool_calls += 1;
                }
                ModelResponse::ToolCalls(calls) => {
                    if calls.is_empty() {
                        return Err(ModelError::MissingToolCall.into());
                    }
                    ensure_unique_tool_call_ids(&calls)?;
                    if calls.len() > self.max_tool_calls.saturating_sub(completed_tool_calls) {
                        return Err(TurnError::ToolCallLimit {
                            limit: self.max_tool_calls,
                        });
                    }
                    let mut results = Vec::with_capacity(calls.len());
                    for call in &calls {
                        let (result, activity) = self
                            .execute_tool_call(
                                call,
                                read_tool.as_ref(),
                                write_tool.as_ref(),
                                completed_tool_calls,
                                turn_id,
                            )
                            .await?;
                        results.push(result);
                        tool_calls.push(activity);
                        completed_tool_calls += 1;
                    }
                    request.record_tool_results(calls, results);
                }
            }
        }
    }

    fn prepare_request(
        &self,
        mut request: ModelRequest,
    ) -> Result<(ModelRequest, Option<ReadTool>, Option<WriteTool>), TurnError> {
        if self.lifecycle.is_enabled() {
            request.mark_lifecycle_observed();
        }
        let read_allowed = self.policy.allows(Tool::Read);
        let write_allowed = self.policy.allows(Tool::Write);
        if !read_allowed && !write_allowed {
            return Ok((request, None, None));
        }
        let repository = self
            .repository
            .as_ref()
            .ok_or(TurnError::RepositoryRequired)?;
        let read_tool = read_allowed.then(|| {
            request = request.clone().with_tool(ToolDefinition::read());
            ReadTool::with_git(
                self.file_system.clone(),
                repository.root().to_path_buf(),
                repository.git_executable().to_path_buf(),
            )
        });
        let write_tool = write_allowed.then(|| {
            request = request.clone().with_tool(ToolDefinition::write());
            WriteTool::new(self.file_system.clone(), repository.root().to_path_buf())
        });

        Ok((request, read_tool, write_tool))
    }

    async fn complete_model_request(
        &self,
        request: &ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<
        (
            ModelResponse,
            Vec<ModelRequestActivity>,
            Option<String>,
            bool,
        ),
        TurnError,
    > {
        let native_resume = request.provider_session_id().is_some();
        match self
            .complete_model_attempt(request.clone(), model_request_index, turn_id)
            .await
        {
            Ok((response, activity, provider_session_id)) => {
                Ok((response, vec![activity], provider_session_id, false))
            }
            Err(ModelAttemptError {
                duration,
                error: ModelError::ResumeUnavailable,
            }) if native_resume => {
                let rejected_activity = ModelRequestActivity {
                    completion: None,
                    duration,
                    response_type: crate::lifecycle::ModelResponseType::ResumeUnavailable,
                };
                let mut replay_request = request.clone();
                replay_request.set_provider_session_id(None);
                let replay_index = model_request_index.saturating_add(1);
                match self
                    .complete_model_attempt(replay_request, replay_index, turn_id)
                    .await
                {
                    Ok((response, replay_activity, provider_session_id)) => Ok((
                        response,
                        vec![rejected_activity, replay_activity],
                        provider_session_id,
                        true,
                    )),
                    Err(failure) => Err(ResumeFailure::Replay {
                        source: failure.error,
                    }
                    .into_model_error()
                    .into()),
                }
            }
            Err(failure) if native_resume => Err(ResumeFailure::Native {
                source: failure.error,
            }
            .into_model_error()
            .into()),
            Err(failure) => Err(failure.error.into()),
        }
    }

    async fn complete_model_attempt(
        &self,
        request: ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<(ModelResponse, ModelRequestActivity, Option<String>), ModelAttemptError> {
        let started_at = Instant::now();
        let model_lifecycle =
            self.lifecycle
                .start_model_request(self.model.metadata(), model_request_index, turn_id);
        let operation = self.model.complete(request.clone());
        let completion = match model_lifecycle.as_ref() {
            Some(model_lifecycle) => model_lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (response, completion, provider_session_id) = match completion {
            Ok(completion) => completion.into_parts(),
            Err(error) => {
                if let Some(model_lifecycle) = model_lifecycle {
                    model_lifecycle.failed(error.error_type(), error.http_status());
                }

                return Err(ModelAttemptError {
                    duration: started_at.elapsed(),
                    error,
                });
            }
        };
        if let Some(output) = response.output()
            && let Err(error) = request.schema().validate_value(output)
        {
            let error = ModelError::from(error);
            if let Some(model_lifecycle) = model_lifecycle {
                model_lifecycle.failed(error.error_type(), error.http_status());
            }

            return Err(ModelAttemptError {
                duration: started_at.elapsed(),
                error,
            });
        }
        let response_type = response.response_type();
        let activity = ModelRequestActivity {
            completion: completion.as_ref().map(sanitized_completion_metadata),
            duration: started_at.elapsed(),
            response_type,
        };
        if let Some(model_lifecycle) = model_lifecycle {
            model_lifecycle.completed(completion, response_type);
        }

        Ok((response, activity, provider_session_id))
    }

    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        read_tool: Option<&ReadTool>,
        write_tool: Option<&WriteTool>,
        completed_tool_calls: usize,
        turn_id: Option<LifecycleId>,
    ) -> Result<(String, ToolActivity), TurnError> {
        let mut tool_lifecycle =
            self.lifecycle
                .request_tool(call.id().to_string(), call.name().to_string(), turn_id);
        let execution = match call.arguments() {
            ToolCallArguments::Read(arguments) => {
                read_tool.map(|tool| ToolExecution::Read(tool, arguments))
            }
            ToolCallArguments::Write(arguments) => {
                write_tool.map(|tool| ToolExecution::Write(tool, arguments))
            }
        };
        let Some(execution) = execution else {
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.denied();
            }

            return Err(TurnError::ToolDenied {
                name: call.name().to_string(),
            });
        };
        if completed_tool_calls >= self.max_tool_calls {
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.failed(ToolErrorType::CallLimit);
            }

            return Err(TurnError::ToolCallLimit {
                limit: self.max_tool_calls,
            });
        }
        if let Some(tool_lifecycle) = tool_lifecycle.as_mut() {
            tool_lifecycle.started();
        }
        let operation = execute_tool(execution);
        let result = match tool_lifecycle.as_ref() {
            Some(tool_lifecycle) => tool_lifecycle.scope(operation).await,
            None => operation.await,
        };

        Self::finish_tool_call(result, tool_lifecycle)
    }

    fn finish_tool_call(
        result: Result<(String, ToolActivity), TurnError>,
        tool_lifecycle: Option<ToolLifecycle>,
    ) -> Result<(String, ToolActivity), TurnError> {
        match result {
            Ok(result) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    if matches!(
                        &result.1,
                        ToolActivity::ReadInspectionRejected { .. }
                            | ToolActivity::ReadRejected { .. }
                            | ToolActivity::WriteRejected { .. }
                    ) {
                        tool_lifecycle.failed(ToolErrorType::Execution);
                    } else {
                        tool_lifecycle.completed();
                    }
                }

                Ok(result)
            }
            Err(error) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    tool_lifecycle.failed(ToolErrorType::Execution);
                }

                Err(error)
            }
        }
    }
}

/// Failure returned by a complete harness turn.
#[derive(Debug, Error)]
pub enum TurnError {
    /// Provider request, response decoding, or terminal validation failed.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The model requested a tool unavailable under the configured policy.
    #[error("tool `{name}` is denied by policy")]
    ToolDenied {
        /// Denied native function name.
        name: String,
    },
    /// A repository read failed.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// Repository-scoped tools were enabled without a repository root.
    #[error("repository root is required when a repository tool is allowed")]
    RepositoryRequired,
    /// A repository write failed.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// The model exceeded the bounded number of calls in one turn.
    #[error("model exceeded the per-turn tool call limit of {limit}")]
    ToolCallLimit {
        /// Configured maximum calls.
        limit: usize,
    },
}

impl TurnError {
    /// Returns the stable lifecycle classification for this failure.
    pub fn error_type(&self) -> TurnErrorType {
        match self {
            Self::Model(error) => TurnErrorType::Model(error.error_type()),
            Self::ToolDenied { .. } => TurnErrorType::ToolDenied,
            Self::Read(_) | Self::Write(_) => TurnErrorType::Tool,
            Self::RepositoryRequired => TurnErrorType::RepositoryRequired,
            Self::ToolCallLimit { .. } => TurnErrorType::ToolCallLimit,
        }
    }
}

enum ToolExecution<'a> {
    Read(&'a ReadTool, &'a ReadArguments),
    Write(&'a WriteTool, &'a WriteArguments),
}

struct ModelAttemptError {
    duration: Duration,
    error: ModelError,
}

#[derive(Debug, Error)]
enum ResumeFailure {
    #[error("native provider continuation failed: {source}")]
    Native {
        #[source]
        source: ModelError,
    },
    #[error("native provider continuation was unavailable and history replay failed: {source}")]
    Replay {
        #[source]
        source: ModelError,
    },
}

impl ResumeFailure {
    fn into_model_error(self) -> ModelError {
        let source = match &self {
            Self::Native { source } | Self::Replay { source } => source,
        };
        if !matches!(source, ModelError::Request(_)) {
            return match self {
                Self::Native { source } | Self::Replay { source } => source,
            };
        }
        let error_type = source.error_type();
        let http_status = source.http_status();

        ModelError::classified_request(error_type, http_status, Box::new(self))
    }
}

async fn execute_tool(execution: ToolExecution<'_>) -> Result<(String, ToolActivity), TurnError> {
    let started_at = Instant::now();

    match execution {
        ToolExecution::Read(read_tool, arguments) => {
            execute_read_tool(read_tool, arguments, started_at).await
        }
        ToolExecution::Write(write_tool, arguments) => {
            execute_write_tool(write_tool, arguments, started_at).await
        }
    }
}

async fn execute_read_tool(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    if let Some(error) = arguments.validation_error() {
        return reject_invalid_read_arguments(arguments, error, started_at);
    }
    if arguments.action() == ReadAction::File {
        return execute_file_read(read_tool, arguments, started_at).await;
    }

    execute_repository_inspection(read_tool, arguments, started_at).await
}

fn reject_invalid_read_arguments(
    arguments: &ReadArguments,
    error: &str,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let summary = arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read");
    let result = read::invalid_arguments_tool_result(error, summary).map_err(ReadError::from)?;
    let activity = if arguments.action() == ReadAction::File {
        ToolActivity::ReadRejected {
            duration: started_at.elapsed(),
            path: sanitize_report_text(summary),
        }
    } else {
        ToolActivity::ReadInspectionRejected {
            action: arguments.action(),
            duration: started_at.elapsed(),
            summary: sanitize_report_text(summary),
        }
    };

    Ok((result, activity))
}

async fn execute_file_read(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    match read_tool.execute(arguments).await {
        Ok(output) => match output.to_tool_result() {
            Ok(result) => Ok((
                result,
                ToolActivity::Read {
                    duration: started_at.elapsed(),
                    end_line: output.end_line(),
                    path: sanitize_report_text(output.path()),
                    start_line: output.start_line(),
                    truncated: output.truncated(),
                },
            )),
            Err(error) => error
                .to_tool_result(output.path())
                .map_err(ReadError::from)
                .map_err(TurnError::from)
                .map(|result| {
                    (
                        result,
                        ToolActivity::ReadRejected {
                            duration: started_at.elapsed(),
                            path: sanitize_report_text(output.path()),
                        },
                    )
                }),
        },
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(arguments.path())
            .map_err(ReadError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::ReadRejected {
                        duration: started_at.elapsed(),
                        path: sanitize_report_text(arguments.path()),
                    },
                )
            }),
        Err(error) => Err(error.into()),
    }
}

async fn execute_repository_inspection(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let fallback_summary = arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read");
    match read_tool.execute_inspection(arguments).await {
        Ok((result, summary)) => Ok((
            result,
            ToolActivity::ReadInspection {
                action: arguments.action(),
                duration: started_at.elapsed(),
                summary: sanitize_report_text(&summary),
            },
        )),
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(fallback_summary)
            .map_err(ReadError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::ReadInspectionRejected {
                        action: arguments.action(),
                        duration: started_at.elapsed(),
                        summary: sanitize_report_text(fallback_summary),
                    },
                )
            }),
        Err(error) => Err(error.into_read_error(fallback_summary.to_string()).into()),
    }
}

async fn execute_write_tool(
    write_tool: &WriteTool,
    arguments: &WriteArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    match write_tool.execute(arguments).await {
        Ok(output) => {
            let activity = ToolActivity::Write {
                bytes_written: output.bytes_written(),
                duration: started_at.elapsed(),
                path: sanitize_report_text(output.path()),
            };
            let result = output
                .to_tool_result()
                .map_err(WriteError::from)
                .map_err(TurnError::from)?;

            Ok((result, activity))
        }
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(arguments.path())
            .map_err(WriteError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::WriteRejected {
                        duration: started_at.elapsed(),
                        path: sanitize_report_text(arguments.path()),
                    },
                )
            }),
        Err(error) => Err(error.into()),
    }
}

fn format_report_duration(duration: Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn sanitized_completion_metadata(metadata: &CompletionMetadata) -> CompletionMetadata {
    CompletionMetadata::new(
        sanitize_report_text(metadata.finish_reason()),
        metadata.response_id().map(sanitize_report_text),
        metadata.response_model().map(sanitize_report_text),
        metadata.system_fingerprint().map(sanitize_report_text),
        metadata.usage().copied(),
    )
}

fn sanitize_report_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use mockall::Sequence;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::io::AsyncRead;
    use tokio::sync::Notify;

    use super::*;
    use crate::file_system::MockFileSystem;
    use crate::model::ModelMessage;
    use crate::tool::{ReadArguments, ToolDefinition, WriteArguments};

    fn model() -> crate::model::MockModel {
        let mut model = crate::model::MockModel::new();
        model.expect_metadata().return_const(None);

        model
    }

    fn resume_fallback_model() -> crate::model::MockModel {
        let mut model = model();
        let mut sequence = Sequence::new();
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id().is_none())
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "first"
                })))
                .with_provider_session_id("native-session"))
            });
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id() == Some("native-session"))
            .returning(|_| Err(ModelError::ResumeUnavailable));
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| {
                request.provider_session_id().is_none()
                    && request.messages()
                        == [
                            ModelMessage::User("first".to_string()),
                            ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                            ModelMessage::User("second".to_string()),
                        ]
            })
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "second"
                })))
                .with_provider_session_id("replacement-session"))
            });
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id() == Some("replacement-session"))
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "third"
                }))))
            });

        model
    }

    fn object_schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn read_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
        Harness::new(model)
            .repository(Repository::fixture("repo"))
            .allow(Tool::Read)
            .file_system(file_system)
    }

    fn write_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
        Harness::new(model)
            .repository(Repository::fixture("repo"))
            .allow(Tool::Write)
            .file_system(file_system)
    }

    fn read_call(id: &str) -> ToolCall {
        read_call_with_path(id, "Cargo.toml")
    }

    fn read_call_with_path(id: &str, path: &str) -> ToolCall {
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "action": "file",
            "path": path,
            "limit": 1
        }))
        .expect("read arguments should be valid");

        ToolCall::read(id.to_string(), arguments, None)
    }

    fn inspection_call(id: &str, arguments: Value) -> ToolCall {
        let arguments = serde_json::from_value::<ReadArguments>(arguments)
            .expect("inspection arguments should be valid");

        ToolCall::read(id.to_string(), arguments, None)
    }

    fn response_without_metadata(response: ModelResponse) -> crate::ModelCompletion {
        crate::ModelCompletion::from_response(response)
    }

    fn request_error_with_http_status(status: u16) -> ModelError {
        ModelError::classified_request(
            crate::ModelErrorType::Provider,
            Some(status),
            io::Error::other("provider request failed").into(),
        )
    }

    fn response_with_metadata(response: ModelResponse) -> crate::ModelCompletion {
        crate::ModelCompletion::new(
            crate::CompletionMetadata::new(
                "stop\nforged".to_string(),
                Some("response\u{1b}-1".to_string()),
                Some("reported\nmodel".to_string()),
                Some("finger\tprint".to_string()),
                Some(crate::CompletionUsage::new(
                    None,
                    None,
                    Some(12),
                    Some(4),
                    None,
                    Some(16),
                )),
            ),
            response,
        )
    }

    struct SlowModel;

    #[async_trait]
    impl Model for SlowModel {
        async fn complete(
            &self,
            _request: ModelRequest,
        ) -> Result<crate::ModelCompletion, ModelError> {
            tokio::time::sleep(Duration::from_millis(50)).await;

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "done"
            }))))
        }
    }

    struct PendingModel {
        started: Arc<Notify>,
    }

    #[async_trait]
    impl Model for PendingModel {
        async fn complete(
            &self,
            _request: ModelRequest,
        ) -> Result<crate::ModelCompletion, ModelError> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    struct PendingToolFileSystem {
        started: Arc<Notify>,
    }

    #[async_trait]
    impl FileSystem for PendingToolFileSystem {
        async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            if path == Path::new("repo") {
                Ok(PathBuf::from("/repo"))
            } else {
                Ok(PathBuf::from("/repo/Cargo.toml"))
            }
        }

        async fn open_beneath(
            &self,
            _root: &Path,
            _path: &Path,
        ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
            self.started.notify_one();
            std::future::pending().await
        }

        async fn replace_beneath(
            &self,
            _root: &Path,
            _path: &Path,
            _expected: Option<Vec<u8>>,
            _content: Vec<u8>,
        ) -> io::Result<()> {
            Err(io::Error::other(
                "pending read fixture must not replace files",
            ))
        }
    }

    struct LeaseExpiryModel {
        call_count: AtomicUsize,
        release_first: Arc<Notify>,
        started_first: Arc<Notify>,
    }

    #[async_trait]
    impl Model for LeaseExpiryModel {
        async fn complete(
            &self,
            _request: ModelRequest,
        ) -> Result<crate::ModelCompletion, ModelError> {
            if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started_first.notify_one();
                self.release_first.notified().await;
            }

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "done"
            }))))
        }
    }

    struct RequestDropNotifier {
        dropped: Arc<Notify>,
    }

    impl Drop for RequestDropNotifier {
        fn drop(&mut self) {
            self.dropped.notify_one();
        }
    }

    struct LeaseOwnershipModel {
        call_count: AtomicUsize,
        dropped_first: Arc<Notify>,
        started_first: Arc<Notify>,
    }

    #[async_trait]
    impl Model for LeaseOwnershipModel {
        async fn complete(
            &self,
            _request: ModelRequest,
        ) -> Result<crate::ModelCompletion, ModelError> {
            if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                let _drop_notifier = RequestDropNotifier {
                    dropped: Arc::clone(&self.dropped_first),
                };
                self.started_first.notify_one();
                std::future::pending::<()>().await;
            }

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "done"
            }))))
        }
    }

    async fn send_with_resumed_session(
        harness: Arc<Harness>,
        prompt: &'static str,
    ) -> Result<TurnOutcome, SessionError> {
        let mut session = harness
            .resume("session-a")
            .await
            .expect("session should resume");

        session.send(prompt).await
    }

    async fn wait_for_fixture(notify: &Notify, description: &str) {
        let result = tokio::time::timeout(Duration::from_secs(5), notify.notified()).await;

        assert!(result.is_ok(), "timed out waiting for {description}");
    }

    async fn stored_turn_state(database: &Database) -> (String, Option<String>) {
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status, error_type FROM session_turn ORDER BY turn_position DESC LIMIT 1",
        )
        .fetch_one(database.pool())
        .await
        .expect("stored turn state should load")
    }

    async fn wait_for_stored_turn_state(
        database: &Database,
        expected: &(String, Option<String>),
    ) -> (String, Option<String>) {
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            let state = stored_turn_state(database).await;
            if &state == expected || Instant::now() >= deadline {
                return state;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn stored_lease_expiry(database: &Database) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT lease_expires_at FROM session_turn WHERE session_id = ?",
        )
        .bind("session-a")
        .fetch_one(database.pool())
        .await
        .expect("active lease should load")
    }

    fn elapsed_timestamp(origin: i64, started_at: tokio::time::Instant) -> i64 {
        let elapsed_seconds = i64::try_from(started_at.elapsed().as_secs()).unwrap_or(i64::MAX);

        origin.saturating_add(elapsed_seconds)
    }

    async fn wait_for_lease_extension(database: &Database, previous_expiry: i64) -> Option<i64> {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let lease_expiry = stored_lease_expiry(database).await;
                if lease_expiry > previous_expiry {
                    return lease_expiry;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .ok()
    }

    fn write_call(id: &str, patch: &str) -> ToolCall {
        let arguments = serde_json::from_value::<WriteArguments>(json!({
            "path": "src/lib.rs",
            "patch": patch
        }))
        .expect("write arguments should be valid");

        ToolCall::write(id.to_string(), arguments, None)
    }

    fn readable_file_system() -> MockFileSystem {
        readable_file_system_with(b"[workspace]\nmember = true\n".to_vec())
    }

    fn readable_file_system_with(content: Vec<u8>) -> MockFileSystem {
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/Cargo.toml")));
        file_system
            .expect_open_beneath()
            .times(1)
            .return_once(move |_, _| Ok(Box::new(Cursor::new(content))));

        file_system
    }

    fn turn_started_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
        match event.kind() {
            crate::LifecycleEventKind::TurnStarted { turn_id } => Some(*turn_id),
            _ => None,
        }
    }

    fn model_started_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
        match event.kind() {
            crate::LifecycleEventKind::ModelRequestStarted { model_call_id, .. } => {
                Some(*model_call_id)
            }
            _ => None,
        }
    }

    fn tool_requested_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
        match event.kind() {
            crate::LifecycleEventKind::ToolRequested { tool_call_id, .. } => Some(*tool_call_id),
            _ => None,
        }
    }

    fn assert_read_tool_lifecycle(events: &[crate::LifecycleEvent]) {
        let turn_id = turn_started_id(&events[0]).expect("first event should start the turn");
        let first_model_call_id =
            model_started_id(&events[1]).expect("second event should start the model request");
        assert!(matches!(
            events[1].kind(),
            crate::LifecycleEventKind::ModelRequestStarted {
                model: None,
                request_index: 0,
                turn_id: Some(event_turn_id),
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[2].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted {
                completion: None,
                model_call_id,
                response_type: crate::ModelResponseType::ToolCall,
                turn_id: Some(event_turn_id),
                ..
            } if *model_call_id == first_model_call_id && *event_turn_id == turn_id
        ));
        let tool_call_id =
            tool_requested_id(&events[3]).expect("fourth event should request the tool");
        assert!(matches!(
            events[3].kind(),
            crate::LifecycleEventKind::ToolRequested {
                provider_call_id,
                tool_name,
                turn_id: event_turn_id,
                ..
            } if provider_call_id == "provider-call-id"
                && tool_name == "read"
                && *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[4].kind(),
            crate::LifecycleEventKind::ToolStarted {
                tool_call_id: event_tool_call_id,
                turn_id: event_turn_id,
            } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[5].kind(),
            crate::LifecycleEventKind::ToolCompleted {
                tool_call_id: event_tool_call_id,
                turn_id: event_turn_id,
                ..
            } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[6].kind(),
            crate::LifecycleEventKind::ModelRequestStarted {
                request_index: 1,
                turn_id: Some(event_turn_id),
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[7].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted {
                completion: None,
                response_type: crate::ModelResponseType::Output,
                turn_id: Some(event_turn_id),
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[8].kind(),
            crate::LifecycleEventKind::TurnCompleted {
                turn_id: event_turn_id,
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(turn_started_id(&events[1]).is_none());
        assert!(model_started_id(&events[0]).is_none());
        assert!(tool_requested_id(&events[0]).is_none());
    }

    #[tokio::test]
    async fn completes_read_tool_round_trip() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let call_index = call_count.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                assert_eq!(request.tools(), &[ToolDefinition::read()]);

                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("call_read"),
                )));
            }
            assert_eq!(request.messages().len(), 3);
            assert!(matches!(
                &request.messages()[0],
                ModelMessage::User(prompt) if prompt == "inspect the manifest"
            ));
            assert!(matches!(
                &request.messages()[1],
                ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
            ));
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult {
                    call_id,
                    content,
                    name,
                }
                    if call_id == "call_read"
                        && name == "read"
                        && serde_json::from_str::<Value>(content)
                            .is_ok_and(|value| value["content"] == "[workspace]")
            ));

            Ok(response_without_metadata(ModelResponse::Output(
                json!({ "summary": "workspace" }),
            )))
        });
        let harness = read_harness(model, readable_file_system());

        // Act
        let output = harness
            .run_once("inspect the manifest", object_schema())
            .await
            .expect("tool round trip should succeed");

        // Assert
        assert_eq!(output.output(), &json!({ "summary": "workspace" }));
    }

    #[tokio::test]
    async fn completes_repository_inspection_round_trip() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    inspection_call(
                        "call_list",
                        json!({ "action": "list", "path": "Cargo.toml" }),
                    ),
                )));
            }
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult { content, .. }
                    if serde_json::from_str::<Value>(content)
                        .is_ok_and(|value| value["result"] == json!(["Cargo.toml"]))
            ));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "listed"
            }))))
        });
        let harness = Harness::new(model)
            .repository(Repository::fixture(env!("CARGO_MANIFEST_DIR")))
            .allow(Tool::Read);

        // Act
        let outcome = harness
            .run_once("list the manifest", object_schema())
            .await
            .expect("repository inspection should succeed");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "listed" }));
        assert!(matches!(
            &outcome.report().tool_calls()[0],
            ToolActivity::ReadInspection {
                action: ReadAction::List,
                summary,
                ..
            } if summary == "Cargo.toml"
        ));
    }

    #[tokio::test]
    async fn returns_encoded_file_result_rejection_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete()
            .times(2)
            .returning(move |request| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        read_call("call_read"),
                    )));
                }
                assert!(request.messages().iter().any(|message| {
                    matches!(
                        message,
                        ModelMessage::ToolResult { content, .. }
                            if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                                value["status"] == "rejected"
                                    && value["error"]
                                        .as_str()
                                        .is_some_and(|error| error.contains("exceeds the read size limit"))
                            })
                    )
                }));

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let content = "\u{1}".repeat(16 * 1024).into_bytes();
        let harness = read_harness(model, readable_file_system_with(content));

        // Act
        let outcome = harness
            .run_once("read the manifest", object_schema())
            .await
            .expect("model should recover from an encoded-size rejection");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
        assert!(matches!(
            &outcome.report().tool_calls()[0],
            ToolActivity::ReadRejected { path, .. } if path == "Cargo.toml"
        ));
    }

    #[tokio::test]
    async fn returns_schema_valid_read_argument_rejections_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete()
            .times(2)
            .returning(move |request| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                        inspection_call("call_file", json!({})),
                        inspection_call("call_search", json!({ "action": "search" })),
                    ])));
                }
                assert!(request.messages().iter().any(|message| {
                    matches!(
                        message,
                        ModelMessage::ToolResult { content, .. }
                            if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                                value["status"] == "rejected"
                                    && value["error"] == "file requires a path and accepts only offset and limit"
                            })
                    )
                }));
                assert!(request.messages().iter().any(|message| {
                    matches!(
                        message,
                        ModelMessage::ToolResult { content, .. }
                            if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                                value["status"] == "rejected"
                                    && value["error"] == "search requires a query and accepts only an optional path and limit"
                            })
                    )
                }));

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let harness = read_harness(model, MockFileSystem::new());

        // Act
        let outcome = harness
            .run_once("read and search the repository", object_schema())
            .await
            .expect("model should recover from schema-portable argument rejection");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
        assert!(matches!(
            &outcome.report().tool_calls()[0],
            ToolActivity::ReadRejected { path, .. } if path == "read"
        ));
        assert!(matches!(
            &outcome.report().tool_calls()[1],
            ToolActivity::ReadInspectionRejected {
                action: ReadAction::Search,
                summary,
                ..
            } if summary == "read"
        ));
    }

    #[tokio::test]
    async fn returns_correctable_repository_inspection_rejection_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    inspection_call(
                        "call_show",
                        json!({
                            "action": "show",
                            "path": "definitely-missing-review-file",
                            "side": "head"
                        }),
                    ),
                )));
            }
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult { content, .. }
                    if serde_json::from_str::<Value>(content)
                        .is_ok_and(|value| value["status"] == "rejected")
            ));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
        let harness = Harness::new(model)
            .repository(Repository::fixture(env!("CARGO_MANIFEST_DIR")))
            .allow(Tool::Read);

        // Act
        let outcome = harness
            .run_once("show the missing file", object_schema())
            .await
            .expect("model should recover from a rejected inspection");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
        assert!(matches!(
            &outcome.report().tool_calls()[0],
            ToolActivity::ReadInspectionRejected {
                action: ReadAction::Show,
                summary,
                ..
            } if summary == "definitely-missing-review-file"
        ));
    }

    #[tokio::test]
    async fn returns_repository_inspection_boundary_failure() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                inspection_call("call_list", json!({ "action": "list" })),
            )))
        });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Err(io::Error::other("repository unavailable")));
        file_system.expect_open_beneath().times(0);
        let harness = read_harness(model, file_system);

        // Act
        let error = harness
            .run_once("list files", object_schema())
            .await
            .expect_err("repository boundary failure should end the turn");

        // Assert
        assert!(matches!(
            error,
            TurnError::Read(ReadError::RepositoryRoot { .. })
        ));
    }

    #[tokio::test]
    async fn completes_multiple_read_tools_from_one_model_response() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                    read_call("call_one"),
                    read_call("call_two"),
                ])));
            }
            assert!(matches!(
                &request.messages()[1],
                ModelMessage::AssistantToolCalls(calls)
                    if calls.iter().map(ToolCall::id).collect::<Vec<_>>()
                        == ["call_one", "call_two"]
            ));
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult { call_id, .. } if call_id == "call_one"
            ));
            assert!(matches!(
                &request.messages()[3],
                ModelMessage::ToolResult { call_id, .. } if call_id == "call_two"
            ));

            Ok(response_without_metadata(ModelResponse::Output(
                json!({ "summary": "workspace" }),
            )))
        });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(4)
            .returning(|path| {
                if path == std::path::Path::new("repo") {
                    Ok(PathBuf::from("/repo"))
                } else {
                    Ok(PathBuf::from("/repo/Cargo.toml"))
                }
            });
        file_system
            .expect_open_beneath()
            .times(2)
            .returning(|_, _| {
                Ok(Box::new(Cursor::new(
                    b"[workspace]\nmember = true\n".to_vec(),
                )))
            });
        let harness = read_harness(model, file_system);

        // Act
        let outcome = harness
            .run_once("inspect two files", object_schema())
            .await
            .expect("parallel tool round trip should succeed");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "workspace" }));
        assert_eq!(outcome.report().tool_calls().len(), 2);
    }

    #[tokio::test]
    async fn returns_correctable_read_rejection_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("call_read"),
                )));
            }
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult { content, .. }
                    if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                        value["path"] == "Cargo.toml" && value["status"] == "rejected"
                    })
            ));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")));
        file_system.expect_open_beneath().times(0);
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness = read_harness(model, file_system).with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });

        // Act
        let outcome = harness
            .run_once("inspect", object_schema())
            .await
            .expect("model should recover from a rejected read path");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
        assert_eq!(outcome.report().tool_calls().len(), 1);
        let activity = &outcome.report().tool_calls()[0];
        assert_eq!(activity.name(), "read");
        assert_eq!(activity.path(), "Cargo.toml");
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert!(matches!(
            events[5].kind(),
            crate::LifecycleEventKind::ToolFailed {
                error_type: ToolErrorType::Execution,
                ..
            }
        ));
        assert!(matches!(
            events[8].kind(),
            crate::LifecycleEventKind::TurnCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn session_retains_successful_conversation_history() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let call_index = call_count.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                assert_eq!(
                    request.messages(),
                    &[ModelMessage::User("first question".to_string())]
                );

                return Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "first answer"
                }))));
            }
            assert_eq!(
                request.messages(),
                &[
                    ModelMessage::User("first question".to_string()),
                    ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                    ModelMessage::User("second question".to_string()),
                ]
            );

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "second answer"
            }))))
        });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");

        // Act
        let first = session
            .send("first question")
            .await
            .expect("first chat turn should succeed");
        let second = session
            .send("second question")
            .await
            .expect("second chat turn should succeed");

        // Assert
        assert_eq!(first.output(), &json!({"summary": "first answer"}));
        assert_eq!(second.output(), &json!({"summary": "second answer"}));
        assert_eq!(second.report().model_requests().len(), 1);
        assert!(second.report().duration() >= second.report().model_requests()[0].duration());
    }

    #[tokio::test]
    async fn stale_session_handles_acquire_current_canonical_state() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                assert_eq!(
                    request.messages(),
                    &[ModelMessage::User("first question".to_string())]
                );
                assert_eq!(request.provider_session_id(), None);

                return Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "first answer"
                })))
                .with_provider_session_id("native-session"));
            }
            assert_eq!(
                request.messages(),
                &[
                    ModelMessage::User("first question".to_string()),
                    ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                    ModelMessage::User("second question".to_string()),
                ]
            );
            assert_eq!(request.provider_session_id(), Some("native-session"));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "second answer"
            }))))
        });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut first = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        let mut stale = harness
            .resume("session-a")
            .await
            .expect("second handle should resume before the first turn");

        // Act
        first
            .send("first question")
            .await
            .expect("first handle should complete its turn");
        let outcome = stale
            .send("second question")
            .await
            .expect("stale handle should refresh before its turn");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "second answer" }));
    }

    #[tokio::test]
    async fn session_sends_the_system_prompt_on_every_turn() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ModelMessage::System("read-only instructions".to_string()),
                    ModelMessage::User("first".to_string()),
                ]
            } else {
                vec![
                    ModelMessage::System("read-only instructions".to_string()),
                    ModelMessage::User("first".to_string()),
                    ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                    ModelMessage::User("second".to_string()),
                ]
            };
            assert_eq!(request.messages(), expected);

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": if expected.len() == 2 { "one" } else { "two" }
            }))))
        });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut session = harness
            .session("session-a", object_schema())
            .system_prompt("read-only instructions")
            .create()
            .await
            .expect("session should be created");

        // Act
        session
            .send("first")
            .await
            .expect("first chat turn should succeed");
        let second = session
            .send("second")
            .await
            .expect("second chat turn should succeed");

        // Assert
        assert_eq!(second.output(), &json!({"summary": "two"}));
    }

    #[test]
    fn tool_activity_display_formats_every_outcome_safely() {
        // Arrange
        let read = ToolActivity::Read {
            duration: Duration::ZERO,
            end_line: None,
            path: "empty\n\u{1b}]52;c;Y2xpcGJvYXJk\u{7}.txt".to_string(),
            start_line: 1,
            truncated: false,
        };
        let inspection = ToolActivity::ReadInspection {
            action: crate::tool::ReadAction::List,
            duration: Duration::from_millis(1),
            summary: ".".to_string(),
        };
        let rejected_inspection = ToolActivity::ReadInspectionRejected {
            action: crate::tool::ReadAction::Search,
            duration: Duration::from_millis(1),
            summary: "needle".to_string(),
        };
        let rejected_read = ToolActivity::ReadRejected {
            duration: Duration::from_millis(2),
            path: "missing.rs".to_string(),
        };
        let write = ToolActivity::Write {
            bytes_written: 4,
            duration: Duration::from_millis(3),
            path: "src/lib.rs".to_string(),
        };
        let rejected_write = ToolActivity::WriteRejected {
            duration: Duration::from_millis(4),
            path: "src/main.rs".to_string(),
        };

        // Act
        let displays = [
            read.to_string(),
            inspection.to_string(),
            rejected_inspection.to_string(),
            rejected_read.to_string(),
            write.to_string(),
            rejected_write.to_string(),
        ];

        // Assert
        assert_eq!(
            displays,
            [
                "read empty\u{fffd}\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}.txt (line 1; <1 ms)",
                "read list . (completed; 1 ms)",
                "read search needle (rejected; 1 ms)",
                "read missing.rs (rejected; 2 ms)",
                "write src/lib.rs (4 bytes; 3 ms)",
                "write src/main.rs (rejected; 4 ms)",
            ]
        );
        assert_eq!(inspection.duration(), Duration::from_millis(1));
        assert_eq!(inspection.path(), ".");
        assert_eq!(rejected_inspection.duration(), Duration::from_millis(1));
        assert_eq!(rejected_inspection.path(), "needle");
        assert_eq!(rejected_read.duration(), Duration::from_millis(2));
        assert_eq!(rejected_read.name(), "read");
        assert_eq!(rejected_read.path(), "missing.rs");
        assert_eq!(write.duration(), Duration::from_millis(3));
        assert_eq!(write.name(), "write");
        assert_eq!(write.path(), "src/lib.rs");
        assert_eq!(rejected_write.duration(), Duration::from_millis(4));
        assert_eq!(rejected_write.name(), "write");
        assert_eq!(rejected_write.path(), "src/main.rs");
    }

    #[test]
    fn chat_history_evicts_complete_tool_turns() {
        // Arrange
        let tool_turn = vec![
            ModelMessage::User("inspect".to_string()),
            ModelMessage::AssistantToolCall(read_call("call_read")),
            ModelMessage::ToolResult {
                call_id: "call_read".to_string(),
                content: "file contents".to_string(),
                name: "read".to_string(),
            },
            ModelMessage::Assistant(r#"{"summary":"old"}"#.to_string()),
        ];
        let latest_turn = vec![
            ModelMessage::User("latest".to_string()),
            ModelMessage::Assistant(r#"{"summary":"new"}"#.to_string()),
        ];
        let max_bytes = retained_bytes(&tool_turn).max(retained_bytes(&latest_turn));
        let mut history = SessionHistory::new(max_bytes);

        // Act
        history.push(tool_turn);
        history.push(latest_turn.clone());

        // Assert
        assert_eq!(history.messages(), latest_turn);
        assert!(history.bytes <= max_bytes);
    }

    #[tokio::test]
    async fn session_applies_the_configured_history_budget() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(3).returning(move |request| {
            match call_count.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    assert_eq!(
                        request.messages(),
                        &[ModelMessage::User("first".to_string())]
                    );

                    Ok(response_without_metadata(ModelResponse::Output(json!({
                        "summary": "xxxxxxxxxxxxxxxxxxxx"
                    }))))
                }
                1 => {
                    assert_eq!(request.messages().len(), 3);

                    Ok(response_without_metadata(ModelResponse::Output(json!({
                        "summary": "two"
                    }))))
                }
                _ => {
                    assert_eq!(
                        request.messages(),
                        &[
                            ModelMessage::User("second".to_string()),
                            ModelMessage::Assistant(r#"{"summary":"two"}"#.to_string()),
                            ModelMessage::User("third".to_string()),
                        ]
                    );

                    Ok(response_without_metadata(ModelResponse::Output(json!({
                        "summary": "three"
                    }))))
                }
            }
        });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model)
            .database(directory.path().join("harness.db"))
            .max_history_bytes(NonZeroUsize::new(50).expect("history budget should be nonzero"));
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");

        // Act
        session
            .send("first")
            .await
            .expect("first chat turn should succeed");
        session
            .send("second")
            .await
            .expect("second chat turn should succeed");
        let third = session
            .send("third")
            .await
            .expect("third chat turn should succeed");

        // Assert
        assert_eq!(third.output(), &json!({"summary": "three"}));
    }

    #[tokio::test]
    async fn sequential_resumed_handles_reload_completed_canonical_history() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let call_index = call_count.fetch_add(1, Ordering::SeqCst);
            let expected = if call_index == 0 {
                vec![ModelMessage::User("first".to_string())]
            } else {
                vec![
                    ModelMessage::User("first".to_string()),
                    ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                    ModelMessage::User("second".to_string()),
                ]
            };
            assert_eq!(request.messages(), expected);

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": if call_index == 0 { "one" } else { "two" }
            }))))
        });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut first_handle = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        let mut stale_handle = harness
            .resume("session-a")
            .await
            .expect("second handle should resume before the first turn");

        // Act
        let first = first_handle
            .send("first")
            .await
            .expect("first handle should complete its turn");
        let second = stale_handle
            .send("second")
            .await
            .expect("stale handle should reload the completed turn");

        // Assert
        assert_eq!(first.output(), &json!({"summary": "one"}));
        assert_eq!(second.output(), &json!({"summary": "two"}));
    }

    #[tokio::test]
    async fn session_does_not_replay_a_failed_turn() {
        // Arrange
        let mut model = model();
        let mut sequence = Sequence::new();
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(ModelError::InvalidResponse));
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|request| {
                assert_eq!(
                    request.messages(),
                    &[ModelMessage::User("retry".to_string())]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");

        // Act
        let error = session
            .send("failed question")
            .await
            .expect_err("the first turn should fail");
        let recovered = session
            .send("retry")
            .await
            .expect("the next turn should start from clean history");

        // Assert
        assert!(matches!(
            error,
            SessionError::Turn(TurnError::Model(ModelError::InvalidResponse))
        ));
        assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
    }

    #[tokio::test]
    async fn session_invalidates_provider_resume_state_after_a_failed_turn() {
        // Arrange
        let mut model = model();
        let mut sequence = Sequence::new();
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id().is_none())
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "first"
                })))
                .with_provider_session_id("native-session"))
            });
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id() == Some("native-session"))
            .returning(|_| {
                Err(ModelError::SchemaViolation {
                    path: "/summary".to_string(),
                    reason: "required property is missing".to_string(),
                })
            });
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| {
                request.provider_session_id().is_none()
                    && request.messages()
                        == [
                            ModelMessage::User("first".to_string()),
                            ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                            ModelMessage::User("retry".to_string()),
                        ]
            })
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        session
            .send("first")
            .await
            .expect("first turn should succeed");

        // Act
        let error = session
            .send("failed")
            .await
            .expect_err("second turn should fail");
        drop(session);
        let mut resumed = harness
            .resume("session-a")
            .await
            .expect("session should reopen");
        let recovered = resumed
            .send("retry")
            .await
            .expect("replayed turn should succeed");

        // Assert
        assert!(matches!(
            &error,
            SessionError::Turn(TurnError::Model(ModelError::SchemaViolation { path, reason }))
                if path == "/summary" && reason == "required property is missing"
        ));
        assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
    }

    #[tokio::test]
    async fn session_accounts_for_resume_fallback_and_persists_its_continuation() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness = Harness::new(resume_fallback_model())
            .database(directory.path().join("harness.db"))
            .with_lifecycle_observer(move |event| {
                observed_events
                    .lock()
                    .expect("event recorder should not be poisoned")
                    .push(event);
            });
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        session
            .send("first")
            .await
            .expect("first turn should succeed");
        drop(session);
        let mut session = harness
            .resume("session-a")
            .await
            .expect("session should resume");

        // Act
        let outcome = session
            .send("second")
            .await
            .expect("history replay should succeed");
        drop(session);
        let mut session = harness
            .resume("session-a")
            .await
            .expect("session should resume with replacement continuation");
        let third = session
            .send("third")
            .await
            .expect("replacement continuation should succeed");

        // Assert
        assert_eq!(outcome.output(), &json!({"summary": "second"}));
        assert_eq!(outcome.report().model_requests().len(), 2);
        assert_eq!(
            outcome.report().model_requests()[0].response_type(),
            crate::ModelResponseType::ResumeUnavailable
        );
        assert_eq!(
            outcome.report().model_requests()[1].response_type(),
            crate::ModelResponseType::Output
        );
        assert_eq!(third.output(), &json!({"summary": "third"}));
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert!(matches!(
            events[5].kind(),
            crate::LifecycleEventKind::ModelRequestStarted {
                request_index: 0,
                ..
            }
        ));
        assert!(matches!(
            events[6].kind(),
            crate::LifecycleEventKind::ModelRequestFailed {
                error_type: crate::ModelErrorType::Provider,
                ..
            }
        ));
        assert!(matches!(
            events[7].kind(),
            crate::LifecycleEventKind::ModelRequestStarted {
                request_index: 1,
                ..
            }
        ));
        assert!(matches!(
            events[8].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted {
                response_type: crate::ModelResponseType::Output,
                ..
            }
        ));
    }

    #[test]
    fn resume_failure_preserves_request_context_and_http_status() {
        // Arrange
        let failures = [
            ResumeFailure::Native {
                source: request_error_with_http_status(429),
            },
            ResumeFailure::Replay {
                source: request_error_with_http_status(503),
            },
        ];

        // Act
        let errors = failures.map(ResumeFailure::into_model_error);

        // Assert
        assert_eq!(errors[0].http_status(), Some(429));
        assert_eq!(errors[1].http_status(), Some(503));
        assert!(
            errors[0]
                .to_string()
                .starts_with("model request failed: native provider continuation failed:")
        );
        assert!(errors[1].to_string().starts_with(
            "model request failed: native provider continuation was unavailable and history \
             replay failed:"
        ));
    }

    #[tokio::test]
    async fn session_preserves_structured_failure_after_native_resume_fallback() {
        // Arrange
        let mut model = model();
        let mut sequence = Sequence::new();
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "first"
                })))
                .with_provider_session_id("native-session"))
            });
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id() == Some("native-session"))
            .returning(|_| Err(ModelError::ResumeUnavailable));
        model
            .expect_complete()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|request| request.provider_session_id().is_none())
            .returning(|_| Err(ModelError::ResponseBodyTooLarge));
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(model).database(directory.path().join("harness.db"));
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        session
            .send("first")
            .await
            .expect("first turn should succeed");

        // Act
        let error = session
            .send("second")
            .await
            .expect_err("history replay should fail");

        // Assert
        assert!(matches!(
            &error,
            SessionError::Turn(TurnError::Model(ModelError::ResponseBodyTooLarge))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_a_provider_request_durably_interrupts_the_session_turn() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let request_started = Arc::new(Notify::new());
        let harness = Arc::new(
            Harness::new(PendingModel {
                started: Arc::clone(&request_started),
            })
            .database(&database_path),
        );
        let session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        drop(session);
        let turn = tokio::spawn(send_with_resumed_session(
            Arc::clone(&harness),
            "pending provider request",
        ));
        wait_for_fixture(&request_started, "the provider request").await;
        let database = Database::open(&database_path)
            .await
            .expect("database should reopen");
        let expected_state = ("interrupted".to_string(), Some("cancelled".to_string()));

        // Act
        let cancellation = async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            turn.abort();

            turn.await.expect_err("turn should be cancelled")
        };
        let (state, cancellation) = tokio::join!(
            wait_for_stored_turn_state(&database, &expected_state),
            cancellation
        );

        // Assert
        assert!(cancellation.is_cancelled());
        assert_eq!(state, expected_state);
    }

    #[tokio::test]
    async fn pending_tool_file_system_rejects_replace_requests() {
        // Arrange
        let file_system = PendingToolFileSystem {
            started: Arc::new(Notify::new()),
        };

        // Act
        let error = file_system
            .replace_beneath(Path::new("repo"), Path::new("Cargo.toml"), None, Vec::new())
            .await
            .expect_err("pending read fixture should reject replacement");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_a_tool_request_durably_interrupts_the_session_turn() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let tool_started = Arc::new(Notify::new());
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("pending-read"),
            )))
        });
        let harness = Arc::new(
            Harness::new(model)
                .database(&database_path)
                .repository(Repository::fixture("repo"))
                .allow(Tool::Read)
                .file_system(PendingToolFileSystem {
                    started: Arc::clone(&tool_started),
                }),
        );
        let session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        drop(session);
        let turn = tokio::spawn(send_with_resumed_session(
            Arc::clone(&harness),
            "pending tool request",
        ));
        wait_for_fixture(&tool_started, "the tool request").await;

        // Act
        turn.abort();
        let cancellation = turn.await.expect_err("turn should be cancelled");
        let database = Database::open(&database_path)
            .await
            .expect("database should reopen");
        let expected_state = ("interrupted".to_string(), Some("cancelled".to_string()));
        let state = wait_for_stored_turn_state(&database, &expected_state).await;

        // Assert
        assert!(cancellation.is_cancelled());
        assert_eq!(state, expected_state);
    }

    #[tokio::test]
    async fn active_session_turn_renews_its_lease_during_a_long_model_request() {
        // Arrange
        let timestamp_origin = 10;
        let clock_origin = tokio::time::Instant::now();
        let timestamp_source: Arc<dyn crate::session::TimestampSource> =
            Arc::new(move || elapsed_timestamp(timestamp_origin, clock_origin));
        let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
            .await
            .expect("database should open");
        database
            .create_session(
                &NewSession::new("session-a", object_schema()),
                None,
                DEFAULT_MAX_HISTORY_BYTES,
            )
            .await
            .expect("session should be created");
        let first_started = Arc::new(Notify::new());
        let first_release = Arc::new(Notify::new());
        let harness = Harness::new(LeaseExpiryModel {
            call_count: AtomicUsize::new(0),
            release_first: Arc::clone(&first_release),
            started_first: Arc::clone(&first_started),
        });
        let mut first = Session {
            database: database.clone(),
            harness: &harness,
            history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
            id: "session-a".to_string(),
            provider_session_id: None,
            schema: object_schema(),
            system_prompt: None,
        };
        let mut second = Session {
            database,
            harness: &harness,
            history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
            id: "session-a".to_string(),
            provider_session_id: None,
            schema: object_schema(),
            system_prompt: None,
        };

        // Act
        let (first_result, second_result) = tokio::join!(first.send("first"), async {
            wait_for_fixture(&first_started, "the long model request").await;
            tokio::task::yield_now().await;
            let original_expiry = stored_lease_expiry(&second.database).await;
            let lease_duration = Duration::from_secs(
                u64::try_from(crate::session::TURN_LEASE_SECONDS)
                    .expect("turn lease duration should be positive"),
            );
            let renewal_interval =
                Duration::from_secs(crate::session::TURN_LEASE_RENEWAL_INTERVAL_SECONDS);
            assert!(renewal_interval < lease_duration);
            tokio::time::pause();
            tokio::time::advance(renewal_interval).await;
            tokio::time::resume();
            let renewal_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
            assert!(renewal_timestamp < original_expiry);
            let first_renewed_expiry =
                wait_for_lease_extension(&second.database, original_expiry).await;
            let first_renewed_expiry =
                first_renewed_expiry.expect("active turn should renew its lease before expiry");
            tokio::time::pause();
            tokio::time::advance(renewal_interval).await;
            tokio::time::resume();
            let next_renewal_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
            assert!(next_renewal_timestamp < first_renewed_expiry);
            let next_renewed_expiry =
                wait_for_lease_extension(&second.database, first_renewed_expiry).await;
            let next_renewed_expiry = next_renewed_expiry
                .expect("active turn should keep renewing its lease during a long request");
            let current_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
            let recovery_advance = Duration::from_secs(
                u64::try_from(
                    first_renewed_expiry
                        .saturating_add(1)
                        .saturating_sub(current_timestamp),
                )
                .expect("first renewed lease should expire in the future"),
            );
            tokio::time::pause();
            tokio::time::advance(recovery_advance).await;
            tokio::time::resume();
            let recovery_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
            assert!(recovery_timestamp > first_renewed_expiry);
            assert!(next_renewed_expiry > recovery_timestamp);
            let result = second.send("second").await;
            first_release.notify_one();

            result
        });

        // Assert
        first_result.expect("the lease owner should complete its turn");
        assert!(matches!(second_result, Err(SessionError::Busy { .. })));
    }

    #[tokio::test]
    async fn recovered_lease_cancels_the_original_model_request() {
        // Arrange
        let now = Arc::new(AtomicI64::new(10));
        let timestamp_source: Arc<dyn crate::session::TimestampSource> = {
            let now = Arc::clone(&now);

            Arc::new(move || now.load(Ordering::SeqCst))
        };
        let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
            .await
            .expect("database should open");
        database
            .create_session(
                &NewSession::new("session-a", object_schema()),
                None,
                DEFAULT_MAX_HISTORY_BYTES,
            )
            .await
            .expect("session should be created");
        let first_started = Arc::new(Notify::new());
        let first_dropped = Arc::new(Notify::new());
        let harness = Harness::new(LeaseOwnershipModel {
            call_count: AtomicUsize::new(0),
            dropped_first: Arc::clone(&first_dropped),
            started_first: Arc::clone(&first_started),
        });
        let mut first = Session {
            database: database.clone(),
            harness: &harness,
            history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
            id: "session-a".to_string(),
            provider_session_id: None,
            schema: object_schema(),
            system_prompt: None,
        };
        let mut second = Session {
            database,
            harness: &harness,
            history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
            id: "session-a".to_string(),
            provider_session_id: None,
            schema: object_schema(),
            system_prompt: None,
        };

        // Act
        let (first_result, second_result) = tokio::join!(first.send("first"), async {
            wait_for_fixture(&first_started, "the original model request").await;
            let original_expiry = stored_lease_expiry(&second.database).await;
            now.store(original_expiry.saturating_add(1), Ordering::SeqCst);
            let result = second.send("second").await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(
                crate::session::TURN_LEASE_RENEWAL_INTERVAL_SECONDS,
            ))
            .await;
            tokio::time::resume();
            wait_for_fixture(&first_dropped, "the original model request cancellation").await;

            result
        });

        // Assert
        assert!(matches!(
            first_result,
            Err(SessionError::OwnershipLost {
                ref id,
                turn_position: 0,
            }) if id == "session-a"
        ));
        second_result.expect("the replacement turn should complete");
    }

    #[tokio::test]
    async fn lease_renewal_failure_cancels_the_model_request() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("database should open");
        database
            .create_session(
                &NewSession::new("session-a", object_schema()),
                None,
                DEFAULT_MAX_HISTORY_BYTES,
            )
            .await
            .expect("session should be created");
        sqlx::query(
            r"
CREATE TRIGGER reject_turn_lease_renewal
BEFORE UPDATE OF lease_expires_at ON session_turn
WHEN OLD.status = 'running' AND NEW.status = 'running'
BEGIN
    SELECT RAISE(ABORT, 'injected lease renewal failure');
END
",
        )
        .execute(database.pool())
        .await
        .expect("renewal failure trigger should be created");
        let request_started = Arc::new(Notify::new());
        let request_dropped = Arc::new(Notify::new());
        let harness = Harness::new(LeaseOwnershipModel {
            call_count: AtomicUsize::new(0),
            dropped_first: Arc::clone(&request_dropped),
            started_first: Arc::clone(&request_started),
        });
        let mut session = Session {
            database: database.clone(),
            harness: &harness,
            history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
            id: "session-a".to_string(),
            provider_session_id: None,
            schema: object_schema(),
            system_prompt: None,
        };

        // Act
        let (result, ()) = tokio::join!(session.send("pending"), async {
            wait_for_fixture(&request_started, "the model request").await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(
                crate::session::TURN_LEASE_RENEWAL_INTERVAL_SECONDS,
            ))
            .await;
            tokio::time::resume();
            wait_for_fixture(&request_dropped, "the model request cancellation").await;
        });
        let expected_state = ("interrupted".to_string(), Some("interrupted".to_string()));
        let state = wait_for_stored_turn_state(&database, &expected_state).await;

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::QueryContext {
                operation: "renew persistent session turn lease",
                ..
            })
        ));
        assert_eq!(state, expected_state);
    }

    #[tokio::test]
    async fn completion_persistence_failure_interrupts_the_session_turn() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "done"
            }))))
        });
        let harness = Arc::new(Harness::new(model).database(&database_path));
        let session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        drop(session);
        let database = harness.open_database().await.expect("database should open");
        sqlx::query(
            r"
CREATE TRIGGER reject_turn_completion
BEFORE UPDATE OF status ON session_turn
WHEN NEW.status = 'completed'
BEGIN
    SELECT RAISE(ABORT, 'injected completion persistence failure');
END
",
        )
        .execute(database.pool())
        .await
        .expect("completion failure trigger should be created");

        // Act
        let error = send_with_resumed_session(Arc::clone(&harness), "complete").await;
        let database = Database::open(&database_path)
            .await
            .expect("database should reopen");
        let expected_state = ("interrupted".to_string(), Some("interrupted".to_string()));
        let state = wait_for_stored_turn_state(&database, &expected_state).await;

        // Assert
        assert!(
            error
                .expect_err("completion persistence should fail")
                .to_string()
                .contains("injected completion persistence failure")
        );
        assert_eq!(state, expected_state);
    }

    #[tokio::test]
    async fn session_error_preserves_model_and_failure_persistence_errors() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let database_path = directory.path().join("harness.db");
        let mut model = model();
        model
            .expect_complete()
            .times(1)
            .returning(|_| Err(ModelError::InvalidResponse));
        let harness = Harness::new(model).database(&database_path);
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        sqlx::query(
            r"
CREATE TRIGGER reject_turn_failure
BEFORE UPDATE OF status ON session_turn
WHEN NEW.status = 'failed'
BEGIN
    SELECT RAISE(ABORT, 'injected persistence failure');
END
",
        )
        .execute(session.database.pool())
        .await
        .expect("failure trigger should be created");

        // Act
        let error = session
            .send("fail twice")
            .await
            .expect_err("model and persistence failures should be returned");

        // Assert
        assert!(matches!(&error, SessionError::TurnPersistence { .. }));
        if let SessionError::TurnPersistence { turn, persistence } = error {
            assert!(matches!(
                turn,
                TurnError::Model(ModelError::InvalidResponse)
            ));
            assert!(
                persistence
                    .to_string()
                    .contains("injected persistence failure")
            );
        }
    }

    #[tokio::test]
    async fn session_rejects_overlapping_turns_for_the_same_id() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(SlowModel).database(directory.path().join("harness.db"));
        let mut first = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session should be created");
        let mut second = harness
            .resume("session-a")
            .await
            .expect("session should resume");

        // Act
        let (first_result, second_result) =
            tokio::join!(first.send("first"), second.send("second"));

        // Assert
        let results = [first_result, second_result];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(SessionError::Busy { .. })))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn different_sessions_run_concurrently() {
        // Arrange
        let directory = tempdir().expect("temporary directory should be created");
        let harness = Harness::new(SlowModel).database(directory.path().join("harness.db"));
        let mut first = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("first session should be created");
        let mut second = harness
            .session("session-b", object_schema())
            .create()
            .await
            .expect("second session should be created");

        // Act
        let (first_result, second_result) =
            tokio::join!(first.send("first"), second.send("second"));

        // Assert
        assert!(first_result.is_ok());
        assert!(second_result.is_ok());
    }

    #[tokio::test]
    async fn rejects_schema_invalid_output_from_injected_model() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": 42
            }))))
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness = Harness::new(model).with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("schema-invalid custom output should fail");

        // Assert
        assert!(matches!(
            error,
            TurnError::Model(ModelError::SchemaViolation { path, .. }) if path == "/summary"
        ));
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert_eq!(events.len(), 4);
        assert!(events.iter().any(|event| matches!(
            event.kind(),
            crate::LifecycleEventKind::ModelRequestFailed {
                error_type: crate::ModelErrorType::InvalidOutput,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event.kind(),
            crate::LifecycleEventKind::TurnFailed {
                error_type: TurnErrorType::Model(crate::ModelErrorType::InvalidOutput),
                ..
            }
        )));
    }

    #[tokio::test]
    async fn report_describes_model_requests_and_repository_reads() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |_| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call_with_path("call_read", "Cargo\n.toml\u{1b}"),
                )));
            }

            Ok(response_with_metadata(ModelResponse::Output(json!({
                "summary": "workspace"
            }))))
        });
        let harness = read_harness(model, readable_file_system());

        // Act
        let outcome = harness
            .run_once("inspect", object_schema())
            .await
            .expect("reported turn should succeed");

        // Assert
        assert_eq!(outcome.output(), &json!({"summary": "workspace"}));
        assert_eq!(outcome.report().model_requests().len(), 2);
        let final_request = &outcome.report().model_requests()[1];
        assert_eq!(
            final_request.response_type(),
            crate::ModelResponseType::Output
        );
        let metadata = final_request
            .completion()
            .expect("metadata should be present");
        assert_eq!(metadata.finish_reason(), "stop\u{fffd}forged");
        assert_eq!(metadata.response_id(), Some("response\u{fffd}-1"));
        assert_eq!(metadata.response_model(), Some("reported\u{fffd}model"));
        assert_eq!(metadata.system_fingerprint(), Some("finger\u{fffd}print"));
        assert_eq!(
            metadata.usage().and_then(|usage| usage.total_tokens()),
            Some(16)
        );
        assert_eq!(outcome.report().tool_calls().len(), 1);
        let activity = &outcome.report().tool_calls()[0];
        assert_eq!(activity.name(), "read");
        assert_eq!(activity.path(), "Cargo\u{fffd}.toml\u{fffd}");
        assert!(activity.duration() <= outcome.report().duration());
    }

    #[test]
    fn write_activity_exposes_only_sanitized_summary() {
        // Arrange
        let activity = ToolActivity::Write {
            bytes_written: 5,
            duration: Duration::from_millis(3),
            path: "src/lib.rs".to_string(),
        };

        // Act and Assert
        assert_eq!(activity.name(), "write");
        assert_eq!(activity.path(), "src/lib.rs");
        assert_eq!(activity.duration(), Duration::from_millis(3));

        let rejected = ToolActivity::WriteRejected {
            duration: Duration::from_millis(2),
            path: "src/rejected.rs".to_string(),
        };
        assert_eq!(rejected.name(), "write");
        assert_eq!(rejected.path(), "src/rejected.rs");
        assert_eq!(rejected.duration(), Duration::from_millis(2));
    }

    #[tokio::test]
    async fn emits_correlated_lifecycle_for_read_tool_round_trip() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            assert!(request.lifecycle_observed());
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("provider-call-id"),
                )));
            }

            Ok(response_without_metadata(ModelResponse::Output(
                json!({ "summary": "workspace" }),
            )))
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness =
            read_harness(model, readable_file_system()).with_lifecycle_observer(move |event| {
                observed_events
                    .lock()
                    .expect("event recorder should not be poisoned")
                    .push(event);
            });

        // Act
        let output = harness
            .run_once("sensitive prompt", object_schema())
            .await
            .expect("tool round trip should succeed");

        // Assert
        assert_eq!(output.output(), &json!({ "summary": "workspace" }));
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert_eq!(events.len(), 9);
        assert_eq!(
            events
                .iter()
                .map(crate::LifecycleEvent::sequence)
                .collect::<Vec<_>>(),
            (0..9).collect::<Vec<_>>()
        );
        assert_read_tool_lifecycle(&events);
        let event_debug = format!("{events:?}");
        assert!(!event_debug.contains("sensitive prompt"));
        assert!(!event_debug.contains("[workspace]"));
    }

    #[tokio::test]
    async fn completes_write_tool_round_trip() {
        // Arrange
        let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let call_index = call_count.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                assert_eq!(request.tools(), &[ToolDefinition::write()]);

                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    write_call("call_write", patch),
                )));
            }
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult {
                    call_id,
                    content,
                    name,
                }
                    if call_id == "call_write"
                        && name == "write"
                        && serde_json::from_str::<Value>(content).is_ok_and(|value| {
                            value == json!({
                                "bytes_written": 4,
                                "path": "src/lib.rs",
                                "status": "applied"
                            })
                        })
            ));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "updated"
            }))))
        });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
        file_system
            .expect_replace_beneath()
            .times(1)
            .withf(|_, _, expected, content| {
                expected.as_deref() == Some(b"old\n".as_slice()) && content == b"new\n"
            })
            .returning(|_, _, _, _| Ok(()));
        let harness = write_harness(model, file_system);

        // Act
        let output = harness
            .run_once("update the file", object_schema())
            .await
            .expect("write round trip should succeed");

        // Assert
        assert_eq!(output.output(), &json!({ "summary": "updated" }));
    }

    #[tokio::test]
    async fn returns_correctable_write_rejection_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCall(
                    write_call("call_write", "not a unified diff"),
                )));
            }
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult { content, .. }
                    if serde_json::from_str::<Value>(content)
                        .is_ok_and(|value| value["status"] == "rejected")
            ));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
        file_system.expect_replace_beneath().times(0);
        let harness = write_harness(model, file_system);

        // Act
        let output = harness
            .run_once("update", object_schema())
            .await
            .expect("model should recover from rejected patch");

        // Assert
        assert_eq!(output.output(), &json!({ "summary": "recovered" }));
    }

    #[tokio::test]
    async fn returns_terminal_write_boundary_failure() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call(
                    "call_write",
                    "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                ),
            )))
        });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
        let harness = write_harness(model, file_system);

        // Act
        let error = harness
            .run_once("update", object_schema())
            .await
            .expect_err("write boundary failure should end turn");

        // Assert
        assert!(matches!(&error, TurnError::Write(_)));
        assert_eq!(error.error_type(), TurnErrorType::Tool);
    }

    #[tokio::test]
    async fn rejects_disabled_write_call() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|request| {
            assert_eq!(request.tools(), []);

            Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call(
                    "call_denied",
                    "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                ),
            )))
        });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        file_system.expect_replace_beneath().times(0);
        let harness = Harness::new(model);

        // Act
        let error = harness
            .run_once("update", object_schema())
            .await
            .expect_err("denied write should fail");

        // Assert
        assert!(matches!(
            error,
            TurnError::ToolDenied { name } if name == "write"
        ));
    }

    #[tokio::test]
    async fn rejects_disabled_read_call() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|request| {
            assert_eq!(request.tools(), []);

            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_denied"),
            )))
        });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model).with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("denied tool should fail");

        // Assert
        assert!(matches!(
            &error,
            TurnError::ToolDenied { name } if name == "read"
        ));
        assert_eq!(error.error_type(), TurnErrorType::ToolDenied);
    }

    #[tokio::test]
    async fn enforces_tool_call_limit() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(2).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )))
        });
        let harness = read_harness(model, readable_file_system())
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"))
            .with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("second tool call should exceed the limit");

        // Assert
        assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
        assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
    }

    #[tokio::test]
    async fn enforces_tool_call_limit_within_one_model_response() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                read_call("call_one"),
                read_call("call_two"),
            ])))
        });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = read_harness(model, file_system)
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("second batched tool call should exceed the limit");

        // Assert
        assert!(matches!(error, TurnError::ToolCallLimit { limit: 1 }));
    }

    #[tokio::test]
    async fn rejects_batched_writes_before_any_write_executes() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                write_call(
                    "call_one",
                    "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
                ),
                write_call(
                    "call_two",
                    "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
                ),
            ])))
        });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        file_system.expect_replace_beneath().times(0);
        let harness = write_harness(model, file_system)
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

        // Act
        let error = harness
            .run_once("update twice", object_schema())
            .await
            .expect_err("oversized batch should fail before writing");

        // Assert
        assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
        assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
    }

    #[tokio::test]
    async fn rejects_duplicate_batched_call_ids_before_any_write_executes() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                write_call(
                    "duplicate_call",
                    "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
                ),
                write_call(
                    "duplicate_call",
                    "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
                ),
            ])))
        });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        file_system.expect_replace_beneath().times(0);
        let harness = write_harness(model, file_system);

        // Act
        let error = harness
            .run_once("update twice", object_schema())
            .await
            .expect_err("duplicate call identifiers should fail before writing");

        // Assert
        assert!(matches!(
            &error,
            TurnError::Model(ModelError::DuplicateToolCallId { id }) if id == "duplicate_call"
        ));
        assert_eq!(
            error.error_type(),
            TurnErrorType::Model(crate::ModelErrorType::InvalidToolCall)
        );
    }

    #[tokio::test]
    async fn rejects_empty_tool_call_batch() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCalls(
                Vec::new(),
            )))
        });
        let harness = Harness::new(model);

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("empty tool batch should fail immediately");

        // Assert
        assert!(matches!(
            &error,
            TurnError::Model(ModelError::MissingToolCall)
        ));
        assert_eq!(
            error.error_type(),
            TurnErrorType::Model(crate::ModelErrorType::InvalidToolCall)
        );
    }

    #[tokio::test]
    async fn returns_typed_read_failure() {
        // Arrange
        let mut model = model();
        model.expect_complete().times(1).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )))
        });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
        let harness = read_harness(model, file_system).with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("filesystem failure should end the turn");

        // Assert
        assert!(matches!(&error, TurnError::Read(_)));
        assert_eq!(error.error_type(), TurnErrorType::Tool);
    }

    #[tokio::test]
    async fn returns_typed_model_failure() {
        // Arrange
        let mut model = model();
        model
            .expect_complete()
            .times(1)
            .returning(|_| Err(ModelError::request(io::Error::other("offline"))));
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model).with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("model failure should end the turn");

        // Assert
        assert!(matches!(&error, TurnError::Model(ModelError::Request(_))));
        assert_eq!(
            error.error_type(),
            TurnErrorType::Model(crate::ModelErrorType::Request)
        );
    }
}
