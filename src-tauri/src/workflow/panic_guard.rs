//! Panic- and cancellation-safe spawning for workflow runs.
//!
//! `spawn_sequence_with_panic_guard` runs an automation-only or composed
//! workflow future so that a panic or a dropped task always fails the task
//! run instead of leaving it a zombie.

use futures_util::FutureExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info};

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
pub(crate) struct WorkflowDropGuard {
    pub(crate) execution_id: String,
    pub(crate) workflow_name: String,
    pub(crate) completed: Arc<AtomicBool>,
    /// URL lock manager for releasing per-URL reservations on drop.
    pub(crate) url_lock_manager: Option<Arc<crate::executor::UrlLockManager>>,
    /// File registry manager for releasing per-execution file registrations on drop.
    pub(crate) file_registry_manager: Option<Arc<crate::executor::FileRegistryManager>>,
    /// File lock manager for releasing per-file exclusive locks on drop.
    pub(crate) file_lock_manager: Option<Arc<crate::executor::FileLockManager>>,
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

        // Always release file registry entries on drop.
        if let Some(ref mgr) = self.file_registry_manager {
            let mgr = mgr.clone();
            let id = self.execution_id.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    mgr.release_all(&id).await;
                });
            } else {
                mgr.release_all_sync(&self.execution_id);
            }
        }

        // Always release exclusive file locks on drop.
        if let Some(ref mgr) = self.file_lock_manager {
            let mgr = mgr.clone();
            let id = self.execution_id.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    mgr.release_all(&id).await;
                });
            } else {
                mgr.release_all_sync(&self.execution_id);
            }
        }

        if !self.completed.load(Ordering::SeqCst) {
            error!(
                "Workflow task '{}' (id: {}) was dropped without completing — \
                 zombie state possible (PG fail_task_run handled in panic guard)",
                self.workflow_name, self.execution_id
            );
        }
    }
}

// =============================================================================
// Panic-Safe Workflow Sequence Execution
// =============================================================================

/// Spawns a workflow sequence execution with proper panic and cancellation handling.
///
/// Similar to `spawn_workflow_with_panic_guard` but for workflow sequences
/// that don't return a WorkflowResult directly.
///
/// # Arguments
/// * `execution_id` - The task run ID to mark as failed if panic occurs
/// * `sequence_name` - Name of the sequence (for logging)
/// * `fut` - The async future that runs the sequence (returns ())
pub fn spawn_sequence_with_panic_guard<F>(
    execution_id: String,
    sequence_name: String,
    url_lock_manager: Option<Arc<crate::executor::UrlLockManager>>,
    file_registry_manager: Option<Arc<crate::executor::FileRegistryManager>>,
    file_lock_manager: Option<Arc<crate::executor::FileLockManager>>,
    pg_db: Arc<crate::database::pg::PgDb>,
    fut: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let id_for_guard = execution_id.clone();
    let name_for_guard = sequence_name.clone();
    let url_lock_for_guard = url_lock_manager.clone();
    let file_registry_for_guard = file_registry_manager.clone();
    let file_lock_for_guard = file_lock_manager.clone();

    tokio::spawn(async move {
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
                let fail_err = pg_db
                    .fail_task_run(&execution_id, &error_message)
                    .await
                    .err();
                if let Some(e) = fail_err {
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
