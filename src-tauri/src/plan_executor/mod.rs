//! Plan Executor Module
//!
//! Executes structured implementation plans where each phase runs as a separate AI session.
//! Unlike the unified workflow executor which uses a verification-agentic loop, the plan
//! executor runs phases sequentially with no verification step.
//!
//! ## Usage
//!
//! ```ignore
//! let executor = PlanExecutor::new(app_state, app_handle, pid_tracker);
//!
//! let config = PlanConfig {
//!     plan_name: "My Plan".to_string(),
//!     plan_overview: "Overview of the plan".to_string(),
//!     phases: vec![
//!         PlanPhase { name: "Phase 1".to_string(), prompt: "Do step 1".to_string() },
//!         PlanPhase { name: "Phase 2".to_string(), prompt: "Do step 2".to_string() },
//!     ],
//!     next_steps_sweep: true,
//!     max_next_steps_iterations: 5,
//!     execution_id: "plan-123".to_string(),
//! };
//!
//! let result = executor.run(config).await;
//! ```

mod executor;
pub mod states;
pub mod types;

// Panic handling imports
use futures_util::FutureExt;
use std::sync::Arc;
use tracing::{error, info};

pub use executor::PlanExecutor;
pub use types::{PlanConfig, PlanPhase, PlanResult};

/// Spawns a plan execution with proper panic handling.
///
/// If the plan future panics, the task is marked as failed so the user sees the error.
pub fn spawn_plan_with_panic_guard<F>(
    checkpoint_db: Arc<crate::database::CheckpointDb>,
    execution_id: String,
    plan_name: String,
    fut: F,
) where
    F: std::future::Future<Output = PlanResult> + Send + 'static,
{
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;

        match result {
            Ok(plan_result) => {
                info!(
                    "Plan '{}' (id: {}) completed: success={}, phases={}/{}",
                    plan_name,
                    execution_id,
                    plan_result.success,
                    plan_result.completed_phases,
                    plan_result.total_phases
                );
            }
            Err(panic_payload) => {
                let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic (no message)".to_string()
                };

                error!(
                    "PANIC in plan '{}' (id: {}): {}",
                    plan_name, execution_id, panic_msg
                );

                let error_message = format!("Plan panicked: {}", panic_msg);
                if let Err(e) = checkpoint_db.fail_task_run(&execution_id, &error_message) {
                    error!(
                        "Failed to mark panicked plan {} as failed: {}",
                        execution_id, e
                    );
                } else {
                    info!("Marked panicked plan '{}' as failed", plan_name);
                }
            }
        }
    });
}
