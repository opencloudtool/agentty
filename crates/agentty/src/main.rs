use std::io;
use std::path::PathBuf;

use agentty::app::{AGENTTY_WT_DIR, App, agentty_home};
use agentty::db::{DB_DIR, DB_FILE, Database};

#[tokio::main]
async fn main() -> io::Result<()> {
    let home = agentty_home();
    let base_path = home.join(AGENTTY_WT_DIR);
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let git_branch = agentty::git::detect_git_info(&working_dir);
    let lock_path = home.join("lock");
    let _lock = agentty::lock::acquire_lock(&lock_path)
        .map_err(|error| io::Error::other(format!("Error: {error}")))?;

    let db_path = home.join(DB_DIR).join(DB_FILE);
    let db = Database::open(&db_path).await.map_err(io::Error::other)?;

    let mut app = App::new(base_path, working_dir, git_branch, db).await;

    agentty::runtime::run(&mut app).await
}
