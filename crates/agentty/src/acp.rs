//! ACP (Agent Client Protocol) connection management.
//!
//! Each session gets a dedicated OS thread running a single-threaded tokio
//! runtime with `LocalSet`. The main multi-threaded runtime communicates
//! via an `mpsc` channel.

use std::path::PathBuf;

use agent_client_protocol::{
    Agent as _, CancelNotification, ClientCapabilities, ContentBlock, Implementation,
    InitializeRequest, NewSessionRequest, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, SessionId,
    SessionNotification, SessionUpdate, TextContent, ToolCall,
};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use crate::agent::AgentKind;
use crate::app::AgentEvent;

/// Commands sent from the main runtime to the session thread.
enum AcpCommand {
    Prompt {
        text: String,
        reply_tx: oneshot::Sender<Result<PromptResponse, String>>,
    },
    Cancel {
        reply_tx: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

/// Handle to a long-lived ACP agent process running on a dedicated thread.
pub struct AcpSessionHandle {
    command_tx: mpsc::Sender<AcpCommand>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl AcpSessionHandle {
    /// Spawns an ACP agent process on a dedicated thread and returns a handle.
    ///
    /// The blocking wait for the agent to become ready is offloaded via
    /// `spawn_blocking` so this can be called from an async context without
    /// starving the tokio runtime.
    ///
    /// # Errors
    /// Returns an error if the agent process cannot be started or ACP
    /// initialization fails.
    pub async fn spawn(
        agent: AgentKind,
        folder: PathBuf,
        model: &str,
        agent_tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel::<AcpCommand>(32);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let model = model.to_string();

        let thread_handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("Failed to create tokio runtime: {error}"));
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));

                    return;
                }
            };
            let local_set = tokio::task::LocalSet::new();

            local_set.block_on(&runtime, async move {
                let result =
                    Self::run_session_loop(agent, folder, &model, agent_tx, command_rx, &ready_tx)
                        .await;
                if let Err(error) = result {
                    let _ = ready_tx.send(Err(error));
                }
            });
        });

        tokio::task::spawn_blocking(move || {
            ready_rx
                .recv()
                .map_err(|_| "Session thread terminated before ready".to_string())?
                .map_err(|error| format!("ACP initialization failed: {error}"))
        })
        .await
        .map_err(|error| format!("Join error: {error}"))??;

        Ok(Self {
            command_tx,
            thread_handle: Some(thread_handle),
        })
    }

    /// Sends a prompt to the agent and waits for a response.
    ///
    /// # Errors
    /// Returns an error if the command channel is closed or the agent returns
    /// an error.
    pub async fn prompt(&self, text: &str) -> Result<PromptResponse, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(AcpCommand::Prompt {
                text: text.to_string(),
                reply_tx,
            })
            .await
            .map_err(|_| "ACP session thread gone".to_string())?;

        reply_rx
            .await
            .map_err(|_| "ACP session thread dropped reply".to_string())?
    }

    /// Cancels the current operation.
    ///
    /// # Errors
    /// Returns an error if the command channel is closed or the cancellation
    /// fails.
    pub async fn cancel(&self) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(AcpCommand::Cancel { reply_tx })
            .await
            .map_err(|_| "ACP session thread gone".to_string())?;

        reply_rx
            .await
            .map_err(|_| "ACP session thread dropped reply".to_string())?
    }

    /// Shuts down the ACP session and waits for the thread to exit.
    pub async fn shutdown(&self) {
        let _ = self.command_tx.send(AcpCommand::Shutdown).await;
    }

    async fn run_session_loop(
        agent: AgentKind,
        folder: PathBuf,
        model: &str,
        agent_tx: mpsc::UnboundedSender<AgentEvent>,
        mut command_rx: mpsc::Receiver<AcpCommand>,
        ready_tx: &std::sync::mpsc::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        // Spawn agent process
        let mut child = Self::spawn_agent_process(agent, &folder, model)?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture agent stdout".to_string())?;
        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to capture agent stdin".to_string())?;

        // Create ACP connection.
        // outgoing_bytes = stdin (we write to agent), incoming_bytes = stdout (we read
        // from agent)
        let client = AgenttyClient {
            agent_tx: agent_tx.clone(),
        };
        let (connection, io_future) = agent_client_protocol::ClientSideConnection::new(
            client,
            child_stdin.compat_write(),
            child_stdout.compat(),
            |future| {
                tokio::task::spawn_local(future);
            },
        );
        tokio::task::spawn_local(async move {
            let _ = io_future.await;
        });

        // Initialize connection
        let init_request = InitializeRequest::new(agent_client_protocol::ProtocolVersion::LATEST)
            .client_capabilities(ClientCapabilities::default())
            .client_info(Implementation::new("agentty", env!("CARGO_PKG_VERSION")));
        connection
            .initialize(init_request)
            .await
            .map_err(|error| format!("ACP initialize failed: {error}"))?;

        // Create session
        let new_session_request = NewSessionRequest::new(folder);
        let new_session_response = connection
            .new_session(new_session_request)
            .await
            .map_err(|error| format!("ACP new_session failed: {error}"))?;
        let session_id = new_session_response.session_id;

        // Signal ready
        let _ = ready_tx.send(Ok(()));

        // Command loop
        loop {
            let Some(command) = command_rx.recv().await else {
                break;
            };

            match command {
                AcpCommand::Prompt { text, reply_tx } => {
                    let result = Self::send_prompt(&connection, &session_id, &text).await;
                    let _ = reply_tx.send(result);
                }
                AcpCommand::Cancel { reply_tx } => {
                    let result = connection
                        .cancel(CancelNotification::new(session_id.clone()))
                        .await
                        .map_err(|error| format!("ACP cancel failed: {error}"));
                    let _ = reply_tx.send(result);
                }
                AcpCommand::Shutdown => break,
            }
        }

        // Clean up: kill the child process
        let _ = child.kill().await;

        Ok(())
    }

    fn spawn_agent_process(
        agent: AgentKind,
        folder: &PathBuf,
        model: &str,
    ) -> Result<tokio::process::Child, String> {
        let mut cmd = tokio::process::Command::new(agent.acp_command());
        for arg in agent.acp_args() {
            cmd.arg(arg);
        }
        // Pass model via environment variable for agents that support it
        match agent {
            AgentKind::Claude => {
                cmd.env("ANTHROPIC_MODEL", model);
            }
            AgentKind::Gemini | AgentKind::Codex => {}
        }

        cmd.current_dir(folder)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "Failed to start {}. {error}. {}",
                    agent.acp_command(),
                    agent.install_hint()
                )
            })
    }

    /// Sends a prompt and waits for the response. Streaming output is handled
    /// by the `AgenttyClient::session_notification` callback.
    async fn send_prompt(
        connection: &agent_client_protocol::ClientSideConnection,
        session_id: &SessionId,
        text: &str,
    ) -> Result<PromptResponse, String> {
        let prompt_request = PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(text.to_string()))],
        );

        connection
            .prompt(prompt_request)
            .await
            .map_err(|error| format!("ACP prompt failed: {error}"))
    }
}

impl Drop for AcpSessionHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Extracts displayable text from a `SessionUpdate` variant.
fn extract_text_from_update(update: &SessionUpdate) -> Option<String> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => extract_text_from_content_block(&chunk.content),
        SessionUpdate::ToolCall(tool_call) => Some(format_tool_call(tool_call)),
        _ => None,
    }
}

/// Extracts text from a `ContentBlock`.
fn extract_text_from_content_block(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(text_content) => {
            if text_content.text.is_empty() {
                None
            } else {
                Some(text_content.text.clone())
            }
        }
        _ => None,
    }
}

/// Formats a tool call notification for display.
fn format_tool_call(tool_call: &ToolCall) -> String {
    format!("\n\u{27e1} {}\n", &tool_call.title)
}

/// ACP client implementation for agentty.
///
/// Handles streaming session notifications by appending text to the shared
/// output buffer. Auto-approves permission requests since agentty delegates
/// all tool decisions to the agent.
struct AgenttyClient {
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
}

#[async_trait(?Send)]
impl agent_client_protocol::Client for AgenttyClient {
    async fn session_notification(
        &self,
        notification: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        let session_id = notification.session_id.to_string();
        let text = extract_text_from_update(&notification.update);
        if let Some(text) = text {
            let _ = self.agent_tx.send(AgentEvent::Output { session_id, text });
        }

        Ok(())
    }

    async fn request_permission(
        &self,
        request: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        // Auto-approve the first option (agent has full autonomy in agentty)
        let outcome = if let Some(first_option) = request.options.first() {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                first_option.option_id.clone(),
            ))
        } else {
            RequestPermissionOutcome::Cancelled
        };

        Ok(RequestPermissionResponse::new(outcome))
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::ContentChunk;

    use super::*;

    #[test]
    fn test_extract_text_from_agent_message_chunk() {
        // Arrange
        let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("Hello world".to_string()),
        )));

        // Act
        let text = extract_text_from_update(&update);

        // Assert
        assert_eq!(text, Some("Hello world".to_string()));
    }

    #[test]
    fn test_extract_text_from_agent_message_chunk_empty() {
        // Arrange
        let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new(String::new()),
        )));

        // Act
        let text = extract_text_from_update(&update);

        // Assert
        assert_eq!(text, None);
    }

    #[test]
    fn test_extract_text_from_tool_call() {
        // Arrange
        let update = SessionUpdate::ToolCall(ToolCall::new(
            "tc-1".to_string(),
            "Edit file.rs".to_string(),
        ));

        // Act
        let text = extract_text_from_update(&update);

        // Assert
        assert_eq!(text, Some("\n\u{27e1} Edit file.rs\n".to_string()));
    }

    #[test]
    fn test_extract_text_from_thought_chunk_returns_none() {
        // Arrange
        let update = SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("thinking...".to_string()),
        )));

        // Act
        let text = extract_text_from_update(&update);

        // Assert
        assert_eq!(text, None);
    }

    #[test]
    fn test_format_tool_call_with_title() {
        // Arrange
        let tool_call = ToolCall::new("tc-1".to_string(), "Read main.rs".to_string());

        // Act
        let formatted = format_tool_call(&tool_call);

        // Assert
        assert_eq!(formatted, "\n\u{27e1} Read main.rs\n");
    }

    #[test]
    fn test_agentty_client_session_notification_appends_text() {
        // Arrange
        let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
        let client = AgenttyClient { agent_tx };
        let session_id = SessionId::new("test-session".to_string());
        let notification = SessionNotification::new(
            session_id,
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("streamed text".to_string()),
            ))),
        );

        // Act
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build runtime");
        let local_set = tokio::task::LocalSet::new();
        local_set.block_on(&runtime, async {
            use agent_client_protocol::Client as _;
            let _ = client.session_notification(notification).await;
        });

        // Assert
        let event = agent_rx.blocking_recv().expect("failed to receive event");
        if let AgentEvent::Output { text, .. } = event {
            assert_eq!(text, "streamed text");
        } else {
            panic!("unexpected event type");
        }
    }
}
