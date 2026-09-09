use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::sync::Mutex;

use ag_agent::{AgentKind, ReasoningLevel, SpeedMode};
use ag_git::MockGitClient;
use ag_protocol::VerificationVerdictItem;
use ag_session::{
    AnswerQuestionsRequest, ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    Session, SessionBackend, SessionError,
};
use ag_store::{MockOrchestrationRepository, PersistedSessionCreation};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::*;

#[path = "coordinator_test/support_test.rs"]
mod support;

use support::*;

#[path = "coordinator_test/contract_test.rs"]
mod contract;
#[path = "coordinator_test/dispatch_test.rs"]
mod dispatch;
#[path = "coordinator_test/integration_test.rs"]
mod integration;
#[path = "coordinator_test/planning_test.rs"]
mod planning;
#[path = "coordinator_test/recovery_test.rs"]
mod recovery;
#[path = "coordinator_test/research_test.rs"]
mod research;
#[path = "coordinator_test/review_test.rs"]
mod review;
#[path = "coordinator_test/rollup_test.rs"]
mod rollup;
#[path = "coordinator_test/state_test.rs"]
mod state;
#[path = "coordinator_test/verification_test.rs"]
mod verification;
