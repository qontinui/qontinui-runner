//! Unified Workflow Executor
//!
//! This module provides a clean, modular implementation of the verification-agentic loop
//! for unified workflows. It follows the Single Responsibility Principle with clear
//! separation of concerns:
//!
//! - `types.rs`: Data structures for loop state and results
//! - `phases.rs`: Individual phase executors (setup, verification, agentic, completion)
//! - `loop_controller.rs`: The main loop coordination logic
//!
//! ## Key Design Principles
//!
//! 1. **Verification is the sole authority**: The AI cannot bypass verification by
//!    claiming [TASK_COMPLETE]. The loop only exits when verification passes.
//!
//! 2. **No orchestrator integration**: This module calls Claude directly without
//!    going through the session system or orchestrator. The unified workflow has
//!    a fixed architecture that doesn't need dynamic AI management.
//!
//! 3. **Clear completion criteria**: Completion phase ONLY runs if verification passed.
//!    There is no ambiguity about when a task is complete.
//!
//! 4. **Comprehensive logging**: Every decision is logged for debugging and audit.
//!
//! ## Usage
//!
//! ```ignore
//! let controller = LoopController::new(app_state, config_storage, app_handle);
//!
//! let config = LoopConfig {
//!     max_iterations: 5,
//!     timeout_seconds: None, // No timeout (default) - user can stop manually
//!     base_prompt: "Fix the issues".to_string(),
//!     workflow_name: "My Workflow".to_string(),
//!     workflow_id: "wf-123".to_string(),
//!     execution_id: "exec-456".to_string(),
//! };
//!
//! let result = controller.run(
//!     config,
//!     setup_steps,
//!     verification_steps,
//!     agentic_steps,
//!     completion_automation_steps,
//!     completion_prompt_steps,
//! ).await;
//!
//! if result.verification_passed {
//!     println!("Workflow completed successfully!");
//! } else {
//!     println!("Workflow failed: verification did not pass");
//! }
//! ```

mod loop_controller;
mod phase_configs;
mod phases;
mod resume;
pub mod states;
mod types;

// Panic handling imports
use futures_util::FutureExt;
use std::sync::Arc;
use tracing::{error, info};

// Core types used by external code
pub use loop_controller::{
    // Phase-aware versions (preferred for unified workflows)
    convert_json_steps_with_phase,
    extract_prompt_steps_with_phase,
    extract_workflow_id_from_task_id,
    resume_interrupted_workflows,
    LoopController,
    ResumeConfig,
};
pub use types::{get_parent_task_id, LoopConfig, LoopResult, WorkflowPhase};

// Types exposed for API consumers and advanced usage
// These may not be directly used in this crate but are part of the public API
#[allow(unused_imports)]
pub use loop_controller::WorkflowResult;
#[allow(unused_imports)]
pub use types::{AgenticOutcome, IterationResult};

// Re-export phase executors for testing or advanced usage
// Note: These implement the Executor trait for use with FromContext pattern
#[allow(unused_imports)]
pub use phases::{AgenticExecutor, CompletionExecutor, SetupExecutor, VerificationExecutor};

// Re-export phase configs for Executor trait usage
// Note: Used with the Executor trait implementations in phases.rs
#[allow(unused_imports)]
pub use phase_configs::{
    AgenticConfig, CompletionConfig, CompletionResult, SetupConfig, SetupResult,
    VerificationConfig, VerificationResult,
};

// Re-export resume types for restart recovery
pub use resume::{ResumeManager, ResumePoint};

// =============================================================================
// Panic-Safe Workflow Execution
// =============================================================================

/// Spawns a workflow execution with proper panic handling.
///
/// This function wraps the workflow execution in a panic-catching layer.
/// If the workflow panics for any reason, the task is marked as failed
/// so the user can see the error and use the Continue button.
///
/// # Arguments
/// * `checkpoint_db` - Database for marking the task as failed on panic
/// * `execution_id` - The task run ID to mark as failed if panic occurs
/// * `workflow_name` - Name of the workflow (for logging)
/// * `fut` - The async future that runs the workflow
///
/// # Usage
/// ```ignore
/// spawn_workflow_with_panic_guard(
///     checkpoint_db.clone(),
///     &execution_id,
///     &workflow_name,
///     async move {
///         controller.run(config, ...).await
///     },
/// );
/// ```
pub fn spawn_workflow_with_panic_guard<F>(
    checkpoint_db: Arc<crate::database::CheckpointDb>,
    execution_id: String,
    workflow_name: String,
    fut: F,
) where
    F: std::future::Future<Output = loop_controller::WorkflowResult> + Send + 'static,
{
    tokio::spawn(async move {
        // Wrap the future in AssertUnwindSafe and catch panics
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;

        match result {
            Ok(workflow_result) => {
                info!(
                    "Workflow '{}' (id: {}) completed: success={}",
                    workflow_name, execution_id, workflow_result.success
                );
            }
            Err(panic_payload) => {
                // Extract panic message
                let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic (no message)".to_string()
                };

                error!(
                    "PANIC in workflow '{}' (id: {}): {}",
                    workflow_name, execution_id, panic_msg
                );

                // Mark the task as failed so Continue button appears
                let error_message = format!("Workflow panicked: {}", panic_msg);
                if let Err(e) = checkpoint_db.fail_task_run(&execution_id, &error_message) {
                    error!(
                        "Failed to mark panicked workflow {} as failed: {}",
                        execution_id, e
                    );
                } else {
                    info!(
                        "Marked panicked workflow '{}' as failed - Continue button should now be available",
                        workflow_name
                    );
                }
            }
        }
    });
}

/// Spawns a workflow sequence execution with proper panic handling.
///
/// Similar to `spawn_workflow_with_panic_guard` but for workflow sequences
/// that don't return a WorkflowResult directly.
///
/// # Arguments
/// * `checkpoint_db` - Database for marking the task as failed on panic
/// * `execution_id` - The task run ID to mark as failed if panic occurs
/// * `sequence_name` - Name of the sequence (for logging)
/// * `fut` - The async future that runs the sequence (returns ())
pub fn spawn_sequence_with_panic_guard<F>(
    checkpoint_db: Arc<crate::database::CheckpointDb>,
    execution_id: String,
    sequence_name: String,
    fut: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        // Wrap the future in AssertUnwindSafe and catch panics
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;

        match result {
            Ok(()) => {
                info!(
                    "Workflow sequence '{}' (id: {}) completed",
                    sequence_name, execution_id
                );
            }
            Err(panic_payload) => {
                // Extract panic message
                let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic (no message)".to_string()
                };

                error!(
                    "PANIC in workflow sequence '{}' (id: {}): {}",
                    sequence_name, execution_id, panic_msg
                );

                // Mark the task as failed so Continue button appears
                let error_message = format!("Workflow sequence panicked: {}", panic_msg);
                if let Err(e) = checkpoint_db.fail_task_run(&execution_id, &error_message) {
                    error!(
                        "Failed to mark panicked sequence {} as failed: {}",
                        execution_id, e
                    );
                } else {
                    info!(
                        "Marked panicked sequence '{}' as failed - Continue button should now be available",
                        sequence_name
                    );
                }
            }
        }
    });
}
