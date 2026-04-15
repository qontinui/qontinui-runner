//! Schema Export Module
//!
//! Provides functions to export JSON Schemas for key application types.
//! Used by the `export_schemas` binary to produce schemas for cross-language
//! type generation (TypeScript, Python).
//!
//! The types defined here mirror the canonical types in the main application.
//! They are kept in sync by deriving `JsonSchema` on both and verifying schema
//! equivalence in tests. This separation exists because the main application
//! types live in `main.rs`'s module tree (not the lib crate) due to Tauri's
//! architecture.

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

// ============================================================================
// Mirror types for schema export
//
// These types mirror the canonical types in the main crate. They exist solely
// for JSON Schema generation. The generated schemas are the contract — not
// these Rust types. When the canonical types change, update these mirrors
// and regenerate.
// ============================================================================

// --- WorkerOutput (mirrors orchestrator::structured_output::WorkerOutput) ---

/// Structured output from an AI worker agent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerOutput {
    /// Summary of work performed in this iteration.
    pub work_summary: String,
    /// Signals for orchestrator control flow.
    #[serde(default)]
    pub signals: Vec<StructuredSignal>,
    /// Findings discovered during work.
    #[serde(default)]
    pub findings: Vec<StructuredFinding>,
    /// Files that were modified in this iteration.
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Criterion overrides with justifications.
    #[serde(default)]
    pub criterion_overrides: Vec<StructuredOverride>,
    /// Confidence level in the work quality.
    #[serde(default)]
    pub confidence: ConfidenceLevel,
    /// Optional suggestion for next action if work continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action_suggestion: Option<String>,
    /// Optional progress estimate (0.0 to 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_estimate: Option<f32>,
    /// Optional notes for debugging or context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A signal from the worker to the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredSignal {
    /// Signal type (e.g., "complete", "blocked", "needs_input").
    pub signal_type: String,
    /// Optional message providing context for the signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A finding discovered during work.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredFinding {
    /// Finding category (e.g., "bug", "security", "performance").
    pub category: String,
    /// Severity level.
    pub severity: String,
    /// Short title describing the finding.
    pub title: String,
    /// Detailed description.
    #[serde(default)]
    pub description: String,
    /// Whether this finding requires human input.
    #[serde(default)]
    pub needs_input: bool,
}

/// A criterion override with justification.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredOverride {
    /// The criterion being overridden.
    pub criterion: String,
    /// The new status.
    pub status: String,
    /// Justification for the override.
    pub justification: String,
}

/// Confidence level enum.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConfidenceLevel {
    High,
    #[default]
    Medium,
    Low,
}

// --- AgenticPhaseOutput (mirrors unified_workflow_executor::agentic_output) ---

/// Status of the agentic phase execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgenticStatus {
    Success,
    PartialSuccess,
    Failed,
    Unfixable,
}

/// A file change made during the agentic phase.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileChange {
    pub path: String,
    pub action: String,
}

/// A finding reported by the AI.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FindingOutput {
    pub category: String,
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub needs_input: bool,
    #[serde(default)]
    pub resolved: bool,
}

/// A reflection fix reported by the AI.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReflectionFixOutput {
    pub error_id: String,
    pub description: String,
}

/// Canonical structured output from the agentic phase.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgenticPhaseOutput {
    /// Overall status of the agentic phase.
    pub status: AgenticStatus,
    /// Human-readable summary of what was done.
    pub summary: String,
    /// AI's confidence that the fixes will pass verification (0.0-1.0).
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Whether the AI determined errors are unfixable.
    #[serde(default)]
    pub unfixable: bool,
    /// Reason why errors are unfixable (if unfixable is true).
    #[serde(default)]
    pub unfixable_reason: Option<String>,
    /// Files modified during this agentic phase.
    #[serde(default)]
    pub files_modified: Vec<FileChange>,
    /// Dynamically injected verification steps.
    #[serde(default)]
    pub injected_steps: Vec<Value>,
    /// Reflection fixes (when reflection_mode is enabled).
    #[serde(default)]
    pub reflection_fixes: Vec<ReflectionFixOutput>,
    /// Findings reported by the AI.
    #[serde(default)]
    pub findings: Vec<FindingOutput>,
}

// --- AppEvent (mirrors event_system::types::AppEvent) ---

/// Events emitted during flow execution for UI updates.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FlowEvent {
    FlowStarted {
        instance_id: String,
        flow_id: String,
        flow_name: String,
    },
    StepStarted {
        instance_id: String,
        step_id: String,
        step_name: String,
        step_type: String,
    },
    StepCompleted {
        instance_id: String,
        step_id: String,
        success: bool,
        outputs: HashMap<String, Value>,
        error: Option<String>,
        duration_ms: u64,
    },
    FlowCompleted {
        instance_id: String,
        flow_id: String,
        success: bool,
        error: Option<String>,
        total_steps: usize,
        duration_ms: u64,
    },
    WaitingForInput {
        instance_id: String,
        step_id: String,
        prompt: String,
        options: Vec<String>,
    },
    ParallelProgress {
        instance_id: String,
        step_id: String,
        completed: usize,
        total: usize,
    },
}

/// Unified application events for frontend communication.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "event_type", content = "data")]
pub enum AppEvent {
    ExecutorEvent {
        event: String,
        timestamp: i64,
        sequence: u32,
        data: Value,
    },
    ExecutorTreeEvent {
        event_type: String,
        node: Value,
        path: Vec<String>,
        timestamp: i64,
        sequence: u32,
    },
    ExecutorError {
        message: String,
        details: Option<String>,
    },
    ExecutorResponse {
        id: String,
        success: bool,
        data: Option<Value>,
        error: Option<String>,
    },
    ImageRecognition {
        data: Value,
    },
    ExtractionEvent {
        event: String,
        timestamp: i64,
        sequence: u32,
        data: Value,
    },
    ExtractionError {
        message: String,
        details: Option<String>,
    },
    ExtractionResponse {
        id: String,
        success: bool,
        data: Option<Value>,
        error: Option<String>,
    },
    RagProgress {
        project_id: String,
        status: String,
        message: String,
        percent: Option<i32>,
        elements_processed: Option<i32>,
        total_elements: Option<i32>,
        error: Option<String>,
    },
    RagCompletion {
        project_id: String,
        success: bool,
        total_processed: i32,
        successful: i32,
        failed: i32,
        web_sync_success: bool,
        web_sync_error: Option<String>,
    },
    FlowEvent(FlowEvent),
    AiOutput {
        session_id: String,
        content: String,
        content_type: Option<String>,
    },
    FindingDetected {
        finding: Value,
    },
    FindingResolved {
        finding: Value,
    },
    TestNavigation {
        data: Value,
    },
    UiBridgeRequest {
        data: Value,
    },
    OrchestratorStateChange {
        task_run_id: String,
        workflow_stage: String,
        iteration: u32,
        phase: String,
        state_data: Option<Value>,
    },
    StepProgress {
        task_run_id: String,
        step_index: usize,
        step_name: String,
        status: String,
        details: Option<Value>,
        timestamp: i64,
    },
    TaskRunUpdate {
        task_run_id: String,
        status: String,
        iteration: Option<u32>,
        details: Option<Value>,
        timestamp: i64,
    },
    ApprovalRequired {
        task_run_id: String,
        approval_id: String,
        iteration: u32,
        prompt: String,
    },
    ApprovalResolved {
        task_run_id: String,
        approval_id: String,
        approved: bool,
        action: String,
    },
    DeferredQuestionCreated {
        task_run_id: String,
        question_id: String,
        iteration: u32,
        question: String,
        confidence: f64,
        risk_level: String,
    },
    DeferredQuestionReviewed {
        task_run_id: String,
        question_id: String,
        status: String,
        rework_triggered: bool,
    },
    CanvasUpdate {
        action: String,
        panel_id: String,
        panel: Option<Value>,
        task_run_id: Option<String>,
    },
    AiOutputChunk {
        task_run_id: String,
        chunk: String,
        accumulated_length: usize,
    },
    IterationMetrics {
        task_run_id: String,
        iteration: u32,
        failed_step_count: u32,
        passed_step_count: u32,
        skipped_step_count: u32,
        new_failures: u32,
        repeated_failures: u32,
        is_stalled: bool,
    },
    BlameAttribution {
        task_run_id: String,
        iteration: u32,
        attributed_failures: u32,
        oscillating_files: u32,
        revert_patterns: u32,
        blame_json: String,
    },
    ConstraintResults {
        task_run_id: String,
        iteration: u32,
        summary: String,
        has_blocking: bool,
        results: Value,
    },
    WorkflowQueued {
        task_run_id: String,
        workflow_name: String,
        queue_position: usize,
    },
    WorkflowDequeued {
        task_run_id: String,
        workflow_name: String,
        wait_time_ms: u64,
    },
    Error {
        message: String,
        context: Option<String>,
    },
}

// ============================================================================
// Export function
// ============================================================================

/// Export all registered JSON Schemas as a single JSON object.
///
/// Keys are type names, values are full JSON Schema objects.
/// Used by the `export_schemas` binary and the type generation pipeline.
///
/// In addition to the runner-local mirror types above, this re-exports the
/// canonical DTO schemas from the `qontinui-types` crate
/// (`qontinui-schemas/rust/`). Those types are the source of truth for the
/// TypeScript and Python bindings; we emit schemas via `schema_for!` on the
/// remote types directly so there is no duplication.
pub fn export_all_schemas() -> Value {
    use qontinui_types::{
        constraints as qc, scheduler as qs, workflow as qw, workflow_step as qws,
    };

    // Built via a plain Map instead of `json!` to avoid the
    // `recursion_limit` ceiling that the `json!` macro hits on large
    // object literals.
    let mut m: Map<String, Value> = Map::new();

    macro_rules! add {
        ($name:literal, $ty:ty) => {
            m.insert(
                $name.to_string(),
                serde_json::to_value(schema_for!($ty)).unwrap(),
            );
        };
    }

    // ── Runner-local mirror types ──
    add!("WorkerOutput", WorkerOutput);
    add!("StructuredSignal", StructuredSignal);
    add!("StructuredFinding", StructuredFinding);
    add!("StructuredOverride", StructuredOverride);
    add!("ConfidenceLevel", ConfidenceLevel);
    add!("AgenticPhaseOutput", AgenticPhaseOutput);
    add!("AgenticStatus", AgenticStatus);
    add!("FileChange", FileChange);
    add!("FindingOutput", FindingOutput);
    add!("ReflectionFixOutput", ReflectionFixOutput);
    add!("FlowEvent", FlowEvent);
    add!("AppEvent", AppEvent);

    // ── qontinui-types: constraints ──
    add!("ConstraintSeverity", qc::ConstraintSeverity);
    add!("ConstraintCheck", qc::ConstraintCheck);
    add!("Constraint", qc::Constraint);
    add!("ConstraintResult", qc::ConstraintResult);
    add!("ConstraintViolation", qc::ConstraintViolation);
    add!("ResourceLimits", qc::ResourceLimits);
    add!("ConstraintProposal", qc::ConstraintProposal);
    add!("NewConstraintProposal", qc::NewConstraintProposal);
    add!("BuiltinOverrideProposal", qc::BuiltinOverrideProposal);
    add!("ValidateConfigRequest", qc::ValidateConfigRequest);
    add!("ValidateConfigResponse", qc::ValidateConfigResponse);
    add!("ReadConfigResponse", qc::ReadConfigResponse);
    add!("WriteConfigRequest", qc::WriteConfigRequest);
    add!("WriteConfigResponse", qc::WriteConfigResponse);

    // ── qontinui-types: scheduler ──
    add!("ScheduleExpression", qs::ScheduleExpression);
    add!("ConditionScheduleConfig", qs::ConditionScheduleConfig);
    add!("IdleCondition", qs::IdleCondition);
    add!("RepositoryWatch", qs::RepositoryWatch);
    add!(
        "RepositoryInactiveCondition",
        qs::RepositoryInactiveCondition
    );
    add!("ScheduleConditions", qs::ScheduleConditions);
    add!("ConditionStatus", qs::ConditionStatus);
    add!("ScheduledTaskType", qs::ScheduledTaskType);
    add!("ScheduledTaskStatus", qs::ScheduledTaskStatus);
    add!("TaskExecutionRecord", qs::TaskExecutionRecord);
    add!("ScheduledTask", qs::ScheduledTask);
    add!("SchedulerSettings", qs::SchedulerSettings);
    add!("SchedulerStatus", qs::SchedulerStatus);
    add!("NextTaskInfo", qs::NextTaskInfo);
    add!("CreateScheduledTaskRequest", qs::CreateScheduledTaskRequest);
    add!("UpdateScheduledTaskRequest", qs::UpdateScheduledTaskRequest);

    // ── qontinui-types: workflow frame ──
    add!("RoutingRule", qw::RoutingRule);
    add!("ModelOverrideConfig", qw::ModelOverrideConfig);
    add!("LogSourceSelection", qw::LogSourceSelection);
    add!("HealthCheckUrl", qw::HealthCheckUrl);
    add!("WorkflowArchitecture", qw::WorkflowArchitecture);
    add!("StageCondition", qw::StageCondition);
    add!("RetryPolicy", qw::RetryPolicy);
    add!("StageOutput", qw::StageOutput);
    add!("StageInput", qw::StageInput);
    add!("WorkflowStage", qw::WorkflowStage);
    add!("UnifiedWorkflow", qw::UnifiedWorkflow);

    // ── qontinui-types: workflow_step (canonical 4-variant step DTOs) ──
    // `CanonicalStep` is the strict 4-variant discriminated union.
    // `UnifiedStep` is the canonical-first, Value-fallback wrapper used for
    // payloads that may include runner-specific step types.
    add!("CanonicalStep", qws::CanonicalStep);
    add!("UnifiedStep", qws::UnifiedStep);
    add!("CommandStep", qws::CommandStep);
    add!("PromptStep", qws::PromptStep);
    add!("UiBridgeStep", qws::UiBridgeStep);
    add!("WorkflowStep", qws::WorkflowStep);
    add!("BaseStepFields", qws::BaseStepFields);
    add!("RetrySpec", qws::RetrySpec);
    add!("HttpMethod", qws::HttpMethod);
    add!("ApiContentType", qws::ApiContentType);
    add!("ApiAssertion", qws::ApiAssertion);
    add!("ApiAssertionType", qws::ApiAssertionType);
    add!("ApiAssertionOperator", qws::ApiAssertionOperator);
    add!("ApiVariableExtraction", qws::ApiVariableExtraction);
    add!("TestType", qws::TestType);
    add!("PlaywrightExecutionMode", qws::PlaywrightExecutionMode);
    add!("CheckType", qws::CheckType);
    add!("CommandMode", qws::CommandMode);
    add!("UiBridgeAction", qws::UiBridgeAction);
    add!("UiBridgeAssertType", qws::UiBridgeAssertType);
    add!("UiBridgeComparisonMode", qws::UiBridgeComparisonMode);
    add!("UiBridgeSeverity", qws::UiBridgeSeverity);
    add!("VerificationCategoryKind", qws::VerificationCategoryKind);
    add!("CommandStepPhase", qws::CommandStepPhase);
    add!("PromptStepPhase", qws::PromptStepPhase);
    add!("UiBridgeStepPhase", qws::UiBridgeStepPhase);
    add!("WorkflowStepPhase", qws::WorkflowStepPhase);

    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_all_schemas() {
        let schemas = export_all_schemas();
        let obj = schemas.as_object().unwrap();

        // Verify all expected types are present
        assert!(
            obj.contains_key("WorkerOutput"),
            "Missing WorkerOutput schema"
        );
        assert!(
            obj.contains_key("AgenticPhaseOutput"),
            "Missing AgenticPhaseOutput schema"
        );
        assert!(obj.contains_key("AppEvent"), "Missing AppEvent schema");
        assert!(obj.contains_key("FlowEvent"), "Missing FlowEvent schema");
        assert_eq!(obj.len(), 80, "Expected 80 schema entries");

        // Sanity-check that qontinui_types re-exports are present
        assert!(
            obj.contains_key("Constraint"),
            "Missing Constraint schema from qontinui_types::constraints"
        );
        assert!(
            obj.contains_key("ScheduledTask"),
            "Missing ScheduledTask schema from qontinui_types::scheduler"
        );
        assert!(
            obj.contains_key("UnifiedWorkflow"),
            "Missing UnifiedWorkflow schema from qontinui_types::workflow"
        );
    }

    #[test]
    fn test_worker_output_schema_has_properties() {
        let schemas = export_all_schemas();
        let worker = &schemas["WorkerOutput"];
        assert!(
            worker.get("properties").is_some() || worker.get("$defs").is_some(),
            "WorkerOutput schema should have properties or $defs"
        );
    }

    #[test]
    fn test_app_event_schema_has_variants() {
        let schemas = export_all_schemas();
        let app_event = &schemas["AppEvent"];
        // Tagged enum should produce oneOf or similar structure
        let schema_str = serde_json::to_string(app_event).unwrap();
        assert!(
            schema_str.contains("ExecutorEvent") || schema_str.contains("event_type"),
            "AppEvent schema should reference its variants"
        );
    }
}
