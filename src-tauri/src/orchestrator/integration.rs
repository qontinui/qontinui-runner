//! Orchestrator Integration Module
//!
//! This module provides the main entry points for running orchestrated tasks.
//! It ties together:
//! - Planning: Create verification plan at task start
//! - Execution: Run workers with knowledge context
//! - Verification: Run checks after work completion
//! - Iteration: Loop with knowledge accumulation until success or max iterations

use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

use std::collections::HashMap;

use crate::database::CheckpointDb;
use crate::mcp_api::AiOutputSessionContext;
use crate::orchestrator::{
    checkpoint::{
        CheckpointTrigger, CriterionResult, FindingSnapshot, KnowledgeEntry, StateSnapshot,
        VerificationSnapshot,
    },
    compression::CompressionConfig,
    context_propagation::RuntimeContext,
    hooks::{Hook, HookContext, HookExecutor, HookTrigger},
    knowledge::{
        build_iteration_context_with_compression, process_worker_output, KnowledgeBase,
        KnowledgeCategory,
    },
    output::{
        emit_context_injection, emit_deterministic_result, emit_finding_recorded,
        emit_iteration_start, emit_orchestrator_task_complete, emit_orchestrator_task_start,
        emit_plan_created, emit_planning_complete, emit_planning_start, emit_replanning,
        emit_verification_complete, emit_verification_feedback_recorded, emit_verification_start,
        emit_worker_signal,
    },
    planning::{create_replan, create_simple_plan, inject_plan_context},
    realtime_events::{
        emit_checkpoint_created, emit_learning_update, emit_task_completed, emit_task_started,
        emit_iteration_started as emit_realtime_iteration_started,
    },
    types::{
        DomainAssignment, DomainVerificationResult, IterationVerificationResults,
        TaskCompletionResult, VerificationPlan, WorkerCoordinationMessage, WorkerInstance,
        WorkerSignal, WorkerStatus,
    },
    verification::VerificationOrchestrator,
};

use super::types::{CriterionType, VerificationMethod};

// ============================================================================
// Worker Instructions (Auto-Injected)
// ============================================================================

/// Core instructions for worker agents in an orchestrated workflow.
/// These are automatically injected into worker prompts.
const WORKER_ORCHESTRATOR_INSTRUCTIONS: &str = r#"
## Orchestrator Context

You are a Worker Agent in an orchestrated workflow. The orchestrator manages:
- **Verification**: Running checks to determine if your work succeeded
- **Iteration**: Re-running you with feedback if verification fails
- **Completion**: Deciding when the task is done based on verification results

### Your Responsibilities

1. **Fix failing verification criteria** - You will receive feedback about what failed
2. **Use structured output** - Record findings with [FINDING:...] markers
3. **Stay focused** - Work on the issues identified, don't over-engineer

### Completion Protocol

**Do NOT output [TASK_COMPLETE]** - The orchestrator manages task completion based on verification results.

When you've addressed the issues you're aware of, simply finish your work. The orchestrator will:
1. Run verification to check your fixes
2. Give you feedback if more work is needed
3. Mark the task complete when all criteria pass

### Criterion Overrides

If a failing criterion should NOT be fixed (e.g., it's a false positive, an intentional pattern, or would require inappropriate refactoring), you can override it:

```
[CRITERION_OVERRIDE:criterion_id]
Item: ClassName or file/path being overridden
Justification: Clear explanation of why this is acceptable
[/CRITERION_OVERRIDE]
```

Use overrides sparingly and only with clear justification.
"#;

/// Generate verification guidance based on the criteria in a verification plan.
///
/// This creates context-specific instructions for each type of verification
/// the worker will encounter, so users don't have to manually document
/// verification types in their prompts.
pub fn generate_verification_guidance(plan: &VerificationPlan) -> String {
    let mut guidance = String::new();
    let mut seen_methods: Vec<VerificationMethod> = Vec::new();

    // Collect unique verification methods
    for criterion in &plan.success_criteria {
        if criterion.criterion_type == CriterionType::Deterministic {
            if let Some(method) = criterion.verification_method {
                if !seen_methods.contains(&method) {
                    seen_methods.push(method);
                }
            }
        }
    }

    // Check for AI-evaluated criteria
    let has_ai_criteria = plan.success_criteria.iter().any(|c| c.criterion_type == CriterionType::AiEvaluated);

    if seen_methods.is_empty() && !has_ai_criteria {
        return guidance;
    }

    guidance.push_str("\n### Verification Types in This Workflow\n\n");

    for method in &seen_methods {
        let (name, description) = get_verification_method_guidance(method);
        guidance.push_str(&format!("**{}**: {}\n\n", name, description));
    }

    if has_ai_criteria {
        guidance.push_str("**AI-Evaluated**: Some criteria are evaluated by AI reviewing screenshots or output. These will be assessed based on visual/semantic matching of expected results.\n\n");
    }

    guidance
}

/// Get human-readable guidance for a verification method.
fn get_verification_method_guidance(method: &VerificationMethod) -> (&'static str, &'static str) {
    match method {
        VerificationMethod::BuildSuccess => (
            "Build Check",
            "The project must build successfully. Run the build command to see errors and fix them.",
        ),
        VerificationMethod::UnitTest => (
            "Unit Tests",
            "Unit tests must pass. Run the test suite to identify failures and fix the underlying issues.",
        ),
        VerificationMethod::IntegrationTest => (
            "Integration Tests",
            "Integration tests must pass. These test component interactions and may require running services.",
        ),
        VerificationMethod::Playwright => (
            "Playwright Tests",
            "Playwright browser tests must pass. These test UI behavior in a real browser.",
        ),
        VerificationMethod::LogPattern => (
            "Log Pattern Check",
            "Specific patterns in log files are checked. Ensure the expected patterns appear (or don't appear) in logs.",
        ),
        VerificationMethod::GuiAutomation => (
            "GUI Automation",
            "Visual automation tests run against the application UI. The app must reach expected visual states.",
        ),
        VerificationMethod::TypeCheck => (
            "Type Check",
            "Type checking must pass (e.g., mypy, TypeScript). Run the type checker to see errors and add/fix type annotations.",
        ),
        VerificationMethod::LintCheck => (
            "Lint Check",
            "Linting must pass. Run the linter and either fix issues or apply auto-fixes where available.",
        ),
        VerificationMethod::CustomCommand => (
            "Custom Command",
            "A custom verification command must succeed (exit code 0). Check the command output for failure details.",
        ),
    }
}

/// Generate a brief summary of what criteria exist in the plan.
fn generate_criteria_summary(plan: &VerificationPlan) -> String {
    let mut summary = String::new();

    if plan.success_criteria.is_empty() {
        return summary;
    }

    summary.push_str("\n### Success Criteria Summary\n\n");

    for criterion in &plan.success_criteria {
        let critical_marker = if criterion.is_critical { "🔴" } else { "🟡" };
        summary.push_str(&format!(
            "- {} **{}**: {}\n",
            critical_marker, criterion.id, criterion.description
        ));
    }

    summary.push_str("\n🔴 = Critical (must pass), 🟡 = Non-critical (informational)\n");

    summary
}

// ============================================================================
// Orchestrator Configuration
// ============================================================================

/// Configuration for the orchestrator.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Maximum iterations before stopping
    pub max_iterations: u32,
    /// Timeout for AI calls (planning, verification)
    pub ai_timeout_seconds: u64,
    /// Working directory for running commands
    pub working_directory: String,
    /// Whether to enable planning phase
    pub enable_planning: bool,
    /// Whether to enable AI verification (in addition to deterministic)
    pub enable_ai_verification: bool,
    /// Whether to run verification before the first worker iteration.
    ///
    /// This is useful for non-automation workflows (like improve-all) where:
    /// - There is no GUI automation to test
    /// - The "work" is fixing code issues identified by verification
    /// - The worker needs verification results FIRST to know what to fix
    ///
    /// When true, after planning:
    /// 1. Run deterministic verification immediately
    /// 2. Store results as initial feedback for the worker
    /// 3. Worker gets feedback showing what needs to be fixed
    pub run_initial_verification: bool,
    /// Memory compression configuration for context management.
    /// When enabled, old knowledge entries are compressed into summaries
    /// when the context approaches token limits.
    pub compression: Option<CompressionConfig>,
    /// Whether to enable automatic checkpoint recording.
    /// When enabled, checkpoints are saved at key moments:
    /// - After planning completes
    /// - At iteration boundaries
    /// - Before and after verification runs
    /// - On errors and task completion
    pub enable_checkpointing: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            ai_timeout_seconds: 300,
            working_directory: ".".to_string(),
            enable_planning: true,
            enable_ai_verification: true,
            run_initial_verification: false,
            compression: Some(CompressionConfig::default()),
            enable_checkpointing: true,
        }
    }
}

// ============================================================================
// Orchestrator State
// ============================================================================

/// Timing information for a single iteration.
#[derive(Debug, Clone)]
pub struct IterationTiming {
    /// Iteration number (1-based)
    pub iteration: u32,
    /// When this iteration started
    pub started_at: Instant,
    /// Duration in seconds (set when iteration completes)
    pub duration_secs: Option<f64>,
    /// ISO timestamp when iteration started (for display)
    pub started_at_iso: String,
    /// ISO timestamp when iteration completed (for display)
    pub completed_at_iso: Option<String>,
}

/// Current state of an orchestrated task.
#[derive(Debug, Clone)]
pub struct OrchestratorState {
    /// Task run ID
    pub task_run_id: String,
    /// Current iteration (1-based)
    pub iteration: u32,
    /// Current verification plan
    pub plan: Option<VerificationPlan>,
    /// Plan database ID
    pub plan_id: Option<String>,
    /// Last verification results
    pub last_verification: Option<IterationVerificationResults>,
    /// Whether the task is complete
    pub is_complete: bool,
    /// Completion result (if complete)
    pub completion_result: Option<TaskCompletionResult>,

    /// Whether initial verification has been run (for verification-first workflows)
    pub initial_verification_run: bool,
    /// Initial verification results (for verification-first workflows)
    /// This is populated when run_initial_verification is true
    pub initial_verification: Option<IterationVerificationResults>,
    /// Initial feedback for the worker (populated from initial verification)
    pub initial_worker_feedback: Option<String>,

    // Multi-worker support (Phase 5)
    /// Individual worker instances tracked by worker_id
    pub workers: HashMap<String, WorkerInstance>,
    /// Domain assignments for this task
    pub domain_assignments: Vec<DomainAssignment>,
    /// Coordination messages between workers
    pub coordination_messages: Vec<WorkerCoordinationMessage>,
    /// Domain-specific verification results
    pub domain_verification_results: HashMap<String, DomainVerificationResult>,

    /// Runtime context for execution context propagation
    pub runtime_context: RuntimeContext,

    // ========================================================================
    // Task Duration Tracking
    // ========================================================================

    /// When the task was initialized (for total duration calculation)
    pub started_at: Option<Instant>,
    /// ISO timestamp when task started (for display/logging)
    pub started_at_iso: Option<String>,
    /// ISO timestamp when task completed (for display/logging)
    pub completed_at_iso: Option<String>,
    /// Per-iteration timing information
    pub iteration_timings: Vec<IterationTiming>,
    /// When the current iteration started (used to calculate iteration duration)
    pub current_iteration_started_at: Option<Instant>,
}

impl OrchestratorState {
    pub fn new(task_run_id: String) -> Self {
        let runtime_context = RuntimeContext::with_task_run_id(&task_run_id);
        Self {
            task_run_id,
            iteration: 0,
            plan: None,
            plan_id: None,
            last_verification: None,
            is_complete: false,
            completion_result: None,
            initial_verification_run: false,
            initial_verification: None,
            initial_worker_feedback: None,
            workers: HashMap::new(),
            domain_assignments: Vec::new(),
            coordination_messages: Vec::new(),
            domain_verification_results: HashMap::new(),
            runtime_context,
            started_at: None,
            started_at_iso: None,
            completed_at_iso: None,
            iteration_timings: Vec::new(),
            current_iteration_started_at: None,
        }
    }

    /// Calculate total task duration in seconds from started_at to now.
    pub fn total_duration_secs(&self) -> Option<f64> {
        self.started_at.map(|start| start.elapsed().as_secs_f64())
    }

    /// Calculate average iteration duration in seconds.
    pub fn average_iteration_duration_secs(&self) -> Option<f64> {
        let completed_timings: Vec<_> = self
            .iteration_timings
            .iter()
            .filter_map(|t| t.duration_secs)
            .collect();

        if completed_timings.is_empty() {
            None
        } else {
            let sum: f64 = completed_timings.iter().sum();
            Some(sum / completed_timings.len() as f64)
        }
    }

    /// Start timing for the current iteration.
    pub fn start_iteration_timing(&mut self, iteration: u32) {
        let now = Instant::now();
        let now_iso = chrono::Utc::now().to_rfc3339();

        self.current_iteration_started_at = Some(now);

        // Create timing record for this iteration
        self.iteration_timings.push(IterationTiming {
            iteration,
            started_at: now,
            duration_secs: None,
            started_at_iso: now_iso,
            completed_at_iso: None,
        });
    }

    /// Complete timing for the current iteration.
    pub fn complete_iteration_timing(&mut self) {
        if let Some(start) = self.current_iteration_started_at.take() {
            let duration_secs = start.elapsed().as_secs_f64();
            let completed_at_iso = chrono::Utc::now().to_rfc3339();

            // Update the last timing record (which should be the current iteration)
            if let Some(timing) = self.iteration_timings.last_mut() {
                timing.duration_secs = Some(duration_secs);
                timing.completed_at_iso = Some(completed_at_iso);
            }
        }
    }

    /// Create a new worker and add it to the state.
    pub fn create_worker(&mut self, worker_id: &str, name: &str, max_iterations: u32) -> &mut WorkerInstance {
        let worker = WorkerInstance::new(worker_id, name, max_iterations);
        self.workers.insert(worker_id.to_string(), worker);
        self.workers.get_mut(worker_id).unwrap()
    }

    /// Get a worker by ID.
    pub fn get_worker(&self, worker_id: &str) -> Option<&WorkerInstance> {
        self.workers.get(worker_id)
    }

    /// Get a mutable reference to a worker by ID.
    pub fn get_worker_mut(&mut self, worker_id: &str) -> Option<&mut WorkerInstance> {
        self.workers.get_mut(worker_id)
    }

    /// Get all active workers.
    pub fn active_workers(&self) -> Vec<&WorkerInstance> {
        self.workers.values().filter(|w| w.is_active()).collect()
    }

    /// Get workers assigned to a specific domain.
    pub fn workers_for_domain(&self, domain_id: &str) -> Vec<&WorkerInstance> {
        self.workers
            .values()
            .filter(|w| w.domain.as_deref() == Some(domain_id))
            .collect()
    }

    /// Check if all workers have completed.
    pub fn all_workers_complete(&self) -> bool {
        !self.workers.is_empty()
            && self.workers.values().all(|w| {
                matches!(w.status, WorkerStatus::Completed | WorkerStatus::Error)
            })
    }

    /// Check if all workers are awaiting verification.
    pub fn all_workers_awaiting_verification(&self) -> bool {
        !self.workers.is_empty()
            && self.workers.values().all(|w| {
                matches!(w.status, WorkerStatus::AwaitingVerification | WorkerStatus::Completed)
            })
    }

    /// Add a domain assignment.
    pub fn add_domain(&mut self, domain: DomainAssignment) {
        self.domain_assignments.push(domain);
    }

    /// Get a domain assignment by ID.
    pub fn get_domain(&self, domain_id: &str) -> Option<&DomainAssignment> {
        self.domain_assignments.iter().find(|d| d.domain_id == domain_id)
    }

    /// Get a mutable reference to a domain assignment by ID.
    pub fn get_domain_mut(&mut self, domain_id: &str) -> Option<&mut DomainAssignment> {
        self.domain_assignments.iter_mut().find(|d| d.domain_id == domain_id)
    }

    /// Assign a worker to a domain.
    pub fn assign_worker_to_domain(&mut self, worker_id: &str, domain_id: &str) -> Result<(), String> {
        // Update the worker
        if let Some(worker) = self.workers.get_mut(worker_id) {
            worker.assign_to_domain(domain_id);
        } else {
            return Err(format!("Worker '{}' not found", worker_id));
        }

        // Update the domain
        if let Some(domain) = self.domain_assignments.iter_mut().find(|d| d.domain_id == domain_id) {
            domain.assign_worker(worker_id);
        } else {
            return Err(format!("Domain '{}' not found", domain_id));
        }

        info!("Assigned worker '{}' to domain '{}'", worker_id, domain_id);
        Ok(())
    }

    /// Add a coordination message.
    pub fn add_coordination_message(&mut self, message: WorkerCoordinationMessage) {
        self.coordination_messages.push(message);
    }

    /// Get coordination messages for a specific worker.
    pub fn coordination_messages_for_worker(&self, worker_id: &str) -> Vec<&WorkerCoordinationMessage> {
        self.coordination_messages
            .iter()
            .filter(|m| match m {
                WorkerCoordinationMessage::FilesModified { worker_id: id, .. } => id != worker_id,
                WorkerCoordinationMessage::SharedFinding { worker_id: id, .. } => id != worker_id,
                WorkerCoordinationMessage::Blocked { waiting_for, .. } => waiting_for == worker_id,
                WorkerCoordinationMessage::ReadyForVerification { worker_id: id, .. } => id != worker_id,
                WorkerCoordinationMessage::SyncPoint { worker_ids, .. } => worker_ids.contains(&worker_id.to_string()),
            })
            .collect()
    }

    /// Store domain verification result.
    pub fn store_domain_verification(&mut self, result: DomainVerificationResult) {
        self.domain_verification_results.insert(result.domain_id.clone(), result);
    }

    /// Check if all domains have passed verification.
    pub fn all_domains_verified(&self) -> bool {
        if self.domain_assignments.is_empty() {
            return true;
        }

        self.domain_assignments.iter().all(|domain| {
            self.domain_verification_results
                .get(&domain.domain_id)
                .map(|r| r.all_passed)
                .unwrap_or(false)
        })
    }
}

// ============================================================================
// Orchestrator
// ============================================================================

/// The main orchestrator that coordinates task execution.
pub struct Orchestrator {
    pub config: OrchestratorConfig,
    pub db: Arc<CheckpointDb>,
    pub knowledge_base: KnowledgeBase,
    pub verifier: VerificationOrchestrator,
    /// Optional app handle for emitting AI output events.
    /// If None, no output events are emitted.
    app_handle: Option<tauri::AppHandle>,
    /// Optional session context for AI output events.
    session_ctx: Option<AiOutputSessionContext>,
    /// Hook executor for lifecycle events
    hook_executor: HookExecutor,
}

impl Orchestrator {
    /// Create a new orchestrator without output capabilities.
    pub fn new(config: OrchestratorConfig, db: Arc<CheckpointDb>) -> Self {
        let knowledge_base = KnowledgeBase::new(Arc::clone(&db));
        let verifier = VerificationOrchestrator::new(
            config.working_directory.clone(),
            config.ai_timeout_seconds,
        );

        Self {
            config,
            db,
            knowledge_base,
            verifier,
            app_handle: None,
            session_ctx: None,
            hook_executor: HookExecutor::empty(),
        }
    }

    /// Create a new orchestrator with output capabilities.
    pub fn new_with_output(
        config: OrchestratorConfig,
        db: Arc<CheckpointDb>,
        app_handle: tauri::AppHandle,
        session_ctx: Option<AiOutputSessionContext>,
    ) -> Self {
        let knowledge_base = KnowledgeBase::new(Arc::clone(&db));
        let verifier = VerificationOrchestrator::new(
            config.working_directory.clone(),
            config.ai_timeout_seconds,
        );

        Self {
            config,
            db,
            knowledge_base,
            verifier,
            app_handle: Some(app_handle),
            session_ctx,
            hook_executor: HookExecutor::empty(),
        }
    }

    /// Add hooks to the orchestrator.
    pub fn with_hooks(mut self, hooks: Vec<Hook>) -> Self {
        self.hook_executor = HookExecutor::new(hooks);
        self
    }

    /// Execute hooks for a trigger.
    fn execute_hooks(&self, trigger: HookTrigger, state: &OrchestratorState) {
        let context = HookContext::new(&state.task_run_id, "orchestrated_task")
            .with_iteration(state.iteration)
            .with_status(if state.is_complete { "complete" } else { "running" });

        let results = self.hook_executor.execute_trigger(trigger, &context);
        for result in results {
            if !result.success {
                warn!(
                    "Hook {} failed: {:?}",
                    result.hook_name,
                    result.error
                );
            }
        }
    }

    /// Set the app handle for output events.
    pub fn set_app_handle(&mut self, app_handle: tauri::AppHandle) {
        self.app_handle = Some(app_handle);
    }

    /// Set the session context for output events.
    pub fn set_session_context(&mut self, session_ctx: AiOutputSessionContext) {
        self.session_ctx = Some(session_ctx);
    }

    /// Get the session context reference for output calls.
    fn session_ctx_ref(&self) -> Option<&AiOutputSessionContext> {
        self.session_ctx.as_ref()
    }

    // ========================================================================
    // Checkpoint Recording
    // ========================================================================

    /// Save a checkpoint at the current state.
    ///
    /// This creates a StateSnapshot from the current OrchestratorState and
    /// saves it to the database. Checkpointing is controlled by the
    /// `enable_checkpointing` config flag.
    ///
    /// # Arguments
    /// * `state` - Current orchestrator state
    /// * `trigger` - What triggered this checkpoint
    /// * `name` - Optional human-readable name for the checkpoint
    fn save_checkpoint(
        &self,
        state: &OrchestratorState,
        trigger: CheckpointTrigger,
        name: Option<&str>,
    ) {
        // Skip if checkpointing is disabled
        if !self.config.enable_checkpointing {
            return;
        }

        // Create state snapshot from current OrchestratorState
        let snapshot = self.create_state_snapshot(state);

        // Serialize the trigger to a string representation
        let trigger_str = match &trigger {
            CheckpointTrigger::Automatic { reason } => format!("automatic:{}", reason),
            CheckpointTrigger::Manual => "manual".to_string(),
            CheckpointTrigger::BeforeOperation { operation } => {
                format!("before_operation:{}", operation)
            }
            CheckpointTrigger::AfterSuccess { operation } => {
                format!("after_success:{}", operation)
            }
            CheckpointTrigger::AfterFailure { error } => format!("after_failure:{}", error),
            CheckpointTrigger::VerificationBoundary => "verification_boundary".to_string(),
            CheckpointTrigger::IterationBoundary { iteration } => {
                format!("iteration_boundary:{}", iteration)
            }
            CheckpointTrigger::Custom { trigger } => format!("custom:{}", trigger),
        };

        // Serialize state snapshot to JSON
        let state_json = match serde_json::to_value(&snapshot) {
            Ok(json) => json,
            Err(e) => {
                warn!("Failed to serialize checkpoint state: {}", e);
                return;
            }
        };

        // Generate checkpoint ID
        let checkpoint_id = uuid::Uuid::new_v4().to_string();

        // Determine state name for the event
        let state_name = if state.is_complete {
            match &state.completion_result {
                Some(TaskCompletionResult::Success { .. }) => "completed_success",
                Some(TaskCompletionResult::Failed { .. }) => "completed_failed",
                Some(TaskCompletionResult::Stopped { .. }) => "stopped",
                Some(TaskCompletionResult::Paused { .. }) => "paused",
                None => "complete",
            }
        } else if state.plan.is_some() && state.iteration == 0 {
            "planned"
        } else if state.iteration > 0 {
            "executing"
        } else {
            "initializing"
        };

        // Save to database
        if let Err(e) = self.db.save_orchestrator_checkpoint(
            &checkpoint_id,
            &state.task_run_id,
            state.iteration,
            &trigger_str,
            &state_json,
            name,
        ) {
            warn!("Failed to save checkpoint: {}", e);
        } else {
            info!(
                "Saved checkpoint {} for task {} at iteration {} (trigger: {})",
                checkpoint_id, state.task_run_id, state.iteration, trigger_str
            );

            // Emit realtime event for UI updates
            if let Some(ref app_handle) = self.app_handle {
                emit_checkpoint_created(
                    app_handle,
                    &checkpoint_id,
                    &state.task_run_id,
                    state.iteration,
                    &trigger_str,
                    name,
                    state_name,
                );
            }
        }
    }

    /// Create a StateSnapshot from the current OrchestratorState.
    fn create_state_snapshot(&self, state: &OrchestratorState) -> StateSnapshot {
        // Determine state name based on current status
        let state_name = if state.is_complete {
            match &state.completion_result {
                Some(TaskCompletionResult::Success { .. }) => "completed_success",
                Some(TaskCompletionResult::Failed { .. }) => "completed_failed",
                Some(TaskCompletionResult::Stopped { .. }) => "stopped",
                Some(TaskCompletionResult::Paused { .. }) => "paused",
                None => "complete",
            }
        } else if state.plan.is_some() && state.iteration == 0 {
            "planned"
        } else if state.iteration > 0 {
            "executing"
        } else {
            "initializing"
        };

        let mut snapshot = StateSnapshot::new(state_name, state.iteration);

        // Add verification snapshot
        if let Some(ref verification) = state.last_verification {
            let mut criteria_results = HashMap::new();
            for result in verification
                .deterministic_results
                .iter()
                .chain(verification.ai_results.iter())
            {
                criteria_results.insert(
                    result.criterion_id.clone(),
                    CriterionResult {
                        criterion_id: result.criterion_id.clone(),
                        passed: result.passed,
                        reason: if result.issues.is_empty() {
                            None
                        } else {
                            Some(result.issues.join("; "))
                        },
                        verified_at: chrono::Utc::now().to_rfc3339(),
                    },
                );
            }

            snapshot.verification = VerificationSnapshot {
                criteria_results,
                overall_passed: verification.all_passed,
            };
        }

        // Add knowledge entries from knowledge base
        if let Ok(knowledge) = self.knowledge_base.get_all_knowledge(&state.task_run_id) {
            snapshot.knowledge = knowledge
                .iter()
                .map(|k| KnowledgeEntry {
                    id: k.id.clone(),
                    category: k.category.clone(),
                    content: k.content.clone(),
                    iteration: k.iteration as u32,
                })
                .collect();
        }

        // Add findings
        if let Some(ref completion) = state.completion_result {
            let findings = match completion {
                TaskCompletionResult::Success { findings, .. }
                | TaskCompletionResult::Failed { findings, .. }
                | TaskCompletionResult::Stopped { findings, .. }
                | TaskCompletionResult::Paused { findings, .. } => findings,
            };

            snapshot.findings = findings
                .iter()
                .map(|f| FindingSnapshot {
                    id: f.id.clone(),
                    category: f.finding_type.clone(),
                    severity: format!("{:?}", f.confidence),
                    description: f.description.clone(),
                    resolved: false,
                })
                .collect();
        }

        // Add files modified from workers
        let files_modified: Vec<String> = state
            .workers
            .values()
            .flat_map(|w| w.touched_files.iter().cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        snapshot.files_modified = files_modified;

        // Add plan info to custom data
        if let Some(ref plan) = state.plan {
            snapshot.custom_data.insert(
                "plan_version".to_string(),
                serde_json::json!(plan.version),
            );
            snapshot.custom_data.insert(
                "criteria_count".to_string(),
                serde_json::json!(plan.success_criteria.len()),
            );
        }

        // Add worker status to custom data
        if !state.workers.is_empty() {
            let worker_summary: HashMap<String, String> = state
                .workers
                .iter()
                .map(|(id, w)| (id.clone(), format!("{:?}", w.status)))
                .collect();
            snapshot
                .custom_data
                .insert("workers".to_string(), serde_json::json!(worker_summary));
        }

        snapshot
    }

    /// Initialize a new orchestrated task.
    ///
    /// This creates the verification plan if planning is enabled.
    pub fn initialize_task(
        &self,
        task_run_id: &str,
        goal: &str,
    ) -> Result<OrchestratorState, String> {
        info!(
            "Initializing orchestrated task {} with goal: {}",
            task_run_id, goal
        );

        // Emit task start output
        if let Some(app_handle) = &self.app_handle {
            emit_orchestrator_task_start(app_handle, task_run_id, goal, self.session_ctx_ref());
            // Emit realtime task status event
            emit_task_started(app_handle, task_run_id, self.config.max_iterations, Some(goal));
        }

        let mut state = OrchestratorState::new(task_run_id.to_string());

        // Set task start time for duration tracking
        state.started_at = Some(Instant::now());
        state.started_at_iso = Some(chrono::Utc::now().to_rfc3339());

        // Execute pre-execution hooks
        self.execute_hooks(HookTrigger::PreExecution, &state);

        // Create verification plan if enabled
        if self.config.enable_planning {
            // Emit planning start output
            if let Some(app_handle) = &self.app_handle {
                emit_planning_start(app_handle, goal, self.session_ctx_ref());
            }

            let plan_result = create_simple_plan(
                &self.db,
                task_run_id,
                goal,
                &self.config.working_directory,
                self.config.ai_timeout_seconds,
            )?;

            state.plan = Some(plan_result.plan.clone());
            state.plan_id = Some(plan_result.stored_plan_id);

            // Emit plan created output
            if let Some(app_handle) = &self.app_handle {
                emit_plan_created(app_handle, &plan_result.plan, self.session_ctx_ref());
                emit_planning_complete(app_handle, plan_result.plan.version, self.session_ctx_ref());
            }

            info!(
                "Created verification plan with {} criteria",
                state.plan.as_ref().map(|p| p.success_criteria.len()).unwrap_or(0)
            );

            // Save checkpoint after planning completes
            self.save_checkpoint(
                &state,
                CheckpointTrigger::AfterSuccess {
                    operation: "planning".to_string(),
                },
                Some("After planning complete"),
            );
        }

        Ok(state)
    }

    /// Run initial verification before the first worker iteration.
    ///
    /// This is used for verification-first workflows (like improve-all) where:
    /// - There is no GUI automation to test
    /// - The "work" is fixing code issues identified by verification
    /// - The worker needs verification results FIRST to know what to fix
    ///
    /// This method should be called after `initialize_task` when
    /// `config.run_initial_verification` is true.
    pub async fn run_initial_verification(
        &self,
        state: &mut OrchestratorState,
    ) -> Result<IterationVerificationResults, String> {
        let plan = state
            .plan
            .as_ref()
            .ok_or("No verification plan available")?;

        info!(
            "Running initial verification for task {} (iteration 0)",
            state.task_run_id
        );

        // Emit verification start output
        if let Some(app_handle) = &self.app_handle {
            emit_verification_start(
                app_handle,
                0, // Iteration 0 = initial verification
                plan.deterministic_criteria().len(),
                plan.ai_criteria().len(),
                self.session_ctx_ref(),
            );
        }

        // Run verification (no screenshot for initial verification)
        let results = self
            .verifier
            .verify(plan, 0, None)
            .await;

        // Emit results for each deterministic check
        if let Some(app_handle) = &self.app_handle {
            for result in &results.deterministic_results {
                emit_deterministic_result(app_handle, result, self.session_ctx_ref());
            }
        }

        // Store results in database
        if let Some(plan_id) = &state.plan_id {
            for result in &results.deterministic_results {
                let is_critical = plan
                    .success_criteria
                    .iter()
                    .find(|c| c.id == result.criterion_id)
                    .map(|c| c.is_critical)
                    .unwrap_or(true);

                let _ = self.db.create_orchestrator_verification_result(
                    &state.task_run_id,
                    plan_id,
                    0, // Iteration 0 = initial verification
                    result,
                    is_critical,
                );
            }
        }

        // Build feedback for the worker based on verification results
        let feedback = if !results.all_passed {
            let failed_criteria: Vec<_> = results
                .deterministic_results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| r.criterion_id.clone())
                .collect();

            self.knowledge_base.record_verification_feedback(
                &state.task_run_id,
                0, // Iteration 0
                &results.build_feedback(),
                &failed_criteria,
            )?;

            // Emit verification feedback recorded output
            if let Some(app_handle) = &self.app_handle {
                emit_verification_feedback_recorded(
                    app_handle,
                    0,
                    failed_criteria.len(),
                    self.session_ctx_ref(),
                );
            }

            Some(results.build_feedback())
        } else {
            None
        };

        // Emit verification complete output
        if let Some(app_handle) = &self.app_handle {
            emit_verification_complete(app_handle, &results, self.session_ctx_ref());
        }

        // Update state
        state.initial_verification_run = true;
        state.initial_verification = Some(results.clone());
        state.initial_worker_feedback = feedback;
        state.last_verification = Some(results.clone());

        info!(
            "Initial verification complete: {} passed, {} failed",
            results.deterministic_results.iter().filter(|r| r.passed).count(),
            results.deterministic_results.iter().filter(|r| !r.passed).count()
        );

        Ok(results)
    }

    /// Build the prompt for a worker iteration.
    ///
    /// This injects:
    /// 1. Verification plan context (if available)
    /// 2. Initial verification feedback (for verification-first workflows)
    /// 3. Cross-iteration context (findings, feedback from previous iterations)
    /// 4. Auto-generated orchestrator instructions (completion protocol, verification guidance)
    pub fn build_worker_prompt(
        &self,
        state: &OrchestratorState,
        base_prompt: &str,
    ) -> Result<String, String> {
        let mut prompt = base_prompt.to_string();

        // Inject verification plan context
        if let Some(plan) = &state.plan {
            prompt = inject_plan_context(&prompt, plan);
        }

        // For first iteration with initial verification, inject the verification feedback
        // This ensures the worker knows what issues to fix before starting
        if state.iteration == 0 && state.initial_verification_run {
            if let Some(ref feedback) = state.initial_worker_feedback {
                let initial_context = format!(
                    "## Initial Verification Results\n\n\
                     The system ran verification before you started and found the following issues that need to be fixed:\n\n\
                     {}\n\n\
                     Please address these issues as your primary focus.\n\n\
                     ---\n\n",
                    feedback
                );
                prompt = format!("{}{}", initial_context, prompt);

                info!(
                    "Injected initial verification feedback for task {}",
                    state.task_run_id
                );
            } else if let Some(ref results) = state.initial_verification {
                // Even if all passed, let the worker know verification ran
                if results.all_passed {
                    let initial_context = "## Initial Verification Results\n\n\
                         The system ran verification before you started and all checks passed.\n\
                         Review the verification plan criteria and ensure your work maintains these standards.\n\n\
                         ---\n\n";
                    prompt = format!("{}{}", initial_context, prompt);
                }
            }
        }

        // Inject cross-iteration context (with optional compression)
        let iteration_context = build_iteration_context_with_compression(
            &self.knowledge_base,
            &state.task_run_id,
            state.iteration,
            self.config.compression.as_ref(),
        )?;

        if !iteration_context.is_empty() {
            prompt = format!("{}\n{}", iteration_context, prompt);

            // Emit context injection output
            if let Some(app_handle) = &self.app_handle {
                // Count knowledge by category for output
                let findings_count = self.knowledge_base
                    .get_knowledge_by_category(&state.task_run_id, KnowledgeCategory::Finding)
                    .map(|k| k.len())
                    .unwrap_or(0);
                let feedback_count = self.knowledge_base
                    .get_knowledge_by_category(&state.task_run_id, KnowledgeCategory::VerificationFeedback)
                    .map(|k| k.len())
                    .unwrap_or(0);
                let observations_count = self.knowledge_base
                    .get_knowledge_by_category(&state.task_run_id, KnowledgeCategory::Observation)
                    .map(|k| k.len())
                    .unwrap_or(0);

                emit_context_injection(
                    app_handle,
                    findings_count,
                    feedback_count,
                    observations_count,
                    self.session_ctx_ref(),
                );
            }
        }

        // Append auto-generated orchestrator instructions
        // These come at the end so the worker sees them as a "reminder" after the task context
        prompt.push_str("\n\n---\n");
        prompt.push_str(WORKER_ORCHESTRATOR_INSTRUCTIONS);

        // Generate and append verification-type-specific guidance from the plan
        if let Some(plan) = &state.plan {
            let verification_guidance = generate_verification_guidance(plan);
            if !verification_guidance.is_empty() {
                prompt.push_str(&verification_guidance);
            }

            // Add criteria summary so worker knows exactly what will be checked
            let criteria_summary = generate_criteria_summary(plan);
            if !criteria_summary.is_empty() {
                prompt.push_str(&criteria_summary);
            }
        }

        Ok(prompt)
    }

    /// Process worker output and determine next action.
    ///
    /// This:
    /// 1. Parses worker signals and findings
    /// 2. Records findings in knowledge base
    /// 3. Returns the appropriate next action
    pub fn process_worker_output(
        &self,
        state: &mut OrchestratorState,
        output: &str,
    ) -> Result<WorkerOutputAction, String> {
        // Complete timing for the previous iteration (if any)
        state.complete_iteration_timing();

        state.iteration += 1;
        // Sync runtime context iteration
        state.runtime_context.set_iteration(state.iteration);

        // Start timing for this iteration
        state.start_iteration_timing(state.iteration);

        // Save checkpoint at iteration boundary (before processing)
        self.save_checkpoint(
            state,
            CheckpointTrigger::IterationBoundary {
                iteration: state.iteration,
            },
            Some(&format!("Iteration {} start", state.iteration)),
        );

        // Emit iteration start output
        if let Some(app_handle) = &self.app_handle {
            emit_iteration_start(
                app_handle,
                state.iteration,
                self.config.max_iterations,
                self.session_ctx_ref(),
            );
            // Emit realtime iteration event
            emit_realtime_iteration_started(
                app_handle,
                &state.task_run_id,
                state.iteration,
                self.config.max_iterations,
                None,
            );
        }

        // Execute pre-iteration hooks
        self.execute_hooks(HookTrigger::PreIteration, state);

        let (signal, findings) = process_worker_output(output);

        // Record any findings and emit output
        for finding in &findings {
            self.knowledge_base.record_finding(
                &state.task_run_id,
                finding,
                state.iteration,
            )?;

            // Emit finding recorded output
            if let Some(app_handle) = &self.app_handle {
                emit_finding_recorded(app_handle, finding, self.session_ctx_ref());
            }
        }

        // Determine action based on signal
        match signal {
            Some(WorkerSignal::WorkComplete { reason }) => {
                info!(
                    "Worker signaled WORK_COMPLETE (reason: {:?}) at iteration {}",
                    reason, state.iteration
                );

                // Emit worker signal output
                if let Some(app_handle) = &self.app_handle {
                    emit_worker_signal(
                        app_handle,
                        "WORK_COMPLETE",
                        reason.as_deref(),
                        self.session_ctx_ref(),
                    );
                }

                Ok(WorkerOutputAction::RunVerification)
            }
            Some(WorkerSignal::NeedReplan { reason }) => {
                info!(
                    "Worker signaled NEED_REPLAN (reason: {}) at iteration {}",
                    reason, state.iteration
                );

                // Emit worker signal and replanning output
                if let Some(app_handle) = &self.app_handle {
                    emit_worker_signal(
                        app_handle,
                        "NEED_REPLAN",
                        Some(&reason),
                        self.session_ctx_ref(),
                    );
                    emit_replanning(app_handle, &reason, self.session_ctx_ref());
                }

                Ok(WorkerOutputAction::Replan { reason })
            }
            Some(WorkerSignal::Finding(_)) => {
                // Finding was already recorded, continue working
                Ok(WorkerOutputAction::Continue)
            }
            Some(WorkerSignal::Continue) | None => {
                // Execute post-iteration hooks
                self.execute_hooks(HookTrigger::PostIteration, state);

                // Check if max iterations reached
                if state.iteration >= self.config.max_iterations {
                    warn!(
                        "Max iterations ({}) reached for task {}",
                        self.config.max_iterations, state.task_run_id
                    );
                    Ok(WorkerOutputAction::MaxIterationsReached)
                } else {
                    Ok(WorkerOutputAction::Continue)
                }
            }
        }
    }

    /// Run verification for the current iteration.
    ///
    /// Returns the verification results and updates the state.
    pub async fn run_verification(
        &self,
        state: &mut OrchestratorState,
        screenshot_base64: Option<&str>,
    ) -> Result<IterationVerificationResults, String> {
        let plan = state
            .plan
            .as_ref()
            .ok_or("No verification plan available")?;

        info!(
            "Running verification for iteration {} of task {}",
            state.iteration, state.task_run_id
        );

        // Save checkpoint before verification runs
        self.save_checkpoint(
            state,
            CheckpointTrigger::BeforeOperation {
                operation: "verification".to_string(),
            },
            Some(&format!(
                "Before verification (iteration {})",
                state.iteration
            )),
        );

        // Emit verification start output
        if let Some(app_handle) = &self.app_handle {
            emit_verification_start(
                app_handle,
                state.iteration,
                plan.deterministic_criteria().len(),
                plan.ai_criteria().len(),
                self.session_ctx_ref(),
            );
        }

        // Run verification
        let results = self
            .verifier
            .verify(plan, state.iteration, screenshot_base64)
            .await;

        // Emit results for each deterministic check
        if let Some(app_handle) = &self.app_handle {
            for result in &results.deterministic_results {
                emit_deterministic_result(app_handle, result, self.session_ctx_ref());
            }
        }

        // Emit results for each AI check
        if let Some(app_handle) = &self.app_handle {
            use crate::orchestrator::output::emit_ai_verification_result;
            for result in &results.ai_results {
                emit_ai_verification_result(app_handle, result, self.session_ctx_ref());
            }
        }

        // Store results in database
        if let Some(plan_id) = &state.plan_id {
            for result in &results.deterministic_results {
                let is_critical = plan
                    .success_criteria
                    .iter()
                    .find(|c| c.id == result.criterion_id)
                    .map(|c| c.is_critical)
                    .unwrap_or(true);

                let _ = self.db.create_orchestrator_verification_result(
                    &state.task_run_id,
                    plan_id,
                    state.iteration,
                    result,
                    is_critical,
                );
            }

            for result in &results.ai_results {
                let is_critical = plan
                    .success_criteria
                    .iter()
                    .find(|c| c.id == result.criterion_id)
                    .map(|c| c.is_critical)
                    .unwrap_or(true);

                let _ = self.db.create_orchestrator_verification_result(
                    &state.task_run_id,
                    plan_id,
                    state.iteration,
                    result,
                    is_critical,
                );
            }
        }

        // Record verification feedback if failed
        if !results.all_passed {
            let feedback = results.build_feedback();
            let failed_criteria: Vec<_> = results
                .deterministic_results
                .iter()
                .chain(results.ai_results.iter())
                .filter(|r| !r.passed)
                .map(|r| r.criterion_id.clone())
                .collect();

            self.knowledge_base.record_verification_feedback(
                &state.task_run_id,
                state.iteration,
                &feedback,
                &failed_criteria,
            )?;

            // Emit verification feedback recorded output
            if let Some(app_handle) = &self.app_handle {
                emit_verification_feedback_recorded(
                    app_handle,
                    state.iteration,
                    failed_criteria.len(),
                    self.session_ctx_ref(),
                );
            }

            // Execute verification fail hooks
            self.execute_hooks(HookTrigger::OnVerificationFail, state);
        }

        // Emit verification complete output
        if let Some(app_handle) = &self.app_handle {
            emit_verification_complete(app_handle, &results, self.session_ctx_ref());
        }

        state.last_verification = Some(results.clone());

        // Save checkpoint after verification completes (especially important on failure)
        if results.all_passed {
            self.save_checkpoint(
                state,
                CheckpointTrigger::AfterSuccess {
                    operation: "verification".to_string(),
                },
                Some(&format!(
                    "After verification passed (iteration {})",
                    state.iteration
                )),
            );
        } else {
            self.save_checkpoint(
                state,
                CheckpointTrigger::AfterFailure {
                    error: "verification_failed".to_string(),
                },
                Some(&format!(
                    "After verification failed (iteration {})",
                    state.iteration
                )),
            );
        }

        Ok(results)
    }

    /// Handle a replan request from a worker.
    pub fn handle_replan(
        &self,
        state: &mut OrchestratorState,
        reason: &str,
    ) -> Result<(), String> {
        info!(
            "Handling replan for task {} (reason: {})",
            state.task_run_id, reason
        );

        let plan_result = create_replan(
            &self.db,
            &state.task_run_id,
            &self.config.working_directory,
            reason,
            self.config.ai_timeout_seconds,
        )?;

        state.plan = Some(plan_result.plan.clone());
        state.plan_id = Some(plan_result.stored_plan_id);

        // Emit plan created output for the new plan
        if let Some(app_handle) = &self.app_handle {
            emit_plan_created(app_handle, &plan_result.plan, self.session_ctx_ref());
            emit_planning_complete(app_handle, plan_result.plan.version, self.session_ctx_ref());
        }

        Ok(())
    }

    /// Mark the task as complete.
    pub fn complete_task(
        &self,
        state: &mut OrchestratorState,
        result: TaskCompletionResult,
    ) {
        let (status_name, is_success, reason) = match &result {
            TaskCompletionResult::Success { .. } => {
                ("Success", true, None)
            }
            TaskCompletionResult::Failed { reason, .. } => {
                ("Failed", false, Some(reason.as_str()))
            }
            TaskCompletionResult::Stopped { .. } => {
                ("Stopped", false, Some("Task stopped by user"))
            }
            TaskCompletionResult::Paused { .. } => {
                ("Paused", false, Some("Max iterations reached"))
            }
        };

        info!("Marking task {} as complete: {}", state.task_run_id, status_name);

        // Complete timing for the final iteration
        state.complete_iteration_timing();

        // Set completion timestamp
        state.completed_at_iso = Some(chrono::Utc::now().to_rfc3339());

        // Execute completion hooks
        if is_success {
            self.execute_hooks(HookTrigger::OnComplete, state);
        }
        self.execute_hooks(HookTrigger::PostExecution, state);

        // Emit task complete output
        if let Some(app_handle) = &self.app_handle {
            emit_orchestrator_task_complete(
                app_handle,
                is_success,
                state.iteration,
                reason,
                self.session_ctx_ref(),
            );
            // Emit realtime task completion event
            emit_task_completed(
                app_handle,
                &state.task_run_id,
                state.iteration,
                self.config.max_iterations,
                None,
                is_success,
            );
        }

        // Record learning outcome to database for AI learning system
        self.record_learning_outcome(state, &result);

        // Set completion state (needed before checkpoint to capture final state)
        state.is_complete = true;
        state.completion_result = Some(result.clone());

        // Save checkpoint on task completion
        let trigger = if is_success {
            CheckpointTrigger::AfterSuccess {
                operation: "task_completion".to_string(),
            }
        } else {
            CheckpointTrigger::AfterFailure {
                error: reason.unwrap_or("unknown").to_string(),
            }
        };

        self.save_checkpoint(
            state,
            trigger,
            Some(&format!("Task {} - {}", status_name, state.task_run_id)),
        );
    }

    /// Record a learning outcome to the database for the AI learning system.
    fn record_learning_outcome(
        &self,
        state: &OrchestratorState,
        result: &TaskCompletionResult,
    ) {
        // Map completion result to status and extract data
        let (status, iterations, error_message, findings) = match result {
            TaskCompletionResult::Success { iterations, findings, .. } => {
                ("success", *iterations, None, findings.clone())
            }
            TaskCompletionResult::Failed { reason, iterations, findings, .. } => {
                ("failure", *iterations, Some(reason.clone()), findings.clone())
            }
            TaskCompletionResult::Stopped { at_iteration, findings, .. } => {
                ("abandoned", *at_iteration, Some("Task stopped by user".to_string()), findings.clone())
            }
            TaskCompletionResult::Paused { at_iteration, findings, .. } => {
                ("partial", *at_iteration, Some("Max iterations reached".to_string()), findings.clone())
            }
        };

        // Extract files touched from workers (collect unique files)
        let files_modified: Vec<String> = state.workers.values()
            .flat_map(|w| w.touched_files.iter().cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Strategy is not tracked at the orchestrator config level
        let strategy: Option<String> = None;

        // Calculate duration from started_at
        let duration_secs = state.total_duration_secs();

        // Build feedback JSON including findings and iteration timing metadata
        let mut feedback_data = serde_json::Map::new();

        if !findings.is_empty() {
            if let Ok(findings_json) = serde_json::to_value(&findings) {
                feedback_data.insert("findings".to_string(), findings_json);
            }
        }

        // Add iteration timing metadata for analysis
        if let Some(avg_duration) = state.average_iteration_duration_secs() {
            feedback_data.insert(
                "average_iteration_duration_secs".to_string(),
                serde_json::json!(avg_duration),
            );
        }

        // Add individual iteration timings
        let iteration_timing_data: Vec<serde_json::Value> = state
            .iteration_timings
            .iter()
            .map(|t| {
                serde_json::json!({
                    "iteration": t.iteration,
                    "duration_secs": t.duration_secs,
                    "started_at": t.started_at_iso,
                    "completed_at": t.completed_at_iso
                })
            })
            .collect();

        if !iteration_timing_data.is_empty() {
            feedback_data.insert(
                "iteration_timings".to_string(),
                serde_json::json!(iteration_timing_data),
            );
        }

        // Add task-level timestamps
        if let Some(ref started) = state.started_at_iso {
            feedback_data.insert("task_started_at".to_string(), serde_json::json!(started));
        }
        if let Some(ref completed) = state.completed_at_iso {
            feedback_data.insert(
                "task_completed_at".to_string(),
                serde_json::json!(completed),
            );
        }

        let feedback_json = if feedback_data.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(feedback_data))
        };

        // Record to database
        if let Err(e) = self.db.record_learning_outcome(
            &state.task_run_id,
            status,
            duration_secs,
            Some(iterations),
            strategy.as_deref(),
            None, // tools_used - not tracked at worker level
            if files_modified.is_empty() {
                None
            } else {
                Some(&files_modified)
            },
            None, // error_type
            error_message.as_deref(),
            feedback_json.as_ref(),
        ) {
            warn!("Failed to record learning outcome: {}", e);
        } else {
            info!(
                "Recorded learning outcome for task {} (status: {}, duration: {:.2}s)",
                state.task_run_id,
                status,
                duration_secs.unwrap_or(0.0)
            );

            // Emit realtime event for UI updates
            if let Some(ref app_handle) = self.app_handle {
                emit_learning_update(
                    app_handle,
                    &state.task_run_id,
                    status,
                    duration_secs,
                    Some(iterations),
                    strategy.as_deref(),
                    files_modified.clone(),
                );
            }
        }
    }

    /// Get accumulated findings for a task.
    pub fn get_findings(&self, task_run_id: &str) -> Result<Vec<crate::database::StoredTaskKnowledge>, String> {
        self.knowledge_base.get_all_knowledge(task_run_id)
    }

    // ========================================================================
    // Multi-Worker Methods (Phase 5)
    // ========================================================================

    /// Initialize a multi-worker task with domain assignments.
    ///
    /// This creates workers and assigns them to domains based on the plan.
    pub fn initialize_multi_worker_task(
        &self,
        task_run_id: &str,
        goal: &str,
        domains: Vec<DomainAssignment>,
    ) -> Result<OrchestratorState, String> {
        info!(
            "Initializing multi-worker task {} with {} domains",
            task_run_id,
            domains.len()
        );

        let mut state = self.initialize_task(task_run_id, goal)?;

        // Add domain assignments
        for domain in domains {
            state.add_domain(domain);
        }

        // Create workers for each domain
        for (i, domain) in state.domain_assignments.clone().iter().enumerate() {
            let worker_id = format!("worker-{}", i + 1);
            let worker_name = format!("Worker for {}", domain.name);
            state.create_worker(&worker_id, &worker_name, self.config.max_iterations);
            state.assign_worker_to_domain(&worker_id, &domain.domain_id)?;
        }

        info!(
            "Created {} workers for multi-worker task {}",
            state.workers.len(),
            task_run_id
        );

        Ok(state)
    }

    /// Assign a worker to a domain.
    ///
    /// This updates both the worker and domain state.
    pub fn assign_worker_to_domain(
        &self,
        state: &mut OrchestratorState,
        worker_id: &str,
        domain_id: &str,
    ) -> Result<(), String> {
        state.assign_worker_to_domain(worker_id, domain_id)
    }

    /// Get all workers assigned to a specific domain.
    pub fn get_workers_for_domain<'a>(
        &self,
        state: &'a OrchestratorState,
        domain_id: &str,
    ) -> Vec<&'a WorkerInstance> {
        state.workers_for_domain(domain_id)
    }

    /// Build a domain-scoped prompt for a worker.
    ///
    /// This injects:
    /// 1. Domain context (what the worker should focus on)
    /// 2. Verification plan context (filtered for domain)
    /// 3. Cross-iteration context
    /// 4. Coordination messages from other workers
    pub fn build_worker_prompt_for_domain(
        &self,
        state: &OrchestratorState,
        worker_id: &str,
        base_prompt: &str,
    ) -> Result<String, String> {
        let worker = state
            .get_worker(worker_id)
            .ok_or_else(|| format!("Worker '{}' not found", worker_id))?;

        let domain_id = worker
            .domain
            .as_ref()
            .ok_or_else(|| format!("Worker '{}' is not assigned to a domain", worker_id))?;

        let domain = state
            .get_domain(domain_id)
            .ok_or_else(|| format!("Domain '{}' not found", domain_id))?;

        let mut prompt = String::new();

        // Add domain context
        prompt.push_str("## Domain Assignment\n\n");
        prompt.push_str(&format!("**Domain:** {}\n", domain.name));
        prompt.push_str(&format!("**Description:** {}\n", domain.description));
        if !domain.file_patterns.is_empty() {
            prompt.push_str(&format!(
                "**Focus Areas:** {}\n",
                domain.file_patterns.join(", ")
            ));
        }
        prompt.push_str("\nYou are responsible for work within this domain. ");
        prompt.push_str("Other workers are handling different domains.\n\n");

        // Add domain-specific system prompt if provided
        if let Some(context) = &domain.system_prompt_context {
            prompt.push_str(&format!("### Domain Guidelines\n\n{}\n\n", context));
        }

        // Add coordination messages from other workers
        let coord_messages = state.coordination_messages_for_worker(worker_id);
        if !coord_messages.is_empty() {
            prompt.push_str("## Updates from Other Workers\n\n");
            for msg in coord_messages {
                match msg {
                    WorkerCoordinationMessage::FilesModified { worker_id: id, files } => {
                        prompt.push_str(&format!(
                            "- **{}** modified: {}\n",
                            id,
                            files.join(", ")
                        ));
                    }
                    WorkerCoordinationMessage::SharedFinding { worker_id: id, finding } => {
                        prompt.push_str(&format!(
                            "- **{}** found: {} ({})\n",
                            id, finding.description, finding.finding_type
                        ));
                    }
                    WorkerCoordinationMessage::Blocked { worker_id: id, reason, .. } => {
                        prompt.push_str(&format!("- **{}** is blocked: {}\n", id, reason));
                    }
                    WorkerCoordinationMessage::ReadyForVerification { worker_id: id, .. } => {
                        prompt.push_str(&format!("- **{}** is ready for verification\n", id));
                    }
                    WorkerCoordinationMessage::SyncPoint { reason, .. } => {
                        prompt.push_str(&format!("- **Sync point:** {}\n", reason));
                    }
                }
            }
            prompt.push_str("\n");
        }

        // Add verification plan context (filtered for domain)
        if let Some(plan) = &state.plan {
            let domain_criteria = plan.criteria_for_domain(domain_id);
            if !domain_criteria.is_empty() {
                prompt.push_str("## Domain Success Criteria\n\n");
                for (i, criterion) in domain_criteria.iter().enumerate() {
                    let critical_marker = if criterion.is_critical {
                        " [CRITICAL]"
                    } else {
                        " [informational]"
                    };
                    prompt.push_str(&format!(
                        "{}. **{}**{}: {}\n",
                        i + 1,
                        criterion.id,
                        critical_marker,
                        criterion.description
                    ));
                }
                prompt.push_str("\n");
            }
        }

        // Add cross-iteration context (with optional compression)
        let iteration_context = build_iteration_context_with_compression(
            &self.knowledge_base,
            &state.task_run_id,
            worker.iteration,
            self.config.compression.as_ref(),
        )?;

        if !iteration_context.is_empty() {
            prompt.push_str(&iteration_context);
        }

        // Add worker signals section
        prompt.push_str("## Worker Signals\n\n");
        prompt.push_str("When you complete work in your domain, emit:\n");
        prompt.push_str("```\n[WORK_COMPLETE] Brief reason why your domain work is done\n```\n\n");
        prompt.push_str("The system will coordinate with other workers before running verification.\n\n");

        // Add base prompt
        prompt.push_str("---\n\n");
        prompt.push_str(base_prompt);

        Ok(prompt)
    }

    /// Process output from a specific worker.
    ///
    /// This tracks the worker's state and generates coordination messages.
    pub fn process_worker_output_for_domain(
        &self,
        state: &mut OrchestratorState,
        worker_id: &str,
        output: &str,
    ) -> Result<WorkerOutputAction, String> {
        // First, update worker iteration
        {
            let worker = state
                .get_worker_mut(worker_id)
                .ok_or_else(|| format!("Worker '{}' not found", worker_id))?;
            worker.iteration += 1;
        }

        let (signal, findings) = process_worker_output(output);

        // Get the current iteration and task_run_id for knowledge base recording
        let (worker_iteration, task_run_id) = {
            let worker = state.get_worker(worker_id).unwrap();
            (worker.iteration, state.task_run_id.clone())
        };

        // Record findings
        for finding in &findings {
            // Record in worker
            if let Some(worker) = state.get_worker_mut(worker_id) {
                worker.record_finding(finding.clone());
            }

            // Record in knowledge base
            self.knowledge_base.record_finding(
                &task_run_id,
                finding,
                worker_iteration,
            )?;

            // Add coordination message for shared findings
            state.add_coordination_message(WorkerCoordinationMessage::SharedFinding {
                worker_id: worker_id.to_string(),
                finding: finding.clone(),
            });
        }

        // Process the signal
        match signal {
            Some(WorkerSignal::WorkComplete { reason }) => {
                let worker = state.get_worker_mut(worker_id).unwrap();
                worker.await_verification();
                worker.last_signal = Some(WorkerSignal::WorkComplete { reason: reason.clone() });

                let domain = worker.domain.clone();
                state.add_coordination_message(WorkerCoordinationMessage::ReadyForVerification {
                    worker_id: worker_id.to_string(),
                    domain,
                });

                info!(
                    "Worker '{}' signaled WORK_COMPLETE at iteration {}",
                    worker_id,
                    state.get_worker(worker_id).unwrap().iteration
                );

                // Check if all workers are ready for verification
                if state.all_workers_awaiting_verification() {
                    Ok(WorkerOutputAction::RunVerification)
                } else {
                    Ok(WorkerOutputAction::Continue)
                }
            }
            Some(WorkerSignal::NeedReplan { reason }) => {
                let worker = state.get_worker_mut(worker_id).unwrap();
                worker.last_signal = Some(WorkerSignal::NeedReplan { reason: reason.clone() });
                info!(
                    "Worker '{}' signaled NEED_REPLAN at iteration {}",
                    worker_id,
                    worker.iteration
                );
                Ok(WorkerOutputAction::Replan { reason })
            }
            Some(WorkerSignal::Finding(_)) => {
                // Finding was already recorded
                Ok(WorkerOutputAction::Continue)
            }
            Some(WorkerSignal::Continue) | None => {
                let worker = state.get_worker(worker_id).unwrap();
                if worker.iteration >= worker.max_iterations {
                    warn!(
                        "Worker '{}' reached max iterations ({})",
                        worker_id, worker.max_iterations
                    );
                    // Check if this is the last worker to hit max
                    let all_at_max = state.workers.values().all(|w| {
                        w.iteration >= w.max_iterations || matches!(w.status, WorkerStatus::Completed | WorkerStatus::AwaitingVerification)
                    });
                    if all_at_max {
                        Ok(WorkerOutputAction::MaxIterationsReached)
                    } else {
                        Ok(WorkerOutputAction::Continue)
                    }
                } else {
                    Ok(WorkerOutputAction::Continue)
                }
            }
        }
    }

    /// Coordinate workers across domains.
    ///
    /// This synchronizes worker state and prepares for cross-domain verification.
    pub fn coordinate_workers(
        &self,
        state: &mut OrchestratorState,
    ) -> Result<CoordinationResult, String> {
        let active_count = state.active_workers().len();
        let awaiting_verification = state
            .workers
            .values()
            .filter(|w| matches!(w.status, WorkerStatus::AwaitingVerification))
            .count();
        let completed_count = state
            .workers
            .values()
            .filter(|w| matches!(w.status, WorkerStatus::Completed))
            .count();
        let error_count = state
            .workers
            .values()
            .filter(|w| matches!(w.status, WorkerStatus::Error))
            .count();

        info!(
            "Coordinating workers: {} active, {} awaiting verification, {} completed, {} errors",
            active_count, awaiting_verification, completed_count, error_count
        );

        if state.all_workers_complete() {
            return Ok(CoordinationResult::AllComplete);
        }

        if state.all_workers_awaiting_verification() {
            return Ok(CoordinationResult::ReadyForVerification);
        }

        // Check for blocked workers
        let blocked_messages: Vec<_> = state
            .coordination_messages
            .iter()
            .filter(|m| matches!(m, WorkerCoordinationMessage::Blocked { .. }))
            .collect();

        if !blocked_messages.is_empty() {
            return Ok(CoordinationResult::HasBlockedWorkers {
                blocked_count: blocked_messages.len(),
            });
        }

        Ok(CoordinationResult::ContinueWork {
            active_workers: active_count,
        })
    }

    /// Run domain-scoped verification for a specific domain.
    ///
    /// This verifies only the criteria that belong to the specified domain.
    pub async fn run_domain_verification(
        &self,
        state: &mut OrchestratorState,
        domain_id: &str,
        screenshot_base64: Option<&str>,
    ) -> Result<DomainVerificationResult, String> {
        let plan = state
            .plan
            .as_ref()
            .ok_or("No verification plan available")?;

        let domain_criteria = plan.criteria_for_domain(domain_id);
        if domain_criteria.is_empty() {
            return Ok(DomainVerificationResult {
                domain_id: domain_id.to_string(),
                worker_ids: state.workers_for_domain(domain_id).iter().map(|w| w.worker_id.clone()).collect(),
                results: vec![],
                all_passed: true,
                failure_summary: None,
            });
        }

        info!(
            "Running domain verification for '{}' with {} criteria",
            domain_id,
            domain_criteria.len()
        );

        // Create a filtered plan with only domain criteria
        let mut domain_plan = plan.clone();
        domain_plan.success_criteria = domain_criteria.into_iter().cloned().collect();

        // Run verification
        let results = self
            .verifier
            .verify(&domain_plan, state.iteration, screenshot_base64)
            .await;

        // Combine results
        let all_results: Vec<_> = results
            .deterministic_results
            .into_iter()
            .chain(results.ai_results.into_iter())
            .collect();

        let all_passed = all_results.iter().all(|r| r.passed);

        let failure_summary = if !all_passed {
            let failures: Vec<_> = all_results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| format!("{}: {}", r.criterion_id, r.issues.join(", ")))
                .collect();
            Some(failures.join("; "))
        } else {
            None
        };

        let domain_result = DomainVerificationResult {
            domain_id: domain_id.to_string(),
            worker_ids: state
                .workers_for_domain(domain_id)
                .iter()
                .map(|w| w.worker_id.clone())
                .collect(),
            results: all_results,
            all_passed,
            failure_summary,
        };

        // Store the result
        state.store_domain_verification(domain_result.clone());

        Ok(domain_result)
    }

    /// Run verification for all domains.
    pub async fn run_all_domain_verifications(
        &self,
        state: &mut OrchestratorState,
        screenshot_base64: Option<&str>,
    ) -> Result<Vec<DomainVerificationResult>, String> {
        let domain_ids: Vec<_> = state
            .domain_assignments
            .iter()
            .map(|d| d.domain_id.clone())
            .collect();

        let mut results = Vec::new();
        for domain_id in domain_ids {
            let result = self
                .run_domain_verification(state, &domain_id, screenshot_base64)
                .await?;
            results.push(result);
        }

        Ok(results)
    }

    // ========================================================================
    // State Explorer Interleaving Support
    // ========================================================================

    /// Run state exploration with checkpoint-based interleaving.
    ///
    /// This method runs State Explorer and pauses at checkpoints when:
    /// - A batch of states is explored
    /// - Issue threshold is reached
    /// - A critical failure occurs
    ///
    /// When paused, it generates AI analysis context for the accumulated issues
    /// and returns control for agentic fix work.
    ///
    /// # Arguments
    /// * `state` - Current orchestrator state
    /// * `exploration_config` - Configuration for the exploration
    /// * `app_state` - Tauri app state for accessing Python bridge
    ///
    /// # Returns
    /// * `Ok(ExplorationInterleaveResult)` - Result containing exploration status and any checkpoint
    /// * `Err(String)` - Error message if exploration failed to start
    pub async fn run_interleaved_exploration(
        &self,
        state: &mut OrchestratorState,
        exploration_config: crate::state_explorer::ExplorationConfig,
        app_state: std::sync::Arc<crate::commands::AppState>,
    ) -> Result<ExplorationInterleaveResult, String> {
        use crate::state_explorer::{
            CheckpointConfig, CheckpointManager, ExplorationCheckpoint, ExplorationTask,
        };
        use std::path::PathBuf;

        info!(
            "Starting interleaved exploration for task {}",
            state.task_run_id
        );

        // Create checkpoint configuration from exploration config
        let checkpoint_config = CheckpointConfig {
            batch_size: exploration_config.checkpoint_batch_size,
            issue_threshold: exploration_config.checkpoint_issue_threshold,
            pause_on_critical: exploration_config.checkpoint_on_critical,
            interleave_enabled: exploration_config.interleave_with_agentic,
            checkpoint_dir: exploration_config.output_directory.clone().map(PathBuf::from),
        };

        // Create checkpoint manager
        let output_dir = exploration_config
            .output_directory
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".dev-logs/state-explorer"));

        let _checkpoint_manager = CheckpointManager::new(checkpoint_config.clone(), output_dir.clone());

        // Create and run the exploration task
        let task = ExplorationTask::new(exploration_config.clone(), app_state);
        let result = task.execute().await?;

        // Analyze results and determine if we need to pause for agentic work
        let has_failures = result.states_failed > 0 || result.transitions_failed > 0;
        let should_pause = checkpoint_config.interleave_enabled && has_failures;

        if should_pause {
            // Create checkpoint with accumulated issues
            let checkpoint = ExplorationCheckpoint::new(
                state.task_run_id.clone(),
                exploration_config.clone(),
                result
                    .state_explorations
                    .iter()
                    .filter(|s| matches!(s.status, crate::state_explorer::ExplorationStatus::Passed))
                    .map(|s| s.state_id.clone())
                    .collect(),
                result
                    .state_explorations
                    .iter()
                    .filter(|s| matches!(s.status, crate::state_explorer::ExplorationStatus::Pending))
                    .map(|s| s.state_id.clone())
                    .collect(),
                result.states_visited as usize,
                Vec::new(), // Discrepancies would be extracted from state_explorations
                if result.states_failed > 0 {
                    crate::state_explorer::CheckpointTrigger::IssueThreshold {
                        issue_count: result.states_failed as usize,
                        threshold: checkpoint_config.issue_threshold,
                    }
                } else {
                    crate::state_explorer::CheckpointTrigger::BatchComplete {
                        batch_size: checkpoint_config.batch_size,
                        batch_number: 1,
                    }
                },
                result.total_duration_ms,
            );

            // Generate AI analysis context for the issues
            let issue_summary = checkpoint.get_issue_summary();

            // Record in knowledge base
            self.knowledge_base.record_observation(
                &state.task_run_id,
                crate::orchestrator::knowledge::AgentType::System,
                state.iteration,
                &format!(
                    "State Explorer checkpoint: {} states explored, {} failures detected.\n\n{}",
                    result.states_visited, result.states_failed, issue_summary
                ),
                &[],
            )?;

            info!(
                "Exploration paused at checkpoint with {} issues",
                result.states_failed
            );

            Ok(ExplorationInterleaveResult {
                status: ExplorationInterleaveStatus::PausedForFixes,
                checkpoint: Some(checkpoint),
                result,
                issue_summary: Some(issue_summary),
            })
        } else {
            // Exploration completed without needing to pause
            let final_status = if has_failures {
                ExplorationInterleaveStatus::CompletedWithIssues
            } else {
                ExplorationInterleaveStatus::CompletedSuccess
            };

            info!(
                "Exploration completed with status: {:?}",
                final_status
            );

            Ok(ExplorationInterleaveResult {
                status: final_status,
                checkpoint: None,
                result,
                issue_summary: None,
            })
        }
    }

    /// Resume exploration from a checkpoint after agentic fixes.
    ///
    /// # Arguments
    /// * `state` - Current orchestrator state
    /// * `checkpoint` - Checkpoint to resume from
    /// * `app_state` - Tauri app state
    ///
    /// # Returns
    /// * Result of continuing the exploration
    pub async fn resume_exploration_from_checkpoint(
        &self,
        state: &mut OrchestratorState,
        checkpoint: crate::state_explorer::ExplorationCheckpoint,
        app_state: std::sync::Arc<crate::commands::AppState>,
    ) -> Result<ExplorationInterleaveResult, String> {
        info!(
            "Resuming exploration from checkpoint {} for task {}",
            checkpoint.id, state.task_run_id
        );

        // Update checkpoint config to only explore remaining states
        let mut config = checkpoint.config.clone();
        config.target_state_ids = checkpoint.pending_states.clone();

        // Record that we resumed
        self.knowledge_base.record_observation(
            &state.task_run_id,
            crate::orchestrator::knowledge::AgentType::System,
            state.iteration,
            &format!(
                "Resumed State Explorer from checkpoint {}. {} states remaining to explore.",
                checkpoint.id,
                checkpoint.pending_states.len()
            ),
            &[],
        )?;

        // Run exploration for remaining states
        self.run_interleaved_exploration(state, config, app_state)
            .await
    }
}

/// Status of an interleaved exploration run
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorationInterleaveStatus {
    /// Exploration completed successfully with no issues
    CompletedSuccess,
    /// Exploration completed but found issues (interleaving disabled)
    CompletedWithIssues,
    /// Exploration paused at checkpoint for agentic fixes
    PausedForFixes,
    /// Error during exploration
    Error,
}

/// Result of an interleaved exploration run
#[derive(Debug, Clone)]
pub struct ExplorationInterleaveResult {
    /// Status of the exploration
    pub status: ExplorationInterleaveStatus,
    /// Checkpoint if exploration was paused
    pub checkpoint: Option<crate::state_explorer::ExplorationCheckpoint>,
    /// Full exploration result
    pub result: crate::state_explorer::ExplorationResult,
    /// Issue summary for AI analysis (if paused)
    pub issue_summary: Option<String>,
}

/// Result of coordinating workers.
#[derive(Debug, Clone)]
pub enum CoordinationResult {
    /// All workers have completed (success or error)
    AllComplete,
    /// All workers are ready for verification
    ReadyForVerification,
    /// Some workers are blocked
    HasBlockedWorkers { blocked_count: usize },
    /// Work continues
    ContinueWork { active_workers: usize },
}

// ============================================================================
// Worker Output Actions
// ============================================================================

/// Action to take after processing worker output.
#[derive(Debug, Clone)]
pub enum WorkerOutputAction {
    /// Continue to next iteration
    Continue,
    /// Run verification checks
    RunVerification,
    /// Replan with the given reason
    Replan { reason: String },
    /// Max iterations reached, need user decision
    MaxIterationsReached,
}

// ============================================================================
// High-Level Task Runner
// ============================================================================

/// Run a complete orchestrated task (for testing/standalone use).
///
/// This function runs the full orchestration loop:
/// 1. Initialize with planning
/// 2. Build worker prompt
/// 3. (Worker execution happens externally)
/// 4. Process worker output
/// 5. Run verification if work complete
/// 6. Loop until success, failure, or max iterations
///
/// In practice, the session loop in mcp_api.rs will call these methods
/// individually to integrate with the existing execution flow.
pub async fn run_orchestrated_task(
    db: Arc<CheckpointDb>,
    task_run_id: &str,
    goal: &str,
    base_prompt: &str,
    config: OrchestratorConfig,
    // Callback to run the worker and get output
    worker_fn: impl Fn(&str) -> Result<String, String>,
    // Callback to capture screenshot for AI verification
    screenshot_fn: impl Fn() -> Result<Option<String>, String>,
) -> Result<TaskCompletionResult, String> {
    let orchestrator = Orchestrator::new(config.clone(), db);
    let mut state = orchestrator.initialize_task(task_run_id, goal)?;

    loop {
        // Build worker prompt with context
        let prompt = orchestrator.build_worker_prompt(&state, base_prompt)?;

        // Run worker (external callback)
        let output = worker_fn(&prompt)?;

        // Process output
        let action = orchestrator.process_worker_output(&mut state, &output)?;

        match action {
            WorkerOutputAction::Continue => {
                // Continue to next iteration
                continue;
            }
            WorkerOutputAction::RunVerification => {
                // Capture screenshot if needed
                let screenshot = if orchestrator.config.enable_ai_verification {
                    screenshot_fn()?
                } else {
                    None
                };

                // Run verification
                let results = orchestrator
                    .run_verification(&mut state, screenshot.as_deref())
                    .await?;

                if results.all_passed {
                    // Success!
                    let findings = orchestrator.get_findings(task_run_id)?;
                    let result = TaskCompletionResult::Success {
                        iterations: state.iteration,
                        findings: findings
                            .iter()
                            .filter(|f| f.category == "finding")
                            .map(|f| crate::orchestrator::types::Finding {
                                id: f.id.clone(),
                                finding_type: f.category.clone(),
                                description: f.content.clone(),
                                evidence: f.evidence.clone(),
                                confidence: crate::orchestrator::types::Confidence::Medium,
                                related_files: f.related_files.clone(),
                            })
                            .collect(),
                        verification_results: results,
                    };
                    orchestrator.complete_task(&mut state, result.clone());
                    return Ok(result);
                } else {
                    // Verification failed, continue iteration
                    if state.iteration >= config.max_iterations {
                        let result = TaskCompletionResult::Failed {
                            reason: "Max iterations reached without passing verification".to_string(),
                            iterations: state.iteration,
                            last_results: Some(results),
                            findings: vec![],
                        };
                        orchestrator.complete_task(&mut state, result.clone());
                        return Ok(result);
                    }
                    // Continue to next iteration with verification feedback
                    continue;
                }
            }
            WorkerOutputAction::Replan { reason } => {
                orchestrator.handle_replan(&mut state, &reason)?;
                // Continue with new plan
                continue;
            }
            WorkerOutputAction::MaxIterationsReached => {
                let result = TaskCompletionResult::Paused {
                    at_iteration: state.iteration,
                    max_iterations: config.max_iterations,
                    last_results: state.last_verification.clone(),
                    findings: vec![],
                };
                orchestrator.complete_task(&mut state, result.clone());
                return Ok(result);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestrator_config_default() {
        let config = OrchestratorConfig::default();
        assert_eq!(config.max_iterations, 10);
        assert!(config.enable_planning);
        assert!(config.enable_ai_verification);
    }

    #[test]
    fn test_orchestrator_state_new() {
        let state = OrchestratorState::new("test-task".to_string());
        assert_eq!(state.task_run_id, "test-task");
        assert_eq!(state.iteration, 0);
        assert!(state.plan.is_none());
        assert!(!state.is_complete);
    }

    // ========================================================================
    // Multi-Worker Tests (Phase 5)
    // ========================================================================

    #[test]
    fn test_domain_assignment_creation() {
        let domain = DomainAssignment::new("frontend", "Frontend", "UI and React components")
            .with_file_pattern("src/components/**/*.tsx")
            .with_keyword("react")
            .with_system_prompt("Focus on accessibility");

        assert_eq!(domain.domain_id, "frontend");
        assert_eq!(domain.name, "Frontend");
        assert_eq!(domain.file_patterns.len(), 1);
        assert_eq!(domain.keywords.len(), 1);
        assert!(domain.system_prompt_context.is_some());
    }

    #[test]
    fn test_worker_instance_creation() {
        let worker = WorkerInstance::new("worker-1", "Frontend Worker", 10);

        assert_eq!(worker.worker_id, "worker-1");
        assert_eq!(worker.name, "Frontend Worker");
        assert_eq!(worker.status, WorkerStatus::Idle);
        assert_eq!(worker.iteration, 0);
        assert_eq!(worker.max_iterations, 10);
        assert!(worker.domain.is_none());
    }

    #[test]
    fn test_worker_lifecycle() {
        let mut worker = WorkerInstance::new("worker-1", "Test Worker", 5);

        // Start the worker
        worker.start();
        assert_eq!(worker.status, WorkerStatus::Active);
        assert!(worker.started_at.is_some());

        // Mark awaiting verification
        worker.await_verification();
        assert_eq!(worker.status, WorkerStatus::AwaitingVerification);

        // Complete the worker
        worker.complete();
        assert_eq!(worker.status, WorkerStatus::Completed);
        assert!(worker.completed_at.is_some());
    }

    #[test]
    fn test_worker_error_handling() {
        let mut worker = WorkerInstance::new("worker-1", "Test Worker", 5);
        worker.start();

        worker.error("Something went wrong");
        assert_eq!(worker.status, WorkerStatus::Error);
        assert_eq!(worker.error_message, Some("Something went wrong".to_string()));
        assert!(worker.completed_at.is_some());
    }

    #[test]
    fn test_orchestrator_state_worker_management() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Create workers
        state.create_worker("worker-1", "Worker 1", 10);
        state.create_worker("worker-2", "Worker 2", 10);

        assert_eq!(state.workers.len(), 2);
        assert!(state.get_worker("worker-1").is_some());
        assert!(state.get_worker("worker-2").is_some());
        assert!(state.get_worker("worker-3").is_none());
    }

    #[test]
    fn test_orchestrator_state_domain_management() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Add domains
        let frontend = DomainAssignment::new("frontend", "Frontend", "UI components");
        let backend = DomainAssignment::new("backend", "Backend", "API services");

        state.add_domain(frontend);
        state.add_domain(backend);

        assert_eq!(state.domain_assignments.len(), 2);
        assert!(state.get_domain("frontend").is_some());
        assert!(state.get_domain("backend").is_some());
        assert!(state.get_domain("database").is_none());
    }

    #[test]
    fn test_worker_domain_assignment() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Add domain and worker
        let frontend = DomainAssignment::new("frontend", "Frontend", "UI components");
        state.add_domain(frontend);
        state.create_worker("worker-1", "Worker 1", 10);

        // Assign worker to domain
        let result = state.assign_worker_to_domain("worker-1", "frontend");
        assert!(result.is_ok());

        // Verify assignment
        let worker = state.get_worker("worker-1").unwrap();
        assert_eq!(worker.domain, Some("frontend".to_string()));

        let domain = state.get_domain("frontend").unwrap();
        assert!(domain.assigned_workers.contains(&"worker-1".to_string()));

        // Verify workers_for_domain
        let domain_workers = state.workers_for_domain("frontend");
        assert_eq!(domain_workers.len(), 1);
        assert_eq!(domain_workers[0].worker_id, "worker-1");
    }

    #[test]
    fn test_worker_domain_assignment_errors() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Try to assign non-existent worker
        let result = state.assign_worker_to_domain("worker-1", "frontend");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Worker"));

        // Create worker but try to assign to non-existent domain
        state.create_worker("worker-1", "Worker 1", 10);
        let result = state.assign_worker_to_domain("worker-1", "frontend");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Domain"));
    }

    #[test]
    fn test_all_workers_complete() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Empty state should return false
        assert!(!state.all_workers_complete());

        // Add workers
        state.create_worker("worker-1", "Worker 1", 10);
        state.create_worker("worker-2", "Worker 2", 10);

        // Not all complete yet
        assert!(!state.all_workers_complete());

        // Mark both as complete
        state.get_worker_mut("worker-1").unwrap().complete();
        assert!(!state.all_workers_complete());

        state.get_worker_mut("worker-2").unwrap().complete();
        assert!(state.all_workers_complete());
    }

    #[test]
    fn test_all_workers_awaiting_verification() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Empty state should return false
        assert!(!state.all_workers_awaiting_verification());

        // Add workers
        state.create_worker("worker-1", "Worker 1", 10);
        state.create_worker("worker-2", "Worker 2", 10);

        // Not all awaiting yet
        assert!(!state.all_workers_awaiting_verification());

        // Mark both as awaiting verification
        state.get_worker_mut("worker-1").unwrap().await_verification();
        assert!(!state.all_workers_awaiting_verification());

        state.get_worker_mut("worker-2").unwrap().await_verification();
        assert!(state.all_workers_awaiting_verification());
    }

    #[test]
    fn test_coordination_messages() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // Add coordination messages
        state.add_coordination_message(WorkerCoordinationMessage::FilesModified {
            worker_id: "worker-1".to_string(),
            files: vec!["src/app.tsx".to_string()],
        });

        state.add_coordination_message(WorkerCoordinationMessage::ReadyForVerification {
            worker_id: "worker-2".to_string(),
            domain: Some("backend".to_string()),
        });

        assert_eq!(state.coordination_messages.len(), 2);

        // Get messages for worker-1 (should see worker-2's messages, not its own)
        let messages = state.coordination_messages_for_worker("worker-1");
        assert_eq!(messages.len(), 1);
        match messages[0] {
            WorkerCoordinationMessage::ReadyForVerification { worker_id, .. } => {
                assert_eq!(worker_id, "worker-2");
            }
            _ => panic!("Expected ReadyForVerification message"),
        }
    }

    #[test]
    fn test_coordination_result_enum() {
        // Test that CoordinationResult variants are properly defined
        let _all_complete = CoordinationResult::AllComplete;
        let _ready = CoordinationResult::ReadyForVerification;
        let _blocked = CoordinationResult::HasBlockedWorkers { blocked_count: 2 };
        let _continue = CoordinationResult::ContinueWork { active_workers: 3 };
    }

    #[test]
    fn test_domain_verification_result() {
        let result = DomainVerificationResult {
            domain_id: "frontend".to_string(),
            worker_ids: vec!["worker-1".to_string()],
            results: vec![],
            all_passed: true,
            failure_summary: None,
        };

        assert_eq!(result.domain_id, "frontend");
        assert!(result.all_passed);
        assert!(result.failure_summary.is_none());
    }

    #[test]
    fn test_all_domains_verified() {
        let mut state = OrchestratorState::new("test-task".to_string());

        // No domains = verified
        assert!(state.all_domains_verified());

        // Add domains
        state.add_domain(DomainAssignment::new("frontend", "Frontend", "UI"));
        state.add_domain(DomainAssignment::new("backend", "Backend", "API"));

        // Not all verified yet
        assert!(!state.all_domains_verified());

        // Verify frontend
        state.store_domain_verification(DomainVerificationResult {
            domain_id: "frontend".to_string(),
            worker_ids: vec![],
            results: vec![],
            all_passed: true,
            failure_summary: None,
        });
        assert!(!state.all_domains_verified());

        // Verify backend
        state.store_domain_verification(DomainVerificationResult {
            domain_id: "backend".to_string(),
            worker_ids: vec![],
            results: vec![],
            all_passed: true,
            failure_summary: None,
        });
        assert!(state.all_domains_verified());

        // If one fails, all_domains_verified should return false
        state.store_domain_verification(DomainVerificationResult {
            domain_id: "frontend".to_string(),
            worker_ids: vec![],
            results: vec![],
            all_passed: false,
            failure_summary: Some("Test failure".to_string()),
        });
        assert!(!state.all_domains_verified());
    }
}
