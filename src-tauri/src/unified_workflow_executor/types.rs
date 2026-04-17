//! Types for the unified workflow executor.
//!
//! This module contains all data structures used by the verification-agentic loop.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::conditional_routing::{evaluate_routing_rules, ResolvedModelConfig, RoutingContext};
use crate::unified_workflows::{ModelOverrideConfig, UnifiedWorkflowExt};

/// Structured diff captured after each agentic iteration.
///
/// Tracks exactly what files changed and the git state before/after,
/// enabling cross-iteration context injection and replay support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationDiff {
    /// Iteration number this diff belongs to.
    pub iteration: u32,
    /// List of files changed in this iteration.
    pub files_changed: Vec<String>,
    /// Human-readable diff stat (e.g., "2 files changed, 10 insertions(+), 3 deletions(-)").
    pub diff_stat: String,
    /// Truncated unified diff (max ~4000 chars per iteration).
    pub diff_summary: String,
    /// Number of lines inserted.
    pub insertions: u32,
    /// Number of lines deleted.
    pub deletions: u32,
    /// Commit hash before the agentic phase ran.
    pub commit_before: Option<String>,
    /// Commit hash after the agentic phase completed.
    pub commit_after: Option<String>,
}

/// A recorded commit checkpoint for a single iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationCommit {
    /// Iteration number.
    pub iteration: u32,
    /// Git commit hash after the agentic phase.
    pub commit_hash: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Policy for automatic git rollback on workflow failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RollbackPolicy {
    /// Do nothing on failure (current behavior, default).
    #[default]
    None,
    /// Revert to the commit of the last iteration where verification improved.
    LastGood,
    /// Revert to the source commit (pre-workflow state).
    Clean,
}

impl RollbackPolicy {
    pub fn from_str(s: &str) -> Self {
        match s {
            "last_good" => Self::LastGood,
            "clean" => Self::Clean,
            _ => Self::None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LastGood => "last_good",
            Self::Clean => "clean",
        }
    }
}

/// Available replay points for a completed workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayPoint {
    /// Iteration number.
    pub iteration: u32,
    /// Git commit hash at this iteration (if available).
    pub commit_hash: Option<String>,
    /// Number of verification checks that passed.
    pub passed_checks: usize,
    /// Number of verification checks that failed.
    pub failed_checks: usize,
    /// ISO 8601 timestamp of when this iteration completed.
    pub timestamp: String,
}

/// Result of a single iteration of the verification-agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationResult {
    /// Iteration number (1-indexed)
    pub iteration: u32,
    /// Whether verification passed in this iteration
    pub verification_passed: bool,
    /// Whether there was a critical failure that should stop the loop
    pub critical_failure: bool,
    /// Number of checks that passed
    pub passed_checks: usize,
    /// Number of checks that failed
    pub failed_checks: usize,
    /// Failure context to pass to the AI (if verification failed)
    pub failure_context: String,
    /// Whether the agentic phase ran
    pub agentic_phase_ran: bool,
    /// Whether the agentic phase succeeded (if it ran)
    pub agentic_phase_success: Option<bool>,
    /// JSON-serialized blame attribution report (if blame analysis was performed).
    /// Contains BlameReport with per-step attributions, oscillating files, and revert patterns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blame_json: Option<String>,
    /// IDs of deferred questions whose auto-decisions this iteration depends on.
    /// Populated from `LoopContext.active_contingencies` at the end of each iteration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contingent_on: Vec<String>,
}

/// Final result of the entire verification-agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopResult {
    /// Total number of iterations executed
    pub iterations_run: u32,
    /// Whether the loop exited due to verification passing
    pub verification_passed: bool,
    /// Whether the loop exited due to hitting max iterations
    pub max_iterations_reached: bool,
    /// Whether the loop exited due to a critical failure
    pub critical_failure: bool,
    /// Whether the loop was stopped externally (via stop_ai_analysis endpoint)
    pub was_stopped: bool,
    /// Whether the AI signaled that errors are unfixable (via [UNFIXABLE_ERRORS] marker)
    #[serde(default)]
    pub unfixable_errors: bool,
    /// All iteration results
    pub iteration_results: Vec<IterationResult>,
    /// Total tokens consumed (populated by multi-agent pipeline and agentic verification).
    #[serde(default)]
    pub total_tokens: Option<u64>,
    /// Total cost in USD (populated by multi-agent pipeline and agentic verification).
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    /// Whether the loop exited because fix attempts were exhausted without progress.
    #[serde(default)]
    pub fix_attempts_exhausted: bool,
    /// All unique file paths modified across all iterations (from resource tracker).
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Number of consecutive non-improving fix attempts at loop exit.
    #[serde(default)]
    pub fix_attempts: u32,
    /// Number of CI auto-resumes consumed at loop exit.
    #[serde(default)]
    pub ci_auto_resumes: u32,
}

impl LoopResult {
    /// Check if the workflow should proceed to completion phase.
    /// Proceeds if:
    /// - Verification passed (normal success path)
    /// - AI signaled unfixable errors (graceful exit path)
    ///   Does NOT proceed if:
    /// - Critical failure occurred
    /// - User stopped the workflow
    /// - Max iterations reached without unfixable signal
    pub fn should_run_completion(&self) -> bool {
        !self.critical_failure
            && !self.was_stopped
            && (self.verification_passed || self.unfixable_errors)
    }

    /// Get a human-readable summary of the loop result.
    pub fn summary(&self) -> String {
        if self.was_stopped {
            format!("STOPPED by user after {} iteration(s)", self.iterations_run)
        } else if self.verification_passed {
            format!(
                "Verification PASSED after {} iteration(s)",
                self.iterations_run
            )
        } else if self.unfixable_errors {
            format!(
                "AI determined errors UNFIXABLE after {} iteration(s) - proceeding to completion",
                self.iterations_run
            )
        } else if self.fix_attempts_exhausted {
            format!(
                "Fix attempts EXHAUSTED after {} iteration(s) - no progress detected",
                self.iterations_run
            )
        } else if self.critical_failure {
            format!(
                "CRITICAL FAILURE after {} iteration(s) - loop stopped",
                self.iterations_run
            )
        } else if self.max_iterations_reached {
            format!(
                "Max iterations ({}) reached - verification still failing",
                self.iterations_run
            )
        } else {
            format!("Loop ended after {} iteration(s)", self.iterations_run)
        }
    }
}

/// Configuration for a single stage within a multi-stage workflow.
///
/// Each stage has its own steps and execution settings. The loop controller
/// iterates over stages sequentially, running each stage's verification-agentic
/// loop independently.
#[derive(Debug, Clone)]
pub struct StageConfig {
    /// Unique identifier for this stage.
    pub id: String,
    /// Display name for this stage.
    pub name: String,
    /// Index of this stage (0-indexed).
    pub index: usize,
    /// Total number of stages in the workflow.
    pub total_stages: usize,
    /// Setup automation steps for this stage.
    pub setup_automation_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Setup prompt steps for this stage.
    pub setup_prompt_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Verification steps for this stage.
    pub verification_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Agentic steps for this stage.
    pub agentic_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Completion automation steps for this stage.
    pub completion_automation_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Completion prompt steps for this stage.
    pub completion_prompt_steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Maximum iterations for this stage's loop.
    pub max_iterations: u32,
    /// AI provider override for this stage (None = use workflow default).
    pub provider: Option<String>,
    /// Model override for this stage (None = use workflow default).
    pub model: Option<String>,
    /// Per-phase model overrides for this stage.
    pub model_overrides: HashMap<String, ModelOverrideConfig>,
    /// Timeout in seconds for this stage's AI sessions.
    pub timeout_seconds: Option<u64>,
    /// Whether to pause for human approval after each agentic phase.
    pub approval_gate: bool,
    /// Optional condition for conditional stage execution.
    /// When set, the stage is evaluated against this condition before running.
    /// If the condition is not met, the stage is skipped.
    pub condition: Option<crate::unified_workflows::StageCondition>,
    /// When true, run completion prompt steps BEFORE automation steps.
    /// Default (false) runs automation first, then prompts.
    /// This is used by meta-workflows to ensure the AI hardener runs
    /// before save_workflow_artifact persists the final result.
    pub completion_prompts_first: bool,
}

/// Context from a CI failure that triggered an auto-resume.
/// Populated by the PR watcher and consumed by the failure context builder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CiFailureContext {
    /// Names of CI checks that failed (e.g., "build", "test", "lint").
    pub failed_check_names: Vec<String>,
    /// Truncated log output per check (check_name -> log excerpt).
    pub check_logs: HashMap<String, String>,
    /// PR number that triggered this resume (if applicable).
    pub pr_number: Option<u64>,
    /// Whether the PR has a merge conflict.
    pub merge_conflict: bool,
}

/// Configuration for the verification-agentic loop.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum number of iterations before giving up
    pub max_iterations: u32,
    /// The base prompt for the AI
    pub base_prompt: String,
    /// Workflow name (for logging)
    pub workflow_name: String,
    /// Workflow ID
    pub workflow_id: String,
    /// Execution ID (task_run_id)
    pub execution_id: String,
    /// Error IDs targeted by this workflow (for auto-resolution on success).
    /// When the workflow completes successfully, these errors will be marked as resolved.
    pub targeted_error_ids: Vec<i64>,
    /// Starting iteration for resumed workflows (0 = start fresh, N = resume from iteration N)
    pub starting_iteration: u32,
    /// Run agentic phase before verification on first iteration.
    /// This is useful for error-fix workflows where we want the AI to attempt
    /// a fix before verification runs (since verification may pass immediately
    /// if it only checks current state, not whether fixes were applied).
    pub run_agentic_first: bool,
    /// Artifact directory for sharing files between phases.
    /// Created automatically at the start of execution.
    /// Steps can reference it via `{{artifact_dir}}` variable substitution.
    pub artifact_dir: Option<PathBuf>,
    /// Whether this is a dev mode execution (enables dev-only features like
    /// self-analysis steps and learning outcome recording).
    pub is_dev_mode: bool,
    /// Whether to run a completion sweep after verification passes.
    pub enable_sweep: bool,
    /// Maximum number of sweep iterations.
    pub max_sweep_iterations: u32,
    /// Stages for multi-stage workflows. Empty = single-stage (backward compat).
    pub stages: Vec<StageConfig>,
    /// Whether to stop execution if a stage fails verification (default: false).
    pub stop_on_failure: bool,
    /// Per-constraint overrides: map of constraint_id to enabled (true) / disabled (false).
    /// Applied to the constraint engine at the start of execution.
    pub constraint_overrides: HashMap<String, bool>,
    /// Whether to enable reflection mode during agentic iterations.
    /// When true, the AI investigates root causes before fixing failures.
    pub reflection_mode: bool,
    /// Optional AI provider override for this loop (from stage config).
    pub provider_override: Option<String>,
    /// Optional AI model override for this loop (from stage config).
    pub model_override: Option<String>,
    /// Per-phase model overrides (from stage/workflow config).
    pub model_overrides: HashMap<String, ModelOverrideConfig>,
    /// Stage index when running as part of a multi-stage workflow (None = single-stage).
    pub stage_index: Option<u32>,
    /// Maximum total AI sessions across all stages (None = unlimited).
    /// Checked against the task_run's sessions_count in the database.
    pub max_sessions: Option<u32>,
    /// Whether to auto-run the generated workflow after a meta-workflow completes.
    /// When true, the backend reads `result_data.generated_workflow_id` and spawns
    /// the generated workflow without relying on frontend polling.
    pub auto_run_generated: bool,
    /// Whether to pause for human approval after each agentic phase.
    /// When true, the loop controller registers an approval request and waits
    /// for a human response before continuing to the next verification iteration.
    pub approval_gate: bool,
    /// Whether approval gates should block execution (opt-in interactive mode).
    ///
    /// Default: `false` — approval gates are non-blocking. Questions are displayed
    /// immediately and recorded as deferred questions, but execution continues
    /// autonomously. Users can review decisions post-run.
    ///
    /// When `true`, the legacy blocking behavior is used: execution pauses and
    /// waits for a human response via the approval registry.
    pub blocking_approval: bool,
    /// Confidence threshold for deferred approval gates (0.0-1.0).
    ///
    /// When confidence exceeds this threshold and the risk level is not
    /// Irreversible, the system silently auto-approves without emitting
    /// a deferred question. Below threshold, a question is recorded.
    ///
    /// Default: 0.85
    pub confidence_threshold: f64,
    /// Maximum token budget for iteration context building.
    /// Context sections are added in priority order until this budget is reached.
    /// Default: 100_000 (~400K chars). Set lower to reduce prompt size.
    pub max_context_tokens: usize,
    /// When true, the pipeline will stop execution if accumulated token usage
    /// exceeds `max_context_tokens`. Disabled by default — only logs warnings.
    pub enforce_token_budget: bool,
    /// Whether to include knowledge from similar workflows (cross-workflow learning).
    /// When true, the first iteration queries the knowledge base for insights
    /// from other workflows with similar names.
    pub cross_workflow_learning: bool,
    /// Per-step verification history tracking pass/fail across iterations.
    /// Key: step name, Value: vector of pass (true) / fail (false) results per iteration.
    /// Runtime-only state — not persisted. Populated during loop execution.
    pub verification_history: HashMap<String, Vec<bool>>,
    /// Runtime routing context for conditional model routing.
    /// Updated by the loop controller before each phase invocation.
    pub routing_context: RoutingContext,
    /// Project/workspace path for project-scoped learning.
    /// When set, project reflection knowledge is loaded into AI context.
    pub project_path: Option<String>,
    /// Acceptance criteria from the specification agent (for canvas panel).
    /// When present, the canvas panel manager emits an AcceptanceCriteria panel
    /// and tracks criterion status as verification results come in.
    pub acceptance_criteria: Option<serde_json::Value>,
    /// Enable multi-agent fixer mode for the agentic phase.
    ///
    /// When true, verification failures are triaged by a fast classification agent,
    /// then fixed by specialized agents (quick-fix for lint/compilation, feature-fix
    /// for missing functionality) with targeted verification between each fix.
    /// Falls back to the standard monolithic session if triage fails.
    ///
    /// Default: false (standard single-session agentic phase).
    pub multi_agent_mode: bool,
    /// Policy for automatic git rollback when the workflow fails.
    /// Only applies when the workflow ends due to critical failure, max iterations,
    /// or unfixable errors. Default: None (no rollback).
    pub rollback_policy: RollbackPolicy,
    /// Escalation policy for stuck loops.
    /// Controls warnings, rethink prompts, and time limits when the loop
    /// fails to make progress.
    pub escalation_policy: super::blame::EscalationPolicy,
    /// Accumulated iteration diffs (runtime state, not persisted in config).
    /// Populated during loop execution for cross-iteration context injection.
    pub iteration_diffs: Vec<IterationDiff>,
    /// Restrict working directory resolution to the workspace boundary.
    /// Propagated to HandlerContext.path_scope_policy during step execution.
    pub strict_cwd: bool,
    /// Tags for per-execution tool whitelisting.
    pub tool_tags: Vec<String>,
    /// Run the workflow in an isolated git worktree.
    pub use_worktree: bool,
    /// Worktree path (set at runtime, not user-configurable).
    pub worktree_path: Option<String>,
    /// Worktree branch name (set at runtime).
    pub worktree_branch: Option<String>,
    /// Workflow execution architecture override.
    /// When set to "agentic_verification", the loop controller uses the agentic
    /// verification loop instead of the traditional verification-agentic loop.
    pub workflow_architecture: Option<crate::agentic_verification::WorkflowArchitecture>,
    /// Configuration for the agentic verification loop (only used when
    /// workflow_architecture = AgenticVerification).
    pub agentic_verification_config: Option<crate::agentic_verification::AgenticVerificationConfig>,
    /// Configuration for the multi-agent pipeline architecture (only used when
    /// workflow_architecture = MultiAgentPipeline).
    pub multi_agent_pipeline_config: Option<crate::agentic_verification::MultiAgentPipelineConfig>,
    /// Active canary rollout for this execution (runtime state, set by loop_controller::run).
    /// Tuple of (canary_id, recommendation_id). None if no canary is active.
    pub active_canary: Option<(String, String)>,
    /// Whether this run was selected as the canary variant (vs baseline).
    pub is_canary_run: bool,
    /// Optional per-phase timeout in milliseconds.
    /// When set, each phase (setup/verification/agentic/completion) is capped at this duration.
    pub phase_timeout_ms: Option<u64>,
    /// Maximum consecutive non-improving iterations before escalating to HITL.
    /// Default: 3. Set to 0 to disable fix-attempt tracking (preserves existing behavior).
    pub max_fix_attempts: u32,
    /// Maximum number of CI-triggered auto-resumes before requiring manual intervention.
    /// Used by the PR watcher (Phase 2). Default: 10.
    pub max_ci_auto_resumes: u32,
    /// CI failure context injected by the PR watcher when auto-resuming after CI failure.
    /// When present, failed check names and logs are prepended to the failure context.
    pub ci_failure_context: Option<CiFailureContext>,
    /// HTN planning configuration. When enabled, the loop attempts structured
    /// plan-based fixes before falling back to AI agentic sessions.
    pub htn_config: crate::planning_bridge::HtnConfig,
}

impl LoopConfig {
    /// Build a LoopConfig from a UnifiedWorkflow and runtime parameters.
    ///
    /// This captures the common pattern of converting a saved workflow into
    /// a LoopConfig for execution. Caller-specific fields (execution_id,
    /// base_prompt, stages, run_agentic_first, auto_run_generated) must be
    /// provided explicitly since they differ per call site.
    pub fn from_workflow(
        workflow: &crate::unified_workflows::UnifiedWorkflow,
        execution_id: String,
        base_prompt: String,
        stages: Vec<StageConfig>,
        run_agentic_first: bool,
        auto_run_generated: bool,
    ) -> Self {
        Self {
            max_iterations: workflow.iter_cap(),
            base_prompt,
            workflow_name: workflow.name.clone(),
            workflow_id: workflow.id.clone(),
            execution_id,
            targeted_error_ids: workflow.targeted_error_ids.clone(),
            starting_iteration: 0,
            run_agentic_first,
            artifact_dir: None,
            is_dev_mode: cfg!(debug_assertions),
            enable_sweep: workflow.enable_sweep,
            max_sweep_iterations: workflow.max_sweep_iterations,
            stages,
            stop_on_failure: workflow.stop_on_failure,
            constraint_overrides: workflow.constraint_overrides.clone(),
            reflection_mode: workflow.reflection_mode,
            provider_override: None,
            model_override: None,
            model_overrides: workflow.model_overrides.clone(),
            stage_index: None,
            // max_sessions is a separate concept from max_iterations but has
            // historically tracked it — if the user asked for unlimited
            // iterations, let the session count be unlimited too.
            max_sessions: workflow.max_iterations,
            auto_run_generated,
            approval_gate: workflow.approval_gate,
            blocking_approval: false,
            confidence_threshold: 0.85,
            max_context_tokens: 100_000,
            enforce_token_budget: workflow.enforce_token_budget,
            cross_workflow_learning: true,
            multi_agent_mode: workflow.multi_agent_mode,
            rollback_policy: RollbackPolicy::from_str(
                workflow.rollback_policy.as_deref().unwrap_or("none"),
            ),
            escalation_policy: super::blame::EscalationPolicy::default(),
            iteration_diffs: Vec::new(),
            // strict_cwd is true if either the workflow or global setting enables it
            strict_cwd: workflow.strict_cwd || crate::settings::get_path_settings().strict_mode,
            tool_tags: workflow.tool_tags.clone(),
            use_worktree: workflow.use_worktree,
            worktree_path: None,
            worktree_branch: None,
            workflow_architecture: workflow.workflow_architecture.clone(),
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            active_canary: None,
            is_canary_run: false,
            verification_history: HashMap::new(),
            routing_context: Default::default(),
            project_path: crate::mcp::shared::current_project_path(),
            acceptance_criteria: workflow.acceptance_criteria.clone(),
            phase_timeout_ms: workflow.phase_timeouts_json.as_ref().and_then(|json| {
                serde_json::from_str::<crate::orchestrator::state::PhaseTimeouts>(json)
                    .ok()
                    .and_then(|pt| pt.execution_ms)
            }),
            max_fix_attempts: workflow.max_fix_attempts,
            max_ci_auto_resumes: workflow.max_ci_auto_resumes,
            ci_failure_context: None,
            htn_config: {
                let mut htn = crate::planning_bridge::HtnConfig::default();
                htn.enabled = workflow.htn_enabled;
                htn.ui_bridge_url = workflow.htn_ui_bridge_url.clone();
                htn.state_machine_path = workflow.htn_state_machine_path.clone();
                // Default state_machine_path to bundled runner_state_machine.json when enabled
                if htn.enabled && htn.state_machine_path.is_none() {
                    let data_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
                    htn.state_machine_path = Some(
                        data_dir.join("runner_state_machine.json").display().to_string(),
                    );
                }
                htn
            },
        }
    }

    /// Infer the best workflow architecture if not explicitly set.
    ///
    /// Heuristic: when the workflow has >3 verification steps spanning
    /// distinct concerns (different file paths, step names, or check types),
    /// the multi-agent pipeline is a better fit than the traditional loop
    /// because it can parallelize independent subtrees.
    ///
    /// Only upgrades from None → MultiAgentPipeline; never overrides an
    /// explicit user choice.
    pub fn infer_architecture(&mut self) {
        use crate::agentic_verification::WorkflowArchitecture;

        // Don't override explicit choices
        if self.workflow_architecture.is_some() {
            return;
        }

        // Count total verification steps across all stages
        let total_verification_steps: usize =
            self.stages.iter().map(|s| s.verification_steps.len()).sum();

        // Need >3 verification steps to justify pipeline overhead
        if total_verification_steps <= 3 {
            return;
        }

        // Check for distinct concerns by collecting unique step name prefixes
        // and working directories. More diversity = better pipeline fit.
        let mut distinct_concerns = std::collections::HashSet::new();
        for stage in &self.stages {
            for step in &stage.verification_steps {
                // Use step name prefix (before first ':' or '_') as a concern proxy
                if let Some(ref name) = step.name {
                    let concern = name
                        .split([':', '_', ' '])
                        .next()
                        .unwrap_or(name)
                        .to_lowercase();
                    distinct_concerns.insert(concern);
                }
                // Working directory as a distinct concern
                if let Some(ref wd) = step.shell_command_working_directory {
                    distinct_concerns.insert(wd.clone());
                }
                // Check type as a distinct concern
                distinct_concerns.insert(step.step_type.clone());
            }
        }

        // Require ≥3 distinct concerns alongside >3 steps
        if distinct_concerns.len() >= 3 {
            tracing::info!(
                "Architecture inference: {} verification steps with {} distinct concerns → MultiAgentPipeline",
                total_verification_steps,
                distinct_concerns.len()
            );
            self.workflow_architecture = Some(WorkflowArchitecture::MultiAgentPipeline);
            // Set default pipeline config if not already present
            if self.multi_agent_pipeline_config.is_none() {
                self.multi_agent_pipeline_config = Some(Default::default());
            }
        }
    }

    /// Update the routing context with current loop state.
    pub fn set_routing_context(&mut self, iteration: u32, verification_failures: u32) {
        // Preserve signal enrichment fields from prior iterations
        let prev = &self.routing_context;
        self.routing_context = RoutingContext {
            iteration,
            verification_failures,
            stage_index: self.stage_index,
            word_count: prev.word_count,
            code_block_count: prev.code_block_count,
            cross_file_dep_count: prev.cross_file_dep_count,
            has_error_context: prev.has_error_context,
            iteration_number: iteration,
            previous_fix_failed: prev.previous_fix_failed,
        };
    }

    /// Resolve the model for a specific phase, considering routing rules.
    /// If routing rules exist and the routing context is populated, evaluates rules first.
    /// Fallback chain: routing rules → phase-specific override → workflow-level model_override → None.
    /// The sentinel value `"__smart__"` is treated as None (let the router decide).
    pub fn resolve_model_for_phase(&self, phase: &str) -> Option<String> {
        // Check if there are routing rules for this phase
        if let Some(config) = self.model_overrides.get(phase) {
            if let Some(ref rules) = config.routing_rules {
                if !rules.is_empty() && self.routing_context.iteration > 0 {
                    let resolved = evaluate_routing_rules(rules, &self.routing_context, config);
                    let model = resolved.model.or_else(|| self.model_override.clone());
                    return match model.as_deref() {
                        Some("__smart__") => None,
                        _ => model,
                    };
                }
            }
        }

        let model = self
            .model_overrides
            .get(phase)
            .and_then(|c| c.model.clone())
            .or_else(|| self.model_override.clone());

        // "__smart__" sentinel means "let the router decide"
        match model.as_deref() {
            Some("__smart__") => None,
            _ => model,
        }
    }

    /// Resolve the provider for a specific phase, considering routing rules.
    /// If routing rules exist and the routing context is populated, evaluates rules first.
    /// Fallback chain: routing rules → phase-specific override → workflow-level provider_override → None.
    pub fn resolve_provider_for_phase(&self, phase: &str) -> Option<String> {
        // Check if there are routing rules for this phase
        if let Some(config) = self.model_overrides.get(phase) {
            if let Some(ref rules) = config.routing_rules {
                if !rules.is_empty() && self.routing_context.iteration > 0 {
                    let resolved = evaluate_routing_rules(rules, &self.routing_context, config);
                    return resolved.provider.or_else(|| self.provider_override.clone());
                }
            }
        }

        self.model_overrides
            .get(phase)
            .and_then(|c| c.provider.clone())
            .or_else(|| self.provider_override.clone())
    }

    /// Resolve the temperature override for a specific phase.
    pub fn resolve_temperature_for_phase(&self, phase: &str) -> Option<f32> {
        self.model_overrides.get(phase).and_then(|c| c.temperature)
    }

    /// Resolve the max_tokens override for a specific phase.
    pub fn resolve_max_tokens_for_phase(&self, phase: &str) -> Option<u32> {
        self.model_overrides.get(phase).and_then(|c| c.max_tokens)
    }

    /// Resolve the fallback model for a specific phase.
    pub fn resolve_fallback_model_for_phase(&self, phase: &str) -> Option<String> {
        self.model_overrides
            .get(phase)
            .and_then(|c| c.fallback_model.clone())
    }

    /// Resolve the fallback provider for a specific phase.
    pub fn resolve_fallback_provider_for_phase(&self, phase: &str) -> Option<String> {
        self.model_overrides
            .get(phase)
            .and_then(|c| c.fallback_provider.clone())
    }

    /// Resolve model for a phase with runtime context for conditional routing.
    ///
    /// If the phase has routing_rules, evaluates them against the context.
    /// Otherwise falls back to the static resolve chain.
    pub fn resolve_model_for_phase_with_context(
        &self,
        phase: &str,
        ctx: &RoutingContext,
    ) -> ResolvedModelConfig {
        if let Some(config) = self.model_overrides.get(phase) {
            if let Some(ref rules) = config.routing_rules {
                if !rules.is_empty() {
                    let resolved = evaluate_routing_rules(rules, ctx, config);
                    // Apply workflow-level fallback for model/provider
                    return ResolvedModelConfig {
                        model: resolved.model.or_else(|| self.model_override.clone()),
                        provider: resolved.provider.or_else(|| self.provider_override.clone()),
                        temperature: resolved.temperature,
                        max_tokens: resolved.max_tokens,
                    };
                }
            }
        }

        // No routing rules — use static resolution
        ResolvedModelConfig {
            model: self.resolve_model_for_phase(phase),
            provider: self.resolve_provider_for_phase(phase),
            temperature: self.resolve_temperature_for_phase(phase),
            max_tokens: self.resolve_max_tokens_for_phase(phase),
        }
    }
}

/// Result of the completion sweep phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepResult {
    /// Number of sweep iterations executed.
    pub iterations_run: u32,
    /// Whether the AI signaled no more work needed.
    pub no_more_steps: bool,
}

/// Phase of workflow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    Setup,
    Verification,
    Agentic,
    Completion,
}

impl WorkflowPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkflowPhase::Setup => "setup",
            WorkflowPhase::Verification => "verification",
            WorkflowPhase::Agentic => "agentic",
            WorkflowPhase::Completion => "completion",
        }
    }
}

/// Outcome of attempting to run the agentic phase.
#[derive(Debug, Clone)]
pub enum AgenticOutcome {
    /// AI ran successfully
    Success {
        output: String,
        parsed: Option<super::agentic_output::AgenticPhaseOutput>,
        /// Input tokens consumed (available for API providers only).
        input_tokens: Option<u64>,
        /// Output tokens generated (available for API providers only).
        output_tokens: Option<u64>,
    },
    /// AI ran but reported failure
    Failed {
        output: String,
        error: String,
        parsed: Option<super::agentic_output::AgenticPhaseOutput>,
        /// Input tokens consumed (available for API providers only).
        input_tokens: Option<u64>,
        /// Output tokens generated (available for API providers only).
        output_tokens: Option<u64>,
    },
    /// AI execution errored out
    Error { error: String },
    /// Budget exceeded or circuit breaker tripped — run should stop gracefully
    BudgetExceeded { reason: String },
    /// Skipped (no agentic steps defined)
    Skipped,
}

impl AgenticOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, AgenticOutcome::Success { .. })
    }

    pub fn output(&self) -> Option<&str> {
        match self {
            AgenticOutcome::Success { output, .. } => Some(output),
            AgenticOutcome::Failed { output, .. } => Some(output),
            AgenticOutcome::Error { error } | AgenticOutcome::BudgetExceeded { reason: error } => {
                Some(error)
            }
            AgenticOutcome::Skipped => None,
        }
    }

    /// Get the token usage from the AI session, if available.
    pub fn token_usage(&self) -> (Option<u64>, Option<u64>) {
        match self {
            AgenticOutcome::Success {
                input_tokens,
                output_tokens,
                ..
            }
            | AgenticOutcome::Failed {
                input_tokens,
                output_tokens,
                ..
            } => (*input_tokens, *output_tokens),
            AgenticOutcome::Error { .. }
            | AgenticOutcome::BudgetExceeded { .. }
            | AgenticOutcome::Skipped => (None, None),
        }
    }

    /// Access the parsed structured output, if available.
    pub fn parsed(&self) -> Option<&super::agentic_output::AgenticPhaseOutput> {
        match self {
            AgenticOutcome::Success { parsed, .. } => parsed.as_ref(),
            AgenticOutcome::Failed { parsed, .. } => parsed.as_ref(),
            AgenticOutcome::Error { .. }
            | AgenticOutcome::BudgetExceeded { .. }
            | AgenticOutcome::Skipped => None,
        }
    }
}

/// Extract the parent task ID from a composed run child ID.
///
/// For composed runs, individual workflows have IDs like:
/// `composed-run-{timestamp}-workflow-{n}`
///
/// But only the parent `composed-run-{timestamp}` exists in task_runs.
/// This function extracts the parent ID for use in database operations that
/// reference task_runs (step checkpoints, task_run_events, etc.).
///
/// For non-composed IDs (e.g., `unified-workflow-{id}-{timestamp}`), returns
/// the original ID unchanged.
///
/// # Examples
///
/// ```
/// use qontinui_runner::unified_workflow_executor::get_parent_task_id;
///
/// // Composed run child → parent
/// assert_eq!(
///     get_parent_task_id("composed-run-1234567890-workflow-1"),
///     "composed-run-1234567890"
/// );
///
/// // Non-composed → unchanged
/// assert_eq!(
///     get_parent_task_id("unified-workflow-abc-1234567890"),
///     "unified-workflow-abc-1234567890"
/// );
/// ```
pub fn get_parent_task_id(execution_id: &str) -> String {
    // Check if this is a composed run child: composed-run-{timestamp}-workflow-{n}
    if let Some(pos) = execution_id.rfind("-workflow-") {
        let potential_parent = &execution_id[..pos];
        // Verify it looks like a composed run parent (starts with composed-run-)
        if potential_parent.starts_with("composed-run-") {
            return potential_parent.to_string();
        }
    }
    execution_id.to_string()
}

/// Check if an execution ID is a composed run child.
///
/// Returns true for IDs like `composed-run-{timestamp}-workflow-{n}`.
pub fn is_sequence_child(execution_id: &str) -> bool {
    get_parent_task_id(execution_id) != execution_id
}
