//! Filesystem boundary used by app orchestration workflows.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Boxed async result used by [`FsClient`] trait methods.
pub type FsFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

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
    fn create_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), String>>;

    /// Recursively removes `path` and its contents.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails.
    fn remove_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), String>>;

    /// Reads one file into bytes.
    ///
    /// # Errors
    /// Returns an error when file read fails.
    fn read_file(&self, path: PathBuf) -> Result<Vec<u8>, String>;

    /// Removes one file from disk.
    ///
    /// Missing files are treated as a successful no-op.
    ///
    /// # Errors
    /// Returns an error when filesystem removal fails for any reason other
    /// than the file already being absent.
    fn remove_file(&self, path: PathBuf) -> FsFuture<Result<(), String>>;

    /// Returns whether `path` currently resolves to an existing directory.
    fn is_dir(&self, path: PathBuf) -> bool;
}

/// Production [`FsClient`] implementation backed by real filesystem calls.
pub struct RealFsClient;

impl FsClient for RealFsClient {
    fn create_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), String>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(path)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn remove_dir_all(&self, path: PathBuf) -> FsFuture<Result<(), String>> {
        Box::pin(async move {
            tokio::fs::remove_dir_all(path)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn read_file(&self, path: PathBuf) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|error| error.to_string())
    }

    fn remove_file(&self, path: PathBuf) -> FsFuture<Result<(), String>> {
        Box::pin(async move {
            match tokio::fs::remove_file(path).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn is_dir(&self, path: PathBuf) -> bool {
        path.is_dir()
    }
}
