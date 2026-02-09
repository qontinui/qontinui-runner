//! State explorer handlers for MCP API
//!
//! Provides HTTP handlers for GUI state discovery and exploration
//! via model-based automation.

use crate::safe_lock::safe_lock_or_recover;
use axum::{extract::State, http::StatusCode, response::Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::state_explorer::{
    ExplorationConfig, ExplorationStrategy, ExplorationTask, StateExplorer, StateMachineGraph,
};

// ============================================================================
// State Explorer HTTP API Handlers
// ============================================================================

/// Request body for starting state exploration
#[derive(Debug, Deserialize)]
pub struct StartExplorationRequest {
    /// Path to the qontinui config file
    config_path: String,
    /// Exploration strategy: "exhaustive", "smoke_test", "regression", "random_walk", "targeted"
    #[serde(default = "default_exploration_strategy")]
    strategy: String,
    /// Maximum number of states to visit (0 = unlimited)
    #[serde(default)]
    max_states: u32,
    /// Maximum time in seconds (0 = unlimited)
    #[serde(default)]
    max_duration_seconds: u64,
    /// Target state IDs for targeted strategy
    #[serde(default)]
    target_state_ids: Vec<String>,
    /// Target transition IDs for targeted strategy
    #[serde(default)]
    target_transition_ids: Vec<String>,
    /// Monitor index
    #[serde(default)]
    monitor_index: Option<i32>,
    /// Whether to capture screenshots
    #[serde(default = "default_capture_screenshots")]
    capture_screenshots: bool,
    /// Whether to stop on first failure
    #[serde(default)]
    stop_on_first_failure: bool,
}

pub fn default_exploration_strategy() -> String {
    "exhaustive".to_string()
}

pub fn default_capture_screenshots() -> bool {
    true
}

/// Start a state exploration task
pub async fn start_exploration(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExplorationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting exploration for {} with strategy {}",
        request.config_path, request.strategy
    );

    let config = ExplorationConfig {
        config_path: request.config_path,
        depth: None,
        strategy: request.strategy,
        max_states: request.max_states,
        max_duration_seconds: request.max_duration_seconds,
        target_state_ids: request.target_state_ids,
        target_transition_ids: request.target_transition_ids,
        monitor_index: request.monitor_index,
        capture_screenshots: request.capture_screenshots,
        capture_transition_screenshots: false,
        state_delay_ms: 500,
        output_directory: None,
        stop_on_first_failure: request.stop_on_first_failure,
        random_seed: None,
        checkpoint_batch_size: 10,
        checkpoint_issue_threshold: 5,
        checkpoint_on_critical: true,
        interleave_with_agentic: false,
    };

    let task = ExplorationTask::new(config, state.app_state.clone());

    // Run the task
    match task.execute().await {
        Ok(result) => {
            let result_json = serde_json::to_value(&result).unwrap_or_default();
            Ok(Json(ApiResponse::success(result_json)))
        }
        Err(e) => {
            error!("Exploration failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get available exploration strategies
pub async fn get_exploration_strategies() -> Json<ApiResponse<serde_json::Value>> {
    let strategies = serde_json::json!({
        "strategies": [
            {
                "id": "exhaustive",
                "name": "Exhaustive",
                "description": "Visit every state and transition - complete but slow",
                "recommended_for": "Full exploration runs, nightly builds"
            },
            {
                "id": "smoke_test",
                "name": "Smoke Test",
                "description": "Quick path through critical states with descriptions",
                "recommended_for": "Quick checks, CI/CD pipelines"
            },
            {
                "id": "regression",
                "name": "Regression",
                "description": "Focus on previously-failed areas",
                "recommended_for": "After fixes, before releases"
            },
            {
                "id": "random_walk",
                "name": "Random Walk",
                "description": "Random exploration to discover unexpected behaviors",
                "recommended_for": "Exploratory testing, chaos engineering"
            },
            {
                "id": "targeted",
                "name": "Targeted",
                "description": "Explore only specific states/transitions",
                "recommended_for": "Specific feature exploration"
            }
        ]
    });

    Json(ApiResponse::success(strategies))
}

/// Preview exploration plan without executing
pub async fn preview_exploration(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExplorationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load the current config
    let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
    let qontinui_config = match config_lock.as_ref() {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error("No configuration loaded".to_string())),
            ));
        }
    };

    // Convert to JSON value for graph building
    let config_value = match serde_json::to_value(qontinui_config) {
        Ok(v) => v,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to serialize config: {}", e))),
            ));
        }
    };

    drop(config_lock);

    // Build graph and generate path
    let graph = StateMachineGraph::from_config(&config_value);

    let strategy = ExplorationStrategy::from_str(&request.strategy);
    let mut explorer = StateExplorer::new(graph.clone(), strategy);

    if request.max_states > 0 {
        explorer = explorer.with_max_states(request.max_states);
    }

    if !request.target_state_ids.is_empty() || !request.target_transition_ids.is_empty() {
        explorer = explorer.with_targets(
            request.target_state_ids.clone(),
            request.target_transition_ids.clone(),
        );
    }

    let path = explorer.generate_path();

    let plan = serde_json::json!({
        "strategy": format!("{:?}", strategy),
        "total_states_in_config": graph.states.len(),
        "total_transitions_in_config": graph.transitions.len(),
        "states_to_visit": path.states.len(),
        "transitions_to_explore": path.transitions.len(),
        "estimated_cost": path.estimated_cost,
        "states": path.states,
        "transitions": path.transitions,
    });

    Ok(Json(ApiResponse::success(plan)))
}

/// Get exploration history
pub async fn get_exploration_history(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let reports_dir = crate::paths::get_state_explorer_dir();

    if !reports_dir.exists() {
        return Json(ApiResponse::success(serde_json::json!({ "runs": [] })));
    }

    let mut runs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&reports_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with("exploration-report-"))
                    .unwrap_or(false)
            {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(report) = serde_json::from_str::<serde_json::Value>(&content) {
                        runs.push(serde_json::json!({
                            "run_id": report.get("run_id"),
                            "config_name": report.get("config_name"),
                            "strategy": report.get("strategy"),
                            "started_at": report.get("started_at"),
                            "completed_at": report.get("completed_at"),
                            "summary": report.get("summary"),
                            "report_path": path.to_string_lossy(),
                        }));
                    }
                }
            }
        }
    }

    // Sort by started_at descending and limit
    runs.sort_by(|a, b| {
        let a_time = a.get("started_at").and_then(|t| t.as_str()).unwrap_or("");
        let b_time = b.get("started_at").and_then(|t| t.as_str()).unwrap_or("");
        b_time.cmp(a_time)
    });

    runs.truncate(limit);

    Json(ApiResponse::success(serde_json::json!({ "runs": runs })))
}

/// Get a specific exploration report
pub async fn get_exploration_report(
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let reports_dir = crate::paths::get_state_explorer_dir();
    let report_path = reports_dir.join(format!("exploration-report-{}.json", run_id));

    if !report_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Report not found for run ID: {}",
                run_id
            ))),
        ));
    }

    let content = std::fs::read_to_string(&report_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to read report: {}", e))),
        )
    })?;

    let report: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to parse report: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(report)))
}

/// Get AI analysis prompt for an exploration report
pub async fn get_exploration_prompt(
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let reports_dir = crate::paths::get_state_explorer_dir();
    let report_path = reports_dir.join(format!("exploration-report-{}.json", run_id));

    if !report_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Report not found for run ID: {}",
                run_id
            ))),
        ));
    }

    let content = std::fs::read_to_string(&report_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to read report: {}", e))),
        )
    })?;

    let report: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to parse report: {}", e))),
        )
    })?;

    let ai_prompt = report
        .get("ai_analysis_prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("No analysis prompt available")
        .to_string();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "run_id": run_id,
        "prompt": ai_prompt,
    }))))
}

// ============================================================================
// End State Explorer HTTP API Handlers
// ============================================================================

// Verification Test handlers removed — see crate::mcp module

// AI Context handlers moved to crate::mcp::contexts

// Context handler functions removed — see crate::mcp::contexts
// DOM capture handler functions removed — see crate::mcp::dom_capture
// Saved API request handler functions removed — see crate::mcp::saved_api_requests

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/state-explorer/start", post(start_exploration))
        .route(
            "/state-explorer/strategies",
            get(get_exploration_strategies),
        )
        .route("/state-explorer/preview", post(preview_exploration))
        .route("/state-explorer/history", get(get_exploration_history))
        .route("/state-explorer/:run_id", get(get_exploration_report))
        .route(
            "/state-explorer/:run_id/prompt",
            get(get_exploration_prompt),
        )
}
