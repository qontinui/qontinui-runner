//! Container Settings Commands
//!
//! Tauri commands for managing container isolation settings.
//! Container isolation allows shell commands to run inside Docker containers
//! for sandboxed execution, with automatic fallback to host execution.
//!
//! # Typed errors (Workstream D)
//!
//! This module is the Workstream D proof-of-pattern for typed error adoption.
//! Handlers keep `-> Result<T, String>` at the Tauri boundary (to preserve
//! the frontend's existing reliance on plain string errors) but build errors
//! via [`AppError`] internally and convert at the boundary with
//! `.map_err(String::from)`. The `impl From<AppError> for String` in
//! [`crate::error`] provides the ABI shim. See `commands/mod.rs` for the
//! migration guide.
//!
//! Note: AppError prefixes its Display output (e.g. `"JSON parsing error: ..."`)
//! so the wire string is not byte-identical to the pre-migration format —
//! it is however more structured and still contains the original message as
//! a suffix. Frontend error banners render the string verbatim so this
//! change is backward-compatible for display; it only affects consumers
//! that substring-match the prefix (none in this module's call chain).

use crate::commands::{AppState, CommandResponse};
use crate::container::container_config::ContainerConfig;
use crate::container::docker_client::DockerManager;
use crate::container::isolated_executor::IsolatedExecutor;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;

/// Response for Docker/container status checks
#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerStatusResponse {
    /// Whether Docker daemon is reachable
    pub docker_available: bool,
    /// Whether container isolation is enabled in settings
    pub isolation_enabled: bool,
    /// The configured base Docker image
    pub base_image: String,
    /// Whether the executor is currently initialized and ready
    pub executor_ready: bool,
}

/// Internal implementation of [`get_container_settings`] returning [`AppError`].
///
/// Uses `?` on `serde_json::to_value` because `AppError` has a blanket
/// `From<serde_json::Error>` impl that maps to `AppError::JsonError`.
async fn get_container_settings_impl() -> Result<CommandResponse, AppError> {
    let config = crate::settings::get_container_settings();
    let data = serde_json::to_value(&config)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Container settings retrieved".to_string()),
        data: Some(data),
    })
}

/// Get the current container isolation settings.
#[tauri::command]
pub async fn get_container_settings(
    _state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    get_container_settings_impl().await.map_err(String::from)
}

/// Internal implementation of [`update_container_settings`] returning [`AppError`].
///
/// `settings::save_container_settings` returns `Result<(), String>` so its
/// error is lifted into [`AppError::ConfigError`], the closest-matching
/// variant for persistence failures. `serde_json::to_value` uses `?` via the
/// blanket `From<serde_json::Error>` impl.
async fn update_container_settings_impl(
    config: ContainerConfig,
    state: &Arc<AppState>,
) -> Result<CommandResponse, AppError> {
    info!(
        "Updating container settings: enabled={}, image={}",
        config.enabled, config.base_image
    );

    // Persist to settings file
    crate::settings::save_container_settings(config.clone()).map_err(AppError::ConfigError)?;

    // Update the runtime executor
    let mut executor_guard = state.container_executor.lock().await;
    if config.enabled {
        // Initialize DockerManager and IsolatedExecutor
        let docker = Arc::new(DockerManager::new().await);
        if docker.is_available() {
            *executor_guard = Some(IsolatedExecutor::new(docker, config.clone()));
            info!(
                "Container executor initialized with image: {}",
                config.base_image
            );
        } else {
            *executor_guard = None;
            info!(
                "Container isolation enabled but Docker is not available; executor not initialized"
            );
        }
    } else {
        *executor_guard = None;
        info!("Container isolation disabled, executor removed");
    }

    let data = serde_json::to_value(&config)?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Container settings updated (enabled: {})",
            config.enabled
        )),
        data: Some(data),
    })
}

/// Update container isolation settings.
/// When enabling, initializes the Docker connection and executor.
/// When disabling, tears down the executor.
#[tauri::command]
pub async fn update_container_settings(
    config: ContainerConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    update_container_settings_impl(config, &state)
        .await
        .map_err(String::from)
}

/// Internal implementation of [`check_docker_status`] returning [`AppError`].
async fn check_docker_status_impl(state: &Arc<AppState>) -> Result<CommandResponse, AppError> {
    info!("Checking Docker status");

    // Check if we have an active executor
    let executor_guard = state.container_executor.lock().await;
    let executor_ready = executor_guard
        .as_ref()
        .map(|e| e.is_available())
        .unwrap_or(false);
    drop(executor_guard);

    // Do a fresh Docker check regardless of executor state
    let docker = DockerManager::new().await;
    let docker_available = docker.is_available();

    let config = crate::settings::get_container_settings();

    let status = ContainerStatusResponse {
        docker_available,
        isolation_enabled: config.enabled,
        base_image: config.base_image,
        executor_ready,
    };

    let data = serde_json::to_value(&status)?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Docker {}",
            if docker_available {
                "is available"
            } else {
                "is not available"
            }
        )),
        data: Some(data),
    })
}

/// Check Docker daemon availability.
/// This performs a live ping to the Docker socket.
#[tauri::command]
pub async fn check_docker_status(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    check_docker_status_impl(&state).await.map_err(String::from)
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_container_settings")
        .invoke_handler(tauri::generate_handler![
            get_container_settings,
            update_container_settings,
            check_docker_status,
        ])
        .build()
}
