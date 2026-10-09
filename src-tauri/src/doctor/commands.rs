//! Tauri commands for the Doctor health monitoring service.
//!
//! These commands force-stop stuck processes. The status query lives with
//! the command handlers instead, because it resolves `AppState` out of
//! Tauri's managed state and that type is owned there.

use serde::Serialize;

/// Health status info for a single monitored process (for frontend display).
#[derive(Debug, Clone, Serialize)]
pub struct ProcessHealthInfo {
    pub pid: u32,
    pub process_type: String,
    pub label: String,
    pub status: String,
    pub inactive_checks: u32,
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

// Tauri commands this module owns — the ONLY registration site (see
// `crate::ipc_registry`). A `#[tauri::command]` fn missing here is
// unreachable from the frontend.
crate::ipc_group!(stop_process_by_pid,);
