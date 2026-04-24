//! Tauri commands for the Doctor health monitoring service.
//!
//! These commands allow the frontend to query Doctor status and force-stop stuck processes.

use std::sync::Arc;

use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;

use crate::commands::AppState;
use crate::doctor::service::ProcessStatus;

/// Health status info for a single monitored process (for frontend display).
#[derive(Debug, Clone, Serialize)]
pub struct ProcessHealthInfo {
    pub pid: u32,
    pub process_type: String,
    pub label: String,
    pub status: String,
    pub inactive_checks: u32,
}

/// Query current Doctor health status of all monitored processes.
#[tauri::command]
pub async fn doctor_get_status(
    app_state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<ProcessStatus>, String> {
    let handle_lock = app_state.doctor_handle.lock().await;
    match handle_lock.as_ref() {
        Some(handle) => handle.query_status().await,
        None => Ok(vec![]), // Doctor not started yet
    }
}

/// Force stop a process by PID.
#[tauri::command]
pub async fn stop_process_by_pid(pid: u32) -> Result<(), String> {
    tracing::warn!("Doctor: Force stopping process PID {} by user request", pid);

    #[cfg(target_os = "windows")]
    {
        let output = crate::process_helpers::no_window("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .map_err(|e| format!("Failed to execute taskkill: {}", e))?;
        if output.status.success() {
            tracing::info!("Doctor: Successfully force-stopped process PID {}", pid);
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("taskkill failed: {}", stderr))
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // On Unix-like systems, send SIGTERM
        let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if result == 0 {
            tracing::info!("Doctor: Successfully sent SIGTERM to process PID {}", pid);
            Ok(())
        } else {
            Err(format!("Failed to kill process {}", pid))
        }
    }
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_doctor_commands")
        .invoke_handler(tauri::generate_handler![
            doctor_get_status,
            stop_process_by_pid,
        ])
        .build()
}
