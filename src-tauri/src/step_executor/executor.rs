//! Step Executor Module
//!
//! Provides unified execution of automation steps (workflows, actions, states,
//! screenshots, Playwright tests, AWAS). This is the core execution layer used by:
//! - Run page (single workflow execution)
//! - AI Builder (multi-step execution before AI session)
//! - MCP API (direct step execution)
//!
//! The design principle: multi-step execution is the foundation, and running
//! a single workflow is just a special case (one step of type "workflow").
//!
//! ## Architecture
//!
//! Step execution uses a polymorphic handler dispatch system:
//!
//! ```text
//! StepExecutor.execute_single_step()
//!     └── HandlerRegistry.get_handler(step_type)
//!             └── handler.execute(step, context)
//! ```
//!
//! All step types are implemented as separate handlers in the `handlers/` module.
//! The `HandlerRegistry` maps step type strings to handler implementations.
//!
//! ## Core Step Types (3 handlers)
//!
//! - **Command**: command (unified: shell command, check, check group, test)
//! - **UI Bridge**: ui_bridge
//! - **AI**: prompt

#![allow(dead_code)]

use regex::Regex;

use crate::action_service::UnifiedActionService;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::database::CreateTaskRunEventInput;
use crate::display::RawEvent;
use crate::executor::file_logger::FileLogger;
use crate::iteration_bundle::{
    parse_action_events, parse_image_recognition_events, RelevantLogSources,
};
use crate::orchestrator::context_propagation::{RuntimeContext, SharedVariableStore};
use crate::str_utils::truncate_str;
use crate::unified_workflow_executor::get_parent_task_id;

// Handler system imports
use super::handlers::{HandlerContext, HandlerRegistry};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

// Types extracted to executor_types.rs (re-exported via mod.rs)
use super::executor_types::*;

// Imports from extracted modules

// Legacy step handlers extracted to legacy_steps.rs
// Verification execution extracted to verification_execution.rs

pub struct StepExecutor {
    pub(crate) action_service: UnifiedActionService,
    pub(crate) app_state: Arc<AppState>,
    /// Configuration storage for loading saved configs
    pub(crate) config_storage: Arc<TokioMutex<ConfigStorage>>,
    /// Optional app handle for emitting events to the Tauri frontend
    pub(crate) app_handle: Option<tauri::AppHandle>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    pub(crate) task_run_id: Option<String>,
    /// Runtime context for variable expansion in commands
    pub(crate) runtime_context: RuntimeContext,
    /// Shared variable store for API request chaining (thread-safe, clone-friendly)
    pub(crate) shared_variables: SharedVariableStore,
    /// Registry of step handlers for polymorphic dispatch
    pub(crate) handler_registry: HandlerRegistry,
    /// PID tracker for AI process management (passed to WorkflowStepHandler)
    pub(crate) pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    /// Path scope policy for working directory resolution boundary enforcement.
    pub(crate) path_scope_policy: crate::paths::PathScopePolicy,
}

impl StepExecutor {
    /// Create a new StepExecutor
    pub fn new(app_state: Arc<AppState>, config_storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage.clone()),
            app_state,
            config_storage,
            app_handle: None,
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
            shared_variables: SharedVariableStore::new(),
            handler_registry: HandlerRegistry::with_standard_handlers(),
            pid_tracker: None,
            path_scope_policy: crate::paths::PathScopePolicy::default(),
        }
    }

    /// Create a new StepExecutor with an app handle for frontend event emission
    pub fn with_app_handle(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage.clone()),
            app_state,
            config_storage,
            app_handle: Some(app_handle),
            task_run_id: None,
            runtime_context: RuntimeContext::new(),
            shared_variables: SharedVariableStore::new(),
            handler_registry: HandlerRegistry::with_standard_handlers(),
            pid_tracker: None,
            path_scope_policy: crate::paths::PathScopePolicy::default(),
        }
    }

    /// Set the task run ID for database logging (builder pattern).
    ///
    /// When set, AWAS step results will be saved to the database.
    pub fn with_task_run_id(mut self, task_run_id: String) -> Self {
        self.runtime_context = RuntimeContext::with_task_run_id(&task_run_id);
        self.task_run_id = Some(task_run_id);
        self
    }

    /// Set the path scope policy for working directory boundary enforcement.
    pub fn set_path_scope_policy(&mut self, policy: crate::paths::PathScopePolicy) {
        self.path_scope_policy = policy;
    }

    /// Set the task run ID for database logging (mutable setter).
    ///
    /// Same as `with_task_run_id` but takes `&mut self` for use after construction.
    pub fn set_task_run_id(&mut self, task_run_id: String) {
        self.runtime_context = RuntimeContext::with_task_run_id(&task_run_id);
        self.task_run_id = Some(task_run_id);
    }

    /// Set a variable in the runtime context for variable expansion in commands.
    ///
    /// Variables can be referenced in shell commands using `{{variable_name}}` syntax.
    pub fn set_context_variable(&mut self, name: &str, value: serde_json::Value) {
        self.runtime_context.set_variable(name, value);
    }

    /// Get the runtime context (for advanced use cases).
    pub fn runtime_context(&self) -> &RuntimeContext {
        &self.runtime_context
    }

    /// Get a mutable reference to the runtime context (for advanced use cases).
    pub fn runtime_context_mut(&mut self) -> &mut RuntimeContext {
        &mut self.runtime_context
    }

    /// Get the shared variable store.
    pub fn shared_variables(&self) -> &SharedVariableStore {
        &self.shared_variables
    }

    /// Create a HandlerContext for executing steps via the handler system.
    ///
    /// This shares the executor's state (runtime_context, shared_variables)
    /// with the handlers to maintain consistency during step execution.
    pub(crate) fn create_handler_context(&self) -> HandlerContext {
        HandlerContext::with_shared_state(
            self.app_state.clone(),
            self.config_storage.clone(),
            self.app_handle.clone(),
            self.runtime_context.clone(),
            self.shared_variables.clone(),
            self.task_run_id.clone(),
            self.pid_tracker.clone(),
        )
        .with_path_scope_policy(self.path_scope_policy.clone())
    }

    /// Expand shared variables in a string.
    ///
    /// Replaces `{{variable_name}}` patterns with values from the shared variable store.
    /// This is used for API request chaining where response data from one request
    /// can be referenced in subsequent requests.
    fn expand_with_shared_vars(&self, text: &str) -> String {
        use once_cell::sync::Lazy;
        static VAR_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{\{([^}]+)\}\}").unwrap());

        let mut result = text.to_string();
        for cap in VAR_PATTERN.captures_iter(text) {
            let var_name = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if let Some(value) = self.shared_variables.get(var_name) {
                result = result.replace(&cap[0], &value);
            }
        }
        result
    }

    /// Persist runtime context (variables) to the database so the Context Tab can display them.
    ///
    /// Merges variables from both RuntimeContext and SharedVariableStore into a single JSON blob
    /// stored in `task_runs.runtime_context_json`.
    fn persist_runtime_context(&self, execution_id: &str) {
        let shared_vars = self.shared_variables.get_all();
        let ctx_vars = &self.runtime_context.variables;

        // Skip if there are no variables to persist
        if shared_vars.is_empty() && ctx_vars.is_empty() {
            return;
        }

        // Build a combined context: merge RuntimeContext variables and SharedVariableStore
        let mut variables = serde_json::Map::new();
        for (name, value) in ctx_vars {
            variables.insert(name.clone(), json!({ "value": value, "source": "system" }));
        }
        for (name, value) in &shared_vars {
            variables.insert(name.clone(), json!({ "value": value, "source": "step" }));
        }

        let context_json = json!({
            "variables": variables,
            "iteration": self.runtime_context.iteration,
        });

        // Remap to parent ID for workflow sequence children
        let task_run_id = get_parent_task_id(execution_id);

        if let Err(e) = self
            .app_state
            .checkpoint_db
            .update_task_run_runtime_context(&task_run_id, &context_json.to_string())
        {
            warn!("Failed to persist runtime context: {}", e);
        }
    }

    /// Log a step execution event to the database
    ///
    /// This logs step start, complete, and error events to the task_run_events table.
    ///
    /// Note: For composed run children (e.g., composed-run-X-workflow-N),
    /// the task_run_id is automatically remapped to the parent task ID because
    /// only parent IDs exist in task_runs (required by foreign key constraint).
    pub(crate) fn log_step_event(
        &self,
        task_run_id: &str,
        step: &ExecutionStepConfig,
        step_index: usize,
        event_subtype: &str,
        message: &str,
        duration_ms: Option<i64>,
        error: Option<&str>,
        exit_code: Option<i32>,
        stdout: Option<&str>,
        stderr: Option<&str>,
    ) {
        // For workflow sequence children, remap to parent ID to satisfy FK constraint
        let parent_id = get_parent_task_id(task_run_id);
        let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());

        // Generate action_id for consistent event aggregation
        // Format matches StepEventBuilder: {phase}-{step_type}-{task_run_id}-{step_index}
        // This ensures start/complete events for the same step are merged in the Timeline
        let phase = step.phase.as_deref().unwrap_or("setup");
        let action_id = format!("{}-{}-{}-{}", phase, step.step_type, parent_id, step_index);

        // Build data JSON with step details (include original task_run_id for context)
        let iteration = self.runtime_context.iteration;
        let data = json!({
            "step_index": step_index,
            "step_type": step.step_type,
            "step_name": step_name,
            "phase": step.phase,
            "iteration": iteration,
            "original_task_run_id": task_run_id,  // Keep original ID for debugging
            "command": step.shell_command.as_ref().or(step.check_command.as_ref()),
            "working_directory": step.shell_command_working_directory.as_ref().or(step.check_working_directory.as_ref()),
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "error": error,
        });

        let event_input = CreateTaskRunEventInput {
            task_run_id: parent_id, // Use parent ID for FK constraint
            event_type: "step_execution".to_string(),
            event_subtype: Some(event_subtype.to_string()),
            message: message.to_string(),
            data: Some(serde_json::to_string(&data).unwrap_or_default()),
            workflow_name: None,
            state_name: None,
            action_id: Some(action_id),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms,
        };

        // PG-primary: fire-and-forget async write to PostgreSQL
        {
            let pg = self.app_state.pg_db.clone();
            let event_clone = event_input.clone();
            tokio::spawn(async move {
                if let Err(e) = pg.create_task_run_event(&event_clone).await {
                    tracing::warn!("PG event write failed: {}", e);
                }
            });
        }
        if let Err(e) = self
            .app_state
            .checkpoint_db
            .create_task_run_event(&event_input)
        {
            warn!("Failed to log step event: {}", e);
        }
    }

    /// Emit a tree event to the Tauri frontend (if app_handle is available)
    pub(crate) fn emit_tree_event(
        &self,
        event_type: &str,
        node: &serde_json::Value,
        timestamp: f64,
        sequence: u32,
    ) {
        use tauri::Emitter;
        if let Some(ref app_handle) = self.app_handle {
            let tree_event = json!({
                "type": "tree_event",
                "event_type": event_type,
                "node": node,
                "path": [],
                "timestamp": timestamp,
                "sequence": sequence,
            });
            if let Err(e) = app_handle.emit("executor-event", &tree_event) {
                warn!("Failed to emit tree event to frontend: {}", e);
            }
        }
    }

    /// Record a screenshot capture event to the RunRecordingHandler.
    ///
    /// This ensures screenshots captured directly by the step executor
    /// (not through Python) are still recorded in the automation logs.
    pub(crate) async fn record_screenshot_event(
        &self,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<i32>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        let monitor_str = monitor.map(|m| m.to_string());
        self.app_state
            .run_recording_handler
            .on_screenshot_captured(
                screenshot_type,
                file_path,
                monitor_str,
                delay_seconds,
                success,
                associated_action,
                error,
            )
            .await;
    }

    /// Execute a list of steps and return results
    ///
    /// This is the core execution function used by all consumers.
    /// Steps are executed in order, and execution continues even if a step fails
    /// (so the caller can see all results and decide how to proceed).
    pub async fn execute_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> ExecutionResult {
        self.execute_steps_with_log_sources(steps, execution_id, &[])
            .await
    }

    /// Execute steps for a specific iteration
    ///
    /// For iterations > 1, filters out setup steps that aren't marked to run on
    /// subsequent iterations. This is the iteration-aware version of execute_steps.
    ///
    /// For Playwright steps, all Playwright steps are combined (since Playwright
    /// closes the browser after each run). Setup Playwright scripts are run first,
    /// followed by verification Playwright scripts.
    pub async fn execute_steps_for_iteration(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
        iteration: u32,
    ) -> ExecutionResult {
        // Preprocess steps for iteration:
        // 1. Filter out setup steps that shouldn't run on subsequent iterations
        // 2. Combine Playwright steps for efficiency (setup + verification)
        let processed_steps = Self::preprocess_steps_for_iteration(steps, iteration);

        if processed_steps.len() != steps.len() {
            info!(
                "Iteration {}: Preprocessed {} steps to {} (filtered/combined)",
                iteration,
                steps.len(),
                processed_steps.len(),
            );
        }

        self.execute_steps_with_log_sources(&processed_steps, execution_id, log_sources)
            .await
    }

    /// Preprocess steps for a specific iteration
    ///
    /// This handles:
    /// 1. Filtering out setup steps that shouldn't run on subsequent iterations
    /// 2. For Playwright steps: combining multiple scripts into a single script
    ///    (setup scripts first, then verification scripts) since Playwright closes
    ///    the browser after each run
    fn preprocess_steps_for_iteration(
        steps: &[ExecutionStepConfig],
        iteration: u32,
    ) -> Vec<ExecutionStepConfig> {
        // For first iteration, return all steps as-is
        if iteration <= 1 {
            return steps.to_vec();
        }

        // For subsequent iterations, filter out steps that shouldn't run
        steps
            .iter()
            .filter(|step| {
                let should_run = step.should_run_on_iteration(iteration);
                if !should_run {
                    info!(
                        "Iteration {}: Skipping step '{}' (type: {})",
                        iteration,
                        step.name.as_deref().unwrap_or("unnamed"),
                        step.step_type
                    );
                }
                should_run
            })
            .cloned()
            .collect()
    }

    /// Execute steps with log source configuration for log capture
    #[tracing::instrument(
        name = "workflow.steps.execute",
        skip(self, steps, log_sources),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id,
            log_source_count = %log_sources.len()
        )
    )]
    pub async fn execute_steps_with_log_sources(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let mut results = Vec::new();
        let total_start = std::time::Instant::now();

        if steps.is_empty() {
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: results,
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        // Determine which logs are relevant based on step types
        let relevant_logs = RelevantLogSources::from_steps(steps);
        relevant_logs.log_relevance();

        // Record log file positions before execution (only for enabled sources)
        let log_positions = Self::capture_log_positions(log_sources);

        // Record runner log positions (only if GUI automation is relevant)
        let runner_log_positions = if relevant_logs.gui_automation {
            Self::capture_runner_log_positions()
        } else {
            HashMap::new()
        };

        info!(
            "Executing {} steps for execution {}",
            steps.len(),
            execution_id
        );

        // Get the task run ID for event logging (prefer self.task_run_id, fall back to execution_id)
        let log_task_run_id = self
            .task_run_id
            .clone()
            .unwrap_or_else(|| execution_id.to_string());

        for (index, step) in steps.iter().enumerate() {
            let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());
            let start_time = std::time::Instant::now();
            let started_at = chrono::Utc::now().to_rfc3339();

            info!(
                "Executing step {}/{}: {} ({})",
                index + 1,
                steps.len(),
                step_name,
                step.step_type
            );

            // Log step start event
            self.log_step_event(
                &log_task_run_id,
                step,
                index,
                "start",
                &format!(
                    "Starting step {}/{}: {} ({})",
                    index + 1,
                    steps.len(),
                    step_name,
                    step.step_type
                ),
                None,
                None,
                None,
                None,
                None,
            );

            let (success, error, screenshot_path, _output_data) =
                self.execute_single_step(step).await;

            let final_screenshot = screenshot_path;

            let duration_ms = start_time.elapsed().as_millis() as u64;

            if success {
                info!(
                    "Step {}/{} completed successfully in {}ms",
                    index + 1,
                    steps.len(),
                    duration_ms
                );
                // Log step completion event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "complete",
                    &format!(
                        "Step {}/{} completed successfully in {}ms",
                        index + 1,
                        steps.len(),
                        duration_ms
                    ),
                    Some(duration_ms as i64),
                    None,
                    None,
                    None,
                    None,
                );
            } else {
                warn!("Step {}/{} failed: {:?}", index + 1, steps.len(), error);
                // Log step error event
                self.log_step_event(
                    &log_task_run_id,
                    step,
                    index,
                    "error",
                    &format!("Step {}/{} failed: {:?}", index + 1, steps.len(), error),
                    Some(duration_ms as i64),
                    error.as_deref(),
                    None,
                    None,
                    None,
                );
            }

            let ended_at = chrono::Utc::now().to_rfc3339();

            // Link any findings detected during this step's execution window
            if let Some(ref task_run_id) = self.task_run_id {
                let sn_c = step_name.clone();
                let idx_c = index as i32;
                match self.app_state.pg_db.link_findings_to_steps_by_timestamp(
                    task_run_id,
                    &sn_c,
                    idx_c,
                    &started_at,
                    &ended_at,
                ).await {
                    Ok(count) if count > 0 => {
                        info!(
                            "Linked {} findings to step '{}' (index {})",
                            count, sn_c, idx_c
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Failed to link findings to step '{}': {}", sn_c, e);
                    }
                }
            }

            results.push(StepExecutionResult {
                step_index: index,
                step_type: step.step_type.clone(),
                step_name,
                step_id: step.id.clone(),
                success,
                error,
                screenshot_path: final_screenshot,
                started_at: Some(started_at),
                ended_at: Some(ended_at),
                duration_ms,
                config: StepExecutionConfig {
                    timeout_seconds: step.timeout_seconds,
                    check_type: step.check_type.clone(),
                    command: step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone()),
                    test_id: step.test_id.clone(),
                    test_type: step.test_type.clone(),
                    working_directory: step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone()),
                    ui_bridge_action: step.ui_bridge_action.clone(),
                },
                verification_details: None,
                output_data: None,
                required: step.required,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
                interrupted: None,
            });
        }

        let successful_steps = results.iter().filter(|r| r.success).count();
        let failed_steps = results.len() - successful_steps;

        info!(
            "Completed {} steps: {} succeeded, {} failed",
            results.len(),
            successful_steps,
            failed_steps
        );

        // Capture logs that were written during execution
        let captured_logs = Self::capture_logs_since(log_sources, log_positions);

        // Capture runner logs (only if GUI automation was relevant)
        let captured_runner_logs = if relevant_logs.gui_automation {
            Self::capture_runner_logs_since(runner_log_positions)
        } else {
            None
        };

        // Persist runtime context (variables + shared variables) to the database
        // so the Context Tab can display them after completion.
        self.persist_runtime_context(execution_id);

        ExecutionResult {
            success: failed_steps == 0,
            total_steps: results.len(),
            successful_steps,
            failed_steps,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            steps: results,
            captured_logs,
            captured_runner_logs,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        }
    }

    // ========================================================================
    // Phase-Based Execution Methods
    // ========================================================================

    /// Filter steps by phase
    pub fn filter_steps_by_phase(
        steps: &[ExecutionStepConfig],
        phase: &str,
    ) -> Vec<ExecutionStepConfig> {
        steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some(phase))
            .cloned()
            .collect()
    }

    /// Check if any steps exist in the given phase
    pub fn has_steps_in_phase(steps: &[ExecutionStepConfig], phase: &str) -> bool {
        steps.iter().any(|s| s.phase.as_deref() == Some(phase))
    }

    /// Count steps in each phase
    pub fn count_steps_by_phase(
        steps: &[ExecutionStepConfig],
    ) -> std::collections::HashMap<String, usize> {
        let mut counts = std::collections::HashMap::new();
        for step in steps {
            if let Some(ref phase) = step.phase {
                *counts.entry(phase.clone()).or_insert(0) += 1;
            } else {
                // Steps without explicit phase are considered "unknown"
                *counts.entry("unknown".to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Execute only setup phase steps.
    ///
    /// This runs setup steps (shell commands, workflows, etc.) that prepare the
    /// environment before the verification loop begins. Setup steps run ONCE
    /// at the start of the workflow.
    ///
    /// Returns the execution result and whether setup completed successfully.
    pub async fn execute_setup_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> (ExecutionResult, bool) {
        let setup_steps = Self::filter_steps_by_phase(steps, "setup");

        if setup_steps.is_empty() {
            info!("No setup steps to execute, setup phase complete by default");
            return (
                ExecutionResult {
                    success: true,
                    total_steps: 0,
                    successful_steps: 0,
                    failed_steps: 0,
                    total_duration_ms: 0,
                    steps: vec![],
                    captured_logs: None,
                    captured_runner_logs: None,
                    verification_passed: None,
                    loop_result: None,
                    task_summary: None,
                },
                true, // Setup phase complete
            );
        }

        info!(
            "Executing {} setup phase steps for {}",
            setup_steps.len(),
            execution_id
        );

        let result = self
            .execute_steps_with_log_sources(&setup_steps, execution_id, log_sources)
            .await;

        let setup_complete = result.success;

        info!(
            "Setup phase {}: {} of {} steps succeeded",
            if setup_complete { "complete" } else { "failed" },
            result.successful_steps,
            result.total_steps
        );

        (result, setup_complete)
    }

    /// Execute only completion phase steps.
    ///
    /// This runs completion steps (cleanup, reports, notifications) that run
    /// ONCE after the verification loop exits (success or max iterations).
    ///
    /// Returns the execution result.
    pub async fn execute_completion_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let completion_steps = Self::filter_steps_by_phase(steps, "completion");

        if completion_steps.is_empty() {
            info!("No completion steps to execute");
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        info!(
            "Executing {} completion phase steps for {}",
            completion_steps.len(),
            execution_id
        );

        let result = self
            .execute_steps_with_log_sources(&completion_steps, execution_id, log_sources)
            .await;

        info!(
            "Completion phase done: {} of {} steps succeeded",
            result.successful_steps, result.total_steps
        );

        result
    }

    /// Execute only verification/agentic phase steps (for iterations).
    ///
    /// This runs verification and agentic steps that may run on each iteration.
    /// On iteration > 1, setup steps are filtered out (unless marked to run on
    /// subsequent iterations).
    ///
    /// Completion steps are always excluded from this method.
    pub async fn execute_verification_phase(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
        iteration: u32,
    ) -> ExecutionResult {
        // Filter out setup and completion steps, keep only verification/agentic
        let mut verification_steps: Vec<ExecutionStepConfig> = steps
            .iter()
            .filter(|s| {
                let phase = s.phase.as_deref().unwrap_or("unknown");
                // Include verification and agentic phase steps
                phase == "verification" || phase == "agentic"
            })
            .cloned()
            .collect();

        // For iteration > 1, also filter based on run_on_subsequent_iterations
        if iteration > 1 {
            verification_steps.retain(|step| step.should_run_on_iteration(iteration));
        }

        if verification_steps.is_empty() {
            info!(
                "No verification/agentic steps to execute for iteration {}",
                iteration
            );
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            };
        }

        info!(
            "Executing {} verification/agentic phase steps for iteration {}",
            verification_steps.len(),
            iteration
        );

        self.execute_steps_with_log_sources(&verification_steps, execution_id, log_sources)
            .await
    }

    /// Get current file positions for configured log sources
    fn capture_log_positions(
        log_sources: &[LogSourceConfig],
    ) -> std::collections::HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let path = std::path::Path::new(&source.path);
            if let Ok(mut file) = std::fs::File::open(path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(source.id.clone(), pos);
                }
            }
        }

        positions
    }

    /// Read log content that was written since the given positions
    fn capture_logs_since(
        log_sources: &[LogSourceConfig],
        positions: std::collections::HashMap<String, u64>,
    ) -> Option<CapturedLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let mut sources = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let start_pos = positions.get(&source.id).copied().unwrap_or(0);
            let path = std::path::Path::new(&source.path);

            if let Ok(mut file) = std::fs::File::open(path) {
                if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                    let mut content = String::new();
                    if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                        sources.insert(source.name.clone(), content);
                    }
                }
            }
        }

        if sources.is_empty() {
            None
        } else {
            Some(CapturedLogs { sources })
        }
    }

    /// Get the .dev-logs directory path
    pub(crate) fn get_dev_logs_dir() -> PathBuf {
        crate::paths::get_dev_logs_dir()
    }

    /// Get current file positions for runner log files (actions + image recognition)
    fn capture_runner_log_positions() -> HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = HashMap::new();
        let dev_logs = Self::get_dev_logs_dir();

        // Track positions for runner-actions.jsonl and runner-image-recognition.jsonl
        for filename in &["runner-actions.jsonl", "runner-image-recognition.jsonl"] {
            let path = dev_logs.join(filename);
            if let Ok(mut file) = std::fs::File::open(&path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(filename.to_string(), pos);
                    info!(
                        "Captured runner log position for {}: {} bytes",
                        filename, pos
                    );
                }
            }
        }

        positions
    }

    /// Read runner logs that were written since the given positions
    fn capture_runner_logs_since(positions: HashMap<String, u64>) -> Option<CapturedRunnerLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let dev_logs = Self::get_dev_logs_dir();
        let mut actions = Vec::new();
        let mut image_recognition = Vec::new();

        // Read runner-actions.jsonl
        let actions_path = dev_logs.join("runner-actions.jsonl");
        let start_pos = positions.get("runner-actions.jsonl").copied().unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&actions_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    actions = parse_action_events(&content);
                    info!("Captured {} action events from runner log", actions.len());
                }
            }
        }

        // Read runner-image-recognition.jsonl
        let ir_path = dev_logs.join("runner-image-recognition.jsonl");
        let start_pos = positions
            .get("runner-image-recognition.jsonl")
            .copied()
            .unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&ir_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    image_recognition = parse_image_recognition_events(&content);
                    info!(
                        "Captured {} image recognition events from runner log",
                        image_recognition.len()
                    );
                }
            }
        }

        if actions.is_empty() && image_recognition.is_empty() {
            None
        } else {
            Some(CapturedRunnerLogs {
                actions,
                image_recognition,
            })
        }
    }

    /// Execute a single step and return (success, error, screenshot_path, output_data)
    pub(crate) async fn execute_single_step(
        &self,
        step: &ExecutionStepConfig,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
    ) {
        // Try to use the handler registry for polymorphic dispatch.
        // This is the new modular approach - handlers are self-contained and testable.
        // If no handler is registered, fall back to the legacy match statement below.
        // Normalize "test" → "command" for backward compatibility with saved workflows
        let lookup_type = if step.step_type == "test" {
            "command"
        } else {
            &step.step_type
        };
        if let Some(handler) = self.handler_registry.get(lookup_type) {
            let context = self.create_handler_context();
            let result = handler.execute(step, &context).await;
            return (
                result.success,
                result.error,
                result.screenshot_path,
                result.output_data,
            );
        }

        // Fallback match statement for step types without registered handlers.
        // Most step types are handled by the handler registry dispatch above.

        // Timeouts are disabled by default - only apply if explicitly specified
        let timeout = step.timeout_seconds;

        match step.step_type.as_str() {
            // NOTE: "test" is no longer a separate step type — it's dispatched through CommandHandler
            // via the lookup_type normalization above. This legacy arm is kept only as a safety net.
            "test" => {
                // Execute verification test with tree event emission
                use std::sync::atomic::{AtomicU32, Ordering};
                static TEST_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("test-{}", sequence);
                let step_name = step
                    .name
                    .clone()
                    .unwrap_or_else(|| "Verification Test".to_string());
                let test_id_display = step
                    .test_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let is_critical = step.test_is_critical.unwrap_or(false);

                // Build action node for tree events
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("TEST: {}", step_name),
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "test_id": &test_id_display,
                        "is_critical": is_critical,
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                // Execute the test
                let result = if let Some(ref test_id) = step.test_id {
                    // Execute stored verification test by ID
                    match self.execute_verification_test(test_id, is_critical).await {
                        Ok((success, error)) => (success, error, None, None),
                        Err(e) => (
                            false,
                            Some(format!("Test execution error: {}", e)),
                            None,
                            None,
                        ),
                    }
                } else if step.test_type.as_deref() == Some("repository") {
                    // Repository test: run a command in the working directory
                    let command = step
                        .check_command
                        .clone()
                        .or_else(|| step.shell_command.clone())
                        .unwrap_or_else(|| "pytest".to_string());
                    let working_dir = step
                        .check_working_directory
                        .clone()
                        .or_else(|| step.shell_command_working_directory.clone())
                        .unwrap_or_else(|| {
                            std::env::current_dir()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| ".".to_string())
                        });
                    // Resolve relative paths to absolute
                    let working_dir = {
                        let p = std::path::Path::new(&working_dir);
                        if p.is_relative() {
                            std::env::current_dir()
                                .ok()
                                .map(|cwd| {
                                    let resolved = cwd.join(p);
                                    resolved
                                        .canonicalize()
                                        .unwrap_or(resolved)
                                        .to_string_lossy()
                                        .to_string()
                                })
                                .unwrap_or(working_dir)
                        } else {
                            working_dir
                        }
                    };

                    info!("Executing repository test: {} in {}", command, working_dir);

                    // Create a temporary step config for the shell command execution
                    let temp_step = ExecutionStepConfig {
                        shell_command: Some(command.clone()),
                        shell_command_working_directory: Some(working_dir),
                        ..Default::default()
                    };
                    // Timeouts are disabled by default
                    let timeout = step.timeout_seconds;
                    let (s, e, p) = self.execute_shell_command_step(&temp_step, timeout).await;
                    (s, e, p, None)
                } else {
                    (
                        false,
                        Some("No test ID specified and test_type is not 'repository'".to_string()),
                        None,
                        None,
                    )
                };

                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let duration = end_timestamp - timestamp;
                let (success, ref error_opt, _, _) = result;

                // Build completed/failed node
                let completed_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("TEST: {}", step_name),
                    "timestamp": end_timestamp,
                    "status": if success { "success" } else { "failed" },
                    "duration": duration,
                    "error": error_opt.clone(),
                    "metadata": {
                        "test_id": &test_id_display,
                        "is_critical": is_critical,
                    }
                });

                let event_type = if success {
                    "action_completed"
                } else {
                    "action_failed"
                };

                // Emit tree event to file log
                FileLogger::log_tree_event(
                    event_type,
                    &completed_node,
                    &[],
                    end_timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: event_type.to_string(),
                        timestamp: end_timestamp,
                        data: json!({ "node": completed_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event(event_type, &completed_node, end_timestamp, sequence);

                result
            }
            "prompt" => {
                // Prompt steps are text for the AI, not executed here - emit tree events for UI visibility
                use std::sync::atomic::{AtomicU32, Ordering};
                static PROMPT_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = PROMPT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
                let action_id = format!("prompt-{}", sequence);
                let step_name = step.name.clone().unwrap_or_else(|| "AI Prompt".to_string());
                let prompt_text = step.prompt_content.clone().unwrap_or_default();
                let prompt_preview = if prompt_text.len() > 100 {
                    format!("{}...", truncate_str(&prompt_text, 100))
                } else {
                    prompt_text.clone()
                };

                // Build action node for tree events
                let action_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("PROMPT: {}", step_name),
                    "timestamp": timestamp,
                    "status": "pending",
                    "metadata": {
                        "prompt_preview": prompt_preview,
                        "type": "ai_prompt",
                    }
                });

                // Emit action_started tree event to file log
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_started".to_string(),
                        timestamp,
                        data: json!({ "node": action_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_started", &action_node, timestamp, sequence);

                // Prompt steps complete immediately (text is passed to AI, not executed here)
                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

                // Build completed node
                let completed_node = json!({
                    "id": &action_id,
                    "node_type": "action",
                    "name": format!("PROMPT: {}", step_name),
                    "timestamp": end_timestamp,
                    "status": "success",
                    "duration": end_timestamp - timestamp,
                    "metadata": {
                        "prompt_preview": prompt_preview,
                        "type": "ai_prompt",
                        "note": "Prompt text passed to AI for processing",
                    }
                });

                // Emit action_completed tree event to file log
                FileLogger::log_tree_event(
                    "action_completed",
                    &completed_node,
                    &[],
                    end_timestamp,
                    sequence,
                );

                // Also add to DisplayProcessor for Session/Actions page
                {
                    let raw_event = RawEvent {
                        id: uuid::Uuid::new_v4().to_string(),
                        event_type: "action_completed".to_string(),
                        timestamp: end_timestamp,
                        data: json!({ "node": completed_node.clone() }),
                        sequence: sequence as u64,
                    };
                    let mut processor = self.app_state.display_processor.lock().await;
                    processor.event_log_mut().add_event(raw_event);
                }

                // Emit to Tauri frontend for action log refresh
                self.emit_tree_event("action_completed", &completed_node, end_timestamp, sequence);

                (true, None, None, None)
            }
            // ================================================================
            // Shell Command Step Type
            // ================================================================
            "shell_command" => {
                let (s, e, p) = self.execute_shell_command_step(step, timeout).await;
                (s, e, p, None)
            }
            // ================================================================
            // Check Step Type (code quality checks)
            // ================================================================
            "check" => {
                let (s, e, p) = self.execute_check_step(step, timeout).await;
                (s, e, p, None)
            }
            // ================================================================
            // Check Group Step Type (run all checks in a group)
            // ================================================================
            "check_group" => {
                let (success, error, summary, _check_results) =
                    self.execute_check_group_step(step, timeout).await;
                (success, error, summary, None)
            }
            // ================================================================
            // Shell Step Type (execute shell command)
            // ================================================================
            "shell" => {
                // Timeouts are disabled by default
                let timeout = step.timeout_seconds;
                let (success, error, output) = self.execute_shell_command_step(step, timeout).await;
                // Return output as the third element for potential logging
                (success, error, output, None)
            }
            // ================================================================
            // Log Watch Step Type (scan dev logs for errors)
            // ================================================================
            "log_watch" => {
                let (success, error, output) = self.execute_log_watch_step(step).await;
                (success, error, output, None)
            }
            // ================================================================
            // Gate Step Type (aggregate verification results)
            // ================================================================
            "gate" => {
                // The gate step is a semantic aggregation marker used by workflow
                // generation. Actual pass/fail aggregation is handled by
                // execute_verification_steps_with_events which checks all required
                // steps. The gate step itself always succeeds at execution time.
                info!(
                    "Gate step '{}' executed (aggregation handled by verification executor)",
                    step.name.as_deref().unwrap_or("unnamed")
                );
                (true, None, None, None)
            }
            _ => {
                // Delegate to handler registry for any unrecognized step type
                warn!("Unknown step type: {}", step.step_type);
                (
                    false,
                    Some(format!("Unknown step type: {}", step.step_type)),
                    None,
                    None,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_executor::dag::compute_execution_layers;

    #[test]
    fn test_execution_result_empty_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 0,
            successful_steps: 0,
            failed_steps: 0,
            total_duration_ms: 0,
            steps: vec![],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        };
        assert_eq!(result.to_markdown_summary(), "");
    }

    #[test]
    fn test_execution_result_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 2,
            successful_steps: 2,
            failed_steps: 0,
            total_duration_ms: 1500,
            steps: vec![
                StepExecutionResult {
                    step_index: 0,
                    step_type: "workflow".to_string(),
                    step_name: "Login".to_string(),
                    step_id: None,
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot1.png".to_string()),
                    started_at: None,
                    ended_at: None,
                    duration_ms: 1000,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                    output_data: None,
                    required: None,
                    resolved_inputs: None,
                    extracted_values: None,
                    failure_category: None,
                    interrupted: None,
                },
                StepExecutionResult {
                    step_index: 1,
                    step_type: "screenshot".to_string(),
                    step_name: "Capture".to_string(),
                    step_id: None,
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot2.png".to_string()),
                    started_at: None,
                    ended_at: None,
                    duration_ms: 500,
                    config: StepExecutionConfig::default(),
                    verification_details: None,
                    output_data: None,
                    required: None,
                    resolved_inputs: None,
                    extracted_values: None,
                    failure_category: None,
                    interrupted: None,
                },
            ],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: None,
            loop_result: None,
            task_summary: None,
        };
        let summary = result.to_markdown_summary();
        assert!(summary.contains("Pre-Execution Results"));
        assert!(summary.contains("Login"));
        assert!(summary.contains("2 of 2 steps completed successfully"));
    }

    // ========================================================================
    // compute_execution_layers tests
    // ========================================================================

    fn make_step(id: &str, step_type: &str) -> ExecutionStepConfig {
        ExecutionStepConfig {
            id: Some(id.to_string()),
            step_type: step_type.to_string(),
            name: Some(id.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_dag_no_dependencies() {
        // Three independent steps should form one layer
        let steps = vec![
            make_step("a", "check"),
            make_step("b", "check"),
            make_step("c", "check"),
        ];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].len(), 3);
    }

    #[test]
    fn test_dag_linear_chain() {
        // a -> b -> c (each depends on the previous)
        let a = make_step("a", "shell_command");
        let mut b = make_step("b", "shell_command");
        b.depends_on = Some(vec!["a".to_string()]);
        let mut c = make_step("c", "shell_command");
        c.depends_on = Some(vec!["b".to_string()]);

        let steps = vec![a, b, c];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![0]); // a
        assert_eq!(layers[1], vec![1]); // b
        assert_eq!(layers[2], vec![2]); // c
    }

    #[test]
    fn test_dag_diamond() {
        // a -> b, a -> c, b -> d, c -> d
        let a = make_step("a", "api_request");
        let mut b = make_step("b", "check");
        b.depends_on = Some(vec!["a".to_string()]);
        let mut c = make_step("c", "check");
        c.depends_on = Some(vec!["a".to_string()]);
        let mut d = make_step("d", "prompt");
        d.depends_on = Some(vec!["b".to_string(), "c".to_string()]);

        let steps = vec![a, b, c, d];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![0]); // a
        assert!(layers[1].contains(&1)); // b and c in parallel
        assert!(layers[1].contains(&2));
        assert_eq!(layers[2], vec![3]); // d
    }

    #[test]
    fn test_dag_input_dependencies() {
        // b reads from a's output via inputs
        let a = make_step("a", "api_request");
        let mut b = make_step("b", "check");
        let mut inputs = std::collections::HashMap::new();
        inputs.insert("response".to_string(), "a.output.body".to_string());
        b.inputs = Some(inputs);

        let steps = vec![a, b];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], vec![0]); // a first
        assert_eq!(layers[1], vec![1]); // then b
    }

    #[test]
    fn test_dag_cycle_detection() {
        // a -> b -> a (cycle)
        let mut a = make_step("a", "check");
        a.depends_on = Some(vec!["b".to_string()]);
        let mut b = make_step("b", "check");
        b.depends_on = Some(vec!["a".to_string()]);

        let steps = vec![a, b];
        let result = compute_execution_layers(&steps);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Circular"));
    }

    #[test]
    fn test_dag_empty_steps() {
        let steps: Vec<ExecutionStepConfig> = vec![];
        let layers = compute_execution_layers(&steps).unwrap();
        assert!(layers.is_empty());
    }

    #[test]
    fn test_dag_single_step() {
        let steps = vec![make_step("a", "shell_command")];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], vec![0]);
    }

    #[test]
    fn test_dag_unknown_dependency_ignored() {
        // Step references a non-existent dependency - should be ignored
        let mut a = make_step("a", "check");
        a.depends_on = Some(vec!["nonexistent".to_string()]);

        let steps = vec![a];
        let layers = compute_execution_layers(&steps).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], vec![0]);
    }
}
