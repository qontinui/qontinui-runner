//! Cloud session sync consent commands (plan
//! `2026-07-09-runner-session-history-cloud-sync`, Phase 1; split into two
//! independent gates by plan `2026-07-10-split-cloud-sync-consent`).
//!
//! Frontend surface for two independent runner-global toggles:
//! - `cloud_sync_enabled` — gate 1, AI content sync. When off (the
//!   default), the transcript emitter and tenant memory sync write nothing
//!   to the session outbox and nothing leaves the machine.
//! - `session_metadata_sync_enabled` — gate 2, session metadata sync. When
//!   off, the restore-record emitter writes nothing. Default ON — this half
//!   carries no conversation content.

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

/// Get the current session metadata sync settings.
#[tauri::command]
pub fn get_session_metadata_sync_settings() -> Result<CommandResponse, String> {
    let enabled = crate::settings::get_session_metadata_sync_enabled();
    Ok(CommandResponse {
        success: true,
        message: Some("Session metadata sync settings retrieved".to_string()),
        data: Some(serde_json::json!({ "session_metadata_sync_enabled": enabled })),
    })
}

/// Save the session metadata sync consent flag.
#[tauri::command]
pub fn save_session_metadata_sync_settings(
    session_metadata_sync_enabled: bool,
) -> Result<CommandResponse, String> {
    info!(
        "Saving session metadata sync settings: session_metadata_sync_enabled={}",
        session_metadata_sync_enabled
    );
    crate::settings::save_session_metadata_sync_enabled(session_metadata_sync_enabled)?;
    Ok(CommandResponse {
        success: true,
        message: Some("Session metadata sync settings saved".to_string()),
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
            save_cloud_sync_settings,
            get_session_metadata_sync_settings,
            save_session_metadata_sync_settings
        ])
        .build()
}
