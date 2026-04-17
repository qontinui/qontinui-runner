//! Unified Workflows
//!
//! The DTO shape of the unified workflow types lives in `qontinui-types`
//! (`qontinui_types::workflow`) and is re-exported here via `pub use`.
//! Runner-specific behavior (iteration-cap helpers, stage normalization,
//! request/response types, conversion helpers, step prepending) stays in
//! this module.
//!
//! Because of Rust's orphan rule we can't define inherent `impl` blocks on
//! foreign types, so methods that used to live on `UnifiedWorkflow` and
//! `WorkflowStage` are exposed through extension traits
//! (`UnifiedWorkflowExt`, `WorkflowStageExt`). The traits are re-exported
//! through this module, so callers that do `use crate::unified_workflows::*;`
//! (or explicitly import the extension trait) can keep calling
//! `workflow.iter_cap()`, `stage.iter_cap_display()`, etc. verbatim.

pub use qontinui_types::workflow::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ============================================================================
// Extension traits (orphan rule prevents inherent `impl` blocks on foreign
// types)
// ============================================================================

/// Runner-side behavior for [`WorkflowStage`].
///
/// Exposed as a trait because `WorkflowStage` is defined in `qontinui-types`
/// and we cannot add inherent methods to it here. Import this trait (or
/// `crate::unified_workflows::*`) to call these methods exactly as if they
/// were inherent.
pub trait WorkflowStageExt {
    /// Resolve the iteration cap for internal loop bounds.
    /// `None` → `u32::MAX` (effectively unlimited).
    fn iter_cap(&self) -> u32;

    /// Human-readable iteration cap for logs and UI (`"∞"` for unlimited).
    fn iter_cap_display(&self) -> String;
}

impl WorkflowStageExt for WorkflowStage {
    fn iter_cap(&self) -> u32 {
        self.max_iterations.unwrap_or(u32::MAX)
    }

    fn iter_cap_display(&self) -> String {
        self.max_iterations
            .map(|n| n.to_string())
            .unwrap_or_else(|| "∞".to_string())
    }
}

/// Runner-side behavior for [`UnifiedWorkflow`].
///
/// Exposed as a trait for the same orphan-rule reason as
/// [`WorkflowStageExt`].
pub trait UnifiedWorkflowExt {
    /// Resolve the iteration cap for internal loop bounds.
    /// `None` → `u32::MAX` (effectively unlimited for any real workflow).
    fn iter_cap(&self) -> u32;

    /// Human-readable iteration cap for logs and UI (`"∞"` for unlimited).
    fn iter_cap_display(&self) -> String;

    /// Normalize any workflow to its stages representation.
    /// If stages is non-empty, return them as-is.
    /// If stages is empty, wrap top-level steps into a single stage.
    fn normalize_to_stages(&self) -> Vec<WorkflowStage>;
}

impl UnifiedWorkflowExt for UnifiedWorkflow {
    fn iter_cap(&self) -> u32 {
        self.max_iterations.unwrap_or(u32::MAX)
    }

    fn iter_cap_display(&self) -> String {
        self.max_iterations
            .map(|n| n.to_string())
            .unwrap_or_else(|| "∞".to_string())
    }

    fn normalize_to_stages(&self) -> Vec<WorkflowStage> {
        if !self.stages.is_empty() {
            // Warn if top-level steps also exist — they'll be ignored
            let has_top_level = !self.setup_steps.is_empty()
                || !self.verification_steps.is_empty()
                || !self.agentic_steps.is_empty()
                || !self.completion_steps.is_empty();
            if has_top_level {
                tracing::warn!(
                    workflow_name = %self.name,
                    "Workflow has both stages and top-level steps; top-level steps will be ignored"
                );
            }
            return self.stages.clone();
        }
        // Wrap top-level steps as a single stage
        vec![WorkflowStage {
            id: format!("{}-phase-1", self.id),
            name: self.name.clone(),
            description: self.description.clone(),
            setup_steps: self.setup_steps.clone(),
            verification_steps: self.verification_steps.clone(),
            agentic_steps: self.agentic_steps.clone(),
            completion_steps: self.completion_steps.clone(),
            max_iterations: self.max_iterations,
            timeout_seconds: self.timeout_seconds,
            provider: self.provider.clone(),
            model: self.model.clone(),
            model_overrides: self.model_overrides.clone(),
            approval_gate: self.approval_gate,
            condition: None,
            completion_prompts_first: self.completion_prompts_first,
            retry_policy: None,
            outputs: None,
            inputs: None,
        }]
    }
}

// ============================================================================
// Runner-local default helpers
// ============================================================================

fn default_category() -> String {
    "general".to_string()
}

fn default_conflict_strategy() -> String {
    "generate".to_string()
}

// ============================================================================
// CreateUnifiedWorkflowRequest — runner HTTP request body
// ============================================================================

/// Request body for creating a new unified workflow
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUnifiedWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub setup_steps: Vec<Value>,
    #[serde(default)]
    pub verification_steps: Vec<Value>,
    #[serde(default)]
    pub agentic_steps: Vec<Value>,
    #[serde(default)]
    pub completion_steps: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Optional inactivity timeout in seconds for AI sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub model_overrides: Option<ModelOverrides>,
    #[serde(default)]
    pub skip_ai_summary: bool,
    #[serde(default)]
    pub log_source_selection: Option<LogSourceSelection>,
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    #[serde(default)]
    pub disabled_context_ids: Option<Vec<String>>,
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,
    /// Custom developer prompt template for this workflow
    pub prompt_template: Option<String>,
    /// Whether to automatically include a log_watch step before verification
    #[serde(default)]
    pub log_watch_enabled: Option<bool>,
    /// Whether to automatically include health check steps before verification
    #[serde(default)]
    pub health_check_enabled: Option<bool>,
    /// URLs to health check before verification (user-configurable)
    #[serde(default)]
    pub health_check_urls: Option<Vec<HealthCheckUrl>>,
    /// Whether to automatically include a pre-flight environment check at the start of setup
    #[serde(default)]
    pub preflight_check_enabled: Option<bool>,
    /// Whether to run a completion sweep after verification passes
    #[serde(default)]
    pub enable_sweep: Option<bool>,
    /// Maximum number of sweep iterations (default: 5)
    #[serde(default)]
    pub max_sweep_iterations: Option<u32>,
    /// Error IDs targeted by this workflow (for auto-resolution on success)
    #[serde(default)]
    pub targeted_error_ids: Option<Vec<i64>>,
    /// Task run ID that generated this workflow (for meta-workflow tracking)
    #[serde(default)]
    pub generated_by_task_run_id: Option<String>,
    /// Optional stages for multi-stage workflows
    #[serde(default)]
    pub stages: Option<Vec<WorkflowStage>>,
    /// Whether to stop execution if a stage fails verification
    #[serde(default)]
    pub stop_on_failure: Option<bool>,
    /// Per-constraint overrides: map of constraint_id to enabled/disabled
    #[serde(default)]
    pub constraint_overrides: Option<HashMap<String, bool>>,
    /// Whether to pause for human approval after each agentic phase
    #[serde(default)]
    pub approval_gate: Option<bool>,
    /// Whether to enable reflection mode during agentic iterations
    #[serde(default)]
    pub reflection_mode: Option<bool>,
    /// When true, run completion prompt steps BEFORE automation steps
    #[serde(default)]
    pub completion_prompts_first: Option<bool>,
    /// Dependency graph (JSON blob, set by generator)
    #[serde(default)]
    pub dependency_graph: Option<Value>,
    /// Cost annotations (JSON blob, set by generator)
    #[serde(default)]
    pub cost_annotations: Option<Value>,
    /// Quality report (JSON blob, set by generator)
    #[serde(default)]
    pub quality_report: Option<Value>,
    /// Acceptance criteria (JSON blob, set by generator)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Value>,
    /// Whether the AI semantic review ran successfully during generation
    #[serde(default)]
    pub ai_reviewed: Option<bool>,
    /// Workflow execution architecture override (e.g., "multi_agent_pipeline").
    #[serde(default)]
    pub workflow_architecture: Option<WorkflowArchitecture>,
    /// When true, the pipeline will stop execution if accumulated token usage
    /// exceeds the token budget. Disabled by default.
    #[serde(default)]
    pub enforce_token_budget: Option<bool>,
    #[serde(default)]
    pub strict_cwd: bool,
    #[serde(default)]
    pub tool_tags: Vec<String>,
    /// Flow control configuration as a JSON string.
    #[serde(default)]
    pub flow_control_json: Option<String>,
    /// Per-phase timeout configuration as a JSON string.
    #[serde(default)]
    pub phase_timeouts_json: Option<String>,
    /// Policy for automatic git rollback on workflow failure.
    #[serde(default)]
    pub rollback_policy: Option<String>,
    /// Whether HTN planning is enabled for this workflow.
    #[serde(default)]
    pub htn_enabled: bool,
    /// UI Bridge URL for HTN planning.
    #[serde(default)]
    pub htn_ui_bridge_url: Option<String>,
    /// Path to a serialized state machine JSON file for HTN planning.
    #[serde(default)]
    pub htn_state_machine_path: Option<String>,
}

/// Build a [`CreateUnifiedWorkflowRequest`] from a [`UnifiedWorkflow`].
///
/// Previously `impl From<&UnifiedWorkflow> for CreateUnifiedWorkflowRequest`.
/// Moved to a free function because `UnifiedWorkflow` is defined in
/// `qontinui-types` and Rust's orphan rule forbids an inherent `From` impl
/// between a foreign source type and a local target type? Actually, the
/// orphan rule *does* allow `impl From<&ForeignType> for LocalType` since
/// the target type is local — but we standardize on the free-function form
/// for consistency with the other DTO migrations (scheduler, constraints).
pub fn unified_workflow_to_create_request(w: &UnifiedWorkflow) -> CreateUnifiedWorkflowRequest {
    CreateUnifiedWorkflowRequest {
        name: w.name.clone(),
        description: w.description.clone(),
        category: w.category.clone(),
        tags: w.tags.clone(),
        setup_steps: w.setup_steps.clone(),
        verification_steps: w.verification_steps.clone(),
        agentic_steps: w.agentic_steps.clone(),
        completion_steps: w.completion_steps.clone(),
        max_iterations: w.max_iterations,
        timeout_seconds: w.timeout_seconds,
        provider: w.provider.clone(),
        model: w.model.clone(),
        skip_ai_summary: w.skip_ai_summary,
        log_source_selection: Some(w.log_source_selection.clone()),
        context_ids: Some(w.context_ids.clone()),
        disabled_context_ids: Some(w.disabled_context_ids.clone()),
        auto_include_contexts: Some(w.auto_include_contexts),
        prompt_template: w.prompt_template.clone(),
        log_watch_enabled: Some(w.log_watch_enabled),
        health_check_enabled: Some(w.health_check_enabled),
        health_check_urls: Some(w.health_check_urls.clone()),
        preflight_check_enabled: Some(w.preflight_check_enabled),
        targeted_error_ids: Some(w.targeted_error_ids.clone()),
        generated_by_task_run_id: w.generated_by_task_run_id.clone(),
        enable_sweep: Some(w.enable_sweep),
        max_sweep_iterations: Some(w.max_sweep_iterations),
        stages: Some(w.stages.clone()),
        stop_on_failure: Some(w.stop_on_failure),
        constraint_overrides: Some(w.constraint_overrides.clone()),
        approval_gate: Some(w.approval_gate),
        reflection_mode: Some(w.reflection_mode),
        completion_prompts_first: Some(w.completion_prompts_first),
        model_overrides: Some(w.model_overrides.clone()),
        dependency_graph: w.dependency_graph.clone(),
        cost_annotations: w.cost_annotations.clone(),
        quality_report: w.quality_report.clone(),
        acceptance_criteria: w.acceptance_criteria.clone(),
        ai_reviewed: Some(w.ai_reviewed),
        workflow_architecture: w.workflow_architecture.clone(),
        enforce_token_budget: Some(w.enforce_token_budget),
        strict_cwd: w.strict_cwd,
        tool_tags: w.tool_tags.clone(),
        flow_control_json: w.flow_control_json.clone(),
        phase_timeouts_json: w.phase_timeouts_json.clone(),
        rollback_policy: w.rollback_policy.clone(),
        htn_enabled: w.htn_enabled,
        htn_ui_bridge_url: w.htn_ui_bridge_url.clone(),
        htn_state_machine_path: w.htn_state_machine_path.clone(),
    }
}

/// Request body for updating a unified workflow
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUnifiedWorkflowRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub setup_steps: Option<Vec<Value>>,
    pub verification_steps: Option<Vec<Value>>,
    pub agentic_steps: Option<Vec<Value>>,
    pub completion_steps: Option<Vec<Value>>,
    pub max_iterations: Option<u32>,
    /// Optional inactivity timeout in seconds for AI sessions.
    /// - None: Not updating this field
    /// - Some(None): Explicitly disable timeout
    /// - Some(Some(N)): Set timeout to N seconds
    pub timeout_seconds: Option<Option<u64>>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_overrides: Option<ModelOverrides>,
    pub skip_ai_summary: Option<bool>,
    pub log_source_selection: Option<LogSourceSelection>,
    pub context_ids: Option<Vec<String>>,
    pub disabled_context_ids: Option<Vec<String>>,
    pub auto_include_contexts: Option<bool>,
    /// Custom developer prompt template for this workflow
    pub prompt_template: Option<String>,
    /// Whether to automatically include a log_watch step before verification
    pub log_watch_enabled: Option<bool>,
    /// Whether to automatically include health check steps before verification
    pub health_check_enabled: Option<bool>,
    /// URLs to health check before verification (user-configurable)
    pub health_check_urls: Option<Vec<HealthCheckUrl>>,
    /// Whether to automatically include a pre-flight environment check at the start of setup
    pub preflight_check_enabled: Option<bool>,
    /// Whether to run a completion sweep after verification passes
    pub enable_sweep: Option<bool>,
    /// Maximum number of sweep iterations (default: 5)
    pub max_sweep_iterations: Option<u32>,
    /// Optional stages for multi-stage workflows
    pub stages: Option<Vec<WorkflowStage>>,
    /// Whether to stop execution if a stage fails verification
    pub stop_on_failure: Option<bool>,
    /// Per-constraint overrides: map of constraint_id to enabled/disabled
    pub constraint_overrides: Option<HashMap<String, bool>>,
    /// Whether to pause for human approval after each agentic phase
    pub approval_gate: Option<bool>,
    /// Whether to enable reflection mode during agentic iterations
    pub reflection_mode: Option<bool>,
    /// When true, run completion prompt steps BEFORE automation steps
    pub completion_prompts_first: Option<bool>,
    /// Dependency graph (JSON blob, set by generator)
    pub dependency_graph: Option<Value>,
    /// Cost annotations (JSON blob, set by generator)
    pub cost_annotations: Option<Value>,
    /// Quality report (JSON blob, set by generator)
    pub quality_report: Option<Value>,
    /// Acceptance criteria (JSON blob, set by generator)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Value>,
    /// Whether the AI semantic review ran successfully during generation
    pub ai_reviewed: Option<bool>,
    /// Workflow execution architecture override (e.g., "multi_agent_pipeline").
    pub workflow_architecture: Option<WorkflowArchitecture>,
    /// When true, the pipeline will stop execution if accumulated token usage
    /// exceeds the token budget. Disabled by default.
    #[serde(default)]
    pub enforce_token_budget: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_cwd: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_tags: Option<Vec<String>>,
    /// Flow control configuration as a JSON string.
    pub flow_control_json: Option<String>,
    /// Per-phase timeout configuration as a JSON string.
    pub phase_timeouts_json: Option<String>,
    /// Policy for automatic git rollback on workflow failure.
    pub rollback_policy: Option<String>,
    /// Whether HTN planning is enabled for this workflow.
    pub htn_enabled: Option<bool>,
    /// UI Bridge URL for HTN planning.
    pub htn_ui_bridge_url: Option<String>,
    /// Path to a serialized state machine JSON file for HTN planning.
    pub htn_state_machine_path: Option<String>,
}

/// Query parameters for searching unified workflows
#[derive(Debug, Clone, Deserialize)]
pub struct SearchUnifiedWorkflowsQuery {
    /// Search query (matches name, description)
    pub q: Option<String>,
    /// Filter by category
    pub category: Option<String>,
    /// Filter by tag
    pub tag: Option<String>,
}

/// Manifest for exported workflow files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowExportManifest {
    /// Export format version
    pub version: String,
    /// When the export was created
    pub exported_at: String,
    /// App version that created the export
    pub app_version: String,
    /// Type of content
    pub content_type: String,
}

/// A single workflow export file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowExport {
    /// Export manifest with version info
    pub manifest: WorkflowExportManifest,
    /// The workflow data
    pub workflow: UnifiedWorkflow,
}

/// Request body for importing a workflow
#[derive(Debug, Clone, Deserialize)]
pub struct ImportWorkflowRequest {
    /// The exported workflow data
    pub workflow: UnifiedWorkflow,
    /// How to handle ID conflicts: "keep" (use original ID), "generate" (new ID), "overwrite" (replace existing)
    #[serde(default = "default_conflict_strategy")]
    pub conflict_strategy: String,
}

/// Result of importing a workflow
#[derive(Debug, Clone, Serialize)]
pub struct ImportWorkflowResult {
    /// The imported workflow
    pub workflow: UnifiedWorkflow,
    /// Whether an existing workflow was overwritten
    pub overwritten: bool,
    /// Original ID if it was changed
    pub original_id: Option<String>,
}

// ============================================================================
// Step prepending helpers
// ============================================================================

/// Prepend a log_watch step to verification steps if log_watch_enabled is true.
///
/// This function creates a default log_watch step that:
/// - Monitors log sources from global settings (Settings > Log Sources)
/// - Scans the last 60 seconds for errors
/// - Is non-critical (won't fail the workflow, just reports errors)
///
/// The step is prepended to the beginning of the verification steps so that
/// log errors are detected before any other verification logic runs.
pub fn prepend_log_watch_step(
    verification_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    log_watch_enabled: bool,
) -> Vec<crate::step_executor::ExecutionStepConfig> {
    if !log_watch_enabled {
        return verification_steps;
    }

    let mut steps = vec![crate::step_executor::ExecutionStepConfig {
        step_type: "log_watch".to_string(),
        name: Some("Log Watch".to_string()),
        phase: Some("verification".to_string()),
        run_on_subsequent_iterations: Some(true),
        required: Some(false),
        ..Default::default()
    }];
    steps.extend(verification_steps);
    steps
}

/// Prepend health check steps to verification steps if health_check_enabled is true.
///
/// This function creates health check steps that:
/// - Verify the backend server (port 8000) is running and healthy
/// - Verify user-configured URLs are reachable
/// - Are marked as critical by default (will stop the workflow if servers are down)
///
/// Health checks are prepended BEFORE log_watch steps so that server availability
/// is verified before scanning for log errors. Order: health_checks -> log_watch -> user steps
pub fn prepend_health_check_steps(
    verification_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    health_check_enabled: bool,
    health_check_urls: &[HealthCheckUrl],
) -> Vec<crate::step_executor::ExecutionStepConfig> {
    if !health_check_enabled || health_check_urls.is_empty() {
        return verification_steps;
    }

    let mut steps: Vec<crate::step_executor::ExecutionStepConfig> = health_check_urls
        .iter()
        .map(|hc| {
            crate::step_executor::ExecutionStepConfig::health_check(
                &hc.name,
                &hc.url,
                hc.expected_status,
                hc.timeout_seconds,
                hc.is_critical,
            )
        })
        .collect();

    steps.extend(verification_steps);
    steps
}

/// Prepend a pre-flight environment check step to setup steps if preflight_check_enabled is true.
///
/// This function creates a shell command step that:
/// - Checks disk space on C: and D: drives
/// - Verifies Node.js, npm, npx are installed and accessible
/// - Verifies Python and Poetry are installed
/// - Verifies Rust (rustc) and Cargo are installed
/// - Verifies Git is installed
/// - Tests temp directory writability
///
/// The step is prepended to the beginning of setup steps so that environment
/// issues are detected before any other setup logic runs.
///
/// Exit codes:
/// - 0: All checks passed
/// - 1: Critical issue found (should abort workflow)
/// - 2: Warning (non-critical issue, workflow can continue)
pub fn prepend_preflight_check_step(
    setup_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    preflight_check_enabled: bool,
) -> Vec<crate::step_executor::ExecutionStepConfig> {
    if !preflight_check_enabled {
        return setup_steps;
    }

    let mut steps = vec![crate::step_executor::ExecutionStepConfig::default_preflight_check()];
    steps.extend(setup_steps);
    steps
}

/// Convert a `UnifiedWorkflow` into a single `StageConfig` for execution.
///
/// This is used by:
/// - The WorkflowStepHandler (running a nested workflow inline)
///
/// Set `include_preflight` to true to prepend a preflight check step
/// (used by the compose endpoint; nested workflows skip it since the
/// parent workflow already ran preflight).
///
/// NOTE: This function does NOT prepend health check or log_watch steps.
/// For full step injection (including health checks and log watch),
/// use `stage_to_stage_config()` instead.
pub fn workflow_to_stage_config(
    workflow: &UnifiedWorkflow,
    index: usize,
    total_stages: usize,
    include_preflight: bool,
) -> crate::unified_workflow_executor::StageConfig {
    use crate::unified_workflow_executor::{
        convert_all_json_steps_with_phase, convert_json_steps_with_phase,
        extract_prompt_steps_with_phase,
    };

    let setup_auto = convert_json_steps_with_phase(&workflow.setup_steps, 0, Some("setup"));
    let setup_auto = if include_preflight {
        prepend_preflight_check_step(setup_auto, workflow.preflight_check_enabled)
    } else {
        setup_auto
    };
    let setup_prompt = extract_prompt_steps_with_phase(&workflow.setup_steps, Some("setup"));
    let verif =
        convert_all_json_steps_with_phase(&workflow.verification_steps, 0, Some("verification"));
    let agentic = extract_prompt_steps_with_phase(&workflow.agentic_steps, Some("agentic"));
    let comp_auto =
        convert_json_steps_with_phase(&workflow.completion_steps, 0, Some("completion"));
    let comp_prompt =
        extract_prompt_steps_with_phase(&workflow.completion_steps, Some("completion"));

    crate::unified_workflow_executor::StageConfig {
        id: workflow.id.clone(),
        name: workflow.name.clone(),
        index,
        total_stages,
        setup_automation_steps: setup_auto,
        setup_prompt_steps: setup_prompt,
        verification_steps: verif,
        agentic_steps: agentic,
        completion_automation_steps: comp_auto,
        completion_prompt_steps: comp_prompt,
        max_iterations: workflow.iter_cap(),
        provider: workflow.provider.clone(),
        model: workflow.model.clone(),
        model_overrides: workflow.model_overrides.clone(),
        timeout_seconds: workflow.timeout_seconds,
        approval_gate: workflow.approval_gate,
        condition: None, // Top-level workflows have no stage condition
        completion_prompts_first: workflow.completion_prompts_first,
    }
}

/// Convert a `WorkflowStage` into a `StageConfig` for execution.
///
/// This is used when normalizing all workflows to multi-stage execution.
/// The workflow-level settings (health checks, log watch, preflight) are passed in
/// since stages don't have their own settings for these.
pub fn stage_to_stage_config(
    stage: &WorkflowStage,
    index: usize,
    total_stages: usize,
    include_preflight: bool,
    log_watch_enabled: bool,
    health_check_enabled: bool,
    health_check_urls: &[HealthCheckUrl],
) -> crate::unified_workflow_executor::StageConfig {
    use crate::unified_workflow_executor::{
        convert_all_json_steps_with_phase, convert_json_steps_with_phase,
        extract_prompt_steps_with_phase,
    };

    let setup_auto = convert_json_steps_with_phase(&stage.setup_steps, 0, Some("setup"));
    let setup_auto = if include_preflight && index == 0 {
        // Only prepend preflight to the first stage
        prepend_preflight_check_step(setup_auto, true)
    } else {
        setup_auto
    };
    let setup_prompt = extract_prompt_steps_with_phase(&stage.setup_steps, Some("setup"));
    let verif =
        convert_all_json_steps_with_phase(&stage.verification_steps, 0, Some("verification"));
    let verif = prepend_health_check_steps(verif, health_check_enabled, health_check_urls);
    let verif = prepend_log_watch_step(verif, log_watch_enabled);
    let agentic = extract_prompt_steps_with_phase(&stage.agentic_steps, Some("agentic"));
    let comp_auto = convert_json_steps_with_phase(&stage.completion_steps, 0, Some("completion"));
    let comp_prompt = extract_prompt_steps_with_phase(&stage.completion_steps, Some("completion"));

    crate::unified_workflow_executor::StageConfig {
        id: stage.id.clone(),
        name: stage.name.clone(),
        index,
        total_stages,
        setup_automation_steps: setup_auto,
        setup_prompt_steps: setup_prompt,
        verification_steps: verif,
        agentic_steps: agentic,
        completion_automation_steps: comp_auto,
        completion_prompt_steps: comp_prompt,
        max_iterations: stage.iter_cap(),
        provider: stage.provider.clone(),
        model: stage.model.clone(),
        model_overrides: stage.model_overrides.clone(),
        timeout_seconds: stage.timeout_seconds,
        approval_gate: stage.approval_gate,
        condition: stage.condition.clone(),
        completion_prompts_first: stage.completion_prompts_first,
    }
}
