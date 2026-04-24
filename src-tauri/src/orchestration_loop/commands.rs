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
