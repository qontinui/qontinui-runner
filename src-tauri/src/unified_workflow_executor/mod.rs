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
//!     base_prompt: "Fix the issues".to_string(),
//!     workflow_name: "My Workflow".to_string(),
//!     workflow_id: "wf-123".to_string(),
//!     execution_id: "exec-456".to_string(),
//!     targeted_error_ids: vec![],
//!     starting_iteration: 0,
//!     run_agentic_first: false, // Set to true for error-fix workflows
//!     artifact_dir: None, // Auto-created if None
//!     is_dev_mode: false,
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

pub mod agentic_output;
mod agentic_verification_loop;
pub mod approval;
pub(crate) mod auto_run;
pub mod blame;
mod canvas_panels;
pub mod compensation;
pub mod conditional_routing;
pub mod convergence;
pub mod deferred_feedback;
mod evaluate_verification;
// The output log's summary line and its verdict words, for the parser's tests
// in `mcp/task_run_structured_output.rs`; the module itself stays private.
#[cfg(test)]
pub(crate) use evaluate_verification::{every_verdict_label, verification_summary_line};
mod health_monitor;
mod loop_controller;
mod loop_handlers;
mod loop_handlers_decisions;
pub(crate) mod loop_state_machine;
pub mod multi_agent_fixer;
pub(crate) mod multi_agent_pipeline_loop;
pub mod output_parser;
mod phase_configs;
mod phase_helpers;
mod phases;
pub mod prehydration;
pub mod replay;
mod resume;
pub(crate) mod startup_resume;
pub mod states;
mod task_lifecycle;
mod task_state;
pub(crate) mod types;

// Panic handling imports
use futures_util::FutureExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::workflow::panic_guard::WorkflowDropGuard;

// Core types used by external code
#[allow(unused_imports)]
pub use conditional_routing::RoutingContext;
pub use loop_controller::LoopController;
pub use startup_resume::{resume_interrupted_workflows, ResumeConfig};
pub use types::{LoopConfig, LoopResult};

// Types exposed for API consumers and advanced usage
// These may not be directly used in this crate but are part of the public API
#[allow(unused_imports)]
pub use startup_resume::WorkflowResult;
#[allow(unused_imports)]
pub use types::{
    AgenticOutcome, IterationCommit, IterationDiff, IterationResult, ReplayPoint, RollbackPolicy,
};

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

/// Spawns a workflow execution with proper panic and cancellation handling.
///
/// This function wraps the workflow execution in two safety layers:
/// 1. **Panic guard**: `catch_unwind` catches panics and marks the task as failed.
/// 2. **Drop guard**: A `WorkflowDropGuard` catches task cancellations (abort,
///    runtime shutdown) that bypass `catch_unwind`, and marks the task as failed.
///
/// Together, these ensure a workflow never becomes a zombie — regardless of how
/// the task exits, the DB state is always cleaned up.
///
/// # Arguments
/// * `execution_id` - The task run ID to mark as failed if panic occurs
/// * `workflow_name` - Name of the workflow (for logging)
/// * `fut` - The async future that runs the workflow
///
/// # Usage
/// ```ignore
/// spawn_workflow_with_panic_guard(
///     &execution_id,
///     &workflow_name,
///     async move {
///         controller.run(config, ...).await
///     },
/// );
/// ```
pub fn spawn_workflow_with_panic_guard<F>(
    execution_id: String,
    workflow_name: String,
    url_lock_manager: Option<Arc<crate::executor::UrlLockManager>>,
    file_registry_manager: Option<Arc<crate::executor::FileRegistryManager>>,
    file_lock_manager: Option<Arc<crate::executor::FileLockManager>>,
    pg_db: Arc<crate::database::pg::PgDb>,
    fut: F,
) where
    F: std::future::Future<Output = loop_controller::WorkflowResult> + Send + 'static,
{
    let id_for_guard = execution_id.clone();
    let name_for_guard = workflow_name.clone();
    let url_lock_for_guard = url_lock_manager.clone();
    let file_registry_for_guard = file_registry_manager.clone();
    let file_lock_for_guard = file_lock_manager.clone();

    tokio::spawn(async move {
        // The drop guard ensures cleanup even if this task is cancelled/aborted.
        // It MUST be created inside the spawned task so its Drop runs when the
        // task is dropped.
        let completed = Arc::new(AtomicBool::new(false));
        let _guard = WorkflowDropGuard {
            execution_id: id_for_guard,
            workflow_name: name_for_guard,
            completed: completed.clone(),
            url_lock_manager: url_lock_for_guard.clone(),
            file_registry_manager: file_registry_for_guard.clone(),
            file_lock_manager: file_lock_for_guard.clone(),
        };

        // Wrap the future in AssertUnwindSafe and catch panics
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;

        // If we reach here, the task was NOT cancelled — mark the guard as completed
        // so its Drop doesn't fire the failsafe.
        completed.store(true, Ordering::SeqCst);

        // Explicitly release URL locks (async path — more reliable than sync Drop)
        if let Some(ref mgr) = url_lock_manager {
            mgr.release_all(&execution_id).await;
        }

        // Explicitly release file registry entries (async path)
        if let Some(ref mgr) = file_registry_manager {
            mgr.release_all(&execution_id).await;
        }

        // Explicitly release exclusive file locks (async path)
        if let Some(ref mgr) = file_lock_manager {
            mgr.release_all(&execution_id).await;
        }

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
                let fail_err = pg_db
                    .fail_task_run(&execution_id, &error_message)
                    .await
                    .err();
                if let Some(e) = fail_err {
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
