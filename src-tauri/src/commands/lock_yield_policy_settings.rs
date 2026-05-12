//! Lock-yield-policy settings commands.
//!
//! Get/save the auto-yield-on-idle policy that drives
//! `executor::auto_yield_policy`. See `settings::LockYieldPolicySettings`
//! for the on-disk schema.

use crate::error::AppError;
use crate::settings::{self, LockYieldPolicySettings};
use anyhow::Result;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::info;

use super::CommandResponse;

/// Internal implementation of [`get_lock_yield_policy_settings`].
fn get_lock_yield_policy_settings_impl() -> Result<CommandResponse, AppError> {
    info!("Getting lock-yield policy settings");

    let policy = settings::get_lock_yield_policy_settings();
    let data = serde_json::to_value(&policy)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Lock-yield policy settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current lock-yield policy settings.
#[tauri::command]
pub fn get_lock_yield_policy_settings() -> Result<CommandResponse, String> {
    get_lock_yield_policy_settings_impl().map_err(String::from)
}

/// Internal implementation of [`save_lock_yield_policy_settings`].
fn save_lock_yield_policy_settings_impl(
    enabled: bool,
    idle_threshold_secs: u64,
    min_wait_secs: u64,
) -> Result<CommandResponse, AppError> {
    info!(
        "Saving lock-yield policy settings: enabled={}, idle_threshold_secs={}, min_wait_secs={}",
        enabled, idle_threshold_secs, min_wait_secs
    );

    let policy = LockYieldPolicySettings {
        enabled,
        idle_threshold_secs,
        min_wait_secs,
    };
    settings::save_lock_yield_policy_settings(policy).map_err(AppError::ConfigError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Lock-yield policy settings saved".to_string()),
        data: None,
    })
}

/// Save lock-yield policy settings.
#[tauri::command]
pub fn save_lock_yield_policy_settings(
    enabled: bool,
    idle_threshold_secs: u64,
    min_wait_secs: u64,
) -> Result<CommandResponse, String> {
    save_lock_yield_policy_settings_impl(enabled, idle_threshold_secs, min_wait_secs)
        .map_err(String::from)
}

/// Tauri plugin exposing the lock-yield-policy settings commands.
///
/// Left in place for parity with the rest of `commands/*_settings.rs`;
/// the runtime registration goes through main.rs's central
/// `generate_handler!`.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_lock_yield_policy_settings")
        .invoke_handler(tauri::generate_handler![
            get_lock_yield_policy_settings,
            save_lock_yield_policy_settings,
        ])
        .build()
}
