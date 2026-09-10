//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model loop, normalized completion
//! metadata, validated structured output, and deny-by-default repository
//! inspection and patch tools. Provider and local filesystem implementations
//! remain behind injectable boundaries.

mod chat_completion;
mod file_system;
mod harness;
mod lifecycle;
mod model;
mod policy;
mod provider;
mod read;
mod repository;
mod schema_contract;
mod session;
mod telemetry;
mod tool;
mod trace;
mod turn;
mod write;

pub use file_system::{FileSystem, LocalFileSystem};
pub use harness::{Harness, Session, SessionBuilder};
pub use lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, LifecycleObserverSet,
    LifecycleOperationGuard, ModelResponseType, ToolErrorType, TurnErrorType,
};
pub use model::{
    CompletionMetadata, CompletionUsage, Model, ModelClient, ModelCompletion, ModelError,
    ModelErrorType, ModelMessage, ModelMetadata, ModelMetadataError, ModelRequest, ModelResponse,
    ReasoningEffort,
};
pub use provider::{
    KIMI_K2_6, KimiConfig, MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, ModelConfiguration,
    ModelConfigurationError, ModelProvider, ModelProviderParseError, Muse, MuseConfig, QWEN_PLUS,
    QwenConfig,
};
pub use read::{ReadError, ReadOutput};
pub use repository::{Repository, RepositoryError};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use session::{SessionError, SessionInfo};
pub use telemetry::LifecycleMetrics;
pub use tool::{
    ReadAction, ReadArguments, ReadSide, Tool, ToolCall, ToolCallArguments, ToolDefinition,
    WriteArguments,
};
pub use trace::LifecycleTraceObserver;
pub use turn::{ModelRequestActivity, ToolActivity, TurnError, TurnOutcome, TurnReport};
pub use write::{WriteError, WriteOutput};
