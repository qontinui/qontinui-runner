//! Tauri IPC commands for the orchestration loop.

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;

use crate::commands::AppState;

use super::loop_engine;
use super::types::*;

// --- Backwards-compatible single-loop commands (use default loop ID) ---

/// Start an orchestration loop (single-loop, backwards-compatible).
#[tauri::command]
pub async fn start_orchestration_loop(
    config: OrchestrationLoopConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    loop_engine::start_loop_compat(state.orchestration_loops.clone(), config).await
}

/// Stop the orchestration loop (single-loop, backwards-compatible).
#[tauri::command]
pub async fn stop_orchestration_loop(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    loop_engine::stop_loop_compat(state.orchestration_loops.clone()).await
}

/// Get orchestration loop status (single-loop, backwards-compatible).
#[tauri::command]
pub async fn get_orchestration_loop_status(
    state: State<'_, Arc<AppState>>,
) -> Result<OrchestrationLoopStatus, String> {
    Ok(loop_engine::get_status_compat(state.orchestration_loops.clone()).await)
}

/// Signal that a restart is needed between iterations (single-loop, backwards-compatible).
#[tauri::command]
pub async fn signal_orchestration_restart(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    loop_engine::signal_restart_compat(state.orchestration_loops.clone()).await
}

// --- Multi-loop commands ---

/// Start multiple orchestration loops targeting different runners.
#[tauri::command]
pub async fn start_multi_orchestration_loop(
    config: MultiLoopConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    loop_engine::start_multi_loop(state.orchestration_loops.clone(), config).await
}

/// Stop a specific orchestration loop by ID.
#[tauri::command]
pub async fn stop_orchestration_loop_by_id(
    loop_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    loop_engine::stop_loop_by_id(state.orchestration_loops.clone(), &loop_id).await
}

/// Stop all running orchestration loops.
#[tauri::command]
pub async fn stop_all_orchestration_loops(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    loop_engine::stop_all_loops(state.orchestration_loops.clone()).await
}

/// Get aggregated status of all orchestration loops.
#[tauri::command]
pub async fn get_multi_orchestration_loop_status(
    state: State<'_, Arc<AppState>>,
) -> Result<MultiLoopStatus, String> {
    Ok(loop_engine::get_multi_status(state.orchestration_loops.clone()).await)
}

/// Signal restart on a specific loop by ID.
#[tauri::command]
pub async fn signal_orchestration_restart_by_id(
    loop_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    loop_engine::signal_restart_by_id(state.orchestration_loops.clone(), &loop_id).await
}

// ============================================================================
// Approach-D Conductor (Phase 3) — orchestration-run control commands
// ============================================================================

use super::conductor::OrchestrationRunConfig;
use uuid::Uuid;

/// Arguments for starting a conductor run from the frontend.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOrchestrationRunArgs {
    /// Optional explicit run id; generated when omitted.
    #[serde(default)]
    pub run_id: Option<Uuid>,
    pub goal: String,
    #[serde(default)]
    pub recipe: Option<String>,
    #[serde(default)]
    pub phases: Vec<String>,
    /// Optional concurrency cap override (default 3).
    #[serde(default)]
    pub concurrency_cap: Option<usize>,
    /// Optional tick interval (seconds) override (default 5).
    #[serde(default)]
    pub tick_interval_secs: Option<u64>,
}

fn run_config_from_args(args: &StartOrchestrationRunArgs) -> OrchestrationRunConfig {
    let mut cfg = OrchestrationRunConfig::default();
    if let Some(c) = args.concurrency_cap {
        cfg.concurrency_cap = c.max(1);
    }
    if let Some(t) = args.tick_interval_secs {
        cfg.tick_interval_secs = t.max(1);
    }
    cfg
}

/// Start a conductor (Approach-D orchestration) run. Creates the run row and
/// kicks the stateless reconciler background task.
#[tauri::command]
pub async fn start_orchestration_run(
    app_handle: tauri::AppHandle,
    args: StartOrchestrationRunArgs,
    state: State<'_, Arc<AppState>>,
) -> Result<super::ledger::Run, String> {
    let run_id = args.run_id.unwrap_or_else(Uuid::new_v4);
    let cfg = run_config_from_args(&args);
    loop_engine::start_orchestration_run(
        state.orchestration_loops.clone(),
        app_handle,
        state.pg_db.clone(),
        run_id,
        &args.goal,
        args.recipe.as_deref(),
        &args.phases,
        cfg,
    )
    .await
}

/// Stop a conductor run by id.
#[tauri::command]
pub async fn stop_orchestration_run(
    run_id: Uuid,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    loop_engine::stop_orchestration_run(state.orchestration_loops.clone(), &state.pg_db, run_id)
        .await
}

/// Get a conductor run's status (run row + subtasks + live loop phase).
#[tauri::command]
pub async fn orchestration_run_status(
    run_id: Uuid,
    state: State<'_, Arc<AppState>>,
) -> Result<loop_engine::OrchestrationRunStatus, String> {
    loop_engine::orchestration_run_status(state.orchestration_loops.clone(), &state.pg_db, run_id)
        .await
}

/// List all conductor runs (newest first).
#[tauri::command]
pub async fn list_orchestration_runs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<super::ledger::Run>, String> {
    loop_engine::list_orchestration_runs(&state.pg_db).await
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_orchestration_loop_commands")
        .invoke_handler(tauri::generate_handler![
            start_orchestration_loop,
            stop_orchestration_loop,
            get_orchestration_loop_status,
            signal_orchestration_restart,
            start_multi_orchestration_loop,
            stop_orchestration_loop_by_id,
            stop_all_orchestration_loops,
            get_multi_orchestration_loop_status,
            signal_orchestration_restart_by_id,
        ])
        .build()
}
