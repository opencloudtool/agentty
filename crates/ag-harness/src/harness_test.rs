use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use async_trait::async_trait;
use mockall::Sequence;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::io::AsyncRead;
use tokio::sync::Notify;

use super::*;
use crate::file_system::MockFileSystem;
use crate::lifecycle::TurnErrorType;
use crate::model::ModelMessage;
use crate::tool::{ReadArguments, ToolDefinition, WriteArguments};

#[path = "harness_test/support_test.rs"]
mod support;

use support::*;

#[path = "harness_test/read_test.rs"]
mod read;

#[path = "harness_test/session_test.rs"]
mod session;

#[path = "harness_test/recovery_test.rs"]
mod recovery;

#[path = "harness_test/lease_test.rs"]
mod lease;

#[path = "harness_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "harness_test/write_test.rs"]
mod write;

#[path = "harness_test/policy_test.rs"]
mod policy;
