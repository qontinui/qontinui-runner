//! Bridge-specific workflow execution commands
//!
//! Commands for running workflows on specific bridges and managing GUI lock transfers.
//! These commands enable multi-bridge scenarios where workflows need to run on
//! different bridges, and GUI lock ownership may need to be transferred.

use crate::commands::compartments::BridgeCompartment;
use crate::commands::CommandResponse;
use crate::executor::bridge_helpers::get_bridge_manager_compartment;
use tauri::State;
use tracing::{debug, info, warn};

/// Run a workflow on a specific bridge.
///
/// This command allows running a workflow on a bridge other than the default.
/// Useful for multi-bridge scenarios where different bridges handle different
/// parts of a workflow or different applications.
///
/// # Arguments
/// * `bridge_id` - The ID of the bridge to run the workflow on
/// * `workflow_id` - The ID of the workflow to execute
/// * `params` - Optional additional parameters to pass to the workflow
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with execution result
/// * `Err(String)` - Error if bridge not found or execution fails
#[tauri::command]
pub async fn run_workflow_on_bridge(
    bridge_id: String,
    workflow_id: String,
    params: Option<serde_json::Value>,
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    info!(
        "Running workflow '{}' on bridge '{}'",
        workflow_id, bridge_id
    );

    let manager = get_bridge_manager_compartment(&bridge)?;

    // Verify the bridge exists and get its info
    let bridge_info = manager
        .get_bridge_info(&bridge_id)
        .await
        .ok_or_else(|| format!("Bridge not found: {}", bridge_id))?;

    debug!(
        "Bridge '{}' found: mode={:?}, state={}, has_gui_lock={}",
        bridge_id, bridge_info.mode, bridge_info.state, bridge_info.has_gui_lock
    );

    // Build execution parameters
    let mut execution_params = serde_json::Map::new();
    execution_params.insert("workflow_id".to_string(), serde_json::json!(workflow_id));

    // Merge in any additional params
    if let Some(serde_json::Value::Object(extra_params)) = params {
        for (key, value) in extra_params {
            execution_params.insert(key, value);
        }
    }

    // If the bridge has monitor indices configured, add them
    if !bridge_info.monitor_indices.is_empty() {
        execution_params.insert(
            "monitor_indices".to_string(),
            serde_json::json!(bridge_info.monitor_indices),
        );
        execution_params.insert(
            "monitor_index".to_string(),
            serde_json::json!(bridge_info.monitor_indices.first().copied().unwrap_or(0)),
        );
    }

    // Execute on the specific bridge
    let result = manager.with_bridge(&bridge_id, |bridge| {
        bridge
            .start_execution_with_params(Some(serde_json::Value::Object(execution_params)))
            .map_err(|e| format!("Failed to start execution: {}", e))
    })?;

    match result {
        Ok(()) => {
            info!(
                "Workflow '{}' started on bridge '{}'",
                workflow_id, bridge_id
            );
            Ok(CommandResponse {
                success: true,
                message: Some(format!(
                    "Workflow '{}' started on bridge '{}'",
                    workflow_id, bridge_id
                )),
                data: Some(serde_json::json!({
                    "bridge_id": bridge_id,
                    "workflow_id": workflow_id,
                    "mode": format!("{:?}", bridge_info.mode).to_lowercase()
                })),
            })
        }
        Err(e) => {
            warn!(
                "Failed to start workflow '{}' on bridge '{}': {}",
                workflow_id, bridge_id, e
            );
            Err(e)
        }
    }
}

/// Transfer the GUI lock from one bridge to another.
///
/// This command atomically transfers the GUI lock from one bridge to another.
/// Useful for handoff scenarios where one bridge finishes GUI operations and
/// another needs to take over.
///
/// # Arguments
/// * `from_bridge_id` - The ID of the bridge currently holding the lock
/// * `to_bridge_id` - The ID of the bridge to transfer the lock to
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with transfer details
/// * `Err(String)` - Error if transfer fails (e.g., source doesn't hold lock)
#[tauri::command]
pub async fn transfer_gui_lock(
    from_bridge_id: String,
    to_bridge_id: String,
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    info!(
        "Transferring GUI lock from '{}' to '{}'",
        from_bridge_id, to_bridge_id
    );

    let manager = get_bridge_manager_compartment(&bridge)?;

    // Verify both bridges exist
    if !manager.has_bridge(&from_bridge_id) {
        return Err(format!("Source bridge not found: {}", from_bridge_id));
    }
    if !manager.has_bridge(&to_bridge_id) {
        return Err(format!("Target bridge not found: {}", to_bridge_id));
    }

    // Perform the transfer
    manager
        .transfer_gui_lock(&from_bridge_id, &to_bridge_id)
        .await?;

    info!(
        "GUI lock transferred from '{}' to '{}'",
        from_bridge_id, to_bridge_id
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "GUI lock transferred from '{}' to '{}'",
            from_bridge_id, to_bridge_id
        )),
        data: Some(serde_json::json!({
            "from_bridge_id": from_bridge_id,
            "to_bridge_id": to_bridge_id
        })),
    })
}

/// List all available bridges with their current status.
///
/// Returns information about all bridges including their mode, state,
/// and whether they hold the GUI lock.
///
/// # Arguments
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of bridge info
/// * `Err(String)` - Error if bridge manager not initialized
#[tauri::command]
pub async fn list_bridges(bridge: State<'_, BridgeCompartment>) -> Result<CommandResponse, String> {
    debug!("Listing all bridges");

    let manager = get_bridge_manager_compartment(&bridge)?;
    let bridges = manager.list_bridges().await;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "bridges": bridges,
            "count": bridges.len()
        })),
    })
}

/// Get information about a specific bridge.
///
/// # Arguments
/// * `bridge_id` - The ID of the bridge to get info for
/// * `state` - Application state containing the bridge manager
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with bridge info
/// * `Err(String)` - Error if bridge not found
#[tauri::command]
pub async fn get_bridge_info(
    bridge_id: String,
    bridge: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    debug!("Getting info for bridge '{}'", bridge_id);

    let manager = get_bridge_manager_compartment(&bridge)?;

    match manager.get_bridge_info(&bridge_id).await {
        Some(info) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::to_value(&info).unwrap_or_default()),
        }),
        None => Err(format!("Bridge not found: {}", bridge_id)),
    }
}
