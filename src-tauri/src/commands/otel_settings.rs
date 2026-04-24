//! OpenTelemetry settings commands
//!
//! Tauri commands for managing OpenTelemetry configuration.
//! OTel settings are persisted to disk, but the tracing subscriber is set once
//! at startup, so changes require a runner restart to take effect.

use crate::otel::OtelConfig;
use crate::settings;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get the current OpenTelemetry settings.
///
/// Returns the OTel settings stored in the persistent settings file.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with OTel settings data
/// * `Err(String)` - Error message if settings cannot be loaded
#[tauri::command]
pub fn get_otel_settings() -> Result<CommandResponse, String> {
    info!("Getting OpenTelemetry settings");

    let otel_settings = settings::get_otel_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("OpenTelemetry settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&otel_settings)
                .map_err(|e| format!("Failed to serialize OTel settings: {}", e))?,
        ),
    })
}

/// Save OpenTelemetry settings.
///
/// Persists the provided OTel settings to the settings file.
/// Note: The tracing subscriber is initialised once at startup, so changes
/// will not take effect until the runner is restarted.
///
/// # Arguments
/// * `config` - The new OpenTelemetry configuration
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message (includes restart notice)
/// * `Err(String)` - Error message if settings cannot be saved
#[tauri::command]
pub fn update_otel_settings(config: OtelConfig) -> Result<CommandResponse, String> {
    info!(
        "Updating OpenTelemetry settings: enabled={}, endpoint={}",
        config.enabled, config.endpoint
    );

    settings::save_otel_settings(config.clone())
        .map_err(|e| format!("Failed to save OTel settings: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(
            "OpenTelemetry settings saved. Restart the runner for changes to take effect."
                .to_string(),
        ),
        data: Some(
            serde_json::to_value(&config)
                .map_err(|e| format!("Failed to serialize OTel settings: {}", e))?,
        ),
    })
}

/// Tauri plugin exposing all OpenTelemetry settings commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_otel_settings")
        .invoke_handler(tauri::generate_handler![
            get_otel_settings,
            update_otel_settings,
        ])
        .build()
}
