//! Container Settings Commands
//!
//! Tauri commands for managing container isolation settings.
//! Container isolation allows shell commands to run inside Docker containers
//! for sandboxed execution, with automatic fallback to host execution.

use crate::commands::{AppState, CommandResponse};
use crate::container::container_config::ContainerConfig;
use crate::container::docker_client::DockerManager;
use crate::container::isolated_executor::IsolatedExecutor;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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

/// Get the current container isolation settings.
#[tauri::command]
pub async fn get_container_settings(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let config = crate::settings::get_container_settings();

    Ok(CommandResponse {
        success: true,
        message: Some("Container settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&config)
                .map_err(|e| format!("Failed to serialize container config: {}", e))?,
        ),
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
    info!(
        "Updating container settings: enabled={}, image={}",
        config.enabled, config.base_image
    );

    // Persist to settings file
    crate::settings::save_container_settings(config.clone())?;

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

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Container settings updated (enabled: {})",
            config.enabled
        )),
        data: Some(
            serde_json::to_value(&config)
                .map_err(|e| format!("Failed to serialize container config: {}", e))?,
        ),
    })
}

/// Check Docker daemon availability.
/// This performs a live ping to the Docker socket.
#[tauri::command]
pub async fn check_docker_status(
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
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
        data: Some(
            serde_json::to_value(&status)
                .map_err(|e| format!("Failed to serialize status: {}", e))?,
        ),
    })
}
