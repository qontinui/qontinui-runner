//! Tauri commands for the Tiered Information Model.
//!
//! These commands expose the tiered information functionality to the frontend
//! and external APIs for debugging context.

use crate::auth::AuthManager;
use crate::commands::AppState;
use crate::discoveries::{
    analyze_run_for_discoveries, queue_discovery, DetectionContext, DetectionThresholds,
};
use crate::executor::flakiness::{ExecutionOptions, FlakinessCache};
use crate::tiered_info::{
    self, ConfigStatistics, DebuggingContext, FlakyItem, RunDetails, RunStatus,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{debug, info, warn};

/// Response for tiered info commands.
#[derive(Debug, Serialize)]
pub struct TieredInfoResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> TieredInfoResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

/// Get config statistics (Tier 4 summary).
///
/// Note: Uses SQLite because config_statistics is a complex aggregation table
/// tightly coupled to the tiered_info module's internal data model.
#[tauri::command]
pub async fn get_config_statistics(
    state: State<'_, Arc<AppState>>,
    config_id: String,
) -> Result<TieredInfoResponse<ConfigStatistics>, String> {
    match state.pg_db.get_config_statistics(&config_id).await {
        Ok(Some(stats_val)) => {
            match serde_json::from_value(stats_val) {
                Ok(stats) => Ok(TieredInfoResponse::ok(stats)),
                Err(e) => Ok(TieredInfoResponse::err(format!("Deserialization error: {}", e))),
            }
        }
        Ok(None) => Ok(TieredInfoResponse::err(format!(
            "No statistics found for config {}",
            config_id
        ))),
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get flaky transitions for a config.
#[tauri::command]
pub async fn get_flaky_transitions(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    threshold: Option<f64>,
) -> Result<TieredInfoResponse<Vec<FlakyItem>>, String> {
    let threshold = threshold.unwrap_or(0.2);
    match state.pg_db.get_flaky_transitions(&config_id, threshold).await {
        Ok(items) => {
            let typed: Vec<FlakyItem> = items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
            Ok(TieredInfoResponse::ok(typed))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get flaky templates for a config.
#[tauri::command]
pub async fn get_flaky_templates(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    threshold: Option<f64>,
) -> Result<TieredInfoResponse<Vec<FlakyItem>>, String> {
    let threshold = threshold.unwrap_or(0.2);
    match state.pg_db.get_flaky_templates(&config_id, threshold).await {
        Ok(items) => {
            let typed: Vec<FlakyItem> = items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
            Ok(TieredInfoResponse::ok(typed))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get debugging context for AI prompts.
#[tauri::command]
pub async fn get_debugging_context(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    config_name: Option<String>,
) -> Result<TieredInfoResponse<DebuggingContext>, String> {
    match state.pg_db.get_debugging_context(&config_id, config_name).await {
        Ok(val) => match serde_json::from_value(val) {
            Ok(context) => Ok(TieredInfoResponse::ok(context)),
            Err(e) => Ok(TieredInfoResponse::err(format!("Deserialization error: {}", e))),
        },
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get debugging context formatted as markdown for AI prompts.
#[tauri::command]
pub async fn get_debugging_context_prompt(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    config_name: Option<String>,
) -> Result<TieredInfoResponse<String>, String> {
    match state.pg_db.get_debugging_context(&config_id, config_name).await {
        Ok(val) => {
            // Format as markdown string from the JSON context
            let formatted = serde_json::to_string_pretty(&val).unwrap_or_default();
            Ok(TieredInfoResponse::ok(formatted))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get automation run details from task_run_automation table.
#[tauri::command]
pub async fn get_run_details(
    state: State<'_, Arc<AppState>>,
    run_id: String,
) -> Result<TieredInfoResponse<RunDetails>, String> {
    // Query from task_run_automation table (run_details table removed)
    match state.pg_db.get_task_run_automation_by_id(&run_id).await {
        Ok(Some(automation)) => {
            // Convert TaskRunAutomation to RunDetails for backward compatibility
            let run = RunDetails {
                id: automation.id,
                config_id: String::new(), // Would need to join with task_runs to get config_id
                workflow_name: automation.workflow_name,
                started_at: automation.started_at,
                ended_at: automation.ended_at,
                duration_ms: automation.duration_ms.map(|d| d as u64),
                status: RunStatus::from_str(&automation.automation_status)
                    .unwrap_or(RunStatus::Running),
                success: automation.success,
                error_type: automation.error_type,
                error_message: automation.error_message,
                actions_summary: automation
                    .actions_summary
                    .and_then(|s| serde_json::from_str(&s).ok()),
                states_visited: automation
                    .states_visited
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                transitions_executed: automation
                    .transitions_executed
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                template_matches: automation
                    .template_matches
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                anomalies: automation
                    .anomalies
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                screenshots: Vec::new(),
            };
            Ok(TieredInfoResponse::ok(run))
        }
        Ok(None) => Ok(TieredInfoResponse::err(format!(
            "Run details not found for {}",
            run_id
        ))),
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get recent runs, optionally filtered by config.
#[tauri::command]
pub async fn get_recent_runs(
    state: State<'_, Arc<AppState>>,
    config_id: Option<String>,
    limit: Option<u32>,
) -> Result<TieredInfoResponse<Vec<RunDetails>>, String> {
    let limit = limit.unwrap_or(10);
    match state.pg_db.get_recent_runs(config_id.as_deref(), limit).await {
        Ok(vals) => {
            let runs: Vec<RunDetails> = vals.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
            Ok(TieredInfoResponse::ok(runs))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get failed runs for a config (for debugging).
#[tauri::command]
pub async fn get_failed_runs(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    limit: Option<u32>,
) -> Result<TieredInfoResponse<Vec<RunDetails>>, String> {
    let limit = limit.unwrap_or(5);
    match state.pg_db.get_failed_runs(&config_id, limit).await {
        Ok(vals) => {
            let runs: Vec<RunDetails> = vals.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
            Ok(TieredInfoResponse::ok(runs))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Input for recording a run.
#[derive(Debug, Deserialize)]
pub struct RecordRunInput {
    pub config_id: String,
    /// Project ID (required for discovery push)
    pub project_id: Option<String>,
    /// Config name for discovery context
    pub config_name: Option<String>,
    pub workflow_name: Option<String>,
    pub status: String, // "completed", "failed", "timeout", "cancelled"
    pub success: Option<bool>,
    pub duration_ms: Option<u64>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub actions_summary: Option<serde_json::Value>,
    pub states_visited: Option<Vec<String>>,
    pub transitions_executed: Option<Vec<serde_json::Value>>,
    pub template_matches: Option<Vec<serde_json::Value>>,
    pub anomalies: Option<Vec<serde_json::Value>>,
}

/// Record a completed run and update statistics.
/// This is the main entry point for recording automation runs.
/// After recording, analyzes for discoveries and queues them for sync.
#[tauri::command]
pub async fn record_run(
    state: State<'_, Arc<AppState>>,
    input: RecordRunInput,
) -> Result<TieredInfoResponse<String>, String> {
    // Parse status
    let status = match RunStatus::from_str(&input.status) {
        Some(s) => s,
        None => {
            return Ok(TieredInfoResponse::err(format!(
                "Invalid status: {}",
                input.status
            )))
        }
    };

    // Build RunDetails
    let mut run = RunDetails::new(
        uuid::Uuid::new_v4().to_string(),
        input.config_id.clone(),
        input.workflow_name.clone(),
    );
    run.status = status;
    run.success = input.success;
    run.duration_ms = input.duration_ms;
    run.ended_at = Some(chrono::Utc::now().to_rfc3339());
    run.error_type = input.error_type;
    run.error_message = input.error_message;

    // Parse complex fields
    if let Some(actions) = input.actions_summary {
        run.actions_summary = serde_json::from_value(actions).ok();
    }
    if let Some(states) = input.states_visited {
        run.states_visited = states;
    }
    if let Some(transitions) = input.transitions_executed {
        run.transitions_executed = transitions
            .into_iter()
            .filter_map(|t| serde_json::from_value(t).ok())
            .collect();
    }
    if let Some(templates) = input.template_matches {
        run.template_matches = templates
            .into_iter()
            .filter_map(|t| serde_json::from_value(t).ok())
            .collect();
    }
    if let Some(anomalies) = input.anomalies {
        run.anomalies = anomalies
            .into_iter()
            .filter_map(|a| serde_json::from_value(a).ok())
            .collect();
    }

    // Note: The run_details table has been removed. Run recording now goes through TaskRecorder.
    // This command now only updates statistics for backward compatibility.
    // For full run recording, use the executor flow which creates task_runs + task_run_automation.
    warn!(
        "record_run command called directly - only updating statistics (use TaskRecorder for full recording)"
    );

    // Update statistics
    let config_id_for_stats = input.config_id.clone();
    let project_id = input.project_id.clone();
    let config_name = input.config_name.clone();
    let run_id = run.id.clone();

    let success = run.success.unwrap_or(false);
    let duration = run.duration_ms;
    if let Err(e) = state.pg_db.update_statistics_after_run(&input.config_id, success, duration).await {
        return Ok(TieredInfoResponse::err(format!("Run recorded but stats update failed: {}", e)));
    }

    Ok(TieredInfoResponse::ok(run_id))
}

/// Delete old automation records to manage storage (keeps most recent N per config).
/// Note: This only cleans up task_run_automation records, not the parent task_runs.
#[tauri::command]
pub async fn cleanup_old_runs(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    keep_count: Option<u32>,
) -> Result<TieredInfoResponse<u32>, String> {
    let keep_count = keep_count.unwrap_or(100);

    match state.pg_db.cleanup_old_runs(&config_id, keep_count).await {
        Ok(deleted) => {
            info!(
                "Cleaned up {} old automation records for config {}",
                deleted, config_id
            );
            Ok(TieredInfoResponse::ok(deleted))
        }
        Err(e) => Ok(TieredInfoResponse::err(format!(
            "Failed to cleanup old runs: {}",
            e
        ))),
    }
}

/// Get flakiness-aware execution options for a transition or template.
///
/// This command returns adjusted execution options (retry count, timeout multiplier,
/// confidence threshold) based on historical flakiness data.
///
/// # Arguments
/// * `config_id` - Configuration ID to load flakiness data for
/// * `transition_id` - Optional transition ID (format: "FromState|ToState")
/// * `template_id` - Optional template/image ID
///
/// If both transition_id and template_id are provided, transition options take precedence.
/// If neither is provided, returns default execution options.
#[tauri::command]
pub async fn get_execution_options(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    transition_id: Option<String>,
    template_id: Option<String>,
) -> Result<TieredInfoResponse<ExecutionOptions>, String> {
    match state.pg_db.get_execution_options(
        &config_id, transition_id.as_deref(), template_id.as_deref(), 0.2,
    ).await {
        Ok(val) => match serde_json::from_value(val) {
            Ok(options) => Ok(TieredInfoResponse::ok(options)),
            Err(_) => Ok(TieredInfoResponse::ok(ExecutionOptions::default())),
        },
        Err(e) => {
            warn!("Failed to load execution options: {}", e);
            Ok(TieredInfoResponse::ok(ExecutionOptions::default()))
        }
    }
}

/// Response containing flakiness summary for a configuration.
#[derive(Debug, Serialize, Deserialize)]
pub struct FlakinessSummary {
    /// Number of flaky transitions
    pub flaky_transition_count: usize,
    /// Number of flaky templates
    pub flaky_template_count: usize,
    /// List of flaky transition IDs
    pub flaky_transition_ids: Vec<String>,
    /// List of flaky template IDs
    pub flaky_template_ids: Vec<String>,
    /// Whether the cache is stale (older than 5 minutes)
    pub cache_is_stale: bool,
}

/// Get a summary of all flaky items for a configuration.
///
/// This is useful for the Python bridge to preload flakiness data
/// before starting execution.
#[tauri::command]
pub async fn get_flakiness_summary(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    threshold: Option<f64>,
) -> Result<TieredInfoResponse<FlakinessSummary>, String> {
    let threshold = threshold.unwrap_or(0.2);

    match state.pg_db.get_flakiness_summary(&config_id, threshold).await {
        Ok(val) => {
            let summary: FlakinessSummary = serde_json::from_value(val)
                .unwrap_or(FlakinessSummary {
                    flaky_transition_count: 0,
                    flaky_template_count: 0,
                    flaky_transition_ids: vec![],
                    flaky_template_ids: vec![],
                    cache_is_stale: false,
                });

            info!(
                "Flakiness summary for config {}: {} transitions, {} templates",
                config_id, summary.flaky_transition_count, summary.flaky_template_count
            );

            Ok(TieredInfoResponse::ok(summary))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

// =============================================================================
// AI Session History Commands (for Runs page)
// =============================================================================

/// AI session run data for the Runs page.
/// Matches the AiSessionRun TypeScript interface in the frontend.
#[derive(Debug, Serialize)]
pub struct AiSessionRun {
    pub id: String,
    pub name: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub status: String,
    #[serde(rename = "loopCount")]
    pub loop_count: u32,
    #[serde(rename = "taskType", skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
}

/// Get AI session history for the Runs page.
///
/// # Arguments
/// * `config_id` - Optional config ID to filter by
/// * `limit` - Maximum number of runs to return (default: 50)
#[tauri::command]
pub async fn get_ai_session_history(
    state: State<'_, Arc<AppState>>,
    config_id: Option<String>,
    limit: Option<u32>,
) -> Result<TieredInfoResponse<Vec<AiSessionRun>>, String> {
    let limit = limit.unwrap_or(50);

    let pg_rows = state
        .pg_db
        .get_ai_session_history(config_id.as_deref(), limit)
        .await?;

    let runs: Vec<AiSessionRun> = pg_rows
        .into_iter()
        .map(|row| {
            let status = row["status"].as_str().unwrap_or("unknown");
            AiSessionRun {
                id: row["id"].as_str().unwrap_or("").to_string(),
                name: row["task_name"].as_str().unwrap_or("").to_string(),
                started_at: row["created_at"].as_str().unwrap_or("").to_string(),
                ended_at: row["completed_at"].as_str().map(|s| s.to_string()),
                status: map_status_to_frontend(status),
                loop_count: row["sessions_count"].as_i64().unwrap_or(0) as u32,
                task_type: row["task_type"].as_str().map(|s| s.to_string()),
            }
        })
        .collect();

    debug!(
        "Retrieved {} AI session runs (config_id: {:?})",
        runs.len(),
        config_id
    );
    Ok(TieredInfoResponse::ok(runs))
}

/// Map internal status to frontend-expected status values.
/// Internal: 'running', 'complete', 'failed', 'stopped'
/// Frontend: 'running', 'completed', 'failed', 'cancelled'
fn map_status_to_frontend(status: &str) -> String {
    match status {
        "complete" => "completed".to_string(),
        "stopped" => "cancelled".to_string(),
        other => other.to_string(),
    }
}

/// Delete an AI session (task run) by ID.
///
/// This will cascade delete all related records including:
/// - task_run_output_chunks
/// - task_run_findings
/// - task_run_automation
/// - task_run_events
/// - task_run_screenshots
/// - task_run_playwright_results
/// - task_knowledge
/// - verification_plans
/// - orchestrator_verification_results
#[tauri::command]
pub async fn delete_ai_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<TieredInfoResponse<bool>, String> {
    match state.pg_db.delete_task_run(&session_id).await {
        Ok(deleted) => {
            if deleted {
                info!("Deleted AI session: {}", session_id);
                Ok(TieredInfoResponse::ok(true))
            } else {
                warn!("AI session not found for deletion: {}", session_id);
                Ok(TieredInfoResponse::err(format!(
                    "Session not found: {}",
                    session_id
                )))
            }
        }
        Err(e) => {
            warn!("Failed to delete AI session {}: {}", session_id, e);
            Ok(TieredInfoResponse::err(e))
        }
    }
}
