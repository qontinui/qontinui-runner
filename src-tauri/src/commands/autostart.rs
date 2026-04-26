//! Phase F.1 — System-startup autostart commands.
//!
//! Tauri commands wrapping `tauri-plugin-autostart`'s `ManagerExt` so the
//! runner UI's settings tab can offer a "Launch on system startup" toggle.
//!
//! When enabled, the OS auto-starts the runner at user login (Windows registry
//! `Run` key, macOS LaunchAgent, Linux .desktop autostart). This is the
//! durable solution for overnight scheduling: scheduled tasks fire reliably
//! even if the user closed the runner. The Phase F.1 deep-link wake handler
//! complements this for the user-is-at-desk-but-runner-crashed case.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;
use tracing::info;

use super::CommandResponse;

#[derive(Serialize)]
struct AutostartStatus {
    enabled: bool,
}

/// Return whether the runner is currently registered to launch at system startup.
#[tauri::command]
pub fn get_autostart_enabled(app: AppHandle) -> Result<CommandResponse, String> {
    let manager = app.autolaunch();
    let enabled = manager
        .is_enabled()
        .map_err(|e| format!("Failed to read autostart state: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(
            serde_json::to_value(AutostartStatus { enabled })
                .map_err(|e| format!("Failed to serialize autostart status: {}", e))?,
        ),
    })
}

/// Enable or disable launch-on-startup. Returns the resulting state so the UI
/// can reconcile after a registry write fails silently.
#[tauri::command]
pub fn set_autostart_enabled(app: AppHandle, enabled: bool) -> Result<CommandResponse, String> {
    let manager = app.autolaunch();

    if enabled {
        info!("Enabling launch-on-system-startup");
        manager
            .enable()
            .map_err(|e| format!("Failed to enable autostart: {}", e))?;
    } else {
        info!("Disabling launch-on-system-startup");
        manager
            .disable()
            .map_err(|e| format!("Failed to disable autostart: {}", e))?;
    }

    let actual = manager
        .is_enabled()
        .map_err(|e| format!("Failed to read autostart state after toggle: {}", e))?;

    Ok(CommandResponse {
        success: actual == enabled,
        message: Some(format!(
            "Autostart {}",
            if actual { "enabled" } else { "disabled" }
        )),
        data: Some(
            serde_json::to_value(AutostartStatus { enabled: actual })
                .map_err(|e| format!("Failed to serialize autostart status: {}", e))?,
        ),
    })
}
