//! Tauri commands for managing runner instances (dev feature).

use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;

use super::CommandResponse;
use crate::commands::compartments::HealthCompartment;
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
    if port < 1024 {
        return Err(format!(
            "Port {} is in the privileged range (0-1023). Use a port >= 1024.",
            port
        ));
    }
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

/// Get identity info about this runner instance: whether it's secondary and what primary it proxies to.
#[tauri::command]
pub async fn get_runner_identity(
    health: State<'_, HealthCompartment>,
) -> Result<serde_json::Value, String> {
    let is_secondary = crate::process_capture::primary_proxy::is_secondary();
    let instance_name = std::env::var("QONTINUI_INSTANCE_NAME").ok();
    let primary_port = crate::process_capture::primary_proxy::primary_port();
    let own_port = health
        .api_port()
        .load(std::sync::atomic::Ordering::Relaxed);

    Ok(serde_json::json!({
        "is_secondary": is_secondary,
        "instance_name": instance_name,
        "primary_port": primary_port,
        "port": own_port,
    }))
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_instances")
        .invoke_handler(tauri::generate_handler![
            get_runner_instances,
            save_runner_instance,
            delete_runner_instance,
            launch_runner_instance,
            stop_runner_instance,
            get_runner_identity,
        ])
        .build()
}
