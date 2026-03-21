//! Unified Workflows
//!
//! This module provides types for the unified Workflow Builder system.
//! All workflows are organized into three phases: Setup, Verification, Agentic.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// A conditional routing rule that selects model/provider based on runtime context.
///
/// Rules are evaluated in order; the first matching rule wins.
/// Condition syntax: `"<variable> <op> <value>"` where:
/// - Variables: `verification_failures`, `iteration`, `stage_index`
/// - Operators: `>=`, `>`, `<=`, `<`, `==`, `!=`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Condition expression, e.g. "verification_failures >= 2"
    pub condition: String,
    /// Model to use when this rule matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider to use when this rule matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Temperature override when this rule matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Max tokens override when this rule matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// Per-phase model override configuration.
/// Each phase can independently specify a provider and/or model,
/// along with optional temperature, max_tokens, fallback config,
/// and conditional routing rules.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelOverrideConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Temperature override for this phase (0.0–1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Max output tokens override for this phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Fallback provider if the primary fails with a retryable error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_provider: Option<String>,
    /// Fallback model if the primary fails with a retryable error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
    /// Conditional routing rules evaluated at runtime.
    /// First matching rule wins; unmatched falls back to this config's static fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_rules: Option<Vec<RoutingRule>>,
}

/// Map of phase name → model override config.
/// Valid keys: "setup", "agentic", "completion", "verification",
///             "investigation", "summary", "generation"
pub type ModelOverrides = HashMap<String, ModelOverrideConfig>;

/// Deserialize a Vec field that might be null in JSON (e.g., from Python's `None`).
/// Returns an empty Vec for null, or the actual Vec for a valid array.
fn deserialize_null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let opt: Option<Vec<T>> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

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

/// Condition for conditional stage execution.
///
/// When attached to a `WorkflowStage`, the stage is skipped if the condition
/// evaluates to "should skip". All condition fields are optional and combine
/// with AND semantics — all specified conditions must be met for the stage to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageCondition {
    /// Run this stage only if the previous stage had this outcome.
    /// - `"passed"`: run only if previous stage verification passed
    /// - `"failed"`: run only if previous stage verification failed
    /// - `"any"`: always run regardless of previous outcome (default behavior)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_previous: Option<String>,

    /// Run this stage only after this many loop iterations have occurred
    /// (across all stages). Useful for "escalation" stages that only kick in
    /// after initial attempts have failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_iteration: Option<u32>,

    /// Skip this stage if the total number of failed stages so far is below
    /// this threshold. Useful for "recovery" stages that only run when
    /// multiple prior stages have failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_failures: Option<u32>,
}

/// A workflow stage — a self-contained unit of execution with its own
/// setup/verification/agentic/completion steps and verification-agentic loop.
///
/// Retry policy for a step or stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Number of retry attempts (0 = no retries)
    #[serde(default)]
    pub count: u32,
    /// Delay between retries in milliseconds
    #[serde(default = "default_retry_delay_ms")]
    pub delay_ms: u64,
    /// Whether to use exponential backoff
    #[serde(default)]
    pub backoff: bool,
}

fn default_retry_delay_ms() -> u64 {
    2000
}

/// An output declared by a stage, available to subsequent stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageOutput {
    /// Unique key for this output (e.g. "api_url", "auth_token")
    pub key: String,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
}

/// An input required by a stage, referencing a prior stage's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageInput {
    /// The key to bind this input to (matches a StageOutput.key from a prior stage)
    pub key: String,
    /// Which stage provides this input (stage id). If omitted, searches all prior stages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_stage: Option<String>,
    /// Whether this input is required (default: true). Missing required inputs are Critical findings.
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Multi-stage workflows execute stages sequentially. Each stage gets its own
/// verification-agentic loop, and later stages see full output from all prior stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStage {
    /// Unique identifier (UUID v4)
    #[serde(default)]
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
    /// Per-phase model overrides for this stage
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_overrides: ModelOverrides,
    /// Whether to pause for human approval after each agentic phase.
    #[serde(default)]
    pub approval_gate: bool,
    /// Optional condition for stage execution.
    /// When set, the stage is evaluated against this condition before running.
    /// If the condition is not met, the stage is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<StageCondition>,
    /// When true, run completion prompt steps BEFORE automation steps.
    /// Default (false) runs automation first, then prompts.
    #[serde(default)]
    pub completion_prompts_first: bool,
    /// Retry policy for this stage (overrides per-step defaults)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
    /// Declared outputs that this stage produces for downstream stages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<StageOutput>>,
    /// Inputs required from prior stages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<StageInput>>,
}

/// A unified workflow with steps organized by phase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedWorkflow {
    /// Unique identifier (UUID v4)
    #[serde(default)]
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

    /// Per-phase model overrides
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_overrides: ModelOverrides,

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
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_null_as_empty_vec"
    )]
    pub stages: Vec<WorkflowStage>,

    /// Whether to stop execution if a stage fails verification.
    /// Default: false (autonomous mode — continue to next stage even if previous failed).
    #[serde(default)]
    pub stop_on_failure: bool,

    /// Per-constraint overrides: map of constraint_id to enabled (true) / disabled (false).
    /// Applied to the constraint engine at execution time, after loading builtins and config.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub constraint_overrides: HashMap<String, bool>,

    /// Whether to pause for human approval after each agentic phase.
    #[serde(default)]
    pub approval_gate: bool,

    /// Whether to enable reflection mode during agentic iterations.
    /// When true, the AI investigates root causes before fixing failures.
    /// Default: true for user-created workflows.
    #[serde(default = "default_reflection_mode")]
    pub reflection_mode: bool,

    /// When true, run completion prompt steps BEFORE automation steps.
    /// Used by meta-workflows so AI hardener runs before save_workflow_artifact.
    /// Default (false) runs automation first, then prompts.
    #[serde(default)]
    pub completion_prompts_first: bool,

    /// Whether this workflow is marked as a favorite for quick access.
    #[serde(default)]
    pub is_favorite: bool,

    /// Dependency graph computed during generation (JSON blob)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_graph: Option<Value>,

    /// Cost annotations computed during generation (JSON blob)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_annotations: Option<Value>,

    /// Acceptance criteria from the specification agent (JSON blob).
    /// Used by the canvas panel manager to show a live requirements tracker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Value>,

    /// Quality report from the revision phase (JSON blob)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_report: Option<Value>,

    /// Enable multi-agent fixer mode for the agentic phase.
    /// When true, verification failures are triaged and fixed by specialized agents
    /// (quick-fix for lint/compilation, feature-fix for missing functionality).
    /// Default: true.
    #[serde(default = "default_multi_agent_mode")]
    pub multi_agent_mode: bool,

    /// Run the workflow in an isolated git worktree.
    /// When true, a new branch and worktree are created before execution.
    /// Changes stay on the worktree branch and can be merged back after review.
    /// Default: false.
    #[serde(default)]
    pub use_worktree: bool,

    /// Workflow execution architecture override.
    /// When set, forces the workflow to use a specific execution architecture
    /// instead of the default Traditional loop. When None, the system infers
    /// the best architecture based on workflow complexity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_architecture:
        Option<crate::autoresearch::agentic_verification::WorkflowArchitecture>,

    /// Whether the AI semantic review actually ran successfully during generation.
    /// When false, the workflow passed through the pipeline without AI verification
    /// (e.g., all verification iterations failed at infrastructure level).
    #[serde(default = "default_ai_reviewed")]
    pub ai_reviewed: bool,

    /// ISO 8601 timestamp of creation
    #[serde(default)]
    pub created_at: String,
    /// ISO 8601 timestamp of last modification (serialized as "modified_at" to match frontend)
    #[serde(rename = "modified_at", default)]
    pub updated_at: String,
}

impl UnifiedWorkflow {
    /// Normalize any workflow to its stages representation.
    /// If stages is non-empty, return them as-is.
    /// If stages is empty, wrap top-level steps into a single stage.
    pub fn normalize_to_stages(&self) -> Vec<WorkflowStage> {
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

fn default_reflection_mode() -> bool {
    true
}

fn default_ai_reviewed() -> bool {
    true
}

fn default_multi_agent_mode() -> bool {
    true
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
    pub workflow_architecture:
        Option<crate::autoresearch::agentic_verification::WorkflowArchitecture>,
}

impl From<&UnifiedWorkflow> for CreateUnifiedWorkflowRequest {
    fn from(w: &UnifiedWorkflow) -> Self {
        Self {
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
        }
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
    pub workflow_architecture:
        Option<crate::autoresearch::agentic_verification::WorkflowArchitecture>,
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
        max_iterations: workflow.max_iterations,
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
        max_iterations: stage.max_iterations,
        provider: stage.provider.clone(),
        model: stage.model.clone(),
        model_overrides: stage.model_overrides.clone(),
        timeout_seconds: stage.timeout_seconds,
        approval_gate: stage.approval_gate,
        condition: stage.condition.clone(),
        completion_prompts_first: stage.completion_prompts_first,
    }
}
