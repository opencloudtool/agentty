//! Filesystem boundary used by app orchestration workflows.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Boxed async result used by [`FsClient`] trait methods.
pub type FsFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Typed error returned by filesystem infrastructure operations.
///
/// Wraps I/O failures so callers can distinguish filesystem errors without
/// parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// A filesystem or file I/O operation failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Async filesystem boundary used by app-layer workflows.
///
/// Production uses [`RealFsClient`], while tests can inject
/// `MockFsClient` to avoid mutating the real filesystem.
#[cfg_attr(test, mockall::automock)]
pub trait FsClient: Send + Sync {
    /// Recursively creates `path` and its missing parents.
    ///
    /// # Errors
    /// Returns an error when filesystem creation fails.
    fn create_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Recursively removes `path` and its contents.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails.
    fn remove_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Reads one file into bytes without blocking the async runtime.
    ///
    /// # Errors
    /// Returns an error when file read fails.
    fn read_file(&self, path: PathBuf) -> FsFuture<Result<Vec<u8>, FsError>>;

    /// Writes one byte buffer to `path`, replacing any existing file.
    ///
    /// # Errors
    /// Returns an error when file creation or write fails.
    fn write_file(&self, path: PathBuf, contents: Vec<u8>) -> FsFuture<Result<(), FsError>>;

    /// Removes one file from disk.
    ///
    /// Missing files are treated as a successful no-op.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails for any reason other
    /// than the file already being absent.
    fn remove_file(&self, path: PathBuf) -> FsFuture<Result<(), FsError>>;

    /// Returns whether `path` currently resolves to an existing directory.
    fn is_dir(&self, path: PathBuf) -> bool;

    /// Returns whether `path` currently resolves to an existing regular file.
    fn is_file(&self, path: PathBuf) -> bool;
}

/// Production [`FsClient`] implementation backed by real filesystem calls.
pub struct RealFsClient;

impl FsClient for RealFsClient {
    fn create_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move { tokio::fs::create_dir_all(path).await.map_err(FsError::from) })
    }

    fn remove_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move { tokio::fs::remove_dir_all(path).await.map_err(FsError::from) })
    }

    fn read_file(&self, path: PathBuf) -> FsFuture<Result<Vec<u8>, FsError>> {
        Box::pin(async move { tokio::fs::read(path).await.map_err(FsError::from) })
    }

    fn write_file(&self, path: PathBuf, contents: Vec<u8>) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move {
            tokio::fs::write(path, contents)
                .await
                .map_err(FsError::from)
        })
    }

    fn remove_file(&self, path: PathBuf) -> FsFuture<Result<(), FsError>> {
        Box::pin(async move {
            match tokio::fs::remove_file(path).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(FsError::from(error)),
            }
        })
    }

    fn is_dir(&self, path: PathBuf) -> bool {
        path.is_dir()
    }

    fn is_file(&self, path: PathBuf) -> bool {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    /// Verifies `RealFsClient::read_file()` reads bytes through the async
    /// filesystem adapter.
    #[tokio::test]
    async fn test_real_fs_client_read_file_reads_existing_file() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let file_path = temp_dir.path().join("example.txt");
        tokio::fs::write(&file_path, b"hello world")
            .await
            .expect("write file");
        let fs_client = RealFsClient;

        // Act
        let content = fs_client
            .read_file(file_path)
            .await
            .expect("read existing file");

        // Assert
        assert_eq!(content, b"hello world");
    }

    /// Verifies `RealFsClient::read_file()` surfaces read failures through the
    /// async boundary.
    #[tokio::test]
    async fn test_real_fs_client_read_file_returns_error_for_missing_file() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let file_path = temp_dir.path().join("missing.txt");
        let fs_client = RealFsClient;

        // Act
        let error = fs_client
            .read_file(file_path)
            .await
            .expect_err("missing file should error");

        // Assert
        let message = error.to_string();
        assert!(message.contains("No such file") || message.contains("cannot find the path"));
    }

    /// Verifies `RealFsClient::is_file()` distinguishes files from
    /// directories.
    #[tokio::test]
    async fn test_real_fs_client_is_file_returns_true_only_for_regular_files() {
        // Arrange
        let temp_dir = tempdir().expect("create temp dir");
        let file_path = temp_dir.path().join("example.txt");
        tokio::fs::write(&file_path, b"hello world")
            .await
            .expect("write file");
        let fs_client = RealFsClient;

        // Act
        let file_exists = fs_client.is_file(file_path);
        let directory_exists = fs_client.is_file(temp_dir.path().to_path_buf());

        // Assert
        assert!(file_exists);
        assert!(!directory_exists);
    }
}
