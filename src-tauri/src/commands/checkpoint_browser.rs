//! Tauri commands for the Checkpoint Browser (Time-Travel Debugging).
//!
//! Provides access to orchestrator checkpoints for browsing execution history,
//! comparing states, and initiating replay sessions.
//!
//! Uses SQLite database for persistence with in-memory CheckpointManager for
//! real-time operations like replay.

use crate::commands::AppState;
use crate::database::CreateTaskRunInput;
use crate::orchestrator::checkpoint::{
    Checkpoint, CheckpointDiff, CheckpointManager, CheckpointSummary, CheckpointTrigger,
    LineageInfo, LineageTree, ReplayManager, ReplaySession, RestorationInstructions,
    StateRestorationConfig, StateSnapshot, VerificationSnapshot,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::State;

/// Global checkpoint manager instance for in-memory operations (replay, comparison).
/// The database stores checkpoints persistently; this provides real-time operations.
static CHECKPOINT_MANAGER: Lazy<Mutex<CheckpointManager>> =
    Lazy::new(|| Mutex::new(CheckpointManager::new().with_max_per_task(50)));

/// Global replay manager instance for managing replay sessions and lineage.
static REPLAY_MANAGER: Lazy<Mutex<ReplayManager>> = Lazy::new(|| Mutex::new(ReplayManager::new()));

/// List all checkpoints, optionally filtered by task ID.
#[tauri::command]
pub fn list_orchestrator_checkpoints(
    state: State<'_, Arc<AppState>>,
    task_id: Option<String>,
) -> Result<Vec<CheckpointSummary>, String> {
    let checkpoints_json = state
        .checkpoint_db
        .get_orchestrator_checkpoints(task_id.as_deref())?;

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
pub fn get_orchestrator_checkpoint(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<Option<Checkpoint>, String> {
    let checkpoint_json = state.checkpoint_db.get_orchestrator_checkpoint(&id)?;

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
pub fn create_orchestrator_checkpoint(
    app_state: State<'_, Arc<AppState>>,
    task_id: String,
    name: Option<String>,
    description: Option<String>,
    state: String,
    iteration: u32,
) -> Result<String, String> {
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
    let mut manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;
    let mut checkpoint =
        Checkpoint::new(&task_id, snapshot.clone()).with_trigger(CheckpointTrigger::Manual);

    if let Some(ref n) = name {
        checkpoint = checkpoint.with_name(n.clone());
    }
    if let Some(ref d) = description {
        checkpoint = checkpoint.with_description(d.clone());
    }

    let id = manager.save(checkpoint);

    // Persist to database
    let state_json = serde_json::to_value(&snapshot).unwrap_or(serde_json::json!(state));
    app_state.checkpoint_db.save_orchestrator_checkpoint(
        &id,
        &task_id,
        iteration,
        "Manual",
        &state_json,
        name.as_deref(),
    )?;

    Ok(id)
}

/// Delete a checkpoint by ID.
#[tauri::command]
pub fn delete_orchestrator_checkpoint(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<bool, String> {
    // Delete from in-memory manager
    let mut manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;
    manager.delete(&id);

    // Delete from database
    state.checkpoint_db.delete_orchestrator_checkpoint(&id)
}

/// Find checkpoints by tag.
#[tauri::command]
pub fn find_checkpoints_by_tag(tag: String) -> Result<Vec<CheckpointSummary>, String> {
    let manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;
    Ok(manager.find_by_tag(&tag))
}

/// Compare two checkpoints and return the diff.
#[tauri::command]
pub fn compare_orchestrator_checkpoints(
    from_id: String,
    to_id: String,
) -> Result<CheckpointDiff, String> {
    let manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;

    let from_checkpoint = manager
        .load(&from_id)
        .ok_or_else(|| format!("Checkpoint '{}' not found", from_id))?;
    let to_checkpoint = manager
        .load(&to_id)
        .ok_or_else(|| format!("Checkpoint '{}' not found", to_id))?;

    Ok(CheckpointDiff::compute(from_checkpoint, to_checkpoint))
}

/// Get the latest checkpoint for a task.
#[tauri::command]
pub fn get_latest_checkpoint(task_id: String) -> Result<Option<Checkpoint>, String> {
    let manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;
    Ok(manager.latest_for_task(&task_id).cloned())
}

/// Start a replay session from a checkpoint.
#[tauri::command]
pub fn start_replay_session(checkpoint_id: String) -> Result<ReplaySession, String> {
    let manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;

    let checkpoint = manager
        .load(&checkpoint_id)
        .ok_or_else(|| format!("Checkpoint '{}' not found", checkpoint_id))?;

    Ok(ReplaySession::from_checkpoint(checkpoint))
}

/// Get total checkpoint count.
#[tauri::command]
pub fn get_checkpoint_count(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let checkpoints = state.checkpoint_db.get_orchestrator_checkpoints(None)?;
    Ok(checkpoints.len())
}

/// Get unique task IDs that have checkpoints.
#[tauri::command]
pub fn get_checkpoint_task_ids(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    state.checkpoint_db.get_checkpoint_task_ids()
}

/// Clear all checkpoints (for testing/reset).
#[tauri::command]
pub fn clear_all_checkpoints() -> Result<(), String> {
    let mut manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;
    *manager = CheckpointManager::new().with_max_per_task(50);
    Ok(())
}

/// Add sample checkpoints for demonstration.
#[tauri::command]
pub fn add_sample_checkpoints(app_state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;

    let task_id = "sample-task-001";

    // Create checkpoints for different iterations
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

        let name = if i == 3 {
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

        // Also save to database
        let state_json = serde_json::to_value(&snapshot).unwrap_or(serde_json::json!(state));
        let _ = app_state.checkpoint_db.save_orchestrator_checkpoint(
            &id,
            task_id,
            i,
            &trigger_str,
            &state_json,
            name,
        );
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
pub fn get_checkpoint_stats(state: State<'_, Arc<AppState>>) -> Result<CheckpointStats, String> {
    let checkpoints = state.checkpoint_db.get_orchestrator_checkpoints(None)?;
    let task_ids = state.checkpoint_db.get_checkpoint_task_ids()?;

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
pub fn replay_from_checkpoint(
    app_state: State<'_, Arc<AppState>>,
    checkpoint_id: String,
) -> Result<ReplayFromCheckpointResponse, String> {
    // Get the checkpoint from in-memory manager
    let manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;
    let checkpoint = manager
        .load(&checkpoint_id)
        .ok_or_else(|| format!("Checkpoint '{}' not found", checkpoint_id))?
        .clone();
    drop(manager);

    // Start replay session
    let mut replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;
    let manager = CHECKPOINT_MANAGER.lock().map_err(|e| e.to_string())?;

    let replay_result = replay_manager.start_replay(&checkpoint, &manager);
    drop(manager);

    let new_task_run_id = replay_result.session.replay_task_id.clone();

    // Create restoration instructions
    let restoration_config = StateRestorationConfig::full();
    let restoration_instructions =
        RestorationInstructions::from_checkpoint(&checkpoint, &restoration_config);

    // Create a new task run in the database (branched from checkpoint)
    let original_prompt = app_state
        .checkpoint_db
        .get_task_run(&checkpoint.task_id)
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
    app_state.checkpoint_db.create_task_run(&input)?;

    // Save replay lineage to database
    let _lineage_json = serde_json::to_string(&replay_result.lineage)
        .map_err(|e| format!("Failed to serialize lineage: {}", e))?;

    // Store lineage in task_runs custom field (runtime_context_json)
    app_state.checkpoint_db.update_task_run_runtime_context(
        &new_task_run_id,
        &serde_json::json!({
            "replay_lineage": replay_result.lineage,
            "source_checkpoint_id": checkpoint_id,
            "restored_iteration": checkpoint.state.iteration,
            "restored_state": checkpoint.state.state,
        })
        .to_string(),
    )?;

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
    let replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;

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
    let mut replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;
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
    let replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;
    Ok(replay_manager.get_lineage(&task_run_id).cloned())
}

/// List all active replay sessions.
#[tauri::command]
pub fn list_active_replay_sessions() -> Result<Vec<ReplaySession>, String> {
    let replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;
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
    let mut replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;
    replay_manager.complete_session(&session_id)
}

/// Fail a replay session.
///
/// # Arguments
/// * `session_id` - The replay session ID.
/// * `error` - Optional error message.
#[tauri::command]
pub fn fail_replay_session(session_id: String, _error: Option<String>) -> Result<(), String> {
    let mut replay_manager = REPLAY_MANAGER.lock().map_err(|e| e.to_string())?;
    replay_manager.fail_session(&session_id)
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
pub fn get_checkpoints_filtered(
    state: State<'_, Arc<AppState>>,
    filter: CheckpointFilter,
) -> Result<Vec<CheckpointSummary>, String> {
    let checkpoints_json = state.checkpoint_db.get_checkpoints_filtered(
        filter.task_id.as_deref(),
        filter.trigger.as_deref(),
        filter.since.as_deref(),
    )?;

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
pub fn get_checkpoints_paginated(
    state: State<'_, Arc<AppState>>,
    task_id: Option<String>,
    offset: i64,
    limit: i64,
) -> Result<PaginatedCheckpointResult, String> {
    let checkpoints_json =
        state
            .checkpoint_db
            .get_checkpoints_paginated(task_id.as_deref(), offset, limit)?;

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
        .checkpoint_db
        .get_checkpoints_count(task_id.as_deref())?;

    Ok(PaginatedCheckpointResult {
        items: summaries,
        total,
        offset,
        limit,
    })
}

/// Get total count of checkpoints.
#[tauri::command]
pub fn get_checkpoints_count(
    state: State<'_, Arc<AppState>>,
    task_id: Option<String>,
) -> Result<i64, String> {
    state
        .checkpoint_db
        .get_checkpoints_count(task_id.as_deref())
}
