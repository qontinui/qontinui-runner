//! Cloud session sync consent commands (plan
//! `2026-07-09-runner-session-history-cloud-sync`, Phase 1).
//!
//! Frontend surface for the runner-global `cloud_sync_enabled` toggle —
//! gate 1 of the consent model. When off (the default), the transcript
//! emitter writes nothing to the session outbox and nothing leaves the
//! machine.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

/// Get the current cloud session sync settings.
#[tauri::command]
pub fn get_cloud_sync_settings() -> Result<CommandResponse, String> {
    let enabled = crate::settings::get_cloud_sync_enabled();
    Ok(CommandResponse {
        success: true,
        message: Some("Cloud sync settings retrieved".to_string()),
        data: Some(serde_json::json!({ "cloud_sync_enabled": enabled })),
    })
}

/// Save the cloud session sync consent flag.
#[tauri::command]
pub fn save_cloud_sync_settings(cloud_sync_enabled: bool) -> Result<CommandResponse, String> {
    info!(
        "Saving cloud sync settings: cloud_sync_enabled={}",
        cloud_sync_enabled
    );
    crate::settings::save_cloud_sync_enabled(cloud_sync_enabled)?;
    Ok(CommandResponse {
        success: true,
        message: Some("Cloud sync settings saved".to_string()),
        data: None,
    })
}

/// Plugin registration (kept for future reuse alongside the central
/// `generate_handler!` list in main.rs — see the rationale comment there).
#[allow(dead_code)]
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_cloud_sync_settings")
        .invoke_handler(tauri::generate_handler![
            get_cloud_sync_settings,
            save_cloud_sync_settings
        ])
        .build()
}
