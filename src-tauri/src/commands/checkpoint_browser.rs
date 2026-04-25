//! Tauri commands for the Checkpoint Browser (Time-Travel Debugging).
//!
//! Provides access to orchestrator checkpoints for browsing execution history,
//! comparing states, and initiating replay sessions.
//!
//! Uses PostgreSQL for persistence with in-memory CheckpointManager for
//! real-time operations like replay.

use crate::commands::compartments::StorageCompartment;
use crate::database::CreateTaskRunInput;
use crate::error::AppError;
use crate::orchestrator::checkpoint::{
    Checkpoint, CheckpointDiff, CheckpointManager, CheckpointSummary, CheckpointTrigger,
    LineageInfo, LineageTree, ReplayManager, ReplaySession, RestorationInstructions,
    StateRestorationConfig, StateSnapshot, VerificationSnapshot,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

/// Global checkpoint manager instance for in-memory operations (replay, comparison).
/// The database stores checkpoints persistently; this provides real-time operations.
static CHECKPOINT_MANAGER: Lazy<Mutex<CheckpointManager>> =
    Lazy::new(|| Mutex::new(CheckpointManager::new().with_max_per_task(50)));

/// Global replay manager instance for managing replay sessions and lineage.
static REPLAY_MANAGER: Lazy<Mutex<ReplayManager>> = Lazy::new(|| Mutex::new(ReplayManager::new()));

/// List all checkpoints, optionally filtered by task ID.
#[tauri::command]
pub async fn list_orchestrator_checkpoints(
    state: State<'_, StorageCompartment>,
    task_id: Option<String>,
) -> Result<Vec<CheckpointSummary>, String> {
    let checkpoints_json = state
        .pg_db()
        .get_orchestrator_checkpoints(task_id.as_deref())
        .await?;

    let summaries: Vec<CheckpointSummary> = checkpoints_json
        .into_iter()
        .map(|cp| CheckpointSummary {
            id: cp["id"].as_str().unwrap_or("").to_string(),
            task_id: cp["task_id"].as_str().unwrap_or("").to_string(),
            iteration: cp["iteration"].as_i64().unwrap_or(0) as u32,
            trigger_type: cp["trigger"].as_str().unwrap_or("Manual").to_string(),
            name: cp["name"].as_str().map(|s| s.to_string()),
            created_at: cp["created_at"].as_str().unwrap_or("").to_string(),
            state: cp["state"].as_str().unwrap_or("Unknown").to_string(),
            tags: cp["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();

    Ok(summaries)
}

/// Get a checkpoint by ID.
#[tauri::command]
pub async fn get_orchestrator_checkpoint(
    state: State<'_, StorageCompartment>,
    id: String,
) -> Result<Option<Checkpoint>, String> {
    let checkpoint_json = state.pg_db().get_orchestrator_checkpoint(&id).await?;

    if let Some(cp) = checkpoint_json {
        // Deserialize the state snapshot from JSON
        let state_json = &cp["state"];
        let snapshot: StateSnapshot =
            serde_json::from_value(state_json.clone()).unwrap_or_else(|_| StateSnapshot {
                state: state_json.as_str().unwrap_or("Unknown").to_string(),
                iteration: cp["iteration"].as_i64().unwrap_or(0) as u32,
                channels: HashMap::new(),
                knowledge: Vec::new(),
                verification: VerificationSnapshot {
                    criteria_results: HashMap::new(),
                    overall_passed: false,
                },
                findings: Vec::new(),
                files_modified: Vec::new(),
                custom_data: HashMap::new(),
            });

        let trigger = match cp["trigger"].as_str().unwrap_or("Manual") {
            "Manual" => CheckpointTrigger::Manual,
            "VerificationBoundary" => CheckpointTrigger::VerificationBoundary,
            s if s.starts_with("IterationBoundary") => CheckpointTrigger::IterationBoundary {
                iteration: cp["iteration"].as_i64().unwrap_or(0) as u32,
            },
            _ => CheckpointTrigger::Automatic {
                reason: "Restored from database".to_string(),
            },
        };

        let mut checkpoint =
            Checkpoint::new(cp["task_id"].as_str().unwrap_or(""), snapshot).with_trigger(trigger);

        if let Some(name) = cp["name"].as_str() {
            checkpoint = checkpoint.with_name(name);
        }

        Ok(Some(checkpoint))
    } else {
        Ok(None)
    }
}

/// Create a manual checkpoint for a task.
#[tauri::command]
pub async fn create_orchestrator_checkpoint(
    app_state: State<'_, StorageCompartment>,
    task_id: String,
    name: Option<String>,
    description: Option<String>,
    state: String,
    iteration: u32,
) -> Result<String, String> {
    create_orchestrator_checkpoint_impl(app_state, task_id, name, description, state, iteration)
        .await
        .map_err(String::from)
}

async fn create_orchestrator_checkpoint_impl(
    app_state: State<'_, StorageCompartment>,
    task_id: String,
    name: Option<String>,
    description: Option<String>,
    state: String,
    iteration: u32,
) -> Result<String, AppError> {
    // Create state snapshot
    let snapshot = StateSnapshot {
        state: state.clone(),
        iteration,
        channels: HashMap::new(),
        knowledge: Vec::new(),
        verification: VerificationSnapshot {
            criteria_results: HashMap::new(),
            overall_passed: false,
        },
        findings: Vec::new(),
        files_modified: Vec::new(),
        custom_data: HashMap::new(),
    };

    // Also save to in-memory manager for real-time operations
    let id = {
        let mut manager = CHECKPOINT_MANAGER.lock()?;
        let mut checkpoint =
            Checkpoint::new(&task_id, snapshot.clone()).with_trigger(CheckpointTrigger::Manual);

        if let Some(ref n) = name {
            checkpoint = checkpoint.with_name(n.clone());
        }
        if let Some(ref d) = description {
            checkpoint = checkpoint.with_description(d.clone());
        }

        manager.save(checkpoint)
    }; // MutexGuard dropped here before .await

    // Persist to database
    let state_json = serde_json::to_value(&snapshot).unwrap_or(serde_json::json!(state));
    app_state
        .pg_db()
        .save_orchestrator_checkpoint(
            &id,
            &task_id,
            iteration,
            "Manual",
            &state_json,
            name.as_deref(),
        )
        .await
        .map_err(AppError::Raw)?;

    Ok(id)
}

/// Delete a checkpoint by ID.
#[tauri::command]
pub async fn delete_orchestrator_checkpoint(
    state: State<'_, StorageCompartment>,
    id: String,
) -> Result<bool, String> {
    delete_orchestrator_checkpoint_impl(state, id)
        .await
        .map_err(String::from)
}

async fn delete_orchestrator_checkpoint_impl(
    state: State<'_, StorageCompartment>,
    id: String,
) -> Result<bool, AppError> {
    // Delete from in-memory manager
    {
        let mut manager = CHECKPOINT_MANAGER.lock()?;
        manager.delete(&id);
    } // MutexGuard dropped before .await

    // Delete from database
    state
        .pg_db()
        .delete_orchestrator_checkpoint(&id)
        .await
        .map_err(AppError::Raw)
}

/// Find checkpoints by tag.
#[tauri::command]
pub fn find_checkpoints_by_tag(tag: String) -> Result<Vec<CheckpointSummary>, String> {
    find_checkpoints_by_tag_impl(tag).map_err(String::from)
}

fn find_checkpoints_by_tag_impl(tag: String) -> Result<Vec<CheckpointSummary>, AppError> {
    let manager = CHECKPOINT_MANAGER.lock()?;
    Ok(manager.find_by_tag(&tag))
}

/// Compare two checkpoints and return the diff.
#[tauri::command]
pub fn compare_orchestrator_checkpoints(
    from_id: String,
    to_id: String,
) -> Result<CheckpointDiff, String> {
    compare_orchestrator_checkpoints_impl(from_id, to_id).map_err(String::from)
}

fn compare_orchestrator_checkpoints_impl(
    from_id: String,
    to_id: String,
) -> Result<CheckpointDiff, AppError> {
    let manager = CHECKPOINT_MANAGER.lock()?;

    let from_checkpoint = manager
        .load(&from_id)
        .ok_or_else(|| AppError::Raw(format!("Checkpoint '{}' not found", from_id)))?;
    let to_checkpoint = manager
        .load(&to_id)
        .ok_or_else(|| AppError::Raw(format!("Checkpoint '{}' not found", to_id)))?;

    Ok(CheckpointDiff::compute(from_checkpoint, to_checkpoint))
}

/// Get the latest checkpoint for a task.
#[tauri::command]
pub fn get_latest_checkpoint(task_id: String) -> Result<Option<Checkpoint>, String> {
    get_latest_checkpoint_impl(task_id).map_err(String::from)
}

fn get_latest_checkpoint_impl(task_id: String) -> Result<Option<Checkpoint>, AppError> {
    let manager = CHECKPOINT_MANAGER.lock()?;
    Ok(manager.latest_for_task(&task_id).cloned())
}

/// Start a replay session from a checkpoint.
#[tauri::command]
pub fn start_replay_session(checkpoint_id: String) -> Result<ReplaySession, String> {
    start_replay_session_impl(checkpoint_id).map_err(String::from)
}

fn start_replay_session_impl(checkpoint_id: String) -> Result<ReplaySession, AppError> {
    let manager = CHECKPOINT_MANAGER.lock()?;

    let checkpoint = manager
        .load(&checkpoint_id)
        .ok_or_else(|| AppError::Raw(format!("Checkpoint '{}' not found", checkpoint_id)))?;

    Ok(ReplaySession::from_checkpoint(checkpoint))
}

/// Get total checkpoint count.
#[tauri::command]
pub async fn get_checkpoint_count(state: State<'_, StorageCompartment>) -> Result<usize, String> {
    let checkpoints = state.pg_db().get_orchestrator_checkpoints(None).await?;
    Ok(checkpoints.len())
}

/// Get unique task IDs that have checkpoints.
#[tauri::command]
pub async fn get_checkpoint_task_ids(
    state: State<'_, StorageCompartment>,
) -> Result<Vec<String>, String> {
    state.pg_db().get_checkpoint_task_ids().await
}

/// Clear all checkpoints (for testing/reset).
#[tauri::command]
pub fn clear_all_checkpoints() -> Result<(), String> {
    clear_all_checkpoints_impl().map_err(String::from)
}

fn clear_all_checkpoints_impl() -> Result<(), AppError> {
    let mut manager = CHECKPOINT_MANAGER.lock()?;
    *manager = CheckpointManager::new().with_max_per_task(50);
    Ok(())
}

/// Add sample checkpoints for demonstration.
#[tauri::command]
pub async fn add_sample_checkpoints(app_state: State<'_, StorageCompartment>) -> Result<(), String> {
    add_sample_checkpoints_impl(app_state)
        .await
        .map_err(String::from)
}

async fn add_sample_checkpoints_impl(
    app_state: State<'_, StorageCompartment>,
) -> Result<(), AppError> {
    let task_id = "sample-task-001";

    // Collect items to save to PG after releasing the mutex
    let mut pg_saves: Vec<(String, u32, String, serde_json::Value, Option<&'static str>)> =
        Vec::new();

    // Save to in-memory manager (sync, mutex held briefly)
    {
        let mut manager = CHECKPOINT_MANAGER.lock()?;

        for i in 1..=5 {
            let state = match i {
                1 => "Planning",
                2 => "Executing",
                3 => "Executing",
                4 => "Verifying",
                5 => "Completed",
                _ => "Unknown",
            };

            let trigger = match i {
                1 => CheckpointTrigger::IterationBoundary { iteration: i },
                3 => CheckpointTrigger::Manual,
                4 => CheckpointTrigger::VerificationBoundary,
                5 => CheckpointTrigger::AfterSuccess {
                    operation: "Task completed".to_string(),
                },
                _ => CheckpointTrigger::Automatic {
                    reason: "Auto checkpoint".to_string(),
                },
            };

            let trigger_str = match i {
                1 => format!("IterationBoundary({})", i),
                3 => "Manual".to_string(),
                4 => "VerificationBoundary".to_string(),
                5 => "AfterSuccess".to_string(),
                _ => "Automatic".to_string(),
            };

            let snapshot = StateSnapshot {
                state: state.to_string(),
                iteration: i,
                channels: HashMap::new(),
                knowledge: (0..i)
                    .map(|j| crate::orchestrator::checkpoint::KnowledgeEntry {
                        id: format!("knowledge-{}", j),
                        category: "finding".to_string(),
                        content: format!("Knowledge item {}", j),
                        iteration: j,
                    })
                    .collect(),
                verification: VerificationSnapshot {
                    criteria_results: HashMap::new(),
                    overall_passed: i >= 4,
                },
                findings: if i >= 2 {
                    vec![crate::orchestrator::checkpoint::FindingSnapshot {
                        id: "finding-1".to_string(),
                        category: "bug".to_string(),
                        severity: "medium".to_string(),
                        description: "Potential null pointer".to_string(),
                        resolved: i >= 4,
                    }]
                } else {
                    vec![]
                },
                files_modified: if i >= 3 {
                    vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]
                } else {
                    vec![]
                },
                custom_data: HashMap::new(),
            };

            let name: Option<&'static str> = if i == 3 {
                Some("Before risky change")
            } else {
                None
            };

            let mut checkpoint = Checkpoint::new(task_id, snapshot.clone())
                .with_trigger(trigger)
                .with_tag("sample");

            if i == 3 {
                checkpoint = checkpoint
                    .with_name("Before risky change")
                    .with_description("Saving state before attempting database migration");
            }

            let id = manager.save(checkpoint);
            let state_json = serde_json::to_value(&snapshot).unwrap_or(serde_json::json!(state));
            pg_saves.push((id, i, trigger_str, state_json, name));
        }
    } // MutexGuard dropped here

    // Now persist to PG (async, no mutex held)
    for (id, i, trigger_str, state_json, name) in &pg_saves {
        let _ = app_state
            .pg_db()
            .save_orchestrator_checkpoint(id, task_id, *i, trigger_str, state_json, *name)
            .await;
    }

    Ok(())
}

/// Checkpoint browser statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointStats {
    pub total_checkpoints: usize,
    pub tasks_with_checkpoints: usize,
    pub checkpoints_by_trigger: HashMap<String, usize>,
}

/// Get checkpoint statistics.
#[tauri::command]
pub async fn get_checkpoint_stats(
    state: State<'_, StorageCompartment>,
) -> Result<CheckpointStats, String> {
    let checkpoints = state.pg_db().get_orchestrator_checkpoints(None).await?;
    let task_ids = state.pg_db().get_checkpoint_task_ids().await?;

    let mut by_trigger: HashMap<String, usize> = HashMap::new();
    for cp in &checkpoints {
        let trigger = cp["trigger"].as_str().unwrap_or("Unknown").to_string();
        *by_trigger.entry(trigger).or_insert(0) += 1;
    }

    Ok(CheckpointStats {
        total_checkpoints: checkpoints.len(),
        tasks_with_checkpoints: task_ids.len(),
        checkpoints_by_trigger: by_trigger,
    })
}

// ============================================================================
// Replay Commands
// ============================================================================

/// Response from replay_from_checkpoint command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayFromCheckpointResponse {
    /// The replay session.
    pub session: ReplaySession,
    /// Lineage information for the new task.
    pub lineage: LineageInfo,
    /// The new task run ID.
    pub new_task_run_id: String,
    /// Instructions for restoring state.
    pub restoration_instructions: RestorationInstructions,
}

/// Start a replay from a checkpoint.
///
/// This creates a new task run that branches from the checkpoint's state.
/// The new task run has its own ID but maintains lineage to the original.
///
/// # Arguments
/// * `checkpoint_id` - The ID of the checkpoint to replay from.
///
/// # Returns
/// A ReplayFromCheckpointResponse containing the session, lineage, and restoration info.
#[tauri::command]
pub async fn replay_from_checkpoint(
    app_state: State<'_, StorageCompartment>,
    checkpoint_id: String,
) -> Result<ReplayFromCheckpointResponse, String> {
    replay_from_checkpoint_impl(app_state, checkpoint_id)
        .await
        .map_err(String::from)
}

async fn replay_from_checkpoint_impl(
    app_state: State<'_, StorageCompartment>,
    checkpoint_id: String,
) -> Result<ReplayFromCheckpointResponse, AppError> {
    // Get the checkpoint from in-memory manager
    let checkpoint = {
        let manager = CHECKPOINT_MANAGER.lock()?;
        manager
            .load(&checkpoint_id)
            .ok_or_else(|| AppError::Raw(format!("Checkpoint '{}' not found", checkpoint_id)))?
            .clone()
    };

    // Start replay session
    let replay_result = {
        let mut replay_manager = REPLAY_MANAGER.lock()?;
        let manager = CHECKPOINT_MANAGER.lock()?;
        replay_manager.start_replay(&checkpoint, &manager)
    };

    let new_task_run_id = replay_result.session.replay_task_id.clone();

    // Create restoration instructions
    let restoration_config = StateRestorationConfig::full();
    let restoration_instructions =
        RestorationInstructions::from_checkpoint(&checkpoint, &restoration_config);

    // Create a new task run in the database (branched from checkpoint)
    let original_prompt = app_state
        .pg_db()
        .get_task_run(&checkpoint.task_id)
        .await
        .ok()
        .flatten()
        .and_then(|tr| tr.prompt);

    let replay_prompt = format!(
        "[Replayed from checkpoint {} at iteration {}]\n\n{}",
        checkpoint_id,
        checkpoint.state.iteration,
        original_prompt.unwrap_or_default()
    );

    let replay_task_name = format!(
        "Replay: {} (from iteration {})",
        checkpoint.task_id, checkpoint.state.iteration
    );

    // Create the new task run
    let input = CreateTaskRunInput::new(&new_task_run_id, &replay_task_name)
        .with_prompt(&replay_prompt)
        .with_auto_continue(true);
    app_state
        .pg_db()
        .create_task_run(&input)
        .await
        .map_err(AppError::Raw)?;

    // Save replay lineage to database
    let _lineage_json = serde_json::to_string(&replay_result.lineage)?;

    // Store lineage in task_runs custom field (runtime_context_json)
    let runtime_ctx = serde_json::json!({
        "replay_lineage": replay_result.lineage,
        "source_checkpoint_id": checkpoint_id,
        "restored_iteration": checkpoint.state.iteration,
        "restored_state": checkpoint.state.state,
    })
    .to_string();
    app_state
        .pg_db()
        .update_task_run_runtime_context(&new_task_run_id, &runtime_ctx)
        .await
        .map_err(AppError::Raw)?;

    Ok(ReplayFromCheckpointResponse {
        session: replay_result.session,
        lineage: replay_result.lineage,
        new_task_run_id,
        restoration_instructions,
    })
}

/// Get the replay lineage for a task run.
///
/// Returns the lineage tree showing the relationship between original tasks
/// and their replays.
///
/// # Arguments
/// * `task_run_id` - The task run ID to get lineage for.
///
/// # Returns
/// The lineage tree if the task has lineage info, or None if it's unknown.
#[tauri::command]
pub fn get_replay_lineage(task_run_id: String) -> Result<Option<LineageTree>, String> {
    get_replay_lineage_impl(task_run_id).map_err(String::from)
}

fn get_replay_lineage_impl(task_run_id: String) -> Result<Option<LineageTree>, AppError> {
    let replay_manager = REPLAY_MANAGER.lock()?;

    // Get lineage info
    let lineage = replay_manager.get_lineage(&task_run_id);

    if lineage.is_none() {
        return Ok(None);
    }

    // Get full lineage tree
    let tree = replay_manager.get_lineage_tree(&task_run_id);

    if tree.nodes.is_empty() {
        return Ok(None);
    }

    Ok(Some(tree))
}

/// Register an original (non-replayed) task for lineage tracking.
///
/// Call this when creating a new task run that isn't a replay.
/// This allows the task to be tracked as a potential parent for future replays.
///
/// # Arguments
/// * `task_run_id` - The task run ID to register.
#[tauri::command]
pub fn register_task_for_lineage(task_run_id: String) -> Result<(), String> {
    register_task_for_lineage_impl(task_run_id).map_err(String::from)
}

fn register_task_for_lineage_impl(task_run_id: String) -> Result<(), AppError> {
    let mut replay_manager = REPLAY_MANAGER.lock()?;
    replay_manager.register_original_task(task_run_id);
    Ok(())
}

/// Get lineage info for a specific task.
///
/// # Arguments
/// * `task_run_id` - The task run ID.
///
/// # Returns
/// The lineage info if available.
#[tauri::command]
pub fn get_task_lineage_info(task_run_id: String) -> Result<Option<LineageInfo>, String> {
    get_task_lineage_info_impl(task_run_id).map_err(String::from)
}

fn get_task_lineage_info_impl(task_run_id: String) -> Result<Option<LineageInfo>, AppError> {
    let replay_manager = REPLAY_MANAGER.lock()?;
    Ok(replay_manager.get_lineage(&task_run_id).cloned())
}

/// List all active replay sessions.
#[tauri::command]
pub fn list_active_replay_sessions() -> Result<Vec<ReplaySession>, String> {
    list_active_replay_sessions_impl().map_err(String::from)
}

fn list_active_replay_sessions_impl() -> Result<Vec<ReplaySession>, AppError> {
    let replay_manager = REPLAY_MANAGER.lock()?;
    Ok(replay_manager
        .list_active_sessions()
        .into_iter()
        .cloned()
        .collect())
}

/// Complete a replay session.
///
/// # Arguments
/// * `session_id` - The replay session ID.
#[tauri::command]
pub fn complete_replay_session(session_id: String) -> Result<(), String> {
    complete_replay_session_impl(session_id).map_err(String::from)
}

fn complete_replay_session_impl(session_id: String) -> Result<(), AppError> {
    let mut replay_manager = REPLAY_MANAGER.lock()?;
    replay_manager
        .complete_session(&session_id)
        .map_err(AppError::Raw)
}

/// Fail a replay session.
///
/// # Arguments
/// * `session_id` - The replay session ID.
/// * `error` - Optional error message.
#[tauri::command]
pub fn fail_replay_session(session_id: String, _error: Option<String>) -> Result<(), String> {
    fail_replay_session_impl(session_id, _error).map_err(String::from)
}

fn fail_replay_session_impl(
    session_id: String,
    _error: Option<String>,
) -> Result<(), AppError> {
    let mut replay_manager = REPLAY_MANAGER.lock()?;
    replay_manager
        .fail_session(&session_id)
        .map_err(AppError::Raw)
}

// ============================================================================
// Enhanced Checkpoint Queries (Filtering, Pagination)
// ============================================================================

/// Filter options for checkpoint query.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointFilter {
    pub task_id: Option<String>,
    pub trigger: Option<String>,
    pub since: Option<String>,
}

/// Paginated checkpoint result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedCheckpointResult {
    pub items: Vec<CheckpointSummary>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// Get checkpoints with optional filtering.
#[tauri::command]
pub async fn get_checkpoints_filtered(
    state: State<'_, StorageCompartment>,
    filter: CheckpointFilter,
) -> Result<Vec<CheckpointSummary>, String> {
    let checkpoints_json = state
        .pg_db()
        .get_checkpoints_filtered(
            filter.task_id.as_deref(),
            filter.trigger.as_deref(),
            filter.since.as_deref(),
        )
        .await?;

    let summaries: Vec<CheckpointSummary> = checkpoints_json
        .into_iter()
        .map(|cp| CheckpointSummary {
            id: cp["id"].as_str().unwrap_or("").to_string(),
            task_id: cp["task_id"].as_str().unwrap_or("").to_string(),
            iteration: cp["iteration"].as_i64().unwrap_or(0) as u32,
            trigger_type: cp["trigger"].as_str().unwrap_or("Manual").to_string(),
            name: cp["name"].as_str().map(|s| s.to_string()),
            created_at: cp["created_at"].as_str().unwrap_or("").to_string(),
            state: cp["state"].as_str().unwrap_or("Unknown").to_string(),
            tags: cp["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();

    Ok(summaries)
}

/// Get checkpoints with pagination.
#[tauri::command]
pub async fn get_checkpoints_paginated(
    state: State<'_, StorageCompartment>,
    task_id: Option<String>,
    offset: i64,
    limit: i64,
) -> Result<PaginatedCheckpointResult, String> {
    let checkpoints_json = state
        .pg_db()
        .get_checkpoints_paginated(task_id.as_deref(), offset, limit)
        .await?;

    let summaries: Vec<CheckpointSummary> = checkpoints_json
        .into_iter()
        .map(|cp| CheckpointSummary {
            id: cp["id"].as_str().unwrap_or("").to_string(),
            task_id: cp["task_id"].as_str().unwrap_or("").to_string(),
            iteration: cp["iteration"].as_i64().unwrap_or(0) as u32,
            trigger_type: cp["trigger"].as_str().unwrap_or("Manual").to_string(),
            name: cp["name"].as_str().map(|s| s.to_string()),
            created_at: cp["created_at"].as_str().unwrap_or("").to_string(),
            state: cp["state"].as_str().unwrap_or("Unknown").to_string(),
            tags: cp["tags"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();

    let total = state
        .pg_db()
        .get_checkpoints_count(task_id.as_deref())
        .await?;

    Ok(PaginatedCheckpointResult {
        items: summaries,
        total,
        offset,
        limit,
    })
}

/// Get total count of checkpoints.
#[tauri::command]
pub async fn get_checkpoints_count(
    state: State<'_, StorageCompartment>,
    task_id: Option<String>,
) -> Result<i64, String> {
    state.pg_db().get_checkpoints_count(task_id.as_deref()).await
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_checkpoint_browser")
        .invoke_handler(tauri::generate_handler![
            list_orchestrator_checkpoints,
            get_orchestrator_checkpoint,
            create_orchestrator_checkpoint,
            delete_orchestrator_checkpoint,
            find_checkpoints_by_tag,
            compare_orchestrator_checkpoints,
            get_latest_checkpoint,
            start_replay_session,
            get_checkpoint_count,
            get_checkpoint_task_ids,
            clear_all_checkpoints,
            add_sample_checkpoints,
            get_checkpoint_stats,
            replay_from_checkpoint,
            get_replay_lineage,
            register_task_for_lineage,
            get_task_lineage_info,
            list_active_replay_sessions,
            complete_replay_session,
            fail_replay_session,
            get_checkpoints_filtered,
            get_checkpoints_paginated,
            get_checkpoints_count,
        ])
        .build()
}
