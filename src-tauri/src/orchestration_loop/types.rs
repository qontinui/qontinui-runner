//! Types for the orchestration loop.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a loop instance within the multi-loop manager.
pub type LoopId = String;

// --- Configuration ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StallDetectorConfig {
    pub max_repeated_actions: u32,
    pub max_total_steps: u32,
    pub stall_timeout_secs: u64,
    pub oscillation_window: u32,
}

impl Default for StallDetectorConfig {
    fn default() -> Self {
        Self { max_repeated_actions: 5, max_total_steps: 100, stall_timeout_secs: 300, oscillation_window: 10 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizationConfig {
    pub enabled: bool,
    pub token_threshold_pct: f32,
    pub max_tokens_budget: usize,
    pub preserve_last_n_iterations: u32,
    pub summary_max_tokens: usize,
}

impl Default for SummarizationConfig {
    fn default() -> Self {
        Self { enabled: true, token_threshold_pct: 0.75, max_tokens_budget: 80000, preserve_last_n_iterations: 2, summary_max_tokens: 2000 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposerConfig {
    pub enabled: bool,
    pub min_subtasks: u32,
    pub max_subtasks: u32,
    pub model_override: Option<String>,
}

impl Default for DecomposerConfig {
    fn default() -> Self {
        Self { enabled: true, min_subtasks: 3, max_subtasks: 7, model_override: None }
    }
}

/// Configuration for an orchestration loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationLoopConfig {
    /// Target runner port to execute workflows on.
    /// If None, targets self (this runner's own port).
    pub target_runner_port: Option<u16>,

    /// Target runner ID (for supervisor restart calls). If None, uses "primary".
    pub target_runner_id: Option<String>,

    /// Supervisor port for restart/build operations.
    #[serde(default = "default_supervisor_port")]
    pub supervisor_port: u16,

    /// Workflow ID to execute each iteration.
    /// Required for simple mode; optional in pipeline mode if build phase is configured.
    #[serde(default)]
    pub workflow_id: String,

    /// Maximum number of iterations.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    /// How to decide when to stop.
    #[serde(default)]
    pub exit_strategy: ExitStrategy,

    /// What to do between iterations.
    #[serde(default)]
    pub between_iterations: BetweenIterations,

    /// When true, a failed workflow iteration doesn't count as a terminal failure.
    /// Instead, the loop waits for the fixer workflow to complete, then re-runs.
    /// The loop only exits on success or max_iterations.
    #[serde(default)]
    pub retry_on_failure: bool,

    /// Whether to wait for the fixer workflow before starting next iteration.
    /// Only applies when retry_on_failure is true.
    #[serde(default = "default_wait_for_fixer")]
    pub wait_for_fixer: bool,

    /// Pipeline mode configuration. When present, enables build/reflect/fix phases.
    pub pipeline: Option<PipelineConfig>,

    #[serde(default)]
    pub stall_detection: Option<StallDetectorConfig>,
    #[serde(default)]
    pub summarization: Option<SummarizationConfig>,
    #[serde(default)]
    pub decomposition: Option<DecomposerConfig>,
}

/// Pipeline mode configuration for build → execute → reflect → fix cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Generate workflow from description (optional — if absent, uses workflow_id).
    pub build: Option<BuildPhaseConfig>,
    /// Implement fixes via Claude CLI after reflection finds issues.
    pub implement_fixes: Option<ImplementFixesConfig>,
    /// Diagnostic evaluation phase — runs after Execute, before Reflect.
    /// Captures UI state via UI Bridge and classifies failure root causes.
    #[serde(default)]
    pub diagnose: Option<DiagnosePhaseConfig>,
}

/// Configuration for the build (workflow generation) phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPhaseConfig {
    /// Human description of the desired workflow.
    pub description: String,
    /// Additional context to pass to the generator.
    pub context: Option<String>,
    /// Context IDs to include.
    pub context_ids: Option<Vec<String>>,
}

/// Configuration for the implement-fixes phase (Claude CLI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementFixesConfig {
    /// Model to use (e.g., "claude-opus-4-6"). Defaults to claude-opus-4-6.
    pub model: Option<String>,
    /// Timeout in seconds for the fix agent. Defaults to 600.
    pub timeout_secs: Option<u64>,
    /// Additional context to include in the fix prompt.
    pub additional_context: Option<String>,
}

/// Configuration for the diagnostic evaluation phase.
/// Captures UI state after workflow execution and classifies failure root causes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosePhaseConfig {
    /// Assertions to run against the UI after workflow execution.
    /// Each assertion is a JSON object passed to POST /ui-bridge/control/assert.
    #[serde(default)]
    pub assertions: Vec<serde_json::Value>,
    /// Whether to capture a full DOM snapshot for AI triage context.
    #[serde(default = "default_capture_snapshot")]
    pub capture_snapshot: bool,
    /// Maximum characters to include from the snapshot in the AI triage prompt.
    #[serde(default = "default_snapshot_max_chars")]
    pub snapshot_max_chars: usize,
    /// Model override for the triage AI call. If None, uses default routing.
    pub model_override: Option<String>,
}

fn default_capture_snapshot() -> bool {
    true
}

fn default_snapshot_max_chars() -> usize {
    8000
}

/// Root cause classification for a diagnostic failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RootCauseCategory {
    /// The UI itself rendered incorrectly (wrong components, broken layout).
    BadUiRendering,
    /// UI Bridge misread the page (element not found, wrong selector).
    BadUiBridgeEvaluation,
    /// The assertions/verification are wrong (testing the wrong thing).
    BadVerificationSteps,
    /// The workflow generation prompt was unclear or incomplete.
    BadGenerationPrompt,
    /// The generated state machine has logical errors (wrong transitions, missing states).
    BadStateMachineLogic,
    /// Network error, timeout, crashed app, or other infrastructure failure.
    InfrastructureIssue,
    /// Cannot determine.
    Unknown,
}

/// Result of a single diagnostic evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticResult {
    /// Whether all assertions passed and the page is healthy.
    pub passed: bool,
    /// Page health status from UI Bridge.
    pub page_health: serde_json::Value,
    /// Assertion results (each with pass/fail and details).
    pub assertion_results: Vec<serde_json::Value>,
    /// Classified root cause (only meaningful when passed == false).
    pub root_cause: Option<RootCauseCategory>,
    /// AI-generated explanation of the failure.
    pub diagnosis: Option<String>,
    /// AI-generated recommendation for the next iteration's prompt.
    pub prompt_rewrite_suggestion: Option<String>,
}

/// How to evaluate whether the loop should exit.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExitStrategy {
    /// Exit when reflection finds 0 new fixes.
    #[default]
    Reflection,
    /// Exit when workflow verification passes on first iteration.
    WorkflowVerification,
    /// Always run max_iterations times.
    FixedIterations,
    /// Exit when diagnostic evaluation passes (all assertions pass + healthy page).
    DiagnosticEvaluation,
}

/// Action to take between iterations.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BetweenIterations {
    /// Restart the target runner between iterations.
    RestartRunner {
        #[serde(default)]
        rebuild: bool,
    },
    /// Only restart if the workflow signaled a restart is needed.
    RestartOnSignal {
        #[serde(default)]
        rebuild: bool,
    },
    /// Wait for the target runner to be healthy (no restart).
    WaitHealthy,
    /// No action between iterations.
    #[default]
    None,
}

fn default_max_iterations() -> u32 {
    5
}

fn default_supervisor_port() -> u16 {
    9875
}

fn default_wait_for_fixer() -> bool {
    true
}

// --- State ---

/// Current phase of the orchestration loop.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LoopPhase {
    #[default]
    Idle,
    BuildingWorkflow,
    RunningWorkflow,
    Diagnosing,
    Reflecting,
    ImplementingFixes,
    EvaluatingExit,
    WaitingForFixer,
    BetweenIterations,
    WaitingForRunner,
    StallDetecting,
    Planning,
    Complete,
    Stopped,
    Error,
}

/// Runtime state of the orchestration loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationLoopStatus {
    pub running: bool,
    pub phase: LoopPhase,
    pub current_iteration: u32,
    pub max_iterations: u32,
    pub workflow_id: String,
    pub target_runner_port: u16,
    pub target_runner_id: Option<String>,
    pub is_pipeline: bool,
    pub started_at: Option<String>,
    pub error: Option<String>,
    pub iteration_results: Vec<IterationResult>,
}

/// Result of a single iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationResult {
    pub iteration: u32,
    pub started_at: String,
    pub completed_at: String,
    pub task_run_id: String,
    pub reflection_task_run_id: Option<String>,
    pub fix_count: Option<u32>,
    pub exit_check: ExitCheckResult,
    /// Pipeline mode: ID of the workflow generated during the build phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_workflow_id: Option<String>,
    /// Pipeline mode: whether fixes were implemented during this iteration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixes_implemented: Option<bool>,
    /// Pipeline mode: whether a rebuild was triggered for the next iteration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuild_triggered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall_detected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_summarized: Option<bool>,
    /// Diagnostic phase result (if diagnose phase is configured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_result: Option<DiagnosticResult>,
}

/// Result of evaluating whether the loop should exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitCheckResult {
    pub should_exit: bool,
    pub reason: String,
}

// --- Multi-Loop Types ---

/// Configuration for launching multiple loops at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiLoopConfig {
    /// Individual loop configurations, each targeting a different runner.
    pub loops: Vec<MultiLoopEntry>,
    /// Stop all loops if any single loop errors out.
    #[serde(default)]
    pub stop_all_on_error: bool,
}

/// A single entry in a multi-loop configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiLoopEntry {
    /// Unique identifier for this loop instance.
    pub loop_id: LoopId,
    /// Human label (e.g., "Pages 1-10" or app section name).
    pub label: Option<String>,
    /// The loop configuration.
    pub config: OrchestrationLoopConfig,
}

/// Aggregated status across all active loops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiLoopStatus {
    pub loops: Vec<LoopInstanceStatus>,
    pub all_complete: bool,
    pub any_error: bool,
    pub stop_all_on_error: bool,
}

/// Status of a single loop instance within a multi-loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInstanceStatus {
    pub loop_id: LoopId,
    pub label: Option<String>,
    pub status: OrchestrationLoopStatus,
}

/// Metadata stored alongside each loop's state.
#[derive(Debug, Clone, Default)]
pub struct LoopMetadata {
    pub label: Option<String>,
    pub stop_all_on_error: bool,
}

/// Maps loop IDs to per-loop metadata.
pub type LoopMetadataMap = HashMap<LoopId, LoopMetadata>;
