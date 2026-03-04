//! Semantic conventions for qontinui observability.
//!
//! Provides standardized span names, attribute keys, and phase identifiers
//! inspired by OpenTelemetry conventions. All span names use a `qontinui.`
//! prefix hierarchy for grep-ability and namespace isolation.
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::semantic_conventions::{spans, attributes, phases};
//!
//! // Reference canonical span names
//! assert_eq!(spans::WORKFLOW_EXECUTE, "qontinui.workflow");
//!
//! // Use attribute key constants
//! tracing::info!({ %execution_id }, "starting workflow");
//! ```

/// Canonical span name constants.
///
/// All names use a `qontinui.` prefix hierarchy. The `SUMMARY_SPANS` array
/// lists spans that should be persisted to SQLite for AI analysis.
pub mod spans {
    // =========================================================================
    // Workflow spans
    // =========================================================================

    /// Root span for an entire workflow execution.
    pub const WORKFLOW_EXECUTE: &str = "qontinui.workflow";

    /// Setup phase of a workflow.
    pub const PHASE_SETUP: &str = "qontinui.workflow.phase.setup";

    /// Verification phase of a workflow.
    pub const PHASE_VERIFICATION: &str = "qontinui.workflow.phase.verification";

    /// Agentic phase of a workflow.
    pub const PHASE_AGENTIC: &str = "qontinui.workflow.phase.agentic";

    /// Completion phase of a workflow.
    pub const PHASE_COMPLETION: &str = "qontinui.workflow.phase.completion";

    /// The verification-agentic loop within a workflow execution.
    pub const VERIFICATION_LOOP: &str = "qontinui.workflow.verification_loop";

    /// A single iteration of the workflow loop.
    pub const WORKFLOW_LOOP: &str = "qontinui.workflow.loop";

    /// Execution of a single workflow step.
    pub const STEP_EXECUTE: &str = "qontinui.workflow.step";

    // =========================================================================
    // AI spans
    // =========================================================================

    /// An AI session (e.g., Claude conversation).
    pub const AI_SESSION: &str = "qontinui.ai.session";

    // =========================================================================
    // Python subprocess spans
    // =========================================================================

    /// Python interpreter startup.
    pub const PYTHON_STARTUP: &str = "qontinui.python.startup";

    /// A Python command execution.
    pub const PYTHON_COMMAND: &str = "qontinui.python.command";

    // =========================================================================
    // API spans
    // =========================================================================

    /// An HTTP API request handled by the runner.
    pub const API_REQUEST: &str = "qontinui.api.request";

    // =========================================================================
    // Image recognition spans
    // =========================================================================

    /// An image recognition operation.
    pub const IMAGE_RECOGNITION: &str = "qontinui.image.recognition";

    // =========================================================================
    // Summary spans configuration
    // =========================================================================

    /// Span names that should be persisted to SQLite for AI analysis.
    ///
    /// This is the canonical list used by `SpanLayerConfig::default()`.
    /// Only "summary" spans are stored in the database to keep it lean.
    pub const SUMMARY_SPANS: &[&str] = &[
        // Critical path
        WORKFLOW_EXECUTE,
        PHASE_SETUP,
        PHASE_VERIFICATION,
        PHASE_AGENTIC,
        PHASE_COMPLETION,
        VERIFICATION_LOOP,
        WORKFLOW_LOOP,
        // Hot path
        AI_SESSION,
        // Diagnostic
        PYTHON_STARTUP,
        PYTHON_COMMAND,
    ];
}

/// Standard attribute key constants for span fields.
pub mod attributes {
    // =========================================================================
    // Common
    // =========================================================================

    /// Unique execution/task-run identifier.
    pub const EXECUTION_ID: &str = "execution_id";

    /// Human-readable workflow name.
    pub const WORKFLOW_NAME: &str = "workflow_name";

    // =========================================================================
    // Phase
    // =========================================================================

    /// Current workflow phase (setup, verification, agentic, completion).
    pub const WORKFLOW_PHASE: &str = "workflow.phase";

    /// Current loop iteration number.
    pub const WORKFLOW_ITERATION: &str = "workflow.iteration";

    /// Number of steps in the current phase.
    pub const WORKFLOW_STEP_COUNT: &str = "workflow.step_count";

    // =========================================================================
    // Step
    // =========================================================================

    /// Name of the workflow step.
    pub const STEP_NAME: &str = "step.name";

    /// Type of the workflow step (ai_session, shell_command, etc.).
    pub const STEP_TYPE: &str = "step.type";

    /// Index of the step within the phase.
    pub const STEP_INDEX: &str = "step.index";

    // =========================================================================
    // AI
    // =========================================================================

    /// AI model identifier.
    pub const AI_MODEL: &str = "ai.model";

    /// AI provider name (claude_cli, claude_api, etc.).
    pub const AI_PROVIDER: &str = "ai.provider";

    /// Phase context for the AI session.
    pub const AI_PHASE: &str = "ai.phase";

    // =========================================================================
    // Python
    // =========================================================================

    /// Python command being executed.
    pub const PYTHON_COMMAND: &str = "python.command";

    /// Timeout for the Python command in milliseconds.
    pub const PYTHON_TIMEOUT_MS: &str = "python.timeout_ms";

    // =========================================================================
    // API
    // =========================================================================

    /// API endpoint path.
    pub const API_ENDPOINT: &str = "api.endpoint";

    /// HTTP method (GET, POST, etc.).
    pub const API_METHOD: &str = "api.method";

    // =========================================================================
    // Error
    // =========================================================================

    /// Type of error (e.g., "TypeError", "ConnectionError").
    pub const ERROR_TYPE: &str = "error.type";

    /// Severity level of the error.
    pub const ERROR_SEVERITY: &str = "error.severity";

    /// Source of the error (e.g., "python", "rust", "javascript").
    pub const ERROR_SOURCE: &str = "error.source";

    // =========================================================================
    // Trace propagation
    // =========================================================================

    /// Trace ID for cross-service correlation.
    pub const TRACE_ID: &str = "trace_id";

    /// Parent span ID for hierarchical correlation.
    pub const PARENT_SPAN_ID: &str = "parent_span_id";
}

/// Phase value constants.
pub mod phases {
    /// Setup phase.
    pub const SETUP: &str = "setup";

    /// Verification phase.
    pub const VERIFICATION: &str = "verification";

    /// Agentic phase.
    pub const AGENTIC: &str = "agentic";

    /// Completion phase.
    pub const COMPLETION: &str = "completion";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summary_spans_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for span in spans::SUMMARY_SPANS {
            assert!(seen.insert(*span), "Duplicate summary span: {}", span);
        }
    }

    #[test]
    fn test_all_summary_spans_have_qontinui_prefix() {
        for span in spans::SUMMARY_SPANS {
            assert!(
                span.starts_with("qontinui."),
                "Summary span '{}' missing 'qontinui.' prefix",
                span
            );
        }
    }
}
