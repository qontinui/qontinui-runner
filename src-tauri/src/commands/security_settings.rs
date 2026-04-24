//! Tauri commands for security settings management.
//!
//! # Typed errors (Workstream D)
//!
//! Handlers keep `-> Result<T, String>` at the Tauri boundary and build
//! errors via [`AppError`] internally, converting with
//! `.map_err(String::from)`. See `commands/mod.rs` for the migration guide.

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;
use crate::error::AppError;
use crate::security::engine::SecuritySettings;
use crate::security::profiles::{self, ProfileSummary};

/// Get the current security settings.
#[tauri::command]
pub async fn get_security_settings() -> Result<SecuritySettings, String> {
    let settings = crate::settings::get_security_settings();
    Ok(settings)
}

/// Internal implementation of [`update_security_settings`] returning [`AppError`].
async fn update_security_settings_impl(
    config: SecuritySettings,
) -> Result<CommandResponse, AppError> {
    info!(
        "Updating security settings: profile={}, audit={}, cred_proxy={}, net_proxy={}",
        config.default_profile,
        config.audit_enabled,
        config.credential_proxy_enabled,
        config.network_proxy_enabled,
    );

    // Validate profile name
    let valid_profiles = ["permissive", "standard", "strict", "custom"];
    if !valid_profiles.contains(&config.default_profile.as_str()) {
        return Err(AppError::ValidationError(format!(
            "Invalid security profile '{}'. Valid profiles: {:?}",
            config.default_profile, valid_profiles
        )));
    }

    // If custom profile selected, ensure custom policy exists
    if config.default_profile == "custom" && config.custom_policy.is_none() {
        return Err(AppError::ValidationError(
            "Custom profile selected but no custom policy provided".to_string(),
        ));
    }

    crate::settings::save_security_settings(config.clone()).map_err(AppError::ConfigError)?;

    let data = serde_json::to_value(&config)?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Security settings updated (profile: {})",
            config.default_profile
        )),
        data: Some(data),
    })
}

/// Update security settings.
#[tauri::command]
pub async fn update_security_settings(config: SecuritySettings) -> Result<CommandResponse, String> {
    update_security_settings_impl(config)
        .await
        .map_err(String::from)
}

/// Get the list of available security profiles.
#[tauri::command]
pub async fn get_security_profiles() -> Result<Vec<ProfileSummary>, String> {
    Ok(profiles::list_profiles())
}

/// Tauri plugin exposing all security-settings commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_security_settings")
        .invoke_handler(tauri::generate_handler![
            get_security_settings,
            update_security_settings,
            get_security_profiles,
        ])
        .build()
}
