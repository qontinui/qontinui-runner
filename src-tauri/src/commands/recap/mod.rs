//! Recap module for the Session Recap page.
//!
//! Provides a quick overview of what happened during a task run,
//! prominently showing failure reasons and displaying all steps with brief summaries.
//!
//! # Module Structure
//!
//! - `types`: Data structures for recap data
//! - `utils`: Utility functions for parsing and formatting
//! - `extractors`: Functions to extract failure info, stats, and summaries
//! - `step_builder`: Functions to build recap steps
//! - `stage_builder`: Functions to build recap stages

mod extractors;
mod stage_builder;
mod step_builder;
pub mod types;
mod utils;

use crate::commands::AppState;
use std::sync::Arc;
use tauri::State;
use tracing::{info, warn};

pub use types::{RecapData, RecapResponse};

/// Get recap data for a specific task run.
#[tauri::command]
pub async fn get_task_run_recap(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<RecapResponse<RecapData>, String> {
    info!("Getting recap for task run: {}", task_run_id);

    // Get the task run
    let task_run_result = state.pg_db.get_task_run(&task_run_id).await;
    let task_run = match task_run_result {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(RecapResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => {
            warn!("Failed to get task run {}: {}", task_run_id, e);
            return Ok(RecapResponse::err(e));
        }
    };

    // Get automation records
    let automations = state
        .checkpoint_db
        .get_task_run_automations(&task_run_id)
        .unwrap_or_else(|e| {
            warn!("Failed to get automations for {}: {}", task_run_id, e);
            Vec::new()
        });

    // Get events (limited for performance)
    // TODO: Wire to PG when Tauri commands go async
    let events = state
        .checkpoint_db
        .get_task_run_events(&task_run_id, None, Some(500))
        .unwrap_or_else(|e| {
            warn!("Failed to get events for {}: {}", task_run_id, e);
            Vec::new()
        });

    info!(
        "get_task_run_recap: task_run_id='{}', events_count={}, automations_count={}",
        task_run_id,
        events.len(),
        automations.len()
    );

    // Get orchestrator verification results for this task run (criterion-based)
    let verification_results = state
        .checkpoint_db
        .get_latest_verification_results(&task_run_id)
        .unwrap_or_else(|e| {
            warn!(
                "Failed to get verification results for {}: {}",
                task_run_id, e
            );
            Vec::new()
        });

    // Get workflow verification phase results (step-based, from execute_verification_steps)
    let workflow_verification_results = state
        .checkpoint_db
        .get_all_verification_phase_results(&task_run_id)
        .unwrap_or_else(|e| {
            warn!(
                "Failed to get workflow verification phase results for {}: {}",
                task_run_id, e
            );
            Vec::new()
        });

    // Get workflow step checkpoints (from unified workflow executor)
    let step_checkpoints = state
        .checkpoint_db
        .get_all_workflow_step_checkpoints(&task_run_id)
        .unwrap_or_else(|e| {
            warn!(
                "Failed to get workflow step checkpoints for {}: {}",
                task_run_id, e
            );
            Vec::new()
        });

    info!(
        "get_task_run_recap: step_checkpoints_count={}",
        step_checkpoints.len()
    );

    // Build steps - include workflow verification results and step checkpoints for richer step data
    let steps = step_builder::build_steps(
        &task_run,
        &automations,
        &events,
        &verification_results,
        &workflow_verification_results,
        &step_checkpoints,
    );

    // Build stages from transition history or heuristically
    let stages = stage_builder::build_stages(&task_run, &steps, &verification_results);

    // Extract failure info
    let failure_info = extractors::extract_failure_info(&task_run, &automations);

    // Calculate stats
    let stats = extractors::calculate_stats(&task_run, &automations, &events);

    // Calculate duration
    let duration_ms = if let Some(ref completed) = task_run.completed_at {
        utils::calculate_duration_ms(&task_run.created_at, completed)
    } else {
        None
    };

    // Use AI-generated summary only. When absent, the frontend shows
    // "No AI summary available" with a "Generate Summary" button — much better
    // than the heuristic extraction which often returns raw AI conversation text.
    let summary = task_run.summary.clone();

    let recap = RecapData {
        task_run_id: task_run.id.clone(),
        task_name: task_run.task_name.clone(),
        status: task_run.status.clone(),
        duration_ms,
        created_at: task_run.created_at.clone(),
        completed_at: task_run.completed_at.clone(),
        failure_info,
        summary,
        goal_achieved: task_run.goal_achieved,
        stages,
        steps,
        stats,
    };

    Ok(RecapResponse::ok(recap))
}
