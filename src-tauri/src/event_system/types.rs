//! Event types for the centralized event emission system.
//!
//! This module defines the unified event types that are emitted to the frontend
//! and WebSocket clients.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Re-export FlowEvent from the flow executor module
pub use crate::orchestrator::flow_executor::FlowEvent as FlowEventData;

/// Unified application events for frontend communication.
///
/// All events emitted to the Tauri frontend should go through this enum
/// to ensure consistent handling and error management.
#[derive(Debug, Clone, Serialize, JsonSchema)]
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
    // Deferred Feedback Events
    // ========================================================================
    /// A deferred question was created during autonomous execution.
    /// The system continued without waiting — this is for real-time visibility.
    DeferredQuestionCreated {
        /// Task run ID
        task_run_id: String,
        /// Deferred question ID
        question_id: String,
        /// Iteration when the question was raised
        iteration: u32,
        /// The question text
        question: String,
        /// Confidence score (0.0-1.0)
        confidence: f64,
        /// Risk level
        risk_level: String,
    },

    /// A deferred question was reviewed by a human (post-run or mid-run).
    DeferredQuestionReviewed {
        /// Task run ID
        task_run_id: String,
        /// Deferred question ID
        question_id: String,
        /// Review status: "approved" or "rejected"
        status: String,
        /// Whether rework was triggered
        rework_triggered: bool,
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
    // Cost Management Events
    // ========================================================================
    /// Real-time cost update after each AI call.
    CostUpdate {
        task_run_id: String,
        phase: String,
        iteration: Option<u32>,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        cost_usd: f64,
        cumulative_cost_usd: f64,
        cache_hit_rate: f64,
        timestamp: i64,
    },

    /// Budget warning when consumption exceeds 80%.
    BudgetWarning {
        task_run_id: String,
        remaining_fraction: f64,
        total_cost_usd: f64,
        budget_limit_usd: f64,
        message: String,
        timestamp: i64,
    },

    /// Cost anomaly detected via statistical analysis.
    CostAnomaly {
        task_run_id: String,
        cost_usd: f64,
        mean_cost_usd: f64,
        std_dev: f64,
        z_score: f64,
        message: String,
        timestamp: i64,
    },

    // ========================================================================
    // Accessibility Events
    // ========================================================================
    /// Accessibility backend connected to a target.
    AccessibilityConnected {
        /// Name of the backend that was connected (e.g., "uia", "cdp", "atspi", "ax").
        backend: String,
        /// Target descriptor (e.g., "desktop", window title, "pid:1234"), if known.
        target: Option<String>,
        /// Unix timestamp in milliseconds.
        timestamp: i64,
    },

    /// Accessibility tree capture completed.
    AccessibilityCaptureComplete {
        /// Name of the backend that performed the capture.
        backend: String,
        /// Number of nodes in the captured snapshot.
        node_count: u32,
        /// Duration of the capture in milliseconds.
        duration_ms: u64,
        /// Unix timestamp in milliseconds.
        timestamp: i64,
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

/// All distinct event channel names produced by `AppEvent::event_name()`.
///
/// Hand-maintained to match the match arms in `AppEvent::event_name()`. Several
/// variants map to the same channel (`executor-event`, `extraction-event`, etc.)
/// — those are deduplicated here.
///
/// This constant is exposed over HTTP (`GET /ui-bridge/sdk/tauri-event-names`)
/// so SDK clients can subscribe to runner-emitted Tauri events by name.
///
/// The unit test `every_event_name_is_in_all_event_names` in this file
/// asserts drift: if anyone adds a variant without updating this list, CI
/// fails.
pub const ALL_EVENT_NAMES: &[&str] = &[
    // Executor
    "executor-event",
    "executor-error",
    "executor-response",
    // Extraction
    "extraction-event",
    "extraction-error",
    "extraction-response",
    // RAG
    "rag-progress",
    "rag-completion",
    // Flow
    "flow-event",
    // AI output
    "ai-output",
    // Findings
    "finding_detected",
    "finding_resolved",
    // Navigation
    "test-navigation",
    "ui-bridge-request",
    // Orchestrator
    "orchestrator-state-change",
    "step-progress",
    "task-run-update",
    // Approval
    "approval-required",
    "approval-resolved",
    // Deferred feedback
    "deferred-question-created",
    "deferred-question-reviewed",
    // Canvas
    "canvas-update",
    // AI streaming
    "ai-output-chunk",
    // Convergence
    "iteration-metrics",
    // Blame
    "blame-attribution",
    // Constraints
    "constraint-results",
    // Queue
    "workflow-queued",
    "workflow-dequeued",
    // Cost
    "cost-update",
    "budget-warning",
    "cost-anomaly",
    // Accessibility
    "a11y-connected",
    "a11y-capture-complete",
    // Generic
    "error",
];

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

            // Deferred feedback events
            AppEvent::DeferredQuestionCreated { .. } => "deferred-question-created",
            AppEvent::DeferredQuestionReviewed { .. } => "deferred-question-reviewed",

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

            // Cost management events
            AppEvent::CostUpdate { .. } => "cost-update",
            AppEvent::BudgetWarning { .. } => "budget-warning",
            AppEvent::CostAnomaly { .. } => "cost-anomaly",

            // Accessibility events
            AppEvent::AccessibilityConnected { .. } => "a11y-connected",
            AppEvent::AccessibilityCaptureComplete { .. } => "a11y-capture-complete",

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

    /// Create a cost update event.
    #[allow(clippy::too_many_arguments)]
    pub fn cost_update(
        task_run_id: impl Into<String>,
        phase: impl Into<String>,
        iteration: Option<u32>,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        cost_usd: f64,
        cumulative_cost_usd: f64,
    ) -> Self {
        let cache_total = cache_read_tokens + cache_creation_tokens + input_tokens;
        let cache_hit_rate = if cache_total > 0 {
            cache_read_tokens as f64 / cache_total as f64
        } else {
            0.0
        };
        AppEvent::CostUpdate {
            task_run_id: task_run_id.into(),
            phase: phase.into(),
            iteration,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            cost_usd,
            cumulative_cost_usd,
            cache_hit_rate,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create a budget warning event.
    pub fn budget_warning(
        task_run_id: impl Into<String>,
        remaining_fraction: f64,
        total_cost_usd: f64,
        budget_limit_usd: f64,
        message: impl Into<String>,
    ) -> Self {
        AppEvent::BudgetWarning {
            task_run_id: task_run_id.into(),
            remaining_fraction,
            total_cost_usd,
            budget_limit_usd,
            message: message.into(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create a cost anomaly event.
    pub fn cost_anomaly(
        task_run_id: impl Into<String>,
        cost_usd: f64,
        mean_cost_usd: f64,
        std_dev: f64,
        z_score: f64,
    ) -> Self {
        AppEvent::CostAnomaly {
            task_run_id: task_run_id.into(),
            cost_usd,
            mean_cost_usd,
            std_dev,
            z_score,
            message: format!(
                "Cost anomaly: ${:.4} is {:.1} std devs above mean ${:.4}",
                cost_usd, z_score, mean_cost_usd
            ),
            timestamp: chrono::Utc::now().timestamp_millis(),
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

    /// Create a deferred question created event.
    pub fn deferred_question_created(
        task_run_id: impl Into<String>,
        question_id: impl Into<String>,
        iteration: u32,
        question: impl Into<String>,
        confidence: f64,
        risk_level: impl Into<String>,
    ) -> Self {
        AppEvent::DeferredQuestionCreated {
            task_run_id: task_run_id.into(),
            question_id: question_id.into(),
            iteration,
            question: question.into(),
            confidence,
            risk_level: risk_level.into(),
        }
    }

    /// Create a deferred question reviewed event.
    pub fn deferred_question_reviewed(
        task_run_id: impl Into<String>,
        question_id: impl Into<String>,
        status: impl Into<String>,
        rework_triggered: bool,
    ) -> Self {
        AppEvent::DeferredQuestionReviewed {
            task_run_id: task_run_id.into(),
            question_id: question_id.into(),
            status: status.into(),
            rework_triggered,
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

    /// Create an accessibility-connected event.
    pub fn accessibility_connected(backend: impl Into<String>, target: Option<String>) -> Self {
        AppEvent::AccessibilityConnected {
            backend: backend.into(),
            target,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Create an accessibility-capture-complete event.
    pub fn accessibility_capture_complete(
        backend: impl Into<String>,
        node_count: u32,
        duration_ms: u64,
    ) -> Self {
        AppEvent::AccessibilityCaptureComplete {
            backend: backend.into(),
            node_count,
            duration_ms,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// Payload structure for executor events (for serialization compatibility).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorEventPayload {
    pub event_type: String,
    pub event: String,
    pub timestamp: i64,
    pub sequence: u32,
    pub data: serde_json::Value,
}

/// Payload structure for executor responses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

    /// Drift guard: every variant's `event_name()` must be listed in
    /// `ALL_EVENT_NAMES`, and the set of names in `ALL_EVENT_NAMES` must
    /// exactly equal the set of names produced by the enum.
    ///
    /// Add new `AppEvent` variants → this test forces you to add an arm to
    /// both `event_name()` and `ALL_EVENT_NAMES`.
    #[test]
    fn every_event_name_is_in_all_event_names() {
        use std::collections::{BTreeSet, HashMap};

        // Construct one concrete instance of every `AppEvent` variant.
        // Fields use minimal / default values — the test only exercises
        // `event_name()`, not serialization.
        let events: Vec<AppEvent> = vec![
            // Executor
            AppEvent::ExecutorEvent {
                event: String::new(),
                timestamp: 0,
                sequence: 0,
                data: serde_json::Value::Null,
            },
            AppEvent::ExecutorTreeEvent {
                event_type: String::new(),
                node: serde_json::Value::Null,
                path: Vec::new(),
                timestamp: 0,
                sequence: 0,
            },
            AppEvent::ExecutorError {
                message: String::new(),
                details: None,
            },
            AppEvent::ExecutorResponse {
                id: String::new(),
                success: true,
                data: None,
                error: None,
            },
            AppEvent::ImageRecognition {
                data: serde_json::Value::Null,
            },
            // Extraction
            AppEvent::ExtractionEvent {
                event: String::new(),
                timestamp: 0,
                sequence: 0,
                data: serde_json::Value::Null,
            },
            AppEvent::ExtractionError {
                message: String::new(),
                details: None,
            },
            AppEvent::ExtractionResponse {
                id: String::new(),
                success: true,
                data: None,
                error: None,
            },
            // RAG
            AppEvent::RagProgress {
                project_id: String::new(),
                status: String::new(),
                message: String::new(),
                percent: None,
                elements_processed: None,
                total_elements: None,
                error: None,
            },
            AppEvent::RagCompletion {
                project_id: String::new(),
                success: true,
                total_processed: 0,
                successful: 0,
                failed: 0,
                web_sync_success: false,
                web_sync_error: None,
            },
            // Flow
            AppEvent::FlowEvent(FlowEventData::FlowStarted {
                instance_id: String::new(),
                flow_id: String::new(),
                flow_name: String::new(),
            }),
            // AI output
            AppEvent::AiOutput {
                session_id: String::new(),
                content: String::new(),
                content_type: None,
            },
            // Findings
            AppEvent::FindingDetected {
                finding: serde_json::Value::Null,
            },
            AppEvent::FindingResolved {
                finding: serde_json::Value::Null,
            },
            // Navigation
            AppEvent::TestNavigation {
                data: serde_json::Value::Null,
            },
            AppEvent::UiBridgeRequest {
                data: serde_json::Value::Null,
            },
            // Orchestrator
            AppEvent::OrchestratorStateChange {
                task_run_id: String::new(),
                workflow_stage: String::new(),
                iteration: 0,
                phase: String::new(),
                state_data: None,
            },
            AppEvent::StepProgress {
                task_run_id: String::new(),
                step_index: 0,
                step_name: String::new(),
                status: String::new(),
                details: None,
                timestamp: 0,
            },
            AppEvent::TaskRunUpdate {
                task_run_id: String::new(),
                status: String::new(),
                iteration: None,
                details: None,
                timestamp: 0,
            },
            // Approval
            AppEvent::ApprovalRequired {
                task_run_id: String::new(),
                approval_id: String::new(),
                iteration: 0,
                prompt: String::new(),
            },
            AppEvent::ApprovalResolved {
                task_run_id: String::new(),
                approval_id: String::new(),
                approved: true,
                action: String::new(),
            },
            // Deferred feedback
            AppEvent::DeferredQuestionCreated {
                task_run_id: String::new(),
                question_id: String::new(),
                iteration: 0,
                question: String::new(),
                confidence: 0.0,
                risk_level: String::new(),
            },
            AppEvent::DeferredQuestionReviewed {
                task_run_id: String::new(),
                question_id: String::new(),
                status: String::new(),
                rework_triggered: false,
            },
            // Canvas
            AppEvent::CanvasUpdate {
                action: String::new(),
                panel_id: String::new(),
                panel: None,
                task_run_id: None,
            },
            // AI streaming
            AppEvent::AiOutputChunk {
                task_run_id: String::new(),
                chunk: String::new(),
                accumulated_length: 0,
            },
            // Convergence
            AppEvent::IterationMetrics {
                task_run_id: String::new(),
                iteration: 0,
                failed_step_count: 0,
                passed_step_count: 0,
                skipped_step_count: 0,
                new_failures: 0,
                repeated_failures: 0,
                is_stalled: false,
            },
            // Blame
            AppEvent::BlameAttribution {
                task_run_id: String::new(),
                iteration: 0,
                attributed_failures: 0,
                oscillating_files: 0,
                revert_patterns: 0,
                blame_json: String::new(),
            },
            // Constraints
            AppEvent::ConstraintResults {
                task_run_id: String::new(),
                iteration: 0,
                summary: String::new(),
                has_blocking: false,
                results: serde_json::Value::Null,
            },
            // Queue
            AppEvent::WorkflowQueued {
                task_run_id: String::new(),
                workflow_name: String::new(),
                queue_position: 0,
            },
            AppEvent::WorkflowDequeued {
                task_run_id: String::new(),
                workflow_name: String::new(),
                wait_time_ms: 0,
            },
            // Cost
            AppEvent::CostUpdate {
                task_run_id: String::new(),
                phase: String::new(),
                iteration: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cost_usd: 0.0,
                cumulative_cost_usd: 0.0,
                cache_hit_rate: 0.0,
                timestamp: 0,
            },
            AppEvent::BudgetWarning {
                task_run_id: String::new(),
                remaining_fraction: 0.0,
                total_cost_usd: 0.0,
                budget_limit_usd: 0.0,
                message: String::new(),
                timestamp: 0,
            },
            AppEvent::CostAnomaly {
                task_run_id: String::new(),
                cost_usd: 0.0,
                mean_cost_usd: 0.0,
                std_dev: 0.0,
                z_score: 0.0,
                message: String::new(),
                timestamp: 0,
            },
            // Accessibility
            AppEvent::AccessibilityConnected {
                backend: String::new(),
                target: None,
                timestamp: 0,
            },
            AppEvent::AccessibilityCaptureComplete {
                backend: String::new(),
                node_count: 0,
                duration_ms: 0,
                timestamp: 0,
            },
            // Generic
            AppEvent::Error {
                message: String::new(),
                context: None,
            },
        ];

        // Every produced name must be in ALL_EVENT_NAMES.
        for e in &events {
            let name = e.event_name();
            assert!(
                ALL_EVENT_NAMES.contains(&name),
                "event name '{}' produced by enum is not declared in ALL_EVENT_NAMES",
                name,
            );
        }

        // Count how many variants we instantiated per name, and confirm every
        // name in ALL_EVENT_NAMES is produced by at least one variant.
        let mut produced_counts: HashMap<&'static str, u32> = HashMap::new();
        for e in &events {
            *produced_counts.entry(e.event_name()).or_insert(0) += 1;
        }
        let produced: BTreeSet<&'static str> = produced_counts.keys().copied().collect();
        let declared: BTreeSet<&'static str> = ALL_EVENT_NAMES.iter().copied().collect();

        // Detect drift in either direction.
        let missing_in_declared: Vec<&'static str> =
            produced.difference(&declared).copied().collect();
        let missing_in_produced: Vec<&'static str> =
            declared.difference(&produced).copied().collect();
        assert!(
            missing_in_declared.is_empty(),
            "enum produces names that ALL_EVENT_NAMES is missing: {:?}",
            missing_in_declared,
        );
        assert!(
            missing_in_produced.is_empty(),
            "ALL_EVENT_NAMES declares names that no enum variant produces: {:?}",
            missing_in_produced,
        );

        // ALL_EVENT_NAMES must have no duplicates.
        assert_eq!(
            ALL_EVENT_NAMES.len(),
            declared.len(),
            "ALL_EVENT_NAMES contains duplicate entries",
        );
    }
}
