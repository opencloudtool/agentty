use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rustix::fs as rustix_fs;
use thiserror::Error;

/// Validated host configuration for repository-scoped tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    git_executable: PathBuf,
    root: PathBuf,
}

impl Repository {
    /// Validates a repository root and the host-selected Git executable.
    ///
    /// Both paths are canonicalized immediately. The executable and its
    /// configured location must be outside the containing worktree. On
    /// Unix, the executable must also be an executable regular file. Other
    /// platforms enforce the regular file check but defer executable-access
    /// validation to process creation.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError`] when either path cannot be resolved or the
    /// executable does not satisfy the host-trust requirements.
    pub fn new(
        root: impl AsRef<Path>,
        git_executable: impl AsRef<Path>,
    ) -> Result<Self, RepositoryError> {
        let root = canonical_directory(root.as_ref()).map_err(|source| RepositoryError::Root {
            path: root.as_ref().to_path_buf(),
            source,
        })?;
        if root.components().any(|component| {
            component
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        }) {
            return Err(RepositoryError::RootIsGitAdministrative { path: root });
        }

        let worktree_root = containing_worktree_root(&root, |path| fs::symlink_metadata(path))?;
        let requested_executable = git_executable.as_ref();
        if !requested_executable.is_absolute() {
            return Err(RepositoryError::GitExecutableNotAbsolute {
                path: requested_executable.to_path_buf(),
            });
        }
        let git_executable = fs::canonicalize(requested_executable).map_err(|source| {
            RepositoryError::GitExecutable {
                path: requested_executable.to_path_buf(),
                source,
            }
        })?;
        if !git_executable.is_file() {
            return Err(RepositoryError::GitExecutableNotFile {
                path: git_executable,
            });
        }
        if !is_executable(&git_executable) {
            return Err(RepositoryError::GitExecutableNotExecutable {
                path: git_executable,
            });
        }
        let requested_parent =
            canonical_executable_parent(requested_executable, |path| fs::canonicalize(path))?;
        let target_inside_worktree = git_executable.starts_with(&worktree_root);
        if requested_executable.starts_with(&worktree_root)
            || requested_parent.is_some_and(|parent| parent.starts_with(&worktree_root))
            || target_inside_worktree
        {
            return Err(RepositoryError::GitExecutableInsideRepository {
                path: if target_inside_worktree {
                    git_executable
                } else {
                    requested_executable.to_path_buf()
                },
                root: worktree_root,
            });
        }

        Ok(Self {
            git_executable,
            root,
        })
    }

    pub(crate) fn git_executable(&self) -> &Path {
        &self.git_executable
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(test)]
    pub(crate) fn fixture(root: impl Into<PathBuf>) -> Self {
        Self {
            git_executable: test_git_executable(),
            root: root.into(),
        }
    }
}

#[cfg(test)]
pub(crate) fn test_git_executable() -> PathBuf {
    let executable_name = format!("git{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH");
    assert!(path.is_some(), "test PATH should be configured");
    let executables = path
        .iter()
        .flat_map(|path| std::env::split_paths(path))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name))
        .filter_map(|candidate| candidate.canonicalize().ok())
        .filter(|path| path.is_file() && is_executable(path))
        .collect::<Vec<_>>();
    assert!(
        !executables.is_empty(),
        "trusted Git executable should be available on PATH"
    );

    executables[0].clone()
}

/// Invalid host configuration for repository-scoped tools.
#[derive(Debug, Error)]
pub enum RepositoryError {
    /// The configured repository root could not be resolved to a directory.
    #[error("failed to resolve repository root `{path}`: {source}")]
    Root {
        /// Repository root supplied by the host.
        path: PathBuf,
        /// Filesystem failure returned while resolving the root.
        #[source]
        source: std::io::Error,
    },
    /// The configured root was itself inside Git administrative state.
    #[error("repository root must not access Git administrative state: `{path}`")]
    RootIsGitAdministrative {
        /// Canonical repository root.
        path: PathBuf,
    },
    /// The Git executable could not be resolved or inspected.
    #[error("failed to resolve Git executable `{path}`: {source}")]
    GitExecutable {
        /// Git executable supplied by the host.
        path: PathBuf,
        /// Filesystem failure returned while resolving the executable.
        #[source]
        source: std::io::Error,
    },
    /// The Git executable path was not absolute.
    #[error("Git executable must be absolute: `{path}`")]
    GitExecutableNotAbsolute {
        /// Relative executable path supplied by the host.
        path: PathBuf,
    },
    /// The configured Git executable location or canonical target was inside
    /// the containing worktree.
    #[error("Git executable `{path}` must be outside repository-controlled worktree `{root}`")]
    GitExecutableInsideRepository {
        /// Rejected configured location or canonical executable path.
        path: PathBuf,
        /// Canonical containing worktree root.
        root: PathBuf,
    },
    /// The canonical Git executable was not a regular file.
    #[error("Git executable is not a regular file: `{path}`")]
    GitExecutableNotFile {
        /// Canonical executable path.
        path: PathBuf,
    },
    /// The canonical Git executable lacked executable permissions.
    #[error("Git executable is not executable: `{path}`")]
    GitExecutableNotExecutable {
        /// Canonical executable path.
        path: PathBuf,
    },
}

fn canonical_directory(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if !fs::metadata(&canonical)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a directory",
        ));
    }

    Ok(canonical)
}

fn canonical_executable_parent(
    path: &Path,
    canonicalize: impl Fn(&Path) -> std::io::Result<PathBuf>,
) -> Result<Option<PathBuf>, RepositoryError> {
    path.parent()
        .map(canonicalize)
        .transpose()
        .map_err(|source| RepositoryError::GitExecutable {
            path: path.to_path_buf(),
            source,
        })
}

fn containing_worktree_root(
    root: &Path,
    inspect_entry: impl Fn(&Path) -> std::io::Result<fs::Metadata>,
) -> Result<PathBuf, RepositoryError> {
    let mut worktree_root = root;
    for ancestor in root.ancestors() {
        match inspect_entry(&ancestor.join(".git")) {
            Ok(_) => worktree_root = ancestor,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(RepositoryError::Root {
                    path: root.to_path_buf(),
                    source,
                });
            }
        }
    }

    Ok(worktree_root.to_path_buf())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    rustix_fs::accessat(
        rustix_fs::CWD,
        path,
        rustix_fs::Access::EXEC_OK,
        rustix_fs::AtFlags::EACCESS,
    )
    .is_ok()
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    fn executable(path: &Path) {
        fs::write(path, "#!/bin/sh\nexit 0\n").expect("executable fixture should be written");
        let mut permissions = fs::metadata(path)
            .expect("executable metadata should exist")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("fixture should be executable");
    }

    #[cfg(unix)]
    #[test]
    fn validates_and_canonicalizes_host_repository_configuration() {
        // Arrange
        let parent = tempdir().expect("temporary parent should exist");
        let root = parent.path().join("repository");
        let executable_path = parent.path().join("git");
        fs::create_dir(&root).expect("repository fixture should exist");
        executable(&executable_path);
        let linked_executable = parent.path().join("git-link");
        symlink(&executable_path, &linked_executable).expect("executable symlink should exist");

        // Act
        let repository = Repository::new(&root, &linked_executable)
            .expect("valid host configuration should be accepted");

        // Assert
        assert_eq!(
            repository.root(),
            root.canonicalize()
                .expect("repository fixture should canonicalize")
        );
        assert_eq!(
            repository.git_executable(),
            executable_path
                .canonicalize()
                .expect("executable fixture should canonicalize")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_untrusted_git_executable_configurations() {
        // Arrange
        let parent = tempdir().expect("temporary parent should exist");
        let root = parent.path().join("repository");
        fs::create_dir(&root).expect("repository fixture should exist");
        let directory = parent.path().join("git-directory");
        fs::create_dir(&directory).expect("directory fixture should exist");
        let inert = parent.path().join("inert-git");
        fs::write(&inert, "not executable").expect("inert fixture should be written");
        let inside = root.join("git");
        executable(&inside);
        let administrative_root = parent.path().join(".GIT");
        fs::create_dir(&administrative_root).expect("administrative root fixture should exist");

        // Act
        let relative = Repository::new(&root, "git");
        let missing = Repository::new(&root, parent.path().join("missing"));
        let non_file = Repository::new(&root, &directory);
        let non_executable = Repository::new(&root, &inert);
        let inside_repository = Repository::new(&root, &inside);
        let missing_root = Repository::new(parent.path().join("missing-root"), &inside);
        let file_root = Repository::new(&inert, &inside);
        let git_root = Repository::new(&administrative_root, &inside);

        // Assert
        assert!(matches!(
            relative,
            Err(RepositoryError::GitExecutableNotAbsolute { .. })
        ));
        assert!(
            matches!(missing, Err(RepositoryError::GitExecutable { .. })),
            "unexpected missing executable result: {missing:?}"
        );
        assert!(matches!(
            non_file,
            Err(RepositoryError::GitExecutableNotFile { .. })
        ));
        assert!(matches!(
            non_executable,
            Err(RepositoryError::GitExecutableNotExecutable { .. })
        ));
        assert!(matches!(
            inside_repository,
            Err(RepositoryError::GitExecutableInsideRepository { .. })
        ));
        assert!(matches!(missing_root, Err(RepositoryError::Root { .. })));
        assert!(matches!(file_root, Err(RepositoryError::Root { .. })));
        assert!(matches!(
            git_root,
            Err(RepositoryError::RootIsGitAdministrative { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_git_executable_inside_containing_worktree() {
        // Arrange
        let parent = tempdir().expect("temporary parent should exist");
        let worktree = parent.path().join("checkout");
        let root = worktree.join("crate");
        fs::create_dir_all(worktree.join(".git"))
            .expect("worktree and administrative directory should exist");
        fs::create_dir(&root).expect("nested repository root should exist");
        let executable_path = worktree.join("fake-git");
        executable(&executable_path);
        let canonical_worktree = worktree
            .canonicalize()
            .expect("worktree fixture should canonicalize");

        // Act
        let result = Repository::new(&root, &executable_path);

        // Assert
        assert!(matches!(
            result,
            Err(RepositoryError::GitExecutableInsideRepository {
                root: rejected_root,
                ..
            }) if rejected_root == canonical_worktree
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_git_symlink_located_inside_containing_worktree() {
        // Arrange
        let parent = tempdir().expect("temporary parent should exist");
        let worktree = parent.path().join("checkout");
        let root = worktree.join("crate");
        fs::create_dir_all(worktree.join(".git"))
            .expect("worktree and administrative directory should exist");
        fs::create_dir(&root).expect("nested repository root should exist");
        let executable_path = parent.path().join("git");
        executable(&executable_path);
        let linked_executable = worktree.join("git-link");
        symlink(&executable_path, &linked_executable).expect("executable symlink should exist");

        // Act
        let result = Repository::new(&root, &linked_executable);

        // Assert
        assert!(matches!(
            result,
            Err(RepositoryError::GitExecutableInsideRepository { path, .. })
                if path == linked_executable
        ));
    }

    #[test]
    fn rejects_uninspectable_containing_worktree_boundary() {
        // Arrange
        let repository = tempdir().expect("temporary repository root should exist");
        let root = repository
            .path()
            .canonicalize()
            .expect("repository root should canonicalize");
        let expected_root = root.clone();

        // Act
        let result = containing_worktree_root(&root, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        });

        // Assert
        assert!(matches!(
            result,
            Err(RepositoryError::Root { path, source })
                if path == expected_root && source.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn rejects_uninspectable_git_executable_parent() {
        // Arrange
        let executable = Path::new("/host/git");

        // Act
        let result = canonical_executable_parent(executable, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            ))
        });

        // Assert
        assert!(matches!(
            result,
            Err(RepositoryError::GitExecutable { path, source })
                if path == executable && source.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[test]
    fn validates_execute_permission_for_effective_identity() {
        // Arrange
        let parent = tempdir().expect("temporary parent should exist");
        let root = parent.path().join("repository");
        fs::create_dir(&root).expect("repository fixture should exist");
        let executable_path = parent.path().join("git");
        executable(&executable_path);
        let mut permissions = fs::metadata(&executable_path)
            .expect("executable metadata should exist")
            .permissions();
        permissions.set_mode(u32::from(!rustix::process::geteuid().is_root()));
        fs::set_permissions(&executable_path, permissions)
            .expect("fixture should not be executable by the effective identity");

        // Act
        let result = Repository::new(&root, &executable_path);

        // Assert
        assert!(matches!(
            result,
            Err(RepositoryError::GitExecutableNotExecutable { .. })
        ));
    }
}
