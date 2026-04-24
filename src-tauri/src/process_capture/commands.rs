//! Tauri IPC commands for process capture management.
//!
//! When running as a secondary instance (QONTINUI_INSTANCE_NAME is set),
//! process management commands are proxied to the primary runner's HTTP API
//! so that all runners have equal access to managed process state and logs.

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
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
///
/// For running processes, reads from the in-memory ring buffer (live source of truth).
/// For stopped processes, falls back to the most recent session in the PG database.
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

    // Try the in-memory ring buffer first
    let live = manager.get_output(&id, tail).await?;
    if !live.is_empty() {
        return Ok(live);
    }

    // Process has no live output (likely stopped). Fall back to most recent DB session.
    let sessions = state
        .pg_db
        .get_process_sessions(Some(&id), 1)
        .await
        .unwrap_or_default();

    if let Some(session) = sessions.first() {
        let lines = state
            .pg_db
            .get_process_session_output(&session.id, tail as u32, 0)
            .await
            .unwrap_or_default();

        // Convert ProcessSessionOutputLine -> OutputLine
        return Ok(lines
            .into_iter()
            .map(|l| OutputLine {
                timestamp: l.timestamp,
                stream: if l.stream == "stderr" {
                    OutputStream::Stderr
                } else {
                    OutputStream::Stdout
                },
                line: l.line,
            })
            .collect());
    }

    Ok(Vec::new())
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
    state
        .pg_db
        .get_process_sessions(config_id.as_deref(), limit.unwrap_or(50))
        .await
}

/// Get process session output from database (historical).
#[tauri::command]
pub async fn get_process_session_output_from_db(
    session_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::database::ProcessSessionOutputLine>, String> {
    state
        .pg_db
        .get_process_session_output(&session_id, limit.unwrap_or(5000), offset.unwrap_or(0))
        .await
}

/// Get N lines of context around an error's timestamp from the relevant
/// process session. Used by the Error Monitor "show context" feature.
#[tauri::command]
pub async fn get_process_log_context(
    process_name: String,
    around_timestamp: String,
    before: Option<u32>,
    after: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::database::ProcessSessionOutputLine>, String> {
    state
        .pg_db
        .get_process_log_context(
            &process_name,
            &around_timestamp,
            before.unwrap_or(20),
            after.unwrap_or(20),
        )
        .await
}

/// Search across all process logs (current and historical sessions).
#[tauri::command]
pub async fn search_process_logs(
    query: String,
    config_id: Option<String>,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::database::ProcessLogSearchHit>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    state
        .pg_db
        .search_process_logs(&query, config_id.as_deref(), limit.unwrap_or(200))
        .await
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_process_capture_commands")
        .invoke_handler(tauri::generate_handler![
            start_managed_process,
            stop_managed_process,
            restart_managed_process,
            rebuild_and_restart_process,
            start_all_managed_processes,
            stop_all_managed_processes,
            get_managed_processes,
            get_process_output,
            get_process_configs,
            save_process_config,
            delete_process_config,
            get_process_sessions_from_db,
            get_process_session_output_from_db,
            get_process_log_context,
            search_process_logs,
        ])
        .build()
}
