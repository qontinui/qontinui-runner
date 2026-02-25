//! Unified Workflows
//!
//! This module provides types for the unified Workflow Builder system.
//! All workflows are organized into three phases: Setup, Verification, Agentic.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Log source selection mode for a workflow
/// - "default": Use the global default profile (from Settings)
/// - "ai": Let AI automatically select relevant sources
/// - "all": Use all enabled log sources
/// - { "profile_id": "..." }: Use a specific profile
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LogSourceSelection {
    /// Simple string modes: "default", "ai", "all"
    Mode(String),
    /// Specific profile selection
    Profile { profile_id: String },
}

impl Default for LogSourceSelection {
    fn default() -> Self {
        LogSourceSelection::Mode("default".to_string())
    }
}

/// Configuration for a health check URL
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthCheckUrl {
    /// Display name for the health check (e.g., "Backend Server")
    pub name: String,
    /// URL to check (e.g., "http://localhost:8000/health")
    pub url: String,
    /// Expected HTTP status code (default: 200)
    #[serde(default = "default_expected_status")]
    pub expected_status: u16,
    /// Timeout in seconds (default: 5)
    #[serde(default = "default_health_timeout")]
    pub timeout_seconds: u64,
    /// Whether failure should stop the workflow (default: true)
    #[serde(default = "default_is_critical")]
    pub is_critical: bool,
}

fn default_expected_status() -> u16 {
    200
}

/// Default timeout for health checks.
/// Health checks need a reasonable timeout to avoid hanging on unresponsive services.
fn default_health_timeout() -> u64 {
    30 // 30 seconds - reasonable for health checks
}

fn default_is_critical() -> bool {
    true
}

/// A workflow stage — a self-contained unit of execution with its own
/// setup/verification/agentic/completion steps and verification-agentic loop.
///
/// Multi-stage workflows execute stages sequentially. Each stage gets its own
/// verification-agentic loop, and later stages see full output from all prior stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStage {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for this stage
    pub name: String,
    /// Description of what this stage does
    #[serde(default)]
    pub description: String,
    /// Setup phase steps for this stage
    #[serde(default)]
    pub setup_steps: Vec<Value>,
    /// Verification phase steps for this stage
    #[serde(default)]
    pub verification_steps: Vec<Value>,
    /// Agentic phase steps for this stage
    #[serde(default)]
    pub agentic_steps: Vec<Value>,
    /// Completion phase steps for this stage
    #[serde(default)]
    pub completion_steps: Vec<Value>,
    /// Maximum iterations for this stage's verification-agentic loop
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Optional inactivity timeout in seconds for this stage's AI sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// AI provider override for this stage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model override for this stage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A unified workflow with steps organized by phase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedWorkflow {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name
    pub name: String,
    /// Description of what this workflow does
    #[serde(default)]
    pub description: String,
    /// Category for organization
    #[serde(default = "default_category")]
    pub category: String,
    /// Tags for filtering
    #[serde(default)]
    pub tags: Vec<String>,

    /// Setup phase steps (JSON array)
    #[serde(default)]
    pub setup_steps: Vec<Value>,
    /// Verification phase steps (JSON array)
    #[serde(default)]
    pub verification_steps: Vec<Value>,
    /// Agentic phase steps (JSON array)
    #[serde(default)]
    pub agentic_steps: Vec<Value>,
    /// Completion phase steps (JSON array) - runs once after the verification loop exits
    #[serde(default)]
    pub completion_steps: Vec<Value>,

    /// Maximum iterations for agentic phase
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    /// Optional inactivity timeout in seconds for AI sessions.
    ///   - None (default): No timeout, runs until completion or manual stop
    ///   - Some(N): Kill AI session after N seconds of no output
    ///
    /// Takes precedence over the global AI settings timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,

    /// AI provider override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Skip AI summary generation at the end (default: false, meaning AI summary is generated)
    #[serde(default)]
    pub skip_ai_summary: bool,

    /// Error IDs targeted by this workflow (for auto-resolution on success).
    /// When the workflow completes successfully, these errors will be marked as resolved.
    /// Used by error fix workflows generated from the Error Monitor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targeted_error_ids: Vec<i64>,

    /// Log source selection for this workflow
    /// - "default": Use the global default profile (from Settings → Log Sources)
    /// - "ai": Let AI automatically select relevant sources based on context
    /// - "all": Use all enabled log sources
    /// - { "profile_id": "..." }: Use a specific profile
    #[serde(default, skip_serializing_if = "is_default_log_source")]
    pub log_source_selection: LogSourceSelection,

    /// Manually added context IDs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_ids: Vec<String>,

    /// Disabled context IDs (excluded from auto-include)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_context_ids: Vec<String>,

    /// Whether to auto-include contexts based on task mentions (default: true)
    #[serde(default = "default_auto_include_contexts")]
    pub auto_include_contexts: bool,

    /// Custom developer prompt template for this workflow
    /// When set, this template is used instead of the global default when running the workflow.
    /// Supports variables: {{SESSION_ID}}, {{ITERATION}}, {{MAX_ITERATIONS}}, {{GOAL}},
    /// {{EXECUTION_STEPS}}, {{WORKSPACE_ESCAPED}}
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_template: Option<String>,

    /// Whether to automatically include a log_watch step before verification
    /// When enabled (default), a log_watch step is prepended to verification steps
    /// to detect runtime errors in backend/frontend logs
    #[serde(default = "default_log_watch_enabled")]
    pub log_watch_enabled: bool,

    /// Whether to automatically include health check steps before verification
    /// When enabled and health_check_urls is non-empty, health check steps are prepended
    /// to verification steps to verify configured servers are running
    #[serde(default = "default_health_check_enabled")]
    pub health_check_enabled: bool,

    /// URLs to health check before verification (user-configurable)
    /// Each entry specifies a URL to check, expected status, and timeout
    /// If empty, no health checks are performed even if health_check_enabled is true
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub health_check_urls: Vec<HealthCheckUrl>,

    /// Whether to automatically include a pre-flight environment check at the start of setup.
    /// When enabled (default), a shell command step runs to verify:
    ///   - Disk space, Node.js/npm, Python/Poetry, Rust/Cargo, Git availability
    ///
    /// Uses global setting from Settings if not explicitly set per-workflow
    #[serde(default = "default_preflight_check_enabled")]
    pub preflight_check_enabled: bool,

    /// Whether to run a completion sweep after verification passes.
    /// The sweep reviews all completed work for gaps before proceeding to completion.
    #[serde(default)]
    pub enable_sweep: bool,

    /// Maximum number of sweep iterations (default: 5).
    #[serde(default = "default_max_sweep_iterations")]
    pub max_sweep_iterations: u32,

    /// Task run ID that generated this workflow (for meta-workflow tracking)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by_task_run_id: Option<String>,

    /// Optional stages for multi-stage workflows.
    /// When non-empty, the workflow executes stages sequentially instead of using top-level steps.
    /// Each stage has its own setup/verification/agentic/completion steps and loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<WorkflowStage>,

    /// Whether to stop execution if a stage fails verification.
    /// Default: false (autonomous mode — continue to next stage even if previous failed).
    #[serde(default)]
    pub stop_on_failure: bool,

    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification (serialized as "modified_at" to match frontend)
    #[serde(rename = "modified_at")]
    pub updated_at: String,
}

fn default_auto_include_contexts() -> bool {
    true
}

fn default_log_watch_enabled() -> bool {
    true
}

fn default_health_check_enabled() -> bool {
    true
}

fn default_preflight_check_enabled() -> bool {
    true
}

fn default_max_sweep_iterations() -> u32 {
    5
}

fn default_category() -> String {
    "general".to_string()
}

fn default_max_iterations() -> u32 {
    10
}

fn is_default_log_source(selection: &LogSourceSelection) -> bool {
    matches!(selection, LogSourceSelection::Mode(s) if s == "default")
}

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
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Optional inactivity timeout in seconds for AI sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    pub provider: Option<String>,
    pub model: Option<String>,
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

fn default_conflict_strategy() -> String {
    "generate".to_string()
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

/// Convert a `UnifiedWorkflow` into a `StageConfig` for execution.
///
/// This is the canonical conversion used by both:
/// - The compose endpoint (running multiple workflows in sequence)
/// - The WorkflowStepHandler (running a nested workflow inline)
///
/// Set `include_preflight` to true to prepend a preflight check step
/// (used by the compose endpoint; nested workflows skip it since the
/// parent workflow already ran preflight).
pub fn workflow_to_stage_config(
    workflow: &UnifiedWorkflow,
    index: usize,
    total_stages: usize,
    include_preflight: bool,
) -> crate::unified_workflow_executor::StageConfig {
    use crate::unified_workflow_executor::{
        convert_json_steps_with_phase, extract_prompt_steps_with_phase,
    };

    let setup_auto = convert_json_steps_with_phase(&workflow.setup_steps, 0, Some("setup"));
    let setup_auto = if include_preflight {
        prepend_preflight_check_step(setup_auto, workflow.preflight_check_enabled)
    } else {
        setup_auto
    };
    let setup_prompt = extract_prompt_steps_with_phase(&workflow.setup_steps, Some("setup"));
    let verif =
        convert_json_steps_with_phase(&workflow.verification_steps, 0, Some("verification"));
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
        max_iterations: workflow.max_iterations,
        provider: workflow.provider.clone(),
        model: workflow.model.clone(),
        timeout_seconds: workflow.timeout_seconds,
    }
}
