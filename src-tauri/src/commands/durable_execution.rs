//! Tauri commands for Conductor-inspired durable execution features:
//! replay from arbitrary checkpoint, manual rollback, and iteration inspection.

use crate::commands::compartments::StorageCompartment;
use crate::database::pg::PgDb;
use crate::unified_workflow_executor::compensation::CompensationManager;
use crate::unified_workflow_executor::replay::{ReplayManager, ReplayTarget};
use crate::unified_workflow_executor::types::ReplayPoint;
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

/// Resolve the working directory for a task run by checking:
/// 1. Active worktree associated with the task run
/// 2. Fallback: current working directory
fn resolve_working_dir(pg_db: &Arc<PgDb>, execution_id: &str) -> Result<String, String> {
    // Check for a worktree associated with this task run (PG-primary via block_in_place)
    let pg = pg_db.clone();
    let worktrees_result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::block_in_place(|| {
                handle.block_on(async move { pg.list_worktrees(None).await })
            })
        }))
        .unwrap_or_else(|_| Err("block_in_place panicked".to_string()))
    } else {
        Err("No tokio runtime".to_string())
    };
    if let Ok(worktrees) = worktrees_result {
        for wt in &worktrees {
            if wt.task_run_id.as_deref() == Some(execution_id) {
                return Ok(wt.worktree_path.clone());
            }
        }
        // Also check parent task run (execution_id might be a child)
        for wt in &worktrees {
            if execution_id.starts_with(wt.task_run_id.as_deref().unwrap_or(""))
                && wt.task_run_id.is_some()
            {
                return Ok(wt.worktree_path.clone());
            }
        }
    }

    // Fallback: use the repo_path from the first worktree, or cwd
    Err("Cannot determine working directory: no worktree found for this execution. Pass workingDir explicitly.".to_string())
}

/// List all available replay points for a workflow execution.
///
/// Returns one entry per iteration with commit hash, verification pass/fail counts,
/// and timestamp. Used by the UI to show a timeline of iteration states.
#[tauri::command]
pub async fn list_replay_points(
    storage: State<'_, StorageCompartment>,
    execution_id: String,
) -> Result<Vec<ReplayPoint>, String> {
    let replay_manager = ReplayManager::new(storage.pg_db().clone());
    replay_manager.list_replay_points(&execution_id)
}

/// Replay a workflow from a specific iteration.
///
/// Resets git state to the commit before the target iteration, clears downstream
/// checkpoints, and returns a description of the prepared resume point.
/// The caller is responsible for re-entering the loop controller.
///
/// `target_phase` can be: "full" (re-run verification + agentic), "verification"
/// (re-run only verification), or "agentic" (re-run only agentic phase).
/// `working_dir` is optional — if omitted, resolved from the task's worktree.
#[tauri::command]
pub async fn replay_workflow(
    storage: State<'_, StorageCompartment>,
    execution_id: String,
    target_iteration: u32,
    working_dir: Option<String>,
    target_phase: Option<String>,
) -> Result<String, String> {
    let pg_db = storage.pg_db();
    let replay_manager = ReplayManager::new(pg_db.clone());

    let resolved_dir = match working_dir {
        Some(ref d) if !d.is_empty() && d != "." => d.clone(),
        _ => resolve_working_dir(pg_db, &execution_id)?,
    };

    let target = match target_phase.as_deref() {
        Some("verification") => ReplayTarget::VerificationOnly {
            iteration: target_iteration,
        },
        Some("agentic") => ReplayTarget::AgenticOnly {
            iteration: target_iteration,
        },
        _ => ReplayTarget::FromIteration {
            iteration: target_iteration,
        },
    };

    let resume_point = replay_manager
        .prepare_replay(&execution_id, &target, std::path::Path::new(&resolved_dir))
        .await?;

    Ok(format!(
        "Replay prepared: {:?}. Re-launch execution {} to continue.",
        resume_point, execution_id
    ))
}

/// Manually rollback a workflow execution to a specific iteration's commit.
///
/// This performs a `git reset --hard` to the commit recorded at the target iteration.
/// Only safe in worktree-isolated executions.
/// `working_dir` is optional — if omitted, resolved from the task's worktree.
#[tauri::command]
pub async fn rollback_workflow_to_iteration(
    storage: State<'_, StorageCompartment>,
    execution_id: String,
    target_iteration: u32,
    working_dir: Option<String>,
) -> Result<String, String> {
    let pg_db = storage.pg_db();
    let resolved_dir = match working_dir {
        Some(ref d) if !d.is_empty() && d != "." => d.clone(),
        _ => resolve_working_dir(pg_db, &execution_id)?,
    };

    let compensation_manager = CompensationManager::new(pg_db.clone());

    let commit = compensation_manager
        .rollback_to_iteration(
            &execution_id,
            target_iteration,
            std::path::Path::new(&resolved_dir),
        )
        .await?;

    Ok(format!(
        "Rolled back to iteration {} (commit {})",
        target_iteration,
        &commit[..commit.len().min(8)]
    ))
}

/// Get the structured iteration diffs for a workflow execution.
///
/// Returns the JSON array of IterationDiff objects showing what changed
/// in each iteration (files modified, insertions, deletions, commit hashes).
#[tauri::command]
pub async fn get_iteration_diffs(
    storage: State<'_, StorageCompartment>,
    execution_id: String,
) -> Result<Vec<crate::unified_workflow_executor::types::IterationDiff>, String> {
    storage.pg_db().get_iteration_diffs(&execution_id).await
}

/// Get the commit checkpoints for a workflow execution.
///
/// Returns the JSON array of IterationCommit objects mapping each iteration
/// to its git commit hash.
#[tauri::command]
pub async fn get_iteration_commits(
    storage: State<'_, StorageCompartment>,
    execution_id: String,
) -> Result<Vec<crate::unified_workflow_executor::types::IterationCommit>, String> {
    storage.pg_db().get_iteration_commits(&execution_id).await
}

/// Get the structured phase-level results for a workflow execution.
///
/// Returns the JSON array of PhaseResult objects — one row per phase
/// executed, with step-level breakdown, timing, and failure context.
/// Powers the Phase Timeline tab in the run recap.
///
/// Applies `get_parent_task_id` remapping to mirror the write side
/// (`emit_and_persist_phase_result` keys on the parent task id) so that
/// composed-run children surface their parent's phase timeline rather
/// than returning an empty list.
#[tauri::command]
pub async fn get_phase_results(
    storage: State<'_, StorageCompartment>,
    execution_id: String,
) -> Result<Vec<crate::unified_workflow_executor::types::PhaseResult>, String> {
    let parent_id = crate::unified_workflow_executor::types::get_parent_task_id(&execution_id);
    storage.pg_db().get_phase_results(&parent_id).await
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_durable_execution")
        .invoke_handler(tauri::generate_handler![
            list_replay_points,
            replay_workflow,
            rollback_workflow_to_iteration,
            get_iteration_diffs,
            get_iteration_commits,
            get_phase_results,
        ])
        .build()
}
