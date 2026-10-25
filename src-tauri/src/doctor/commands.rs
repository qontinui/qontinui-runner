//! Tauri commands for the Doctor health monitoring service.
//!
//! These commands force-stop stuck processes. The status query lives with
//! the command handlers instead, because it resolves `AppState` out of
//! Tauri's managed state and that type is owned there.

/// Force stop a process by PID: its whole tree, on every OS, and only while
/// the pid is provably the tracked AI process it was registered as. A pid with
/// no recorded spawn identity is reported, never signalled.
#[tauri::command]
pub async fn stop_process_by_pid(pid: u32) -> Result<(), String> {
    tracing::warn!("Doctor: Force stopping process PID {} by user request", pid);
    match crate::tree_kill::kill_tracked_pid_async(pid, std::time::Duration::from_secs(5)).await {
        crate::tree_kill::KillOutcome::Killed(report) => {
            tracing::info!(
                target: "tree_kill",
                "Doctor: force-stopped process PID {} (tree: {:?})",
                pid,
                report.killed
            );
            Ok(())
        }
        crate::tree_kill::KillOutcome::AlreadyExited => {
            tracing::info!(target: "tree_kill", "Doctor: PID {} had already exited", pid);
            Ok(())
        }
        crate::tree_kill::KillOutcome::Refused(why) => {
            tracing::warn!(target: "tree_kill", "Doctor: PID {} not signalled: {}", pid, why);
            Err(format!("Failed to force-stop process tree: {why}"))
        }
    }
}
