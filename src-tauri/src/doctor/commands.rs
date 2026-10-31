//! Tauri commands for the Doctor health monitoring service.
//!
//! These commands force-stop stuck processes. The status query lives with
//! the command handlers instead, because it resolves `AppState` out of
//! Tauri's managed state and that type is owned there.

/// Force stop a process by PID.
#[tauri::command]
pub async fn stop_process_by_pid(pid: u32) -> Result<(), String> {
    tracing::warn!("Doctor: Force stopping process PID {} by user request", pid);

    #[cfg(target_os = "windows")]
    {
        // In-process tree kill: no process is created to end one.
        let report =
            crate::tree_kill::kill_tree_by_pid_async(pid, std::time::Duration::from_secs(5))
                .await
                .map_err(|e| format!("Failed to force-stop process tree: {e}"))?;
        tracing::info!(
            "Doctor: Successfully force-stopped process PID {} (tree: {:?})",
            pid,
            report.killed
        );
        Ok(())
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
