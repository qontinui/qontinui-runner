//! Tauri IPC commands for process capture management.
//!
//! When running as a secondary instance (QONTINUI_INSTANCE_NAME is set),
//! process management commands are proxied to the primary runner's HTTP API
//! so that all runners have equal access to managed process state and logs.

use std::sync::Arc;
use tauri::State;

use crate::commands::AppState;

use super::primary_proxy;
use super::types::*;

/// Start a managed process by ID.
#[tauri::command]
pub async fn start_managed_process(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::start_process(&id).await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.start_process(&id).await
}

/// Stop a managed process by ID.
#[tauri::command]
pub async fn stop_managed_process(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::stop_process(&id).await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.stop_process(&id).await
}

/// Restart a managed process by ID.
#[tauri::command]
pub async fn restart_managed_process(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::restart_process(&id).await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.restart_process(&id).await
}

/// Rebuild and restart a managed process by ID.
/// Runs the configured build command, then starts the process.
#[tauri::command]
pub async fn rebuild_and_restart_process(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::rebuild_and_restart_process(&id).await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.rebuild_and_restart_process(&id).await
}

/// Start all managed processes.
#[tauri::command]
pub async fn start_all_managed_processes(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::start_all().await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.start_auto_processes().await;
    Ok(())
}

/// Stop all managed processes.
#[tauri::command]
pub async fn stop_all_managed_processes(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::stop_all().await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.stop_all().await;
    Ok(())
}

/// Get status of all managed processes.
#[tauri::command]
pub async fn get_managed_processes(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ProcessStatus>, String> {
    if primary_proxy::is_secondary() {
        return primary_proxy::get_all_status().await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    Ok(manager.get_all_status().await)
}

/// Get output from a managed process.
#[tauri::command]
pub async fn get_process_output(
    id: String,
    tail: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<OutputLine>, String> {
    let tail = tail.unwrap_or(500);
    if primary_proxy::is_secondary() {
        return primary_proxy::get_output(&id, tail).await;
    }
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.get_output(&id, tail).await
}

/// Get all process configs.
#[tauri::command]
pub async fn get_process_configs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ProcessConfig>, String> {
    // Configs are loaded from shared settings, so always read locally
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    Ok(manager.get_configs().await)
}

/// Save a process config (add or update).
#[tauri::command]
pub async fn save_process_config(
    config: ProcessConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!(
        "save_process_config called: name={}, id={}, cwd={}",
        config.name,
        config.id,
        config.cwd
    );

    // Save to settings (persists across restarts)
    crate::settings::save_managed_process_config(config.clone())?;
    tracing::info!("save_process_config: saved to settings OK");

    // Register with manager if available. During setup wizard the manager may
    // not be initialized yet — that's fine, configs will be loaded from
    // settings when the manager starts up (main.rs startup sequence).
    let manager = state.process_capture_manager.lock().await;
    if let Some(mgr) = manager.as_ref() {
        mgr.register(config).await;
    }

    Ok(())
}

/// Delete a process config.
#[tauri::command]
pub async fn delete_process_config(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // Remove from manager
    let manager = state.process_capture_manager.lock().await;
    let manager = manager
        .as_ref()
        .ok_or("Process capture manager not initialized")?;
    manager.remove_process(&id).await?;

    // Remove from settings
    crate::settings::delete_managed_process_config(&id)?;

    Ok(())
}

/// Get process sessions from database (historical).
#[tauri::command]
pub async fn get_process_sessions_from_db(
    config_id: Option<String>,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::database::ProcessSession>, String> {
    state.pg_db.get_process_sessions(config_id.as_deref(), limit.unwrap_or(50)).await
}

/// Get process session output from database (historical).
#[tauri::command]
pub async fn get_process_session_output_from_db(
    session_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::database::ProcessSessionOutputLine>, String> {
    state.pg_db.get_process_session_output(&session_id, limit.unwrap_or(5000), offset.unwrap_or(0)).await
}
