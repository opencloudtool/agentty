//! Agentty persistence facade and database-location policy.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

pub use ag_store::*;

use crate::infra::clock;

/// Subdirectory under the Agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default Agentty database filename.
pub const DB_FILE: &str = "agentty.db";

/// Acquires exclusive application ownership of one Agentty root.
///
/// Keep the returned file open until the application stops. The OS releases
/// the lock when the handle closes or the process exits; the lock file must
/// remain in place so concurrent starts always lock the same file.
/// Acquire this before opening the database or running startup recovery.
///
/// # Errors
/// Returns [`io::ErrorKind::WouldBlock`] when another instance owns this root,
/// or an I/O error when the lock directory or file cannot be opened or locked.
pub async fn acquire_instance_lock(root: &Path) -> io::Result<File> {
    let directory = root.join(DB_DIR);
    tokio::fs::create_dir_all(&directory).await?;
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("agentty.lock"))
        .await?
        .into_std()
        .await;
    file.try_lock()?;

    Ok(file)
}

/// Returns a store timestamp source backed by Agentty's environment-selected
/// clock.
///
/// Feature tests pin that clock so database ordering and activity timestamps
/// remain deterministic alongside rendered frame time.
pub fn timestamp_source_from_environment() -> Arc<dyn TimestampSource> {
    let clock = clock::from_environment();

    Arc::new(move || clock::unix_timestamp_seconds(clock.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn instance_lock_is_exclusive_per_root_and_reusable_after_drop() {
        // Arrange
        let first_root = tempfile::tempdir().expect("first root");
        let other_root = tempfile::tempdir().expect("other root");
        let first = acquire_instance_lock(first_root.path())
            .await
            .expect("first lock");

        // Act
        let error = acquire_instance_lock(first_root.path())
            .await
            .expect_err("root is owned");
        let other = acquire_instance_lock(other_root.path())
            .await
            .expect("independent root");
        drop(first);
        let restarted = acquire_instance_lock(first_root.path())
            .await
            .expect("released lock");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(other.metadata().expect("other lock metadata").is_file());
        assert!(
            restarted
                .metadata()
                .expect("restarted lock metadata")
                .is_file()
        );
    }

    #[tokio::test]
    async fn instance_lock_reports_directory_and_file_errors() {
        // Arrange
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join(DB_DIR), "blocked").expect("block directory");

        // Act / Assert
        assert!(acquire_instance_lock(root.path()).await.is_err());
        std::fs::remove_file(root.path().join(DB_DIR)).expect("remove blocker");
        std::fs::create_dir_all(root.path().join(DB_DIR).join("agentty.lock"))
            .expect("block lock file");
        assert!(acquire_instance_lock(root.path()).await.is_err());
    }

    #[tokio::test]
    async fn instance_lock_is_released_after_process_exit_or_kill() {
        const CHILD_ROOT: &str = "AGENTTY_LOCK_HOLDER_TEST_ROOT";

        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            // Arrange
            let root = std::path::PathBuf::from(root);
            let _owner = acquire_instance_lock(&root).await.expect("child lock");

            // Act / Assert: retain ownership until the parent requests exit
            // or kills this process without dropping the handle.
            while !root.join("stop").exists() {
                tokio::fs::write(root.join("ready"), b"")
                    .await
                    .expect("signal ownership");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            return;
        }

        // Normal exit also lets the child flush coverage for the shared holder.
        for terminate_abruptly in [false, true] {
            // Arrange
            let root = tempfile::tempdir().expect("root");
            let mut child =
                tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                    .arg("--exact")
                    .arg("infra::db::tests::instance_lock_is_released_after_process_exit_or_kill")
                    .env(CHILD_ROOT, root.path())
                    .kill_on_drop(true)
                    .spawn()
                    .expect("lock holder process");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while !root.path().join("ready").exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("child should acquire lock");

            // Act
            let error = acquire_instance_lock(root.path())
                .await
                .expect_err("child owns root");
            if terminate_abruptly {
                child.kill().await.expect("kill and reap owner");
            } else {
                tokio::fs::write(root.path().join("stop"), b"")
                    .await
                    .expect("request graceful exit");
                let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
                    .await
                    .expect("child should exit")
                    .expect("reap owner");
                assert!(status.success());
            }
            let restarted = acquire_instance_lock(root.path())
                .await
                .expect("restart after process exit");

            // Assert
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            assert!(restarted.metadata().expect("lock metadata").is_file());
        }
    }

    #[test]
    fn environment_timestamp_source_returns_a_unix_timestamp() {
        // Arrange
        let timestamp_source = timestamp_source_from_environment();

        // Act
        let timestamp = timestamp_source.now_timestamp_seconds();

        // Assert
        assert!(timestamp > 0);
    }
}
