//! Interactive command-line chat powered by the `ag-harness` model runtime.

use std::borrow::Cow;
use std::ffi::OsStr;
#[cfg(not(test))]
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, io};

use ag_harness::{
    Harness, ModelConfiguration, ModelConfigurationError, ModelProvider, OutputSchema,
    ReasoningEffort, Repository, Session, SessionInfo, Tool, TurnOutcome,
};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};

const READ_ONLY_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a read-only repository harness. The read tool supports file, list, ",
    "search, diff, and show actions. For change review, call diff first, then use search, file, ",
    "list, or show for evidence. Use repository tools only when the user explicitly asks about ",
    "repository contents. Treat an unambiguous reference to the repository, project, codebase, ",
    "code, a file, or a change as an explicit repository request. Treat replies, response speed, ",
    "response visibility, and model behavior as casual chat topics only when they are not ",
    "explicitly tied to the repository, project, codebase, code, a file, or a change. ",
    "Do not call tools for casual conversation or ambiguous requests. When needed, call the tool ",
    "immediately and use its result before answering. ",
    "Never narrate, promise, or defer a future tool call. Never claim that you created, ",
    "modified, deleted, or executed files or commands because filesystem mutation and command ",
    "execution are unavailable. If asked to perform an unsupported action, state that it is ",
    "unsupported."
);
const READ_WRITE_SYSTEM_PROMPT: &str = concat!(
    "You are operating in a repository harness with read and write tools. The read tool supports ",
    "file, list, search, diff, and show actions. For change review, call diff first. When a user ",
    "explicitly asks about repository contents, call read immediately and use its result before ",
    "answering. Treat an unambiguous reference to the repository, project, codebase, code, a \
     file, ",
    "or a change as an explicit repository request. Treat replies, response speed, response ",
    "visibility, and model behavior as casual chat topics only when they are not explicitly tied ",
    "to the repository, project, codebase, code, a file, or a change. Do not ",
    "call tools for casual conversation or ambiguous requests. ",
    "When a user asks to create or modify a file, call the write tool ",
    "immediately in the same response. Never narrate, promise, or defer a future tool call. Only ",
    "claim that a file was created or modified after the write tool succeeds. File deletion and ",
    "command execution are unavailable."
);

/// Chats with models through a bounded repository harness.
#[derive(Debug, Parser)]
#[command(
    name = "ag-harness",
    version,
    about = "Chats with models through a repository harness",
    after_help = provider_help()
)]
struct Cli {
    /// SQLite database used for durable session history.
    #[arg(long, global = true, value_name = "FILE")]
    database: Option<PathBuf>,
    /// Absolute Git executable override; defaults to the first valid Git found
    /// in PATH.
    #[arg(long, global = true, value_name = "FILE")]
    git_executable: Option<PathBuf>,
    /// Model reasoning depth used for chat requests.
    #[arg(
        long,
        global = true,
        default_value = "low",
        value_parser = reasoning_effort_parser()
    )]
    reasoning_effort: ReasoningEffort,
    #[command(subcommand)]
    command: Command,
}

/// Supported harness commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Starts a new durable session with a model.
    Run(RunArgs),
    /// Resumes a durable session.
    Resume(ResumeArgs),
}

/// Arguments for a new durable model session.
#[derive(Debug, Args)]
#[command(after_help = provider_help())]
struct RunArgs {
    /// Model identifier sent to the provider.
    model: String,
    /// Optional first prompt. Further prompts are read from standard input.
    #[arg(value_parser = parse_prompt)]
    prompt: Option<String>,
    /// API base URL, overriding the provider-specific environment variable.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Enables repository writes through the write tool.
    #[arg(long)]
    allow_write: bool,
    /// Model provider.
    #[arg(
        long,
        default_value_t = ModelProvider::Muse,
        value_parser = model_provider_parser()
    )]
    provider: ModelProvider,
    /// Repository directory available to enabled tools.
    #[arg(long, value_name = "DIR", default_value = ".")]
    read_dir: PathBuf,
    /// Stable session identifier. A random identifier is generated by default.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
}

/// Arguments for resuming a durable model session.
#[derive(Debug, Args)]
struct ResumeArgs {
    /// Session identifier printed by `ag-harness run`.
    session: String,
    /// Optional first prompt. Further prompts are read from standard input.
    #[arg(value_parser = parse_prompt)]
    prompt: Option<String>,
    /// API base URL, overriding the provider-specific environment variable.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Enables repository writes through the write tool.
    #[arg(long)]
    allow_write: bool,
    /// Repository directory available to enabled tools.
    #[arg(long, value_name = "DIR", default_value = ".")]
    read_dir: PathBuf,
}

fn model_provider_parser() -> impl TypedValueParser<Value = ModelProvider> {
    PossibleValuesParser::new(
        ModelProvider::all()
            .iter()
            .map(|provider| provider.as_str()),
    )
    .try_map(|provider| provider.parse::<ModelProvider>())
}

fn reasoning_effort_parser() -> impl TypedValueParser<Value = ReasoningEffort> {
    PossibleValuesParser::new(ReasoningEffort::ALL.iter().map(|effort| effort.as_str())).try_map(
        |effort| {
            ReasoningEffort::ALL
                .iter()
                .copied()
                .find(|candidate| candidate.as_str() == effort)
                .ok_or_else(|| format!("unsupported reasoning effort `{effort}`"))
        },
    )
}

fn provider_help() -> String {
    let mut help =
        String::from("Supported models (other endpoint-supported model IDs also work):\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        help.push_str(&provider.known_models().join(", "));
        help.push('\n');
    }
    help.push_str("\nCredentials:\n");
    for provider in ModelProvider::all() {
        help.push_str("  ");
        help.push_str(provider.as_str());
        help.push_str(": ");
        help.push_str(provider.api_key_environment());
        if provider.default_base_url().is_some() {
            help.push_str(" (");
            help.push_str(provider.base_url_environment());
            help.push_str(" optional)");
        } else {
            help.push_str(", ");
            help.push_str(provider.base_url_environment());
        }
        help.push('\n');
    }
    help.pop();

    help
}

fn parse_prompt(prompt: &str) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err("prompt must contain a non-whitespace character".to_string());
    }

    Ok(prompt.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatMode {
    Interactive,
    NonInteractive,
    OneShot,
}

impl ChatMode {
    fn detect(cli: &Cli, stdin_is_terminal: bool, stdout_is_terminal: bool) -> Self {
        if stdin_is_terminal && stdout_is_terminal {
            return Self::Interactive;
        }
        let initial_prompt = match &cli.command {
            Command::Run(args) => &args.prompt,
            Command::Resume(args) => &args.prompt,
        };
        if stdin_is_terminal && initial_prompt.is_some() {
            return Self::OneShot;
        }

        Self::NonInteractive
    }
}

#[cfg(not(test))]
#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let mode = ChatMode::detect(&cli, stdin_is_terminal, stdout_is_terminal);
    let input = BufReader::new(tokio::io::stdin());
    let output = tokio::io::stdout();

    report_exit(
        execute(cli, |name| env::var(name), input, output, mode).await,
        io::stderr().lock(),
    )
}

fn report_exit(result: Result<(), CliError>, mut error_output: impl io::Write) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let error = error.to_string();
            let error = single_line_terminal_text(&error);
            let _ = writeln!(error_output, "{error}");

            ExitCode::FAILURE
        }
    }
}

async fn execute<Input, Output>(
    cli: Cli,
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    input: Input,
    output: Output,
    mode: ChatMode,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    let database = database_path(cli.database, &mut environment)?;
    let git_executable = cli.git_executable;
    let reasoning_effort = cli.reasoning_effort;
    match cli.command {
        Command::Run(args) => {
            let session_id = args
                .session
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
            let client = model_client(
                args.provider,
                &args.model,
                args.base_url.as_deref(),
                &mut environment,
            )?;
            let repository = repository_or_default(args.read_dir, git_executable)?;
            let (harness, system_prompt) = configured_harness(
                client,
                database,
                repository,
                args.allow_write,
                reasoning_effort,
            );
            let mut session = harness
                .session(&session_id, chat_schema()?)
                .system_prompt(system_prompt)
                .create()
                .await?;
            let mut output = output;
            announce_session(&mut output, &session_id).await?;

            run_chat(&mut session, &args.model, args.prompt, input, output, mode).await
        }
        Command::Resume(args) => {
            let info = SessionInfo::load(&database, &args.session).await?;
            let (provider, model) = stored_model_identity(&info)?;
            let client =
                model_client(provider, &model, args.base_url.as_deref(), &mut environment)?;
            let repository = repository_or_default(args.read_dir, git_executable)?;
            let (harness, _) = configured_harness(
                client,
                database,
                repository,
                args.allow_write,
                reasoning_effort,
            );
            let mut session = harness.resume(&args.session).await?;
            let mut output = output;
            announce_session(&mut output, &args.session).await?;

            run_chat(&mut session, &model, args.prompt, input, output, mode).await
        }
    }
}

async fn announce_session(
    output: &mut (impl AsyncWrite + Unpin),
    session_id: &str,
) -> Result<(), io::Error> {
    let session_id = single_line_terminal_text(session_id);
    output
        .write_all(format!("session: {session_id}\n").as_bytes())
        .await?;
    output.flush().await
}

fn database_path(
    explicit: Option<PathBuf>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Ok(root) = environment("AG_HARNESS_ROOT")
        && !root.trim().is_empty()
    {
        return Ok(PathBuf::from(root).join("db").join("harness.db"));
    }

    let home = environment("HOME").map_err(|_| CliError::DatabaseLocation)?;
    if home.trim().is_empty() {
        return Err(CliError::DatabaseLocation);
    }

    Ok(PathBuf::from(home).join(".ag-harness/db/harness.db"))
}

fn repository_or_default(root: PathBuf, explicit: Option<PathBuf>) -> Result<Repository, CliError> {
    if let Some(explicit) = explicit {
        return Repository::new(root, explicit).map_err(CliError::from);
    }
    let path = env::var_os("PATH");

    repository_from_path(&root, path.as_deref())
}

fn repository_from_path(root: &Path, path: Option<&OsStr>) -> Result<Repository, CliError> {
    let executable_name = format!("git{}", env::consts::EXE_SUFFIX);

    let candidates = path
        .iter()
        .flat_map(env::split_paths)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name));
    for candidate in candidates {
        match Repository::new(root, candidate) {
            Ok(repository) => return Ok(repository),
            Err(
                error @ (ag_harness::RepositoryError::Root { .. }
                | ag_harness::RepositoryError::RootIsGitAdministrative { .. }),
            ) => return Err(error.into()),
            Err(_) => {}
        }
    }

    Err(CliError::GitExecutableNotFound)
}

fn model_client(
    provider: ModelProvider,
    model: &str,
    base_url: Option<&str>,
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<ag_harness::ModelClient, CliError> {
    let mut configuration = ModelConfiguration::new(provider, model);
    if let Some(base_url) = base_url {
        configuration = configuration.base_url(base_url);
    }

    configuration
        .client_from_environment(environment)
        .map_err(CliError::from)
}

fn configured_harness(
    client: ag_harness::ModelClient,
    database: PathBuf,
    repository: Repository,
    allow_write: bool,
    reasoning_effort: ReasoningEffort,
) -> (Harness, &'static str) {
    let mut harness = Harness::new(client)
        .database(database)
        .model_reasoning_effort(reasoning_effort)
        .repository(repository)
        .allow(Tool::Read);
    if allow_write {
        harness = harness.allow(Tool::Write);

        (harness, READ_WRITE_SYSTEM_PROMPT)
    } else {
        (harness, READ_ONLY_SYSTEM_PROMPT)
    }
}

fn stored_model_identity(info: &SessionInfo) -> Result<(ModelProvider, String), CliError> {
    stored_model_identity_parts(info.provider(), info.model())
}

fn stored_model_identity_parts(
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(ModelProvider, String), CliError> {
    let model = model.ok_or(CliError::MissingModelIdentity)?.to_string();
    let provider = match provider {
        Some("meta") => ModelProvider::Muse,
        Some("moonshot_ai") => ModelProvider::Kimi,
        Some("alibaba_cloud") => ModelProvider::Qwen,
        _ => return Err(CliError::MissingModelIdentity),
    };

    Ok((provider, model))
}

async fn run_chat<Input, Output>(
    session: &mut Session<'_>,
    requested_model: &str,
    initial_prompt: Option<String>,
    mut input: Input,
    mut output: Output,
    mode: ChatMode,
) -> Result<(), CliError>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if mode == ChatMode::Interactive {
        let requested_model = single_line_terminal_text(requested_model);
        output
            .write_all(format!("Chat with {requested_model}. Ctrl-D to exit.\n").as_bytes())
            .await?;
    }

    let mut pending_prompt = initial_prompt;
    let mut turn_failed = false;
    loop {
        let Some(prompt) = read_prompt(&mut pending_prompt, &mut input, &mut output, mode).await?
        else {
            break;
        };
        if prompt.trim().is_empty() {
            continue;
        }
        match session.send(prompt).await {
            Ok(outcome) => write_outcome(&mut output, requested_model, &outcome).await?,
            Err(error) if mode == ChatMode::Interactive => {
                write_turn_error(&mut output, &error).await?;
            }
            Err(error) if mode == ChatMode::OneShot => return Err(error.into()),
            Err(error) => {
                write_turn_error(&mut output, &error).await?;
                turn_failed = true;
            }
        }
        if mode == ChatMode::OneShot {
            break;
        }
    }

    if turn_failed {
        Err(CliError::ChatTurnsFailed)
    } else {
        Ok(())
    }
}

async fn read_prompt<Input, Output>(
    pending_prompt: &mut Option<String>,
    input: &mut Input,
    output: &mut Output,
    mode: ChatMode,
) -> Result<Option<String>, io::Error>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    if let Some(prompt) = pending_prompt.take() {
        return Ok(Some(prompt));
    }
    if mode == ChatMode::Interactive {
        output.write_all(b">>> ").await?;
        output.flush().await?;
    }
    let mut prompt = String::new();
    if input.read_line(&mut prompt).await? == 0 {
        return Ok(None);
    }
    trim_line_ending(&mut prompt);

    Ok(Some(prompt))
}

async fn write_outcome(
    output: &mut (impl AsyncWrite + Unpin),
    requested_model: &str,
    outcome: &TurnOutcome,
) -> Result<(), CliError> {
    let message = outcome
        .output()
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or(CliError::MissingMessage)?;
    output.write_all(assistant_text(message).as_bytes()).await?;
    output.write_all(b"---\n").await?;
    output
        .write_all(format!("turn: {}\n", format_duration(outcome.report().duration())).as_bytes())
        .await?;
    output
        .write_all(format!("model calls: {}\n", outcome.report().model_requests().len()).as_bytes())
        .await?;
    for (index, request) in outcome.report().model_requests().iter().enumerate() {
        let response_type = request.response_type();
        let completion = request.completion();
        let model = completion
            .and_then(|metadata| metadata.response_model())
            .unwrap_or(requested_model);
        let finish_reason =
            completion.map_or("unavailable", ag_harness::CompletionMetadata::finish_reason);
        let model = single_line_terminal_text(model);
        let finish_reason = single_line_terminal_text(finish_reason);
        let usage = completion
            .and_then(|metadata| metadata.usage())
            .map_or_else(|| "tokens unavailable".to_string(), format_usage);
        output
            .write_all(
                format!(
                    "  {}. {response_type}; {model}; {finish_reason}; {}; {usage}\n",
                    index + 1,
                    format_duration(request.duration()),
                )
                .as_bytes(),
            )
            .await?;
    }
    if outcome.report().tool_calls().is_empty() {
        output.write_all(b"tools: none\n").await?;
    } else {
        output.write_all(b"tools:\n").await?;
        for activity in outcome.report().tool_calls() {
            output
                .write_all(format!("  {activity}\n").as_bytes())
                .await?;
        }
    }
    output.flush().await?;

    Ok(())
}

async fn write_turn_error(
    output: &mut (impl AsyncWrite + Unpin),
    error: &(impl std::fmt::Display + ?Sized),
) -> Result<(), io::Error> {
    let error = error.to_string();
    let error = single_line_terminal_text(&error);
    output
        .write_all(format!("error: {error}\n").as_bytes())
        .await?;
    output.flush().await
}

fn format_usage(usage: &ag_harness::CompletionUsage) -> String {
    let input = usage
        .input_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());
    let output = usage
        .output_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());
    let total = usage
        .total_tokens()
        .map_or_else(|| "?".to_string(), |tokens| tokens.to_string());

    format!("tokens {input} in, {output} out, {total} total")
}

fn format_duration(duration: std::time::Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

fn assistant_text(text: &str) -> String {
    let text = terminal_text(text);
    let mut framed = String::new();
    for (index, line) in text.split('\n').enumerate() {
        framed.push_str(if index == 0 {
            "assistant> "
        } else {
            "           "
        });
        framed.push_str(line);
        framed.push('\n');
    }

    framed
}

fn terminal_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_terminal_safe) {
        return Cow::Borrowed(text);
    }

    Cow::Owned(
        text.chars()
            .map(|character| {
                if is_terminal_safe(character) {
                    character
                } else {
                    '\u{fffd}'
                }
            })
            .collect(),
    )
}

fn single_line_terminal_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(|character| !character.is_control()) {
        return Cow::Borrowed(text);
    }

    Cow::Owned(
        text.chars()
            .map(|character| {
                if character.is_control() {
                    '\u{fffd}'
                } else {
                    character
                }
            })
            .collect(),
    )
}

fn is_terminal_safe(character: char) -> bool {
    !character.is_control() || matches!(character, '\n' | '\t')
}

fn chat_schema() -> Result<OutputSchema, CliError> {
    let message = Value::Object(Map::from_iter([(
        "type".to_string(),
        Value::String("string".to_string()),
    )]));
    let properties = Value::Object(Map::from_iter([("message".to_string(), message)]));
    let schema = Value::Object(Map::from_iter([
        ("type".to_string(), Value::String("object".to_string())),
        ("properties".to_string(), properties),
        (
            "required".to_string(),
            Value::Array(vec![Value::String("message".to_string())]),
        ),
        ("additionalProperties".to_string(), Value::Bool(false)),
    ]));

    OutputSchema::new(schema).map_err(CliError::from)
}

#[derive(Debug, Error)]
enum CliError {
    #[error("--base-url or {name} is required")]
    BaseUrlRequired { name: &'static str },
    #[error("one or more chat turns failed")]
    ChatTurnsFailed,
    #[error("--database, AG_HARNESS_ROOT, or HOME is required for durable session storage")]
    DatabaseLocation,
    #[error("No valid Git executable was found in PATH; pass --git-executable <FILE>")]
    GitExecutableNotFound,
    #[error("model output did not contain a message")]
    MissingMessage,
    #[error("stored session does not identify a supported built-in model")]
    MissingModelIdentity,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ModelConfiguration(ModelConfigurationError),
    #[error(transparent)]
    OutputSchema(#[from] ag_harness::OutputSchemaError),
    #[error(transparent)]
    Repository(#[from] ag_harness::RepositoryError),
    #[error(transparent)]
    Session(#[from] ag_harness::SessionError),
    #[error(transparent)]
    Turn(#[from] ag_harness::TurnError),
}

impl From<ModelConfigurationError> for CliError {
    fn from(error: ModelConfigurationError) -> Self {
        match error {
            ModelConfigurationError::BaseUrl { name } => Self::BaseUrlRequired { name },
            error => Self::ModelConfiguration(error),
        }
    }
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
