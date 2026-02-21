use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

#[derive(Debug)]
pub enum LockError {
    AlreadyRunning { pid: String },
    Io(io::Error),
}

impl From<io::Error> for LockError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { pid } => {
                write!(f, "Another agentty session is already running (PID: {pid})")
            }
            Self::Io(err) => write!(f, "Failed to acquire session lock: {err}"),
        }
    }
}

/// Acquire an exclusive session lock.
///
/// Returns the lock file handle which must be kept alive for the entire process
/// lifetime. The OS releases the lock automatically on process exit or crash.
///
/// # Errors
/// Returns an error if the lock file cannot be created, cannot be locked, or
/// lock state cannot be written.
pub fn acquire_lock(path: &Path) -> Result<File, LockError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    if let Err(err) = file.try_lock() {
        return match err {
            std::fs::TryLockError::WouldBlock => {
                let mut pid = String::new();
                let mut reader = &file;
                let _ = reader.read_to_string(&mut pid);

                Err(LockError::AlreadyRunning {
                    pid: pid.trim().to_string(),
                })
            }
            std::fs::TryLockError::Error(error) => Err(LockError::Io(error)),
        };
    }

    // Write our PID into the lock file
    file.set_len(0)?;
    file.seek(io::SeekFrom::Start(0))?;
    write!(&file, "{}", std::process::id())?;

    Ok(file)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_acquire_lock_success() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let lock_path = dir.path().join("lock");

        // Act
        let lock = acquire_lock(&lock_path);

        // Assert
        assert!(lock.is_ok());
    }

    #[test]
    fn test_acquire_lock_failure() {
        // Arrange
        let dir = tempdir().expect("failed to create temp dir");
        let lock_path = dir.path().join("lock");
        let _lock1 = acquire_lock(&lock_path).expect("failed to acquire first lock");

        // Act
        let result = acquire_lock(&lock_path);

        // Assert
        assert!(matches!(result, Err(LockError::AlreadyRunning { .. })));
    }
}
