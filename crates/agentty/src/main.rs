use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use agentty::app::{AGENTTY_WT_DIR, App, agentty_home};
use agentty::infra::db::{DB_DIR, DB_FILE, Database};
use agentty::infra::git::{GitClient, RealGitClient};

/// Runs the `agentty` application runtime using the configured workspace and
/// database.
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
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
async fn run() -> Result<(), String> {
    let auto_update = !std::env::args().any(|arg| arg == "--no-update");

    let home = agentty_home();
    let base_path = home.join(AGENTTY_WT_DIR);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_client = RealGitClient;
    let git_branch = git_client.detect_git_info(working_dir.clone()).await;

    let db_path = home.join(DB_DIR).join(DB_FILE);
    let db = Database::open(&db_path).await?;

    let mut app = App::new(
        auto_update,
        base_path,
        working_dir,
        git_branch,
        db,
        std::sync::Arc::new(agentty::infra::app_server_router::RoutingAppServerClient::new()),
    )
    .await?;

    agentty::runtime::run(&mut app)
        .await
        .map_err(|error| format!("Failed to run terminal UI: {error}"))
}
