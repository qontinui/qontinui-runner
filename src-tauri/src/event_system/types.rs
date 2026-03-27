//! Event types for the centralized event emission system.
//!
//! This module defines the unified event types that are emitted to the frontend
//! and WebSocket clients.

use serde::{Deserialize, Serialize};

// Re-export FlowEvent from the flow executor module
pub use crate::orchestrator::flow_executor::FlowEvent as FlowEventData;

/// Unified application events for frontend communication.
///
/// All events emitted to the Tauri frontend should go through this enum
/// to ensure consistent handling and error management.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type", content = "data")]
pub enum AppEvent {
    // ========================================================================
    // Executor Events
    // ========================================================================
    /// Standard executor event with data payload.
    ExecutorEvent {
        event: String,
        timestamp: i64,
        sequence: u32,
        data: serde_json::Value,
    },

    /// Tree event from workflow execution.
    ExecutorTreeEvent {
        event_type: String,
        node: serde_json::Value,
        path: Vec<String>,
        timestamp: i64,
        sequence: u32,
    },

    /// Executor error event.
    ExecutorError {
        message: String,
        details: Option<String>,
    },

    /// Executor response for command completion.
    ExecutorResponse {
        id: String,
        success: bool,
        data: Option<serde_json::Value>,
        error: Option<String>,
    },

    /// Image recognition result event.
    ImageRecognition { data: serde_json::Value },

    // ========================================================================
    // Extraction Events
    // ========================================================================
    /// Web extraction event with data payload.
    ExtractionEvent {
        event: String,
        timestamp: i64,
        sequence: u32,
        data: serde_json::Value,
    },

    /// Web extraction error event.
    ExtractionError {
        message: String,
        details: Option<String>,
    },

    /// Web extraction response.
    ExtractionResponse {
        id: String,
        success: bool,
        data: Option<serde_json::Value>,
        error: Option<String>,
    },

    // ========================================================================
    // RAG Events
    // ========================================================================
    /// RAG processing progress update.
    RagProgress {
        project_id: String,
        status: String,
        message: String,
        percent: Option<i32>,
        elements_processed: Option<i32>,
        total_elements: Option<i32>,
        error: Option<String>,
    },

    /// RAG processing completion event.
    RagCompletion {
        project_id: String,
        success: bool,
        total_processed: i32,
        successful: i32,
        failed: i32,
        web_sync_success: bool,
        web_sync_error: Option<String>,
    },

    // ========================================================================
    // Flow Events
    // ========================================================================
    /// Flow execution event (wraps FlowEventData).
    FlowEvent(FlowEventData),

    // ========================================================================
    // AI Output Events
    // ========================================================================
    /// AI session output event.
    AiOutput {
        session_id: String,
        content: String,
        content_type: Option<String>,
    },

    // ========================================================================
    // Findings Events
    // ========================================================================
    /// Finding detected during AI analysis.
    FindingDetected { finding: serde_json::Value },

    /// Finding resolved.
    FindingResolved { finding: serde_json::Value },

    // ========================================================================
    // Navigation Events
    // ========================================================================
    /// Test navigation event for UI.
    TestNavigation { data: serde_json::Value },

    /// UI bridge request event.
    UiBridgeRequest { data: serde_json::Value },

    // ========================================================================
    // Orchestrator State Events
    // ========================================================================
    /// Orchestrator state change event for real-time UI updates.
    /// This allows the frontend to receive push notifications instead of polling.
    OrchestratorStateChange {
        /// Task run ID
        task_run_id: String,
        /// Current workflow stage name
        workflow_stage: String,
        /// Current iteration number
        iteration: u32,
        /// Current phase (setup, verification, agentic, completion)
        phase: String,
        /// Optional additional state data
        state_data: Option<serde_json::Value>,
    },

    /// Step progress event for tracking progress through individual steps.
    /// Broadcast via both Tauri and WebSocket for real-time step tracking.
    StepProgress {
        /// Task run ID
        task_run_id: String,
        /// Step index (0-based)
        step_index: usize,
        /// Step name/description
        step_name: String,
        /// Status: "started", "running", "completed", "failed", "skipped"
        status: String,
        /// Optional details about the step
        details: Option<serde_json::Value>,
        /// Timestamp in milliseconds
        timestamp: i64,
    },

    /// Task run update event for status changes.
    /// Broadcast via both Tauri and WebSocket for real-time task tracking.
    TaskRunUpdate {
        /// Task run ID
        task_run_id: String,
        /// Status: "running", "completed", "failed", "stopped", "paused"
        status: String,
        /// Current iteration (if applicable)
        iteration: Option<u32>,
        /// Optional additional details
        details: Option<serde_json::Value>,
        /// Timestamp in milliseconds
        timestamp: i64,
    },

    // ========================================================================
    // Approval Events
    // ========================================================================
    /// Approval required — workflow is paused waiting for human review.
    ApprovalRequired {
        /// Task run ID
        task_run_id: String,
        /// Approval request ID
        approval_id: String,
        /// Current iteration
        iteration: u32,
        /// Prompt shown to the reviewer
        prompt: String,
    },

    /// Approval resolved — human responded to approval request.
    ApprovalResolved {
        /// Task run ID
        task_run_id: String,
        /// Approval request ID
        approval_id: String,
        /// Whether approved
        approved: bool,
        /// Action taken
        action: String,
    },

    // ========================================================================
    // Canvas Events
    // ========================================================================
    /// Canvas panel created, updated, or removed.
    CanvasUpdate {
        action: String,
        panel_id: String,
        panel: Option<serde_json::Value>,
        task_run_id: Option<String>,
    },

    // ========================================================================
    // AI Output Streaming Events
    // ========================================================================
    /// Real-time AI output chunk for live streaming to the frontend.
    AiOutputChunk {
        /// Task run ID this output belongs to
        task_run_id: String,
        /// The text chunk received from the AI
        chunk: String,
        /// Total accumulated output length so far
        accumulated_length: usize,
    },

    // ========================================================================
    // Convergence Tracking Events
    // ========================================================================
    /// Per-iteration metrics for tracking convergence of the verification-agentic loop.
    IterationMetrics {
        /// Task run ID
        task_run_id: String,
        /// Current iteration number (1-indexed)
        iteration: u32,
        /// Number of verification steps that failed
        failed_step_count: u32,
        /// Number of verification steps that passed
        passed_step_count: u32,
        /// Number of verification steps that were skipped
        skipped_step_count: u32,
        /// Failures not present in the previous iteration
        new_failures: u32,
        /// Failures that were also present in the previous iteration
        repeated_failures: u32,
        /// True if failed_step_count has not decreased in 3 consecutive iterations
        is_stalled: bool,
    },

    // ========================================================================
    // Blame Attribution Events
    // ========================================================================
    /// Blame attribution results from the blame engine.
    /// Emitted when verification failures are attributed to specific iterations.
    BlameAttribution {
        /// Task run ID
        task_run_id: String,
        /// Current iteration number
        iteration: u32,
        /// Number of failures with blame attributions
        attributed_failures: u32,
        /// Number of files exhibiting oscillation (modified 3+ consecutive iterations)
        oscillating_files: u32,
        /// Number of files exhibiting revert patterns
        revert_patterns: u32,
        /// Full blame report as JSON
        blame_json: String,
    },

    // ========================================================================
    // Constraint Engine Events
    // ========================================================================
    /// Constraint evaluation results after an agentic phase.
    ConstraintResults {
        /// Task run ID
        task_run_id: String,
        /// Current iteration number (1-indexed)
        iteration: u32,
        /// Human-readable summary of results
        summary: String,
        /// Whether any blocking violations exist
        has_blocking: bool,
        /// Serialized constraint results
        results: serde_json::Value,
    },

    // ========================================================================
    // Queue Events
    // ========================================================================
    /// Workflow has been added to the execution queue.
    WorkflowQueued {
        /// Task run ID for the queued workflow.
        task_run_id: String,
        /// Human-readable workflow name.
        workflow_name: String,
        /// Position in the queue (0-indexed).
        queue_position: usize,
    },

    /// Workflow has been dequeued and is starting execution.
    WorkflowDequeued {
        /// Task run ID for the dequeued workflow.
        task_run_id: String,
        /// Human-readable workflow name.
        workflow_name: String,
        /// Time spent waiting in the queue, in milliseconds.
        wait_time_ms: u64,
    },

    // ========================================================================
    // Generic Events
    // ========================================================================
    /// Generic error event.
    Error {
        message: String,
        context: Option<String>,
    },
}

impl AppEvent {
    /// Get the Tauri event channel name for this event type.
    ///
    /// This determines which frontend event listener will receive the event.
    pub fn event_name(&self) -> &'static str {
        match self {
            // Executor events
            AppEvent::ExecutorEvent { .. } => "executor-event",
            AppEvent::ExecutorTreeEvent { .. } => "executor-event",
            AppEvent::ExecutorError { .. } => "executor-error",
            AppEvent::ExecutorResponse { .. } => "executor-response",
            AppEvent::ImageRecognition { .. } => "executor-event",

            // Extraction events
            AppEvent::ExtractionEvent { .. } => "extraction-event",
            AppEvent::ExtractionError { .. } => "extraction-error",
            AppEvent::ExtractionResponse { .. } => "extraction-response",

            // RAG events
            AppEvent::RagProgress { .. } => "rag-progress",
            AppEvent::RagCompletion { .. } => "rag-completion",

            // Flow events
            AppEvent::FlowEvent(_) => "flow-event",

            // AI events
            AppEvent::AiOutput { .. } => "ai-output",

            // Findings events
            AppEvent::FindingDetected { .. } => "finding_detected",
            AppEvent::FindingResolved { .. } => "finding_resolved",

            // Navigation events
            AppEvent::TestNavigation { .. } => "test-navigation",
            AppEvent::UiBridgeRequest { .. } => "ui-bridge-request",

            // Orchestrator state events
            AppEvent::OrchestratorStateChange { .. } => "orchestrator-state-change",
            AppEvent::StepProgress { .. } => "step-progress",
            AppEvent::TaskRunUpdate { .. } => "task-run-update",

            // Approval events
            AppEvent::ApprovalRequired { .. } => "approval-required",
            AppEvent::ApprovalResolved { .. } => "approval-resolved",

            // Canvas events
            AppEvent::CanvasUpdate { .. } => "canvas-update",

            // AI output streaming events
            AppEvent::AiOutputChunk { .. } => "ai-output-chunk",

            // Convergence tracking events
            AppEvent::IterationMetrics { .. } => "iteration-metrics",

            // Blame attribution events
            AppEvent::BlameAttribution { .. } => "blame-attribution",

            // Constraint engine events
            AppEvent::ConstraintResults { .. } => "constraint-results",

            // Queue events
            AppEvent::WorkflowQueued { .. } => "workflow-queued",
            AppEvent::WorkflowDequeued { .. } => "workflow-dequeued",

            // Generic events
            AppEvent::Error { .. } => "error",
        }
    }

    /// Create an executor event.
    pub fn executor_event(event: impl Into<String>, data: serde_json::Value) -> Self {
        AppEvent::ExecutorEvent {
            event: event.into(),
            timestamp: chrono::Utc::now().timestamp_millis(),
            sequence: 0,
            data,
        }
    }

    /// Create an executor event with sequence number.
    pub fn executor_event_with_sequence(
        event: impl Into<String>,
        data: serde_json::Value,
        timestamp: i64,
        sequence: u32,
    ) -> Self {
        AppEvent::ExecutorEvent {
            event: event.into(),
            timestamp,
            sequence,
            data,
        }
    }

    /// Create an executor error event.
    pub fn executor_error(message: impl Into<String>) -> Self {
        AppEvent::ExecutorError {
            message: message.into(),
            details: None,
        }
    }

    /// Create an executor error event with details.
    pub fn executor_error_with_details(
        message: impl Into<String>,
        details: impl Into<String>,
    ) -> Self {
        AppEvent::ExecutorError {
            message: message.into(),
            details: Some(details.into()),
        }
    }

    /// Create a tree event.
    pub fn tree_event(
        event_type: impl Into<String>,
        node: serde_json::Value,
        path: Vec<String>,
        timestamp: i64,
        sequence: u32,
    ) -> Self {
        AppEvent::ExecutorTreeEvent {
            event_type: event_type.into(),
            node,
            path,
            timestamp,
            sequence,
        }
    }

    /// Create an image recognition event.
    pub fn image_recognition(data: serde_json::Value) -> Self {
        AppEvent::ImageRecognition { data }
    }

    /// Create a RAG progress event.
    pub fn rag_progress(
        project_id: impl Into<String>,
        status: impl Into<String>,
        message: impl Into<String>,
        percent: Option<i32>,
    ) -> Self {
        AppEvent::RagProgress {
            project_id: project_id.into(),
            status: status.into(),
            message: message.into(),
            percent,
            elements_processed: None,
            total_elements: None,
            error: None,
        }
    }

    /// Create a RAG progress event with element counts.
    pub fn rag_progress_with_counts(
        project_id: impl Into<String>,
        status: impl Into<String>,
        message: impl Into<String>,
        percent: Option<i32>,
        elements_processed: Option<i32>,
        total_elements: Option<i32>,
    ) -> Self {
        AppEvent::RagProgress {
            project_id: project_id.into(),
            status: status.into(),
            message: message.into(),
            percent,
            elements_processed,
            total_elements,
            error: None,
        }
    }

    /// Create a RAG completion event.
    pub fn rag_completion(
        project_id: impl Into<String>,
        success: bool,
        total_processed: i32,
        successful: i32,
        failed: i32,
    ) -> Self {
        AppEvent::RagCompletion {
            project_id: project_id.into(),
            success,
            total_processed,
            successful,
            failed,
            web_sync_success: false,
            web_sync_error: None,
        }
    }

    /// Create a RAG completion event with web sync status.
    pub fn rag_completion_with_sync(
        project_id: impl Into<String>,
        success: bool,
        total_processed: i32,
        successful: i32,
        failed: i32,
        web_sync_success: bool,
        web_sync_error: Option<String>,
    ) -> Self {
        AppEvent::RagCompletion {
            project_id: project_id.into(),
            success,
            total_processed,
            successful,
            failed,
            web_sync_success,
            web_sync_error,
        }
    }

    /// Create a flow event.
    pub fn flow_event(event: FlowEventData) -> Self {
        AppEvent::FlowEvent(event)
    }

    /// Create an AI output event.
    pub fn ai_output(session_id: impl Into<String>, content: impl Into<String>) -> Self {
        AppEvent::AiOutput {
            session_id: session_id.into(),
            content: content.into(),
            content_type: None,
        }
    }

    /// Create an AI output event with content type.
    pub fn ai_output_with_type(
        session_id: impl Into<String>,
        content: impl Into<String>,
        content_type: impl Into<String>,
    ) -> Self {
        AppEvent::AiOutput {
            session_id: session_id.into(),
            content: content.into(),
            content_type: Some(content_type.into()),
        }
    }

    /// Create a workflow queued event.
    pub fn workflow_queued(
        task_run_id: impl Into<String>,
        workflow_name: impl Into<String>,
        queue_position: usize,
    ) -> Self {
        AppEvent::WorkflowQueued {
            task_run_id: task_run_id.into(),
            workflow_name: workflow_name.into(),
            queue_position,
        }
    }

    /// Create a workflow dequeued event.
    pub fn workflow_dequeued(
        task_run_id: impl Into<String>,
        workflow_name: impl Into<String>,
        wait_time_ms: u64,
    ) -> Self {
        AppEvent::WorkflowDequeued {
            task_run_id: task_run_id.into(),
            workflow_name: workflow_name.into(),
            wait_time_ms,
        }
    }

    /// Create an AI output chunk event for real-time streaming.
    pub fn ai_output_chunk(
        task_run_id: impl Into<String>,
        chunk: impl Into<String>,
        accumulated_length: usize,
    ) -> Self {
        AppEvent::AiOutputChunk {
            task_run_id: task_run_id.into(),
            chunk: chunk.into(),
            accumulated_length,
        }
    }

    /// Create an iteration metrics event for convergence tracking.
    pub fn iteration_metrics(
        task_run_id: impl Into<String>,
        iteration: u32,
        failed_step_count: u32,
        passed_step_count: u32,
        skipped_step_count: u32,
        new_failures: u32,
        repeated_failures: u32,
        is_stalled: bool,
    ) -> Self {
        AppEvent::IterationMetrics {
            task_run_id: task_run_id.into(),
            iteration,
            failed_step_count,
            passed_step_count,
            skipped_step_count,
            new_failures,
            repeated_failures,
            is_stalled,
        }
    }

    /// Create a blame attribution event.
    pub fn blame_attribution(
        task_run_id: impl Into<String>,
        iteration: u32,
        attributed_failures: u32,
        oscillating_files: u32,
        revert_patterns: u32,
        blame_json: impl Into<String>,
    ) -> Self {
        AppEvent::BlameAttribution {
            task_run_id: task_run_id.into(),
            iteration,
            attributed_failures,
            oscillating_files,
            revert_patterns,
            blame_json: blame_json.into(),
        }
    }

    /// Create a constraint results event.
    pub fn constraint_results(
        task_run_id: impl Into<String>,
        iteration: u32,
        summary: impl Into<String>,
        has_blocking: bool,
        results: serde_json::Value,
    ) -> Self {
        AppEvent::ConstraintResults {
            task_run_id: task_run_id.into(),
            iteration,
            summary: summary.into(),
            has_blocking,
            results,
        }
    }

    /// Create a generic error event.
    pub fn error(message: impl Into<String>) -> Self {
        AppEvent::Error {
            message: message.into(),
            context: None,
        }
    }

    /// Create an error event with context.
    pub fn error_with_context(message: impl Into<String>, context: impl Into<String>) -> Self {
        AppEvent::Error {
            message: message.into(),
            context: Some(context.into()),
        }
    }

    /// Create an approval required event.
    pub fn approval_required(
        task_run_id: impl Into<String>,
        approval_id: impl Into<String>,
        iteration: u32,
        prompt: impl Into<String>,
    ) -> Self {
        AppEvent::ApprovalRequired {
            task_run_id: task_run_id.into(),
            approval_id: approval_id.into(),
            iteration,
            prompt: prompt.into(),
        }
    }

    /// Create an approval resolved event.
    pub fn approval_resolved(
        task_run_id: impl Into<String>,
        approval_id: impl Into<String>,
        approved: bool,
        action: impl Into<String>,
    ) -> Self {
        AppEvent::ApprovalResolved {
            task_run_id: task_run_id.into(),
            approval_id: approval_id.into(),
            approved,
            action: action.into(),
        }
    }

    /// Create an orchestrator state change event.
    pub fn orchestrator_state_change(
        task_run_id: impl Into<String>,
        workflow_stage: impl Into<String>,
        iteration: u32,
        phase: impl Into<String>,
    ) -> Self {
        AppEvent::OrchestratorStateChange {
            task_run_id: task_run_id.into(),
            workflow_stage: workflow_stage.into(),
            iteration,
            phase: phase.into(),
            state_data: None,
        }
    }

    /// Create an orchestrator state change event with additional state data.
    pub fn orchestrator_state_change_with_data(
        task_run_id: impl Into<String>,
        workflow_stage: impl Into<String>,
        iteration: u32,
        phase: impl Into<String>,
        state_data: serde_json::Value,
    ) -> Self {
        AppEvent::OrchestratorStateChange {
            task_run_id: task_run_id.into(),
            workflow_stage: workflow_stage.into(),
            iteration,
            phase: phase.into(),
            state_data: Some(state_data),
        }
    }

    /// Create a step progress event.
    pub fn step_progress(
        task_run_id: impl Into<String>,
        step_index: usize,
        step_name: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        AppEvent::StepProgress {
            task_run_id: task_run_id.into(),
            step_index,
            step_name: step_name.into(),
            status: status.into(),
            details: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create a step progress event with details.
    pub fn step_progress_with_details(
        task_run_id: impl Into<String>,
        step_index: usize,
        step_name: impl Into<String>,
        status: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        AppEvent::StepProgress {
            task_run_id: task_run_id.into(),
            step_index,
            step_name: step_name.into(),
            status: status.into(),
            details: Some(details),
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create a task run update event.
    pub fn task_run_update(task_run_id: impl Into<String>, status: impl Into<String>) -> Self {
        AppEvent::TaskRunUpdate {
            task_run_id: task_run_id.into(),
            status: status.into(),
            iteration: None,
            details: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create a task run update event with iteration.
    pub fn task_run_update_with_iteration(
        task_run_id: impl Into<String>,
        status: impl Into<String>,
        iteration: u32,
    ) -> Self {
        AppEvent::TaskRunUpdate {
            task_run_id: task_run_id.into(),
            status: status.into(),
            iteration: Some(iteration),
            details: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create a task run update event with full details.
    pub fn task_run_update_with_details(
        task_run_id: impl Into<String>,
        status: impl Into<String>,
        iteration: Option<u32>,
        details: serde_json::Value,
    ) -> Self {
        AppEvent::TaskRunUpdate {
            task_run_id: task_run_id.into(),
            status: status.into(),
            iteration,
            details: Some(details),
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// Payload structure for executor events (for serialization compatibility).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorEventPayload {
    pub event_type: String,
    pub event: String,
    pub timestamp: i64,
    pub sequence: u32,
    pub data: serde_json::Value,
}

/// Payload structure for executor responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorResponsePayload {
    pub resp_type: String,
    pub id: String,
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_names() {
        assert_eq!(
            AppEvent::executor_event("test", serde_json::json!({})).event_name(),
            "executor-event"
        );
        assert_eq!(
            AppEvent::executor_error("error").event_name(),
            "executor-error"
        );
        assert_eq!(
            AppEvent::rag_progress("proj", "status", "msg", None).event_name(),
            "rag-progress"
        );
    }

    #[test]
    fn test_executor_event_creation() {
        let event = AppEvent::executor_event("test_event", serde_json::json!({"key": "value"}));
        match event {
            AppEvent::ExecutorEvent { event, data, .. } => {
                assert_eq!(event, "test_event");
                assert_eq!(data["key"], "value");
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[test]
    fn test_error_event_with_context() {
        let event = AppEvent::error_with_context("Something failed", "During processing");
        match event {
            AppEvent::Error { message, context } => {
                assert_eq!(message, "Something failed");
                assert_eq!(context, Some("During processing".to_string()));
            }
            _ => panic!("Wrong event type"),
        }
    }
}
