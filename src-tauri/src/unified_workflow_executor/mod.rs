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

pub(crate) mod auto_run;
mod loop_controller;
mod phase_configs;
mod phases;
mod resume;
pub mod states;
mod types;

// Panic handling imports
use futures_util::FutureExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

// Core types used by external code
pub use loop_controller::{
    // Phase-aware versions (preferred for unified workflows)
    convert_all_json_steps_with_phase,
    convert_json_steps_with_phase,
    extract_prompt_steps_with_phase,
    extract_workflow_id_from_task_id,
    resume_interrupted_workflows,
    LoopController,
    ResumeConfig,
};
pub use types::{get_parent_task_id, LoopConfig, LoopResult, StageConfig, WorkflowPhase};

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
// Drop Guard for Workflow Task Safety
// =============================================================================

/// A guard that ensures a workflow task is marked as failed if the tokio task
/// is dropped without completing normally.
///
/// `catch_unwind` catches panics, but it does NOT catch tokio task cancellations
/// (e.g., `JoinHandle::abort()`, runtime shutdown). When a task is cancelled,
/// the future is simply dropped — no panic occurs, and `catch_unwind` never
/// runs its Ok/Err arms. This leaves the workflow in a zombie state: the DB
/// still says "running" but no executor is alive.
///
/// This guard uses `Drop` to detect that scenario. If the task exits without
/// the `completed` flag being set, the guard marks the workflow as failed.
struct WorkflowDropGuard {
    checkpoint_db: Arc<crate::database::CheckpointDb>,
    execution_id: String,
    workflow_name: String,
    completed: Arc<AtomicBool>,
    /// URL lock manager for releasing per-URL reservations on drop.
    url_lock_manager: Option<Arc<crate::executor::UrlLockManager>>,
}

impl Drop for WorkflowDropGuard {
    fn drop(&mut self) {
        // Always release URL locks on drop, whether completed or not.
        // Spawn an async task for reliable release — this guard always lives
        // inside a tokio::spawn, so a runtime handle is available.
        if let Some(ref mgr) = self.url_lock_manager {
            let mgr = mgr.clone();
            let id = self.execution_id.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    mgr.release_all(&id).await;
                });
            } else {
                // Fallback to sync if no runtime (shouldn't happen in practice)
                mgr.release_all_sync(&self.execution_id);
            }
        }

        if !self.completed.load(Ordering::SeqCst) {
            error!(
                "Workflow task '{}' (id: {}) was dropped without completing — \
                 marking as failed to prevent zombie state",
                self.workflow_name, self.execution_id
            );
            let error_message =
                "Workflow task was unexpectedly terminated (possible task cancellation or runtime shutdown)";
            if let Err(e) = self
                .checkpoint_db
                .fail_task_run(&self.execution_id, error_message)
            {
                error!(
                    "Failed to mark dropped workflow {} as failed: {}",
                    self.execution_id, e
                );
            } else {
                warn!(
                    "Marked dropped workflow '{}' as failed — Continue button should now be available",
                    self.workflow_name
                );
            }
        }
    }
}

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
    url_lock_manager: Option<Arc<crate::executor::UrlLockManager>>,
    fut: F,
) where
    F: std::future::Future<Output = loop_controller::WorkflowResult> + Send + 'static,
{
    let db_for_guard = checkpoint_db.clone();
    let id_for_guard = execution_id.clone();
    let name_for_guard = workflow_name.clone();
    let url_lock_for_guard = url_lock_manager.clone();

    tokio::spawn(async move {
        // The drop guard ensures cleanup even if this task is cancelled/aborted.
        // It MUST be created inside the spawned task so its Drop runs when the
        // task is dropped.
        let completed = Arc::new(AtomicBool::new(false));
        let _guard = WorkflowDropGuard {
            checkpoint_db: db_for_guard,
            execution_id: id_for_guard,
            workflow_name: name_for_guard,
            completed: completed.clone(),
            url_lock_manager: url_lock_for_guard.clone(),
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

/// Spawns a workflow sequence execution with proper panic and cancellation handling.
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
    url_lock_manager: Option<Arc<crate::executor::UrlLockManager>>,
    fut: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let db_for_guard = checkpoint_db.clone();
    let id_for_guard = execution_id.clone();
    let name_for_guard = sequence_name.clone();
    let url_lock_for_guard = url_lock_manager.clone();

    tokio::spawn(async move {
        let completed = Arc::new(AtomicBool::new(false));
        let _guard = WorkflowDropGuard {
            checkpoint_db: db_for_guard,
            execution_id: id_for_guard,
            workflow_name: name_for_guard,
            completed: completed.clone(),
            url_lock_manager: url_lock_for_guard.clone(),
        };

        // Wrap the future in AssertUnwindSafe and catch panics
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;

        completed.store(true, Ordering::SeqCst);

        // Explicitly release URL locks (async path — more reliable than sync Drop)
        if let Some(ref mgr) = url_lock_manager {
            mgr.release_all(&execution_id).await;
        }

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
