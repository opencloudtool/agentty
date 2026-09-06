use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
#[cfg(target_vendor = "apple")]
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::fd::OwnedFd;
#[cfg(any(target_os = "android", target_os = "linux"))]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
#[cfg(target_vendor = "apple")]
use rustix::fs::CopyfileFlags;
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use tokio::io::AsyncRead;

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW);
const FILE_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const TEMPORARY_OPEN_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW);
const DIRECTORY_MODE: Mode = Mode::RWXU
    .union(Mode::RGRP)
    .union(Mode::XGRP)
    .union(Mode::ROTH)
    .union(Mode::XOTH);
const TEMPORARY_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const UPDATE_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const UPDATE_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(target_vendor = "apple")]
const COPYFILE_PACK: CopyfileFlags = CopyfileFlags::from_bits_retain(1 << 22);
#[cfg(any(target_os = "android", target_os = "linux"))]
const XATTR_BUFFER_SIZE: usize = 64 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Asynchronous filesystem boundary used by harness tools.
///
/// The harness uses this boundary for diagnostic path resolution,
/// descriptor-relative opening, and stale-safe replacement, keeping repository
/// containment inside the filesystem operation.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Resolves a path to its canonical absolute representation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the path cannot be resolved.
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    /// Opens a repository-relative file without following symlinks beneath
    /// `root`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `path` is invalid, traverses a symlink, does
    /// not name a regular file, or cannot be opened for reading.
    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>>;

    /// Safely creates or replaces one repository-relative regular file.
    ///
    /// `expected` is `None` for a create-only operation. For replacement it
    /// contains the exact bytes that must still be present when the prepared
    /// file is atomically exchanged with the target, preventing stale model
    /// context from overwriting newer content.
    /// Replacements preserve the target's ownership and access-control
    /// metadata or fail and restore the original file.
    /// Missing parent directories are created without following symlinks.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when containment cannot be enforced, the target
    /// changed, a create target already exists, or the replacement
    /// cannot be completed.
    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()>;
}

struct ParentDirectory {
    descriptor: OwnedFd,
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct AccessMetadata {
    attributes: Vec<ExtendedAttribute>,
    group: rustix::fs::Gid,
    mode: Mode,
    owner: rustix::fs::Uid,
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExtendedAttribute {
    name: OsString,
    value: Vec<u8>,
}

/// Tokio-backed filesystem implementation for local repositories.
pub struct LocalFileSystem;

impl LocalFileSystem {
    fn open_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
        let mut directory =
            rustix::fs::open(root, DIRECTORY_OPEN_FLAGS, Mode::empty()).map_err(io::Error::from)?;
        let components = relative_path
            .components()
            .map(|component| match component {
                Component::Normal(component) => Ok(component),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "read path must be repository-relative",
                )),
            })
            .collect::<io::Result<Vec<&OsStr>>>()?;
        let (file_name, ancestor_components) = components.split_last().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "read path must not be empty")
        })?;

        for component in ancestor_components {
            directory =
                rustix::fs::openat(&directory, *component, DIRECTORY_OPEN_FLAGS, Mode::empty())
                    .map_err(io::Error::from)?;
        }
        let descriptor = rustix::fs::openat(&directory, *file_name, FILE_OPEN_FLAGS, Mode::empty())
            .map_err(io::Error::from)?;
        let metadata = rustix::fs::fstat(&descriptor).map_err(io::Error::from)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read path must name a regular file",
            ));
        }

        Ok(std::fs::File::from(descriptor))
    }

    fn replace_beneath(
        root: &Path,
        relative_path: &Path,
        expected: Option<&[u8]>,
        content: &[u8],
    ) -> io::Result<()> {
        let (parent, file_name) = Self::open_parent_beneath(root, relative_path)?;
        let temporary_name = Self::temporary_name();
        let descriptor = rustix::fs::openat(
            &parent.descriptor,
            &temporary_name,
            TEMPORARY_OPEN_FLAGS,
            TEMPORARY_MODE,
        )
        .map_err(io::Error::from)?;
        let mut temporary_file = std::fs::File::from(descriptor);
        let mut remove_temporary = true;
        let operation = (|| {
            temporary_file.write_all(content)?;
            temporary_file.sync_all()?;
            if let Some(expected) = expected {
                let update = Self::install_update(
                    &parent.descriptor,
                    &temporary_file,
                    &file_name,
                    &temporary_name,
                    expected,
                    &mut remove_temporary,
                );
                update?;
            } else {
                rustix::fs::renameat_with(
                    &parent.descriptor,
                    &temporary_name,
                    &parent.descriptor,
                    &file_name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                remove_temporary = false;
                let _ = rustix::fs::fsync(&parent.descriptor);
            }

            Ok(())
        })();
        if remove_temporary {
            let _ = rustix::fs::unlinkat(&parent.descriptor, &temporary_name, AtFlags::empty());
        }

        operation
    }

    fn install_update(
        parent: &OwnedFd,
        temporary_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        expected: &[u8],
        remove_temporary: &mut bool,
    ) -> io::Result<()> {
        Self::install_update_with_metadata(
            parent,
            temporary_file,
            file_name,
            temporary_name,
            expected,
            remove_temporary,
            Self::copy_metadata,
        )
    }

    fn install_update_with_metadata(
        parent: &OwnedFd,
        temporary_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        expected: &[u8],
        remove_temporary: &mut bool,
        copy_metadata: impl FnOnce(&std::fs::File, &std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        Self::acquire_update_lock(parent, UPDATE_LOCK_TIMEOUT)?;
        let original_file = match Self::verify_target(parent, file_name, expected) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "write target changed since it was read",
                ));
            }
            Err(error) => return Err(error),
        };
        rustix::fs::renameat_with(
            parent,
            temporary_name,
            parent,
            file_name,
            RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from)?;
        *remove_temporary = false;
        let metadata_result =
            copy_metadata(&original_file, temporary_file).and_then(|()| temporary_file.sync_all());
        if let Err(error) = metadata_result {
            Self::roll_back_exchange(
                parent,
                temporary_file,
                file_name,
                temporary_name,
                remove_temporary,
            )?;

            return Err(error);
        }
        Self::validate_exchange(
            parent,
            temporary_file,
            file_name,
            temporary_name,
            expected,
            remove_temporary,
        )
    }

    fn acquire_update_lock(parent: &OwnedFd, timeout: Duration) -> io::Result<()> {
        Self::acquire_update_lock_with(timeout, || {
            rustix::fs::flock(parent, FlockOperation::NonBlockingLockExclusive)
                .map_err(io::Error::from)
        })
    }

    fn acquire_update_lock_with(
        timeout: Duration,
        mut try_lock: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let started_at = Instant::now();
        loop {
            match try_lock() {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if error.kind() != io::ErrorKind::WouldBlock {
                        return Err(error);
                    }
                }
            }

            let remaining = timeout.saturating_sub(started_at.elapsed());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for the write coordination lock",
                ));
            }
            thread::sleep(UPDATE_LOCK_RETRY_INTERVAL.min(remaining));
        }
    }

    #[cfg(target_vendor = "apple")]
    fn copy_metadata(source: &std::fs::File, destination: &std::fs::File) -> io::Result<()> {
        let mut expected = Self::apple_metadata_snapshot(source)?;
        Self::apple_copyfile(source, destination, CopyfileFlags::METADATA)?;

        Self::verify_apple_metadata(&mut expected, source, destination)
    }

    #[cfg(target_vendor = "apple")]
    fn verify_apple_metadata(
        expected: &mut std::fs::File,
        source: &std::fs::File,
        destination: &std::fs::File,
    ) -> io::Result<()> {
        let mut current_source = Self::apple_metadata_snapshot(source)?;
        let mut current_destination = Self::apple_metadata_snapshot(destination)?;
        if !Self::files_match(expected, &mut current_source)?
            || !Self::files_match(expected, &mut current_destination)?
        {
            return Err(Self::metadata_changed_error());
        }

        Ok(())
    }

    #[cfg(target_vendor = "apple")]
    fn apple_metadata_snapshot(source: &std::fs::File) -> io::Result<std::fs::File> {
        let snapshot = tempfile::tempfile()?;
        let flags = CopyfileFlags::METADATA.union(COPYFILE_PACK);
        Self::apple_copyfile(source, &snapshot, flags)?;

        Ok(snapshot)
    }

    #[cfg(target_vendor = "apple")]
    #[expect(
        unsafe_code,
        reason = "rustix exposes Apple's stateful fcopyfile metadata API as unsafe"
    )]
    fn apple_copyfile(
        source: &std::fs::File,
        destination: &std::fs::File,
        flags: CopyfileFlags,
    ) -> io::Result<()> {
        let state = rustix::fs::copyfile_state_alloc().map_err(io::Error::from)?;
        // SAFETY: `state` was allocated immediately above and remains live
        // until the matching `copyfile_state_free` call below.
        let copy_result = unsafe { rustix::fs::fcopyfile(source, destination, state, flags) }
            .map_err(io::Error::from);
        // SAFETY: This is the one matching free for the live state allocated
        // above.
        let free_result =
            unsafe { rustix::fs::copyfile_state_free(state) }.map_err(io::Error::from);

        copy_result.and(free_result)
    }

    #[cfg(target_vendor = "apple")]
    fn files_match(left: &mut std::fs::File, right: &mut std::fs::File) -> io::Result<bool> {
        let left_length = left.metadata()?.len();
        if left_length != right.metadata()?.len() {
            return Ok(false);
        }
        let mut remaining = left_length;
        left.seek(SeekFrom::Start(0))?;
        right.seek(SeekFrom::Start(0))?;
        let mut left_buffer = [0_u8; 8 * 1024];
        let mut right_buffer = [0_u8; 8 * 1024];
        while remaining > 0 {
            let buffer_length = u64::try_from(left_buffer.len()).unwrap_or(u64::MAX);
            let chunk_length =
                usize::try_from(remaining.min(buffer_length)).unwrap_or(left_buffer.len());
            left.read_exact(&mut left_buffer[..chunk_length])?;
            right.read_exact(&mut right_buffer[..chunk_length])?;
            if left_buffer[..chunk_length] != right_buffer[..chunk_length] {
                return Ok(false);
            }
            remaining -= u64::try_from(chunk_length).unwrap_or(remaining);
        }

        Ok(true)
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn copy_metadata(source: &std::fs::File, destination: &std::fs::File) -> io::Result<()> {
        let expected = Self::access_metadata(source)?;
        let destination_names = Self::extended_attribute_names(destination)?;

        rustix::fs::fchown(destination, Some(expected.owner), Some(expected.group))
            .map_err(io::Error::from)?;
        for name in destination_names.iter().filter(|name| {
            !expected
                .attributes
                .iter()
                .any(|attribute| &attribute.name == *name)
        }) {
            rustix::fs::fremovexattr(destination, name).map_err(io::Error::from)?;
        }
        for attribute in &expected.attributes {
            rustix::fs::fsetxattr(
                destination,
                &attribute.name,
                &attribute.value,
                rustix::fs::XattrFlags::empty(),
            )
            .map_err(io::Error::from)?;
        }
        rustix::fs::fchmod(destination, expected.mode).map_err(io::Error::from)?;

        Self::verify_copied_metadata(source, destination, &expected)
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "android", target_os = "linux")))]
    fn copy_metadata(_source: &std::fs::File, _destination: &std::fs::File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe write metadata preservation is unsupported on this platform",
        ))
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn extended_attribute_names(file: &std::fs::File) -> io::Result<Vec<OsString>> {
        let mut buffer = vec![0_u8; XATTR_BUFFER_SIZE];
        let length = rustix::fs::flistxattr(file, &mut buffer).map_err(io::Error::from)?;
        buffer.truncate(length);

        let mut names = buffer
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| OsString::from_vec(name.to_vec()))
            .collect::<Vec<_>>();
        names.sort();

        Ok(names)
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn extended_attribute(file: &std::fs::File, name: &OsStr) -> io::Result<Vec<u8>> {
        let mut value = vec![0_u8; XATTR_BUFFER_SIZE];
        let length = rustix::fs::fgetxattr(file, name, &mut value).map_err(io::Error::from)?;
        value.truncate(length);

        Ok(value)
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn access_metadata(file: &std::fs::File) -> io::Result<AccessMetadata> {
        let metadata = rustix::fs::fstat(file).map_err(io::Error::from)?;
        let attributes = Self::extended_attribute_names(file)?
            .into_iter()
            .map(|name| {
                let value = Self::extended_attribute(file, &name)?;

                Ok(ExtendedAttribute { name, value })
            })
            .collect::<io::Result<Vec<_>>>()?;

        Ok(AccessMetadata {
            attributes,
            group: rustix::fs::Gid::from_raw(metadata.st_gid),
            mode: Mode::from_bits_truncate(metadata.st_mode),
            owner: rustix::fs::Uid::from_raw(metadata.st_uid),
        })
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn verify_copied_metadata(
        source: &std::fs::File,
        destination: &std::fs::File,
        expected: &AccessMetadata,
    ) -> io::Result<()> {
        if Self::access_metadata(source)? != *expected
            || Self::access_metadata(destination)? != *expected
        {
            return Err(Self::metadata_changed_error());
        }

        Ok(())
    }

    fn metadata_changed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "write target metadata changed during replacement",
        )
    }

    fn roll_back_exchange(
        parent: &OwnedFd,
        prepared_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        remove_temporary: &mut bool,
    ) -> io::Result<()> {
        rustix::fs::renameat_with(
            parent,
            temporary_name,
            parent,
            file_name,
            RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from)
        .map_err(Self::map_exchange_error)?;
        *remove_temporary = false;
        if !Self::path_matches_file(parent, temporary_name, prepared_file)? {
            rustix::fs::renameat_with(
                parent,
                temporary_name,
                parent,
                file_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(io::Error::from)?;

            return Err(Self::concurrent_target_error());
        }
        *remove_temporary = true;

        rustix::fs::fsync(parent).map_err(io::Error::from)
    }

    fn validate_exchange(
        parent: &OwnedFd,
        prepared_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        expected: &[u8],
        remove_temporary: &mut bool,
    ) -> io::Result<()> {
        if let Err(error) = Self::verify_target(parent, temporary_name, expected) {
            Self::roll_back_exchange(
                parent,
                prepared_file,
                file_name,
                temporary_name,
                remove_temporary,
            )?;

            return Err(error);
        }
        if !Self::path_matches_file(parent, file_name, prepared_file)? {
            return Err(Self::concurrent_target_error());
        }
        let exchange_is_durable = rustix::fs::fsync(parent).is_ok();
        if exchange_is_durable
            && rustix::fs::unlinkat(parent, temporary_name, AtFlags::empty()).is_ok()
        {
            let _ = rustix::fs::fsync(parent);
        }

        Ok(())
    }

    fn path_matches_file(
        parent: &OwnedFd,
        file_name: &OsStr,
        expected_file: &std::fs::File,
    ) -> io::Result<bool> {
        let expected = rustix::fs::fstat(expected_file).map_err(io::Error::from)?;
        let current = match rustix::fs::statat(parent, file_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(current) => current,
            Err(error) if io::Error::from(error).kind() == io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(error) => return Err(io::Error::from(error)),
        };

        Ok(expected.st_dev == current.st_dev && expected.st_ino == current.st_ino)
    }

    fn concurrent_target_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "write target changed during atomic replacement",
        )
    }

    fn map_exchange_error(error: io::Error) -> io::Error {
        if error.kind() == io::ErrorKind::NotFound {
            return Self::concurrent_target_error();
        }

        error
    }

    fn open_parent_beneath(
        root: &Path,
        relative_path: &Path,
    ) -> io::Result<(ParentDirectory, OsString)> {
        let mut directory =
            rustix::fs::open(root, DIRECTORY_OPEN_FLAGS, Mode::empty()).map_err(io::Error::from)?;
        let mut components = relative_path
            .components()
            .map(|component| match component {
                Component::Normal(component) => Ok(component.to_os_string()),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write path must be repository-relative",
                )),
            })
            .collect::<io::Result<Vec<OsString>>>()?;
        let file_name = components.pop().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write path must not be empty")
        })?;

        for component in components {
            match rustix::fs::openat(&directory, &component, DIRECTORY_OPEN_FLAGS, Mode::empty()) {
                Ok(descriptor) => directory = descriptor,
                Err(error) if io::Error::from(error).kind() == io::ErrorKind::NotFound => {
                    Self::create_directory(&directory, &component)?;
                    directory = rustix::fs::openat(
                        &directory,
                        &component,
                        DIRECTORY_OPEN_FLAGS,
                        Mode::empty(),
                    )
                    .map_err(io::Error::from)?;
                }
                Err(error) => return Err(io::Error::from(error)),
            }
        }

        Ok((
            ParentDirectory {
                descriptor: directory,
            },
            file_name,
        ))
    }

    fn create_directory(parent: &OwnedFd, name: &OsStr) -> io::Result<()> {
        match rustix::fs::mkdirat(parent, name, DIRECTORY_MODE) {
            Ok(()) => Ok(()),
            Err(error) if io::Error::from(error).kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        }
    }

    fn verify_target(
        parent: &OwnedFd,
        file_name: &OsStr,
        expected: &[u8],
    ) -> io::Result<std::fs::File> {
        let descriptor = rustix::fs::openat(parent, file_name, FILE_OPEN_FLAGS, Mode::empty())
            .map_err(io::Error::from)?;
        let metadata = rustix::fs::fstat(&descriptor).map_err(io::Error::from)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write path must name a regular file",
            ));
        }
        let mut target = std::fs::File::from(descriptor);
        if !Self::target_matches(&mut target, expected)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "write target changed since it was read",
            ));
        }

        Ok(target)
    }

    fn target_matches(target: &mut impl io::Read, expected: &[u8]) -> io::Result<bool> {
        let mut buffer = [0_u8; 8 * 1024];
        for expected_chunk in expected.chunks(buffer.len()) {
            match target.read_exact(&mut buffer[..expected_chunk.len()]) {
                Ok(()) if &buffer[..expected_chunk.len()] == expected_chunk => {}
                Ok(()) => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        let mut extra = [0_u8; 1];
        match target.read_exact(&mut extra) {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn temporary_name() -> OsString {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);

        OsString::from(format!(
            ".ag-harness-write-{}-{sequence}",
            std::process::id()
        ))
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        tokio::fs::canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        let file = tokio::task::spawn_blocking(move || Self::open_beneath(&root, &path))
            .await
            .map_err(io::Error::other)??;

        Ok(Box::new(tokio::fs::File::from_std(file)))
    }

    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            Self::replace_beneath(&root, &path, expected.as_deref(), &content)
        })
        .await
        .map_err(io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

    use tokio::io::AsyncReadExt as _;

    use super::*;

    struct FailingReader;

    impl io::Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }

    #[tokio::test]
    async fn local_file_system_canonicalizes_and_opens_file() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let nested_directory = directory.path().join("nested");
        std::fs::create_dir(&nested_directory).expect("nested directory should be created");
        let path = nested_directory.join("input.txt");
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(b"hello"))
            .expect("fixture file should be written");
        let file_system = LocalFileSystem;

        // Act
        let canonical_path = file_system
            .canonicalize(&path)
            .await
            .expect("fixture path should canonicalize");
        let mut file = file_system
            .open_beneath(directory.path(), Path::new("nested/input.txt"))
            .await
            .expect("fixture file should open");
        let mut content = String::new();
        file.read_to_string(&mut content)
            .await
            .expect("fixture file should be readable");

        // Assert
        assert!(canonical_path.is_absolute());
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn local_file_system_reports_missing_paths() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("missing.txt");
        let file_system = LocalFileSystem;

        // Act
        let canonicalize_error = file_system
            .canonicalize(&path)
            .await
            .expect_err("missing path should not canonicalize");
        let open_error = file_system
            .open_beneath(directory.path(), Path::new("missing.txt"))
            .await
            .err()
            .expect("missing path should not open");

        // Assert
        assert_eq!(canonicalize_error.kind(), io::ErrorKind::NotFound);
        assert_eq!(open_error.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn local_file_system_rejects_invalid_relative_paths() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let file_system = LocalFileSystem;

        // Act
        let empty_error = file_system
            .open_beneath(directory.path(), Path::new(""))
            .await
            .err()
            .expect("empty path should fail");
        let parent_error = file_system
            .open_beneath(directory.path(), Path::new("../input.txt"))
            .await
            .err()
            .expect("parent traversal should fail");

        // Assert
        assert_eq!(empty_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(parent_error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn local_file_system_rejects_symlink_traversal() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let outside = tempfile::tempdir().expect("outside directory should be created");
        let outside_file = outside.path().join("outside.txt");
        std::fs::File::create(&outside_file)
            .and_then(|mut file| file.write_all(b"outside"))
            .expect("outside file should be written");
        symlink(&outside_file, repository.path().join("file-link"))
            .expect("file symlink should be created");
        symlink(outside.path(), repository.path().join("directory-link"))
            .expect("directory symlink should be created");
        let file_system = LocalFileSystem;

        // Act
        let file_error = file_system
            .open_beneath(repository.path(), Path::new("file-link"))
            .await
            .err()
            .expect("file symlink should not be followed");
        let directory_error = file_system
            .open_beneath(repository.path(), Path::new("directory-link/outside.txt"))
            .await
            .err()
            .expect("directory symlink should not be followed");

        // Assert
        assert_ne!(file_error.kind(), io::ErrorKind::NotFound);
        assert_ne!(directory_error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn local_file_system_opens_nonblocking_and_rejects_non_regular_file() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::create_dir(repository.path().join("directory"))
            .expect("directory fixture should be created");

        // Act
        let error = LocalFileSystem::open_beneath(repository.path(), Path::new("directory"))
            .expect_err("directory should not be readable as a regular file");

        // Assert
        assert!(FILE_OPEN_FLAGS.contains(OFlags::NONBLOCK));
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn local_file_system_creates_nested_file_atomically() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let file_system = LocalFileSystem;

        // Act
        file_system
            .replace_beneath(
                repository.path(),
                Path::new("nested/output.txt"),
                None,
                b"created\n".to_vec(),
            )
            .await
            .expect("new file should be written");

        // Assert
        assert_eq!(
            std::fs::read(repository.path().join("nested/output.txt"))
                .expect("created file should be readable"),
            b"created\n"
        );
        assert_eq!(
            std::fs::metadata(repository.path().join("nested/output.txt"))
                .expect("created file metadata should be readable")
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }

    #[tokio::test]
    async fn local_file_system_replaces_expected_content_and_preserves_access_metadata() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let nested = repository.path().join("nested");
        std::fs::create_dir(&nested).expect("nested directory should be created");
        let path = nested.join("script.sh");
        std::fs::write(&path, b"old\n").expect("fixture should be written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))
            .expect("fixture mode should be set");
        let original_file = std::fs::File::open(&path).expect("fixture should open");
        rustix::fs::fsetxattr(
            &original_file,
            "user.ag-harness-test",
            b"preserved",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("fixture xattr should be set");
        let original_metadata = original_file
            .metadata()
            .expect("fixture metadata should read");
        let file_system = LocalFileSystem;

        // Act
        file_system
            .replace_beneath(
                repository.path(),
                Path::new("nested/script.sh"),
                Some(b"old\n".to_vec()),
                b"new\n".to_vec(),
            )
            .await
            .expect("existing file should be replaced");

        // Assert
        assert_eq!(
            std::fs::read(&path).expect("file should be readable"),
            b"new\n"
        );
        let updated_file = std::fs::File::open(&path).expect("updated file should open");
        let updated_metadata = updated_file
            .metadata()
            .expect("updated metadata should read");
        assert_eq!(updated_metadata.permissions().mode() & 0o777, 0o750);
        assert_eq!(updated_metadata.uid(), original_metadata.uid());
        assert_eq!(updated_metadata.gid(), original_metadata.gid());
        let mut xattr = [0_u8; 16];
        let xattr_length = rustix::fs::fgetxattr(&updated_file, "user.ag-harness-test", &mut xattr)
            .expect("updated xattr should read");
        assert_eq!(&xattr[..xattr_length], b"preserved");
    }

    #[tokio::test]
    async fn local_file_system_rejects_stale_or_existing_target() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::write(repository.path().join("output.txt"), b"current")
            .expect("fixture should be written");
        let file_system = LocalFileSystem;

        // Act
        let stale_error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("output.txt"),
                Some(b"stale".to_vec()),
                b"replacement".to_vec(),
            )
            .await
            .expect_err("stale replacement should fail");
        let create_error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("output.txt"),
                None,
                b"replacement".to_vec(),
            )
            .await
            .expect_err("create over existing file should fail");
        let missing_error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("deleted.txt"),
                Some(b"previous".to_vec()),
                b"replacement".to_vec(),
            )
            .await
            .expect_err("missing update target should be rejected as stale");

        // Assert
        assert_eq!(stale_error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(create_error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(missing_error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(repository.path().join("output.txt"))
                .expect("original file should remain"),
            b"current"
        );
    }

    #[test]
    fn local_file_system_does_not_overwrite_target_changed_at_install() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::write(repository.path().join("output.txt"), b"newer")
            .expect("newer target should be written");
        std::fs::write(repository.path().join(".prepared"), b"model change")
            .expect("prepared file should be written");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let prepared_file = std::fs::OpenOptions::new()
            .write(true)
            .open(repository.path().join(".prepared"))
            .expect("prepared file should open");
        let mut remove_prepared = true;

        // Act
        let error = LocalFileSystem::install_update(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".prepared"),
            b"stale",
            &mut remove_prepared,
        )
        .expect_err("changed target should reject installation");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(remove_prepared);
        assert_eq!(
            std::fs::read(repository.path().join("output.txt"))
                .expect("newer target should remain"),
            b"newer"
        );
    }

    #[test]
    fn local_file_system_bounds_update_lock_wait() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let (lock_owner, _) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("first parent descriptor should open");
        let (lock_waiter, _) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("second parent descriptor should open");
        rustix::fs::flock(&lock_owner.descriptor, FlockOperation::LockExclusive)
            .expect("fixture lock should be acquired");

        // Act
        let error = LocalFileSystem::acquire_update_lock(
            &lock_waiter.descriptor,
            Duration::from_millis(20),
        )
        .expect_err("competing lock should time out");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn local_file_system_propagates_update_lock_errors() {
        // Arrange
        let expected_kind = io::ErrorKind::PermissionDenied;

        // Act
        let error = LocalFileSystem::acquire_update_lock_with(Duration::ZERO, || {
            Err(io::Error::new(expected_kind, "lock failed"))
        })
        .expect_err("lock error should propagate");

        // Assert
        assert_eq!(error.kind(), expected_kind);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn local_file_system_detects_apple_metadata_added_after_snapshot() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("source");
        std::fs::write(&path, b"source").expect("source should be written");
        let source = std::fs::File::open(path).expect("source should open");
        let mut expected =
            LocalFileSystem::apple_metadata_snapshot(&source).expect("metadata should snapshot");
        rustix::fs::fsetxattr(
            &source,
            "user.ag-harness-added",
            b"added",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("source xattr should be added");

        // Act
        let error = LocalFileSystem::verify_apple_metadata(&mut expected, &source, &source)
            .expect_err("added source metadata should reject the snapshot");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn local_file_system_compares_complete_metadata_archives() {
        // Arrange
        let mut left = tempfile::tempfile().expect("left archive should open");
        let mut different = tempfile::tempfile().expect("different archive should open");
        let mut longer = tempfile::tempfile().expect("longer archive should open");
        left.write_all(b"left").expect("left archive should write");
        different
            .write_all(b"diff")
            .expect("different archive should write");
        longer
            .write_all(b"longer")
            .expect("longer archive should write");

        // Act
        let content_matches = LocalFileSystem::files_match(&mut left, &mut different)
            .expect("equal-length archives should compare");
        let length_matches = LocalFileSystem::files_match(&mut left, &mut longer)
            .expect("different-length archives should compare");

        // Assert
        assert!(!content_matches);
        assert!(!length_matches);
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    #[test]
    fn local_file_system_removes_destination_only_extended_attributes() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let source_path = directory.path().join("source");
        let destination_path = directory.path().join("destination");
        std::fs::write(&source_path, b"source").expect("source should be written");
        std::fs::write(&destination_path, b"destination").expect("destination should be written");
        let source = std::fs::File::open(source_path).expect("source should open");
        let destination = std::fs::File::open(destination_path).expect("destination should open");
        rustix::fs::fsetxattr(
            &destination,
            "user.ag-harness-extra",
            b"remove",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("destination xattr should be set");

        // Act
        LocalFileSystem::copy_metadata(&source, &destination)
            .expect("source metadata should be copied");

        // Assert
        assert_eq!(
            LocalFileSystem::extended_attribute_names(&destination)
                .expect("destination xattrs should list"),
            Vec::<OsString>::new()
        );
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    #[test]
    fn local_file_system_rejects_mismatched_copied_extended_attributes() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let source_path = directory.path().join("source");
        let destination_path = directory.path().join("destination");
        std::fs::write(&source_path, b"source").expect("source should be written");
        std::fs::write(&destination_path, b"destination").expect("destination should be written");
        let source = std::fs::File::open(source_path).expect("source should open");
        let destination = std::fs::File::open(destination_path).expect("destination should open");
        rustix::fs::fsetxattr(
            &source,
            "user.ag-harness-test",
            b"source",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("source xattr should be set");
        rustix::fs::fsetxattr(
            &destination,
            "user.ag-harness-test",
            b"destination",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("destination xattr should be set");
        let expected =
            LocalFileSystem::access_metadata(&source).expect("source metadata should snapshot");

        // Act
        let error = LocalFileSystem::verify_copied_metadata(&source, &destination, &expected)
            .expect_err("different xattr values should fail verification");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    #[test]
    fn local_file_system_detects_extended_attributes_added_after_snapshot() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let source_path = directory.path().join("source");
        let destination_path = directory.path().join("destination");
        std::fs::write(&source_path, b"source").expect("source should be written");
        std::fs::write(&destination_path, b"destination").expect("destination should be written");
        let source = std::fs::File::open(source_path).expect("source should open");
        let destination = std::fs::File::open(destination_path).expect("destination should open");
        let expected =
            LocalFileSystem::access_metadata(&source).expect("source metadata should snapshot");
        rustix::fs::fsetxattr(
            &source,
            "user.ag-harness-added",
            b"added",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("source xattr should be added");

        // Act
        let error = LocalFileSystem::verify_copied_metadata(&source, &destination, &expected)
            .expect_err("added source xattr should fail verification");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn local_file_system_refreshes_target_mode_before_exchange() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let prepared_path = repository.path().join(".prepared");
        std::fs::write(&target_path, b"expected").expect("target should be written");
        std::fs::set_permissions(&target_path, std::fs::Permissions::from_mode(0o750))
            .expect("target mode should be set");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        std::fs::set_permissions(&prepared_path, std::fs::Permissions::from_mode(0o600))
            .expect("prepared mode should be set");
        let prepared_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&prepared_path)
            .expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_prepared = true;

        // Act
        LocalFileSystem::install_update(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".prepared"),
            b"expected",
            &mut remove_prepared,
        )
        .expect("prepared file should be installed");

        // Assert
        assert!(!remove_prepared);
        assert_eq!(
            std::fs::read(&target_path).expect("target should read"),
            b"model change"
        );
        assert_eq!(
            std::fs::metadata(&target_path)
                .expect("target metadata should read")
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert!(!prepared_path.exists());
    }

    #[test]
    fn local_file_system_rolls_back_failed_mode_installation() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let prepared_path = repository.path().join(".prepared");
        std::fs::write(&target_path, b"expected").expect("target should be written");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        let prepared_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&prepared_path)
            .expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_prepared = true;

        // Act
        let error = LocalFileSystem::install_update_with_metadata(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".prepared"),
            b"expected",
            &mut remove_prepared,
            |_, _| Err(io::Error::other("injected metadata failure")),
        )
        .expect_err("failed metadata installation should roll back");

        // Assert
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(remove_prepared);
        assert_eq!(
            std::fs::read(&target_path).expect("target should be readable"),
            b"expected"
        );
        assert_eq!(
            std::fs::read(&prepared_path).expect("prepared file should be readable"),
            b"model change"
        );
    }

    #[test]
    fn local_file_system_preserves_recovery_when_metadata_rollback_fails() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let prepared_path = repository.path().join(".prepared");
        std::fs::write(&target_path, b"expected").expect("target should be written");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        let prepared_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&prepared_path)
            .expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_prepared = true;

        // Act
        let error = LocalFileSystem::install_update_with_metadata(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".prepared"),
            b"expected",
            &mut remove_prepared,
            |_, _| {
                std::fs::remove_file(&target_path)?;

                Err(io::Error::other("injected metadata failure"))
            },
        )
        .expect_err("failed rollback should preserve the recovery file");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!remove_prepared);
        assert!(!target_path.exists());
        assert_eq!(
            std::fs::read(prepared_path).expect("original target should remain recoverable"),
            b"expected"
        );
    }

    #[test]
    fn local_file_system_rolls_back_stale_atomic_exchange() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::write(repository.path().join("output.txt"), b"model change")
            .expect("exchanged model file should be written");
        std::fs::write(repository.path().join(".prepared"), b"newer")
            .expect("captured newer file should be written");
        let prepared_file = std::fs::File::open(repository.path().join("output.txt"))
            .expect("model file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_prepared = false;

        // Act
        let error = LocalFileSystem::validate_exchange(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".prepared"),
            b"expected",
            &mut remove_prepared,
        )
        .expect_err("stale exchange should be rolled back");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(remove_prepared);
        assert_eq!(
            std::fs::read(repository.path().join("output.txt"))
                .expect("newer target should be restored"),
            b"newer"
        );
        assert_eq!(
            std::fs::read(repository.path().join(".prepared"))
                .expect("model file should remain recoverable"),
            b"model change"
        );
    }

    #[test]
    fn local_file_system_rejects_replaced_target_after_exchange() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let captured_path = repository.path().join(".captured");
        let prepared_path = repository.path().join(".opened-prepared");
        std::fs::write(&target_path, b"concurrent").expect("concurrent target should be written");
        std::fs::write(&captured_path, b"expected").expect("captured file should be written");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        let prepared_file = std::fs::File::open(prepared_path).expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_captured = false;

        // Act
        let error = LocalFileSystem::validate_exchange(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".captured"),
            b"expected",
            &mut remove_captured,
        )
        .expect_err("replaced target should be rejected");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!remove_captured);
        assert_eq!(
            std::fs::read(target_path).expect("concurrent target should remain"),
            b"concurrent"
        );
        assert_eq!(
            std::fs::read(captured_path).expect("captured target should remain recoverable"),
            b"expected"
        );
    }

    #[test]
    fn local_file_system_reports_target_identity_lookup_errors() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let expected_path = repository.path().join("expected.txt");
        std::fs::write(&expected_path, b"expected").expect("expected file should be written");
        std::fs::write(repository.path().join("blocked"), b"file")
            .expect("blocking file should be written");
        let expected_file = std::fs::File::open(expected_path).expect("expected file should open");
        let (parent, _) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");

        // Act
        let missing_matches = LocalFileSystem::path_matches_file(
            &parent.descriptor,
            OsStr::new("missing.txt"),
            &expected_file,
        )
        .expect("missing target should compare");
        let traversal_error = LocalFileSystem::path_matches_file(
            &parent.descriptor,
            OsStr::new("blocked/child"),
            &expected_file,
        )
        .expect_err("non-directory traversal should fail");

        // Assert
        assert!(!missing_matches);
        assert_eq!(traversal_error.kind(), io::ErrorKind::NotADirectory);
    }

    #[test]
    fn local_file_system_restores_concurrent_target_during_rollback() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let captured_path = repository.path().join(".captured");
        let prepared_path = repository.path().join(".opened-prepared");
        std::fs::write(&target_path, b"concurrent").expect("concurrent target should be written");
        std::fs::write(&captured_path, b"newer").expect("captured file should be written");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        let prepared_file = std::fs::File::open(prepared_path).expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_captured = false;

        // Act
        let error = LocalFileSystem::validate_exchange(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".captured"),
            b"expected",
            &mut remove_captured,
        )
        .expect_err("concurrent rollback target should be rejected");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!remove_captured);
        assert_eq!(
            std::fs::read(target_path).expect("concurrent target should be restored"),
            b"concurrent"
        );
        assert_eq!(
            std::fs::read(captured_path).expect("captured target should remain recoverable"),
            b"newer"
        );
    }

    #[test]
    fn local_file_system_preserves_recovery_when_exchange_path_disappears() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let prepared_path = repository.path().join(".prepared");
        std::fs::write(&target_path, b"model change").expect("model file should be written");
        std::fs::write(&prepared_path, b"newer").expect("captured file should be written");
        let prepared_file = std::fs::File::open(&target_path).expect("model file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        std::fs::remove_file(&target_path).expect("model path should disappear");
        let mut remove_prepared = false;

        // Act
        let error = LocalFileSystem::validate_exchange(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".prepared"),
            b"expected",
            &mut remove_prepared,
        )
        .expect_err("stale exchange should be rejected");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!remove_prepared);
        assert!(!target_path.exists());
        assert_eq!(
            std::fs::read(prepared_path).expect("captured target should remain recoverable"),
            b"newer"
        );
    }

    #[test]
    fn local_file_system_reports_failed_exchange_rollback() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let prepared_path = repository.path().join(".opened-prepared");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        let prepared_file = std::fs::File::open(prepared_path).expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_prepared = false;

        // Act
        let error = LocalFileSystem::roll_back_exchange(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".missing-prepared"),
            &mut remove_prepared,
        )
        .expect_err("missing rollback paths should fail");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!remove_prepared);
    }

    #[test]
    fn local_file_system_preserves_non_missing_exchange_errors() {
        // Arrange
        let permission_error = io::Error::new(io::ErrorKind::PermissionDenied, "denied");

        // Act
        let mapped = LocalFileSystem::map_exchange_error(permission_error);

        // Assert
        assert_eq!(mapped.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(mapped.to_string(), "denied");
    }

    #[test]
    fn local_file_system_keeps_committed_write_when_cleanup_fails() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let target_path = repository.path().join("output.txt");
        let captured_path = repository.path().join(".captured");
        std::fs::write(&target_path, b"model change").expect("target should be written");
        std::fs::write(&captured_path, b"expected").expect("captured file should be written");
        let prepared_file = std::fs::File::open(&target_path).expect("model file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        std::fs::set_permissions(repository.path(), std::fs::Permissions::from_mode(0o550))
            .expect("repository should become read-only");
        let mut remove_captured = false;

        // Act
        let result = LocalFileSystem::validate_exchange(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".captured"),
            b"expected",
            &mut remove_captured,
        );
        std::fs::set_permissions(repository.path(), std::fs::Permissions::from_mode(0o750))
            .expect("repository permissions should be restored");

        // Assert
        result.expect("post-commit cleanup failure should not fail the write");
        assert_eq!(
            std::fs::read(&target_path).expect("target should read"),
            b"model change"
        );
        assert_eq!(
            std::fs::read(&captured_path).expect("recovery file should read"),
            b"expected"
        );
        assert!(!remove_captured);
    }

    #[test]
    fn local_file_system_preserves_target_when_exchange_fails() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::write(repository.path().join("output.txt"), b"expected")
            .expect("target should be written");
        let prepared_path = repository.path().join(".opened-prepared");
        std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
        let prepared_file = std::fs::OpenOptions::new()
            .write(true)
            .open(prepared_path)
            .expect("prepared file should open");
        let (parent, file_name) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");
        let mut remove_prepared = true;

        // Act
        let error = LocalFileSystem::install_update(
            &parent.descriptor,
            &prepared_file,
            &file_name,
            OsStr::new(".missing-prepared"),
            b"expected",
            &mut remove_prepared,
        )
        .expect_err("missing prepared file should fail exchange");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(remove_prepared);
        assert_eq!(
            std::fs::read(repository.path().join("output.txt"))
                .expect("target should remain available"),
            b"expected"
        );
    }

    #[test]
    fn target_comparison_reads_at_most_one_byte_past_expected() {
        // Arrange
        let expected = b"expected";
        let mut exact = io::Cursor::new(expected);
        let mut shorter = io::Cursor::new(b"expect".as_slice());
        let mut different = io::Cursor::new(b"expEcted".as_slice());
        let mut longer = io::Cursor::new(b"expected and arbitrarily more data".as_slice());
        let mut expected_failure = FailingReader;
        let mut extra_failure = FailingReader;

        // Act
        let exact_matches = LocalFileSystem::target_matches(&mut exact, expected)
            .expect("exact target should compare");
        let shorter_matches = LocalFileSystem::target_matches(&mut shorter, expected)
            .expect("short target should compare");
        let different_matches = LocalFileSystem::target_matches(&mut different, expected)
            .expect("different target should compare");
        let longer_matches = LocalFileSystem::target_matches(&mut longer, expected)
            .expect("long target should compare");
        let expected_error = LocalFileSystem::target_matches(&mut expected_failure, expected)
            .expect_err("expected-content read error should propagate");
        let extra_error = LocalFileSystem::target_matches(&mut extra_failure, b"")
            .expect_err("extra-byte read error should propagate");

        // Assert
        assert!(exact_matches);
        assert!(!shorter_matches);
        assert!(!different_matches);
        assert!(!longer_matches);
        assert_eq!(longer.position(), (expected.len() + 1) as u64);
        assert_eq!(expected_error.kind(), io::ErrorKind::Other);
        assert_eq!(extra_error.kind(), io::ErrorKind::Other);
    }

    #[tokio::test]
    async fn local_file_system_keeps_created_directories_after_failed_write() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let file_system = LocalFileSystem;

        // Act
        let error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("nested/deeper/output.txt"),
                Some(b"missing".to_vec()),
                b"replacement".to_vec(),
            )
            .await
            .expect_err("replacement of missing target should fail");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(repository.path().join("nested/deeper").is_dir());
    }

    #[test]
    fn local_file_system_handles_directory_creation_outcomes() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let (parent, _) =
            LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
                .expect("target parent should open");

        // Act
        LocalFileSystem::create_directory(&parent.descriptor, OsStr::new("created"))
            .expect("missing directory should be created");
        let existing = LocalFileSystem::create_directory(&parent.descriptor, OsStr::new("created"));
        let missing_parent =
            LocalFileSystem::create_directory(&parent.descriptor, OsStr::new("missing/child"))
                .expect_err("missing parent should fail creation");

        // Assert
        existing.expect("existing directory should be accepted");
        assert!(repository.path().join("created").is_dir());
        assert_eq!(missing_parent.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn local_file_system_rejects_write_symlink_traversal() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        let outside = tempfile::tempdir().expect("outside directory should be created");
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, b"outside").expect("outside file should be written");
        symlink(outside.path(), repository.path().join("directory-link"))
            .expect("directory symlink should be created");
        symlink(&outside_file, repository.path().join("file-link"))
            .expect("file symlink should be created");
        let file_system = LocalFileSystem;

        // Act
        let directory_error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("directory-link/outside.txt"),
                Some(b"outside".to_vec()),
                b"changed".to_vec(),
            )
            .await
            .expect_err("directory symlink should not be followed");
        let file_error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("file-link"),
                Some(b"outside".to_vec()),
                b"changed".to_vec(),
            )
            .await
            .expect_err("file symlink should not be followed");

        // Assert
        assert_ne!(directory_error.kind(), io::ErrorKind::NotFound);
        assert_ne!(file_error.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            std::fs::read(outside_file).expect("outside file should remain"),
            b"outside"
        );
    }

    #[tokio::test]
    async fn local_file_system_rejects_invalid_write_paths_and_special_files() {
        // Arrange
        let repository = tempfile::tempdir().expect("repository should be created");
        std::fs::create_dir(repository.path().join("directory"))
            .expect("directory fixture should be created");
        let file_system = LocalFileSystem;

        // Act
        let empty_error = file_system
            .replace_beneath(repository.path(), Path::new(""), None, Vec::new())
            .await
            .expect_err("empty write path should fail");
        let parent_error = file_system
            .replace_beneath(repository.path(), Path::new("../file"), None, Vec::new())
            .await
            .expect_err("parent traversal should fail");
        let directory_error = file_system
            .replace_beneath(
                repository.path(),
                Path::new("directory"),
                Some(Vec::new()),
                Vec::new(),
            )
            .await
            .expect_err("directory target should fail");
        let long_name = "x".repeat(256);
        let long_name_error = file_system
            .replace_beneath(repository.path(), Path::new(&long_name), None, Vec::new())
            .await
            .expect_err("overlong target name should fail");

        // Assert
        assert_eq!(empty_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(parent_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(directory_error.kind(), io::ErrorKind::InvalidInput);
        assert_ne!(long_name_error.kind(), io::ErrorKind::NotFound);
    }
}
