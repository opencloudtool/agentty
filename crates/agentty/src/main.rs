//! Agentty command-line entry point and terminal runtime bootstrap.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use ag_git::{GitClient, RealGitClient};
use agentty::app::{AGENTTY_WT_DIR, App, AppError, agentty_home};
use agentty::infra::db::{
    DB_DIR, DB_FILE, Database, timestamp_source_from_environment as environment_timestamp_source,
};
use clap::Parser;

/// Command-line options for launching Agentty.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Disables automatic application updates.
    #[arg(long)]
    no_update: bool,
}

/// Runs the `agentty` application runtime using the configured workspace and
/// database.
#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");

            ExitCode::FAILURE
        }
    }
}

/// Builds startup dependencies, then launches the `agentty` runtime.
///
/// # Errors
/// Returns an error if database startup, app construction, or runtime
/// execution fails.
async fn run(cli: Cli) -> Result<(), AppError> {
    let home = agentty_home();
    let base_path = home.join(AGENTTY_WT_DIR);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_client = RealGitClient;
    let git_branch = git_client.detect_git_info(working_dir.clone()).await;

    let db_path = home.join(DB_DIR).join(DB_FILE);
    let db = Database::open_with_timestamp_source(&db_path, environment_timestamp_source()).await?;

    let mut app = App::new(!cli.no_update, base_path, working_dir, git_branch, db).await?;

    agentty::runtime::run(&mut app)
        .await
        .map_err(|error| AppError::Workflow(format!("Failed to run terminal UI: {error}")))
}

#[cfg(test)]
mod tests {
    use std::env;

    use clap::error::ErrorKind;

    use super::*;

    const DATABASE_FAILURE_CHILD_ENV: &str = "AGENTTY_DATABASE_FAILURE_CHILD";

    #[test]
    fn cli_defaults_to_automatic_updates() {
        // Arrange / Act
        let cli = Cli::try_parse_from(["agentty"]).expect("default arguments should parse");

        // Assert
        assert!(!cli.no_update);
    }

    #[test]
    fn cli_parses_no_update_flag() {
        // Arrange / Act
        let cli =
            Cli::try_parse_from(["agentty", "--no-update"]).expect("--no-update should parse");

        // Assert
        assert!(cli.no_update);
    }

    #[test]
    fn cli_rejects_unknown_arguments() {
        // Arrange / Act
        let error = Cli::try_parse_from(["agentty", "--no-updte"])
            .expect_err("unknown arguments should fail");

        // Assert
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }

    #[tokio::test]
    async fn run_reports_database_parent_creation_failure() {
        if env::var_os(DATABASE_FAILURE_CHILD_ENV).is_some() {
            // Arrange
            let cli = Cli { no_update: false };

            // Act
            let error = run(cli)
                .await
                .expect_err("database startup should reject a file-backed root");

            // Assert
            assert!(matches!(error, AppError::Db(_)));

            return;
        }

        // Arrange
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let blocking_root = temp_dir.path().join("agentty-root");
        tokio::fs::write(&blocking_root, b"")
            .await
            .expect("blocking root file should be created");
        let test_binary = env::current_exe().expect("test binary path should resolve");

        // Act
        let status = tokio::process::Command::new(test_binary)
            .arg("--exact")
            .arg("tests::run_reports_database_parent_creation_failure")
            .env(DATABASE_FAILURE_CHILD_ENV, "1")
            .env("AGENTTY_ROOT", blocking_root)
            .status()
            .await
            .expect("isolated startup test should run");

        // Assert
        assert!(status.success());
    }
}
