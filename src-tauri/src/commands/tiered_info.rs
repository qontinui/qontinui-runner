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
#[tauri::command]
pub async fn get_config_statistics(
    state: State<'_, Arc<AppState>>,
    config_id: String,
) -> Result<TieredInfoResponse<ConfigStatistics>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    match tiered_info::get_config_statistics(&conn, &config_id) {
        Ok(Some(stats)) => Ok(TieredInfoResponse::ok(stats)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let threshold = threshold.unwrap_or(0.2);

    match tiered_info::get_flaky_transitions(&conn, &config_id, threshold) {
        Ok(items) => Ok(TieredInfoResponse::ok(items)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let threshold = threshold.unwrap_or(0.2);

    match tiered_info::get_flaky_templates(&conn, &config_id, threshold) {
        Ok(items) => Ok(TieredInfoResponse::ok(items)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    match tiered_info::get_debugging_context(&conn, &config_id, config_name) {
        Ok(context) => Ok(TieredInfoResponse::ok(context)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    match tiered_info::get_debugging_context(&conn, &config_id, config_name) {
        Ok(context) => {
            let prompt = tiered_info::format_debugging_context_for_prompt(&context);
            Ok(TieredInfoResponse::ok(prompt))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get run details (Tier 1 detailed data).
#[tauri::command]
pub async fn get_run_details(
    state: State<'_, Arc<AppState>>,
    run_id: String,
) -> Result<TieredInfoResponse<RunDetails>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    match tiered_info::get_run_details(&conn, &run_id) {
        Ok(Some(run)) => Ok(TieredInfoResponse::ok(run)),
        Ok(None) => Ok(TieredInfoResponse::err(format!(
            "Run details not found for {}",
            run_id
        ))),
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}

/// Get recent runs for a config.
#[tauri::command]
pub async fn get_recent_runs(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    limit: Option<u32>,
) -> Result<TieredInfoResponse<Vec<RunDetails>>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let limit = limit.unwrap_or(10);

    match tiered_info::get_recent_runs(&conn, &config_id, limit) {
        Ok(runs) => Ok(TieredInfoResponse::ok(runs)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let limit = limit.unwrap_or(5);

    match tiered_info::get_failed_runs(&conn, &config_id, limit) {
        Ok(runs) => Ok(TieredInfoResponse::ok(runs)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

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

    // Insert run details
    if let Err(e) = tiered_info::insert_run_details(&conn, &run) {
        return Ok(TieredInfoResponse::err(e));
    }

    // Update statistics
    if let Err(e) = tiered_info::update_statistics_after_run(&conn, &run) {
        return Ok(TieredInfoResponse::err(format!(
            "Run recorded but stats update failed: {}",
            e
        )));
    }

    // Analyze for discoveries (only if project_id is provided)
    if let Some(project_id) = input.project_id {
        // Get updated statistics for analysis
        if let Ok(Some(stats)) = tiered_info::get_config_statistics(&conn, &input.config_id) {
            // Get runner info for discovery context
            let auth_manager = AuthManager::new();
            let runner_id = auth_manager
                .get_device_id()
                .unwrap_or_else(|_| "unknown".to_string());
            // Runner name is retrieved from settings, not AuthManager
            // For now, we'll use None and let it be populated later
            let runner_name: Option<String> = None;

            let context = DetectionContext {
                runner_id,
                runner_name,
                project_id,
                config_id: input.config_id.clone(),
                config_name: input.config_name,
            };

            let thresholds = DetectionThresholds::default();
            let discoveries = analyze_run_for_discoveries(&run, &stats, &context, &thresholds);

            if !discoveries.is_empty() {
                info!(
                    "Found {} discoveries from run {}, queueing for sync",
                    discoveries.len(),
                    run.id
                );

                for discovery in discoveries {
                    match queue_discovery(&conn, discovery) {
                        Ok(discovery_id) => {
                            debug!("Queued discovery {} for sync", discovery_id);
                        }
                        Err(e) => {
                            warn!("Failed to queue discovery: {}", e);
                        }
                    }
                }
            }
        }
    }

    Ok(TieredInfoResponse::ok(run.id))
}

/// Delete old runs to manage storage (keeps most recent N runs per config).
#[tauri::command]
pub async fn cleanup_old_runs(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    keep_count: Option<u32>,
) -> Result<TieredInfoResponse<u32>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let keep_count = keep_count.unwrap_or(100);

    match tiered_info::delete_old_runs(&conn, &config_id, keep_count) {
        Ok(deleted) => Ok(TieredInfoResponse::ok(deleted)),
        Err(e) => Ok(TieredInfoResponse::err(e)),
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    // Default flakiness threshold (20% - items with 20-80% success rate)
    let threshold = 0.2;

    // Load flakiness cache
    let cache = match FlakinessCache::load(&conn, &config_id, threshold) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to load flakiness cache: {}", e);
            // Return default options on cache load failure
            return Ok(TieredInfoResponse::ok(ExecutionOptions::default()));
        }
    };

    // Get execution options based on item type
    let options = if let Some(tid) = transition_id {
        let opts = cache.get_transition_options(&tid);
        debug!(
            "Execution options for transition '{}': retries={}, timeout_mult={:.1}",
            tid, opts.retry_count, opts.timeout_multiplier
        );
        opts
    } else if let Some(tmpl) = template_id {
        let opts = cache.get_template_options(&tmpl);
        debug!(
            "Execution options for template '{}': retries={}, confidence={:.2}",
            tmpl, opts.retry_count, opts.confidence_threshold
        );
        opts
    } else {
        ExecutionOptions::default()
    };

    Ok(TieredInfoResponse::ok(options))
}

/// Response containing flakiness summary for a configuration.
#[derive(Debug, Serialize)]
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
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let threshold = threshold.unwrap_or(0.2);

    match FlakinessCache::load(&conn, &config_id, threshold) {
        Ok(cache) => {
            let summary = FlakinessSummary {
                flaky_transition_count: cache.flaky_transition_count(),
                flaky_template_count: cache.flaky_template_count(),
                flaky_transition_ids: cache
                    .get_flaky_transition_ids()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect(),
                flaky_template_ids: cache
                    .get_flaky_template_ids()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect(),
                cache_is_stale: cache.is_stale(),
            };

            info!(
                "Flakiness summary for config {}: {} transitions, {} templates",
                config_id, summary.flaky_transition_count, summary.flaky_template_count
            );

            Ok(TieredInfoResponse::ok(summary))
        }
        Err(e) => Ok(TieredInfoResponse::err(e)),
    }
}
