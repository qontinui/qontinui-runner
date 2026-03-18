//! Tauri IPC commands for the orchestration loop.

use std::sync::Arc;
use tauri::State;

use crate::commands::AppState;

use super::loop_engine;
use super::types::*;

/// Start an orchestration loop.
#[tauri::command]
pub async fn start_orchestration_loop(
    config: OrchestrationLoopConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    loop_engine::start_loop(state.orchestration_loop.clone(), config).await
}

/// Stop the orchestration loop.
#[tauri::command]
pub async fn stop_orchestration_loop(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    loop_engine::stop_loop(state.orchestration_loop.clone()).await
}

/// Get orchestration loop status.
#[tauri::command]
pub async fn get_orchestration_loop_status(
    state: State<'_, Arc<AppState>>,
) -> Result<OrchestrationLoopStatus, String> {
    Ok(loop_engine::get_status(state.orchestration_loop.clone()).await)
}

/// Signal that a restart is needed between iterations.
#[tauri::command]
pub async fn signal_orchestration_restart(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    loop_engine::signal_restart(state.orchestration_loop.clone()).await
}
