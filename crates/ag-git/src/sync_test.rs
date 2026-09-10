use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use mockall::Sequence;
use mockall::predicate::function;
use tempfile::tempdir;

use super::*;
use crate::repo::MockAsyncGitCommandRunner;

#[path = "sync_test/support_test.rs"]
mod support;

use support::{run_git_command, *};

#[path = "sync_test/diff_test.rs"]
mod diff;
#[path = "sync_test/hook_test.rs"]
mod hook;
#[path = "sync_test/remote_test.rs"]
mod remote;
#[path = "sync_test/worktree_test.rs"]
mod worktree;
