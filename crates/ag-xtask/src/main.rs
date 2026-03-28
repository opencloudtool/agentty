mod check_migration;
mod workspace_map;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing::error;

/// Command-line entry point for workspace maintenance tasks.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Supported maintenance subcommands.
#[derive(Subcommand)]
enum Command {
    /// Validates SQL migration numbering across workspace crates.
    CheckMigrations,
    /// Writes the generated workspace map used by tooling and agents.
    WorkspaceMap,
}

/// Runs the selected maintenance command and returns the process exit code.
fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        None => check_migration::run(),
        Some(Command::WorkspaceMap) => workspace_map::run(),
        Some(Command::CheckMigrations) => check_migration::run(),
    };

    if let Err(err) = result {
        error!("{err}");

        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
