//! Task state management for the loop controller.
//!
//! Handles stop/pause checking, activity recording, state persistence, and stage transitions.

use tracing::{debug, info, warn};

use crate::event_system::EventBroadcaster;
use crate::orchestrator::integration::StageTransition;
use crate::workflow_state::{StateMachine, WorkflowState};

use super::loop_controller::LoopController;
use super::states::UnifiedWorkflowState;
use super::types::get_parent_task_id;

impl LoopController {
    /// Check if the task has been stopped externally (via stop_ai_analysis endpoint).
    ///
    /// This allows the loop to gracefully abort when the user clicks the Stop button.
    pub(crate) fn is_task_stopped(&self, execution_id: &str) -> bool {
        // For composed run children (e.g., composed-run-X-workflow-N),
        // check the parent task instead since children don't have their own task_run records
        let task_id_to_check = get_parent_task_id(execution_id);

        // PG-primary: use block_on to call async PG from sync context
        let task_result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let pg = self.app_state.pg_db.clone();
            let id = task_id_to_check.clone();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle.block_on(async move { pg.get_task_run(&id).await })
            }))
            .unwrap_or_else(|_| Err("block_on panicked".to_string()))
        } else {
            Err("no tokio runtime".to_string())
        };

        match task_result {
            Ok(Some(task)) => {
                if task.status == "stopped" {
                    info!(
                        "Task {} has been stopped externally - aborting workflow",
                        task_id_to_check
                    );
                    true
                } else {
                    false
                }
            }
            Ok(None) => {
                // Only treat as stopped if this is not a sequence child
                // (sequence children are expected to not have task_run records)
                if task_id_to_check == execution_id {
                    warn!(
                        "Task {} not found in database - treating as stopped",
                        execution_id
                    );
                    true
                } else {
                    // Parent also not found - this shouldn't happen but continue anyway
                    warn!(
                        "Parent task {} not found for sequence child {} - continuing execution",
                        task_id_to_check, execution_id
                    );
                    false
                }
            }
            Err(e) => {
                warn!(
                    "Failed to check task {} status: {} - continuing execution",
                    task_id_to_check, e
                );
                false
            }
        }
    }

    /// Check if the task has been paused by the user.
    pub(crate) fn is_task_paused(&self, execution_id: &str) -> bool {
        let task_id_to_check = get_parent_task_id(execution_id);

        // PG-primary: use block_on to call async PG from sync context
        let task_result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let pg = self.app_state.pg_db.clone();
            let id = task_id_to_check.clone();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle.block_on(async move { pg.get_task_run(&id).await })
            }))
            .unwrap_or_else(|_| Err("block_on panicked".to_string()))
        } else {
            Err("no tokio runtime".to_string())
        };

        match task_result {
            Ok(Some(task)) => task.status == "paused",
            _ => false,
        }
    }

    /// Wait while the task is paused, polling every 500ms.
    /// Returns immediately if the task is not paused.
    /// Also returns if the task is stopped (so the caller can handle stop).
    pub(crate) async fn wait_while_paused(&self, execution_id: &str) {
        if !self.is_task_paused(execution_id) {
            return;
        }

        info!("Task {} is paused - waiting for resume", execution_id);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Check if stopped (takes priority over pause)
            if self.is_task_stopped(execution_id) {
                info!("Task {} was stopped while paused", execution_id);
                return;
            }

            // Check if still paused
            if !self.is_task_paused(execution_id) {
                info!("Task {} resumed - continuing execution", execution_id);
                return;
            }
        }
    }

    /// Record a workflow activity heartbeat for debugging stuck workflows.
    /// Updates runtime_context_json with the current phase and timestamp.
    pub(crate) fn record_activity(&self, _execution_id: &str, _activity: &str) {
        // Activity recording removed — all persistence now via PgDb.
    }

    /// Persist the workflow state to the database and broadcast a state change event.
    ///
    /// This creates or updates the workflow_execution_state record,
    /// enabling resume from the exact state after a restart.
    ///
    /// After successful persistence, broadcasts an `orchestrator-state-change` event
    /// to both Tauri frontend and WebSocket clients in real-time.
    ///
    /// For composed run children (e.g., composed-run-X-workflow-N),
    /// state is persisted under the parent composed run ID since children don't have
    /// their own task_run records.
    pub(crate) fn persist_workflow_state(&self, execution_id: &str, state: &UnifiedWorkflowState) {
        // For composed run children (e.g., composed-run-X-workflow-N),
        // persist under the parent ID since children don't have their own task_run records.
        let persist_id = get_parent_task_id(execution_id);

        let state_machine = StateMachine::new(
            self.app_state.pg_db.clone(),
            &persist_id,
            "unified",
            state.clone(),
        );

        if let Err(e) = state_machine.persist() {
            warn!("Failed to persist workflow state for {}: {}", persist_id, e);
        } else {
            debug!(
                "Persisted workflow state '{}' for execution {} (db key: {})",
                state.name(),
                execution_id,
                persist_id
            );

            // Broadcast real-time event to notify all clients (Tauri + WebSocket) of state change
            let broadcaster = EventBroadcaster::new(self.app_handle.clone());
            broadcaster.orchestrator_state_change(
                &persist_id,
                state.name(),
                state.iteration().unwrap_or(0),
                state.phase().unwrap_or("unknown"),
            );

            // Also broadcast step-progress so widgets watching step data refetch
            broadcaster.step_progress(
                &persist_id,
                0,
                state.name(),
                state.phase().unwrap_or("unknown"),
                None,
            );
        }
    }

    /// Record a stage transition and persist to database.
    pub(crate) fn record_stage_transition(
        &self,
        execution_id: &str,
        transitions: &mut Vec<StageTransition>,
        current_stage: &mut String,
        to_stage: &str,
        iteration: u32,
    ) {
        if current_stage.as_str() != to_stage {
            let transition = StageTransition {
                from: current_stage.clone(),
                to: to_stage.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                iteration,
            };
            transitions.push(transition);
            *current_stage = to_stage.to_string();

            // Transition history persistence removed — all persistence now via PgDb.
        }
    }
}
