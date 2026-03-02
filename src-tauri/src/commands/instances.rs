//! Tauri commands for managing runner instances (dev feature).

use std::sync::Arc;
use tauri::State;
use tracing::info;

use super::CommandResponse;
use crate::instance_manager::InstanceManager;
use crate::settings::{self, RunnerInstanceConfig};

/// Get all configured instances with their live status.
#[tauri::command]
pub async fn get_runner_instances(
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<serde_json::Value, String> {
    let configs = settings::get_runner_instances();
    let statuses = instance_manager.get_all_statuses(&configs).await;
    serde_json::to_value(&statuses).map_err(|e| e.to_string())
}

/// Save or update an instance configuration.
#[tauri::command]
pub async fn save_runner_instance(
    id: String,
    name: String,
    port: u16,
) -> Result<CommandResponse, String> {
    let config = RunnerInstanceConfig {
        id,
        name: name.clone(),
        port,
    };
    settings::save_runner_instance(config)?;
    info!("Saved runner instance config: {} on port {}", name, port);
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Instance '{}' saved", name)),
        data: None,
    })
}

/// Delete an instance configuration (only when stopped).
#[tauri::command]
pub async fn delete_runner_instance(
    id: String,
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<CommandResponse, String> {
    // Check that it's not running
    let configs = settings::get_runner_instances();
    if let Some(config) = configs.iter().find(|c| c.id == id) {
        let status = instance_manager.get_instance_status(config).await;
        if status.running {
            return Err("Cannot delete a running instance. Stop it first.".into());
        }
    }
    settings::delete_runner_instance(&id)?;
    info!("Deleted runner instance config: {}", id);
    Ok(CommandResponse {
        success: true,
        message: Some("Instance deleted".into()),
        data: None,
    })
}

/// Launch a runner instance.
#[tauri::command]
pub async fn launch_runner_instance(
    id: String,
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<CommandResponse, String> {
    let configs = settings::get_runner_instances();
    let config = configs
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("Instance '{}' not found in settings", id))?;

    let pid = instance_manager.launch_instance(config).await?;
    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Instance '{}' launched (PID: {})",
            config.name, pid
        )),
        data: Some(serde_json::json!({ "pid": pid })),
    })
}

/// Stop a running instance.
#[tauri::command]
pub async fn stop_runner_instance(
    id: String,
    instance_manager: State<'_, Arc<InstanceManager>>,
) -> Result<CommandResponse, String> {
    instance_manager.stop_instance(&id).await?;
    Ok(CommandResponse {
        success: true,
        message: Some("Instance stopped".into()),
        data: None,
    })
}
