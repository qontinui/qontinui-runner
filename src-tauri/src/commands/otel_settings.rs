//! OpenTelemetry settings commands
//!
//! Tauri commands for managing OpenTelemetry configuration.
//! OTel settings are persisted to disk, but the tracing subscriber is set once
//! at startup, so changes require a runner restart to take effect.
//!
//! # Typed errors (Workstream D)
//!
//! Handlers keep `-> Result<T, String>` at the Tauri boundary and build
//! errors via [`AppError`] internally, converting with
//! `.map_err(String::from)`. See `commands/mod.rs` for the migration guide.

use crate::error::AppError;
use crate::otel::OtelConfig;
use crate::settings;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Internal implementation of [`get_otel_settings`] returning [`AppError`].
fn get_otel_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting OpenTelemetry settings");

    let otel_settings = settings::get_otel_settings();
    let data = serde_json::to_value(&otel_settings)?;

    Ok(CommandResponse {
        success: true,
        message: Some("OpenTelemetry settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current OpenTelemetry settings.
#[tauri::command]
pub fn get_otel_settings() -> Result<CommandResponse, String> {
    get_otel_settings_impl().map_err(String::from)
}

/// Internal implementation of [`update_otel_settings`] returning [`AppError`].
fn update_otel_settings_impl(config: OtelConfig) -> Result<CommandResponse, AppError> {
    info!(
        "Updating OpenTelemetry settings: enabled={}, endpoint={}",
        config.enabled, config.endpoint
    );

    settings::save_otel_settings(config.clone()).map_err(AppError::ConfigError)?;

    let data = serde_json::to_value(&config)?;

    Ok(CommandResponse {
        success: true,
        message: Some(
            "OpenTelemetry settings saved. Restart the runner for changes to take effect."
                .to_string(),
        ),
        data: Some(data),
    })
}

/// Save OpenTelemetry settings.
#[tauri::command]
pub fn update_otel_settings(config: OtelConfig) -> Result<CommandResponse, String> {
    update_otel_settings_impl(config).map_err(String::from)
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
