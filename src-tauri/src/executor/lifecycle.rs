use super::state::ExecutorState;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Maximum time to wait for Python executor to send READY signal
/// Increased to 30 seconds to accommodate slow Python startup on Windows
#[allow(dead_code)]
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Event queue capacity (max events buffered during initialization)
const EVENT_QUEUE_CAPACITY: usize = 100;

/// Represents a queued event waiting to be processed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedEvent {
    /// Event type
    pub event_type: String,
    /// Event data as JSON
    pub data: serde_json::Value,
    /// Timestamp when event was queued
    pub queued_at: u64,
}

/// Message types from Python executor
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ExecutorMessage {
    /// Executor is ready to accept commands
    Ready {
        /// Optional metadata about the executor
        data: Option<serde_json::Value>,
    },

    /// Executor event (state change, action completed, etc.)
    Event {
        /// Event type identifier
        event: String,
        /// Event data
        data: serde_json::Value,
        /// Event timestamp
        timestamp: f64,
        /// Event sequence number
        sequence: u32,
    },

    /// Tree-based hierarchical event
    #[serde(rename = "tree_event")]
    TreeEvent {
        /// Tree event type (workflow_started, action_completed, etc.)
        event_type: String,
        /// Node data
        node: serde_json::Value,
        /// Path from root to this node
        path: Vec<serde_json::Value>,
        /// Event timestamp
        timestamp: f64,
        /// Event sequence number
        sequence: u32,
    },

    /// Response to a command
    Response {
        /// Command ID this is responding to
        id: String,
        /// Whether command succeeded
        success: bool,
        /// Response data
        data: Option<serde_json::Value>,
        /// Error message if failed
        error: Option<String>,
    },

    /// Health check response (pong)
    Pong {
        /// Timestamp when pong was sent
        timestamp: f64,
    },

    /// Error from executor
    Error {
        /// Error message
        message: String,
        /// Additional error details
        details: Option<String>,
    },

    /// Image recognition event with debug data
    #[serde(rename = "image_recognition")]
    ImageRecognition {
        /// Image recognition data (pattern info, matches, debug details)
        data: serde_json::Value,
    },
}

/// Result of a workflow execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionCompletionResult {
    /// Whether the execution was successful
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Additional data from the execution
    pub data: Option<serde_json::Value>,
}

/// Manages executor lifecycle and state transitions
pub struct ExecutorLifecycle {
    /// Current executor state
    state: Arc<RwLock<ExecutorState>>,

    /// Event queue for events received during initialization
    event_queue: Arc<RwLock<Vec<QueuedEvent>>>,

    /// Channel for notifying when ready state is reached
    ready_tx: Option<tokio::sync::oneshot::Sender<()>>,

    /// Channel for notifying when execution completes
    execution_complete_tx:
        Arc<RwLock<Option<tokio::sync::oneshot::Sender<ExecutionCompletionResult>>>>,

    /// Whether lifecycle has been started
    #[allow(dead_code)]
    started: Arc<RwLock<bool>>,
}

impl ExecutorLifecycle {
    /// Creates a new lifecycle manager
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ExecutorState::Initializing {
                started_at: current_timestamp(),
            })),
            event_queue: Arc::new(RwLock::new(Vec::new())),
            ready_tx: None,
            execution_complete_tx: Arc::new(RwLock::new(None)),
            started: Arc::new(RwLock::new(false)),
        }
    }

    /// Registers a oneshot channel to receive execution completion notification.
    /// Returns a receiver that will receive the result when execution completes.
    pub async fn register_execution_completion(
        &self,
    ) -> tokio::sync::oneshot::Receiver<ExecutionCompletionResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut lock = self.execution_complete_tx.write().await;
        *lock = Some(tx);
        rx
    }

    /// Notifies any waiting completion channel that execution has completed.
    /// Called when an execution_completed event is received.
    pub async fn notify_execution_complete(&self, result: ExecutionCompletionResult) {
        let mut lock = self.execution_complete_tx.write().await;
        if let Some(tx) = lock.take() {
            if tx.send(result).is_err() {
                debug!("Execution completion receiver was dropped");
            }
        }
    }

    /// Returns the current state
    pub async fn get_state(&self) -> ExecutorState {
        let state = self.state.read().await.clone();
        debug!(
            "[LIFECYCLE] get_state() called, returning: {}",
            state.name()
        );
        state
    }

    /// Sets a new state with validation
    pub async fn set_state(&self, new_state: ExecutorState) -> Result<(), AppError> {
        let mut state = self.state.write().await;

        // Validate transition
        state
            .can_transition_to(&new_state)
            .map_err(|e| AppError::StateError(format!("Invalid state transition: {}", e)))?;

        let old_state = state.clone();
        *state = new_state.clone();

        info!(
            "Executor state transition: {} -> {}",
            old_state.name(),
            new_state.name()
        );

        Ok(())
    }

    /// Waits for the executor to reach Ready state with timeout
    #[allow(dead_code)]
    pub async fn wait_for_ready(&mut self) -> Result<(), AppError> {
        // Create oneshot channel for ready notification
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ready_tx = Some(tx);

        // Check if already ready
        {
            let state = self.state.read().await;
            if state.is_ready() {
                debug!("Executor already in ready state");
                return Ok(());
            }
        }

        // Wait for ready signal with timeout
        match timeout(READY_TIMEOUT, rx).await {
            Ok(Ok(())) => {
                info!("Executor reached ready state");
                Ok(())
            }
            Ok(Err(_)) => {
                error!("Ready notification channel closed unexpectedly");
                self.set_state(ExecutorState::failed(
                    "Ready notification channel closed".to_string(),
                ))
                .await?;
                Err(AppError::ProcessError(
                    "Ready notification channel closed".to_string(),
                ))
            }
            Err(_) => {
                error!("Timeout waiting for executor to be ready");
                self.set_state(ExecutorState::failed(
                    "Initialization timeout - executor did not send READY".to_string(),
                ))
                .await?;
                Err(AppError::ProcessError(
                    "Executor initialization timeout".to_string(),
                ))
            }
        }
    }

    /// Handles a message received from the Python executor
    pub async fn handle_message(
        &mut self,
        message: ExecutorMessage,
    ) -> Result<Option<ExecutorMessage>, AppError> {
        match message {
            ExecutorMessage::Ready { data } => {
                info!(
                    "[LIFECYCLE] Received READY message from executor: {:?}",
                    data
                );
                debug!(
                    "[LIFECYCLE] Current state before transition: {}",
                    self.state.read().await.name()
                );

                // Transition to Ready state
                debug!("[LIFECYCLE] Attempting transition to Ready state...");
                self.set_state(ExecutorState::ready()).await?;
                debug!("[LIFECYCLE] State transition to Ready completed successfully");
                debug!(
                    "[LIFECYCLE] Current state after transition: {}",
                    self.state.read().await.name()
                );

                // Notify ready channel if waiting
                if let Some(tx) = self.ready_tx.take() {
                    debug!("[LIFECYCLE] Notifying ready channel...");
                    let _ = tx.send(());
                }

                // Reconstruct the message since data was moved
                debug!("[LIFECYCLE] Ready handling complete, returning message");
                Ok(Some(ExecutorMessage::Ready { data }))
            }

            ExecutorMessage::Event {
                ref event,
                ref data,
                ..
            } => {
                // Check for execution_completed event and notify any waiters
                if event == "execution_completed" {
                    info!("[LIFECYCLE] Received execution_completed event");
                    let success = data
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let error = data
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| data.get("error").and_then(|v| v.as_str()).map(String::from));

                    let result = ExecutionCompletionResult {
                        success,
                        error,
                        data: Some(data.clone()),
                    };
                    self.notify_execution_complete(result).await;
                }

                // Check if we're ready to process events
                let state = self.state.read().await;

                if state.is_initializing() {
                    // Queue event for later processing
                    warn!("Received event during initialization, queuing for later");
                    self.queue_event(message.clone()).await;
                    Ok(None) // Don't forward yet
                } else if state.can_accept_commands() {
                    // Forward event immediately
                    Ok(Some(message))
                } else {
                    warn!(
                        "Received event in non-ready state: {}, dropping",
                        state.name()
                    );
                    Ok(None)
                }
            }

            ExecutorMessage::TreeEvent { .. } => {
                // Tree events are always forwarded (same logic as regular events)
                let state = self.state.read().await;

                if state.is_initializing() {
                    // Queue event for later processing
                    warn!("Received tree event during initialization, queuing for later");
                    self.queue_event(message.clone()).await;
                    Ok(None)
                } else if state.can_accept_commands() {
                    // Forward event immediately
                    Ok(Some(message))
                } else {
                    warn!(
                        "Received tree event in non-ready state: {}, dropping",
                        state.name()
                    );
                    Ok(None)
                }
            }

            ExecutorMessage::Error {
                message: msg,
                details,
            } => {
                error!("Executor error: {} - {:?}", msg, details);

                // Transition to failed state
                let error_msg = if let Some(ref details_str) = details {
                    format!("{}: {}", msg, details_str)
                } else {
                    msg.clone()
                };
                self.set_state(ExecutorState::failed(error_msg)).await?;

                Ok(Some(ExecutorMessage::Error {
                    message: msg,
                    details,
                }))
            }

            ExecutorMessage::Pong { .. } => {
                // Health check response, don't forward
                debug!("Received PONG from executor");
                Ok(None)
            }

            ExecutorMessage::ImageRecognition { .. } => {
                // Image recognition events are always forwarded
                let state = self.state.read().await;

                if state.is_initializing() {
                    // Queue event for later processing
                    warn!(
                        "Received image recognition event during initialization, queuing for later"
                    );
                    self.queue_event(message.clone()).await;
                    Ok(None)
                } else if state.can_accept_commands() {
                    // Forward event immediately
                    debug!("Forwarding IMAGE_RECOGNITION event");
                    Ok(Some(message))
                } else {
                    warn!(
                        "Received image recognition event in non-ready state: {}, dropping",
                        state.name()
                    );
                    Ok(None)
                }
            }

            _ => {
                // Forward other messages (Response, etc.)
                Ok(Some(message))
            }
        }
    }

    /// Queues an event received during initialization
    async fn queue_event(&self, message: ExecutorMessage) {
        let mut queue = self.event_queue.write().await;

        // Check queue capacity
        if queue.len() >= EVENT_QUEUE_CAPACITY {
            warn!(
                "Event queue at capacity ({}), dropping oldest event",
                EVENT_QUEUE_CAPACITY
            );
            queue.remove(0);
        }

        match message {
            ExecutorMessage::Event {
                event,
                data,
                timestamp: _,
                sequence: _,
            } => {
                queue.push(QueuedEvent {
                    event_type: event,
                    data,
                    queued_at: current_timestamp(),
                });
            }
            ExecutorMessage::ImageRecognition { data } => {
                queue.push(QueuedEvent {
                    event_type: "image_recognition".to_string(),
                    data,
                    queued_at: current_timestamp(),
                });
            }
            _ => {
                warn!("Attempted to queue non-event message type");
            }
        }
    }

    /// Retrieves and clears all queued events
    pub async fn drain_queued_events(&self) -> Vec<QueuedEvent> {
        let mut queue = self.event_queue.write().await;
        let events = queue.clone();
        queue.clear();

        if !events.is_empty() {
            info!("Draining {} queued events", events.len());
        }

        events
    }

    /// Marks execution as started (transitions to Running)
    #[allow(dead_code)]
    pub async fn mark_execution_started(&self) -> Result<(), AppError> {
        let state = self.state.read().await;

        if !state.is_ready() && !state.is_running() {
            return Err(AppError::StateError(format!(
                "Cannot start execution in {} state",
                state.name()
            )));
        }

        drop(state); // Release read lock
        self.set_state(ExecutorState::running()).await?;

        Ok(())
    }

    /// Marks execution as completed (transitions back to Ready)
    #[allow(dead_code)]
    pub async fn mark_execution_completed(&self) -> Result<(), AppError> {
        let state = self.state.read().await;

        if !state.is_running() {
            warn!(
                "mark_execution_completed called but state is {}",
                state.name()
            );
        }

        drop(state); // Release read lock
        self.set_state(ExecutorState::ready()).await?;

        Ok(())
    }

    /// Initiates graceful shutdown
    pub async fn shutdown(&self) -> Result<(), AppError> {
        info!("Initiating executor shutdown");
        self.set_state(ExecutorState::shutdown()).await?;
        Ok(())
    }

    /// Marks executor as failed
    pub async fn mark_failed(&self, error: String) -> Result<(), AppError> {
        error!("Marking executor as failed: {}", error);
        self.set_state(ExecutorState::failed(error)).await?;
        Ok(())
    }

    /// Returns whether the executor can accept commands
    #[allow(dead_code)]
    pub async fn can_accept_commands(&self) -> bool {
        self.state.read().await.can_accept_commands()
    }

    /// Returns whether the executor is in a terminal state
    #[allow(dead_code)]
    pub async fn is_terminal(&self) -> bool {
        self.state.read().await.is_terminal()
    }
}

impl Default for ExecutorLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses a JSON line from Python executor into ExecutorMessage
pub fn parse_executor_message(line: &str) -> Result<ExecutorMessage, AppError> {
    serde_json::from_str(line).map_err(|e| {
        error!("Failed to parse executor message: {} - Line: {}", e, line);
        AppError::JsonError(e)
    })
}

/// Returns current Unix timestamp in milliseconds
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("System time before Unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lifecycle_initialization() {
        let lifecycle = ExecutorLifecycle::new();
        let state = lifecycle.get_state().await;
        assert!(state.is_initializing());
    }

    #[tokio::test]
    async fn test_ready_transition() {
        let mut lifecycle = ExecutorLifecycle::new();

        // Simulate receiving READY message
        let ready_msg = ExecutorMessage::Ready { data: None };
        lifecycle.handle_message(ready_msg).await.unwrap();

        let state = lifecycle.get_state().await;
        assert!(state.is_ready());
    }

    #[tokio::test]
    async fn test_event_queueing_during_init() {
        let mut lifecycle = ExecutorLifecycle::new();

        // Send event while initializing
        let event_msg = ExecutorMessage::Event {
            event: "test".to_string(),
            data: serde_json::json!({}),
            timestamp: 0.0,
            sequence: 0,
        };

        let result = lifecycle.handle_message(event_msg).await.unwrap();
        assert!(result.is_none()); // Should be queued, not forwarded

        // Check queue
        let queued = lifecycle.drain_queued_events().await;
        assert_eq!(queued.len(), 1);
    }

    #[tokio::test]
    async fn test_execution_state_transitions() {
        let mut lifecycle = ExecutorLifecycle::new();

        // Transition to ready
        let ready_msg = ExecutorMessage::Ready { data: None };
        lifecycle.handle_message(ready_msg).await.unwrap();

        // Start execution
        lifecycle.mark_execution_started().await.unwrap();
        assert!(lifecycle.get_state().await.is_running());

        // Complete execution
        lifecycle.mark_execution_completed().await.unwrap();
        assert!(lifecycle.get_state().await.is_ready());
    }

    #[tokio::test]
    async fn test_error_handling() {
        let mut lifecycle = ExecutorLifecycle::new();

        let error_msg = ExecutorMessage::Error {
            message: "Test error".to_string(),
            details: None,
        };

        lifecycle.handle_message(error_msg).await.unwrap();

        let state = lifecycle.get_state().await;
        assert!(state.is_terminal());
    }
}
