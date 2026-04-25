//! State Explorer Tauri Commands
//!
//! This module provides Tauri commands for the state explorer that
//! enables AI-driven exploration of application states.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::{error, info};

use super::CommandResponse;
use crate::commands::compartments::ExecutionCompartment;
use crate::state_explorer::{ExplorationConfig, ExplorationStrategy, ExplorationTask};

/// Start a state exploration task
///
/// Begins exploration of the application using the specified strategy.
/// The task will explore states, capture screenshots, and report discrepancies.
///
/// # Arguments
/// * `config` - Exploration task configuration
/// * `state` - Application state
///
/// # Returns
/// * `Ok(CommandResponse)` - Task started with run_id in data
/// * `Err(String)` - Error if task could not be started
#[tauri::command]
pub async fn start_exploration(
    config: ExplorationConfig,
    execution: State<'_, ExecutionCompartment>,
) -> Result<CommandResponse, String> {
    info!(
        "Starting exploration task for {} with strategy {}",
        config.config_path, config.strategy
    );

    // Validate config path
    if config.config_path.is_empty() {
        return Err("Config path is required".to_string());
    }

    // ExplorationTask is a pre-compartment legacy orchestrator that takes
    // Arc<AppState> directly. Use the explicit `app_state()` escape hatch on
    // the compartment so the dependency stays greppable for a future split.
    let task = ExplorationTask::new(config.clone(), execution.app_state().clone());

    // Run the task asynchronously
    let result = tokio::spawn(async move { task.execute().await }).await;

    match result {
        Ok(Ok(exploration_result)) => {
            info!(
                "Exploration task {} completed: {}",
                exploration_result.run_id, exploration_result.summary
            );

            Ok(CommandResponse {
                success: true,
                message: Some("Exploration task completed".to_string()),
                data: Some(serde_json::to_value(&exploration_result).unwrap_or_default()),
            })
        }
        Ok(Err(e)) => {
            error!("Exploration task failed: {}", e);
            Err(e)
        }
        Err(e) => {
            error!("Exploration task panicked: {}", e);
            Err(format!("Task execution error: {}", e))
        }
    }
}

/// Get available exploration strategies
///
/// Returns a list of available exploration strategies with descriptions.
///
/// # Returns
/// * `Ok(CommandResponse)` - List of strategies in data
#[tauri::command]
pub async fn get_exploration_strategies() -> Result<CommandResponse, String> {
    let strategies = vec![
        serde_json::json!({
            "id": "exhaustive",
            "name": "Exhaustive",
            "description": "Visit every state and transition - complete but slow",
            "recommended_for": "Full exploration runs, nightly builds"
        }),
        serde_json::json!({
            "id": "smoke_test",
            "name": "Smoke Test",
            "description": "Quick path through critical states with descriptions",
            "recommended_for": "Quick checks, CI/CD pipelines"
        }),
        serde_json::json!({
            "id": "regression",
            "name": "Regression",
            "description": "Focus on previously-failed areas",
            "recommended_for": "After fixes, before releases"
        }),
        serde_json::json!({
            "id": "random_walk",
            "name": "Random Walk",
            "description": "Random exploration to discover unexpected behaviors",
            "recommended_for": "Exploratory testing, chaos engineering"
        }),
        serde_json::json!({
            "id": "targeted",
            "name": "Targeted",
            "description": "Explore only specific states/transitions",
            "recommended_for": "Specific feature exploration"
        }),
    ];

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "strategies": strategies })),
    })
}

/// Generate an exploration plan without executing
///
/// Creates an exploration plan showing which states and transitions would be
/// explored with the given configuration. Useful for previewing before running.
///
/// # Arguments
/// * `config` - Exploration task configuration
/// * `state` - Application state
///
/// # Returns
/// * `Ok(CommandResponse)` - Exploration plan in data
/// * `Err(String)` - Error if plan could not be generated
#[tauri::command]
pub async fn preview_exploration_plan(
    config: ExplorationConfig,
    execution: State<'_, ExecutionCompartment>,
) -> Result<CommandResponse, String> {
    info!("Generating exploration plan for {}", config.config_path);

    // Load the current config
    let config_lock =
        crate::safe_lock::safe_lock_or_recover(execution.current_config(), "current_config");
    let qontinui_config = match config_lock.as_ref() {
        Some(c) => c,
        None => {
            return Err("No configuration loaded".to_string());
        }
    };

    // Convert to JSON value for graph building
    let config_value = serde_json::to_value(qontinui_config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    drop(config_lock);

    // Build graph and generate path
    let graph = crate::state_explorer::StateMachineGraph::from_config(&config_value);

    let strategy = ExplorationStrategy::from_str(&config.strategy);
    let mut explorer = crate::state_explorer::StateExplorer::new(graph.clone(), strategy);

    if config.max_states > 0 {
        explorer = explorer.with_max_states(config.max_states);
    }

    if !config.target_state_ids.is_empty() || !config.target_transition_ids.is_empty() {
        explorer = explorer.with_targets(
            config.target_state_ids.clone(),
            config.target_transition_ids.clone(),
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

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Plan generated: {} states, {} transitions",
            path.states.len(),
            path.transitions.len()
        )),
        data: Some(plan),
    })
}

/// Get exploration history
///
/// Returns a list of past exploration runs.
///
/// # Arguments
/// * `limit` - Maximum number of runs to return (default: 20)
///
/// # Returns
/// * `Ok(CommandResponse)` - List of exploration runs in data
#[tauri::command]
pub async fn get_exploration_history(limit: Option<u32>) -> Result<CommandResponse, String> {
    let limit = limit.unwrap_or(20);

    // Read from exploration reports directory
    let reports_dir = crate::paths::get_state_explorer_dir();

    if !reports_dir.exists() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No exploration history found".to_string()),
            data: Some(serde_json::json!({ "runs": [] })),
        });
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

    runs.truncate(limit as usize);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} exploration runs", runs.len())),
        data: Some(serde_json::json!({ "runs": runs })),
    })
}

/// Get a specific exploration report
///
/// Returns the full exploration report for a given run ID.
///
/// # Arguments
/// * `run_id` - The exploration run ID
///
/// # Returns
/// * `Ok(CommandResponse)` - Full report in data
/// * `Err(String)` - Error if report not found
#[tauri::command]
pub async fn get_exploration_report(run_id: String) -> Result<CommandResponse, String> {
    let reports_dir = crate::paths::get_state_explorer_dir();
    let report_path = reports_dir.join(format!("exploration-report-{}.json", run_id));

    if !report_path.exists() {
        return Err(format!("Report not found for run ID: {}", run_id));
    }

    let content = std::fs::read_to_string(&report_path)
        .map_err(|e| format!("Failed to read report: {}", e))?;

    let report: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse report: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(report),
    })
}

/// Get AI analysis prompt for an exploration report
///
/// Takes an exploration report and returns an AI-ready prompt for analysis.
/// The prompt includes all discrepancies and asks specific analysis questions.
///
/// # Arguments
/// * `run_id` - The exploration run ID
///
/// # Returns
/// * `Ok(CommandResponse)` - AI analysis prompt in data
/// * `Err(String)` - Error if report not found
#[tauri::command]
pub async fn get_exploration_analysis_prompt(run_id: String) -> Result<CommandResponse, String> {
    let reports_dir = crate::paths::get_state_explorer_dir();
    let report_path = reports_dir.join(format!("exploration-report-{}.json", run_id));

    if !report_path.exists() {
        return Err(format!("Report not found for run ID: {}", run_id));
    }

    let content = std::fs::read_to_string(&report_path)
        .map_err(|e| format!("Failed to read report: {}", e))?;

    let report: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse report: {}", e))?;

    // Extract the AI analysis prompt from the report
    let ai_prompt = report
        .get("ai_analysis_prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("No analysis prompt available")
        .to_string();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "run_id": run_id,
            "prompt": ai_prompt,
        })),
    })
}

/// Clear exploration history
///
/// Removes all exploration reports older than the specified number of days.
///
/// # Arguments
/// * `older_than_days` - Remove reports older than this many days (default: 30)
///
/// # Returns
/// * `Ok(CommandResponse)` - Number of reports removed
#[tauri::command]
pub async fn clear_exploration_history(
    older_than_days: Option<u32>,
) -> Result<CommandResponse, String> {
    let days = older_than_days.unwrap_or(30);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);

    let reports_dir = crate::paths::get_state_explorer_dir();

    if !reports_dir.exists() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No exploration history to clear".to_string()),
            data: Some(serde_json::json!({ "removed": 0 })),
        });
    }

    let mut removed = 0;

    if let Ok(entries) = std::fs::read_dir(&reports_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Check file modification time
            if let Ok(metadata) = std::fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    let modified_time: chrono::DateTime<chrono::Utc> = modified.into();
                    if modified_time < cutoff && std::fs::remove_file(&path).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }

    info!("Removed {} old exploration reports", removed);

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Removed {} exploration reports", removed)),
        data: Some(serde_json::json!({ "removed": removed })),
    })
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_state_explorer")
        .invoke_handler(tauri::generate_handler![
            start_exploration,
            get_exploration_strategies,
            preview_exploration_plan,
            get_exploration_history,
            get_exploration_report,
            get_exploration_analysis_prompt,
            clear_exploration_history,
        ])
        .build()
}
