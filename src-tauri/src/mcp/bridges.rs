//! Bridge management endpoints for MCP API
//!
//! Contains bridge CRUD operations, bridge workflow execution,
//! GUI lock queries, and headless-only mode management.

use axum::{
    extract::{Path, State},
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::executor::{BridgeInfo, BridgeMode, CreateBridgeResult, GuiLockInfo};
use crate::mcp::types::{ApiResponse, ApiState};

// ============================================================================
// Bridge Management Endpoints
// ============================================================================

/// Request body for creating a new bridge
#[derive(Debug, Deserialize)]
pub struct CreateBridgeRequest {
    /// Operating mode: "gui" or "headless"
    #[serde(default)]
    mode: BridgeMode,
    /// Optional task run ID to associate with this bridge
    run_id: Option<String>,
    /// Monitor indices for GUI mode (default: [0])
    #[serde(default)]
    monitor_indices: Vec<i32>,
    /// Force acquire GUI lock even if held by another bridge
    #[serde(default)]
    force_gui_lock: bool,
}

/// Request body for running a workflow on a specific bridge
#[derive(Debug, Deserialize)]
pub struct BridgeWorkflowRequest {
    /// Workflow name to run
    workflow_name: Option<String>,
    /// Config path to load (optional if already loaded)
    config_path: Option<String>,
    /// Workflow parameters
    params: Option<serde_json::Value>,
}

/// List all active bridges
pub async fn list_bridges(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<BridgeInfo>>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let bridges = bridge_manager.list_bridges().await;
        Json(ApiResponse::success(bridges))
    } else {
        Json(ApiResponse::<Vec<BridgeInfo>>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Create a new bridge
pub async fn create_bridge(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateBridgeRequest>,
) -> Json<ApiResponse<CreateBridgeResult>> {
    info!(
        "Creating new bridge: mode={:?}, run_id={:?}",
        request.mode, request.run_id
    );

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let monitor_indices = if request.monitor_indices.is_empty() {
            vec![0]
        } else {
            request.monitor_indices
        };

        match bridge_manager
            .create_bridge(
                request.mode,
                request.run_id,
                monitor_indices,
                request.force_gui_lock,
            )
            .await
        {
            Ok(result) => Json(ApiResponse::success(result)),
            Err(e) => Json(ApiResponse::<CreateBridgeResult>::error(&e)),
        }
    } else {
        Json(ApiResponse::<CreateBridgeResult>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Get info for a specific bridge
pub async fn get_bridge(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
) -> Json<ApiResponse<Option<BridgeInfo>>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let info = bridge_manager.get_bridge_info(&bridge_id).await;
        Json(ApiResponse::success(info))
    } else {
        Json(ApiResponse::<Option<BridgeInfo>>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Remove a bridge
pub async fn remove_bridge(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
) -> Json<ApiResponse<()>> {
    info!("Removing bridge: {}", bridge_id);

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        match bridge_manager.remove_bridge(&bridge_id).await {
            Ok(()) => Json(ApiResponse::success(())),
            Err(e) => Json(ApiResponse::<()>::error(&e)),
        }
    } else {
        Json(ApiResponse::<()>::error("Bridge manager not initialized"))
    }
}

/// Run a workflow on a specific bridge
pub async fn run_bridge_workflow(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
    Json(request): Json<BridgeWorkflowRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!(
        "Running workflow on bridge {}: {:?}",
        bridge_id, request.workflow_name
    );

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        // Load config if provided
        if let Some(config_path) = request.config_path {
            let load_result = bridge_manager
                .with_bridge(&bridge_id, |bridge| bridge.load_configuration(&config_path));

            if let Err(e) = load_result {
                return Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to access bridge: {}",
                    e
                )));
            }

            if let Ok(Err(e)) = load_result {
                return Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to load config: {}",
                    e
                )));
            }
        }

        // Build execution params
        let params = if request.workflow_name.is_some() || request.params.is_some() {
            Some(serde_json::json!({
                "workflow_name": request.workflow_name,
                "params": request.params,
            }))
        } else {
            None
        };

        // Start execution
        let start_result = bridge_manager.with_bridge(&bridge_id, |bridge| {
            bridge.start_execution_with_params(params)
        });

        match start_result {
            Ok(Ok(())) => Json(ApiResponse::success(serde_json::json!({
                "message": "Workflow started",
                "bridge_id": bridge_id,
            }))),
            Ok(Err(e)) => Json(ApiResponse::<serde_json::Value>::error(format!(
                "Failed to start workflow: {}",
                e
            ))),
            Err(e) => Json(ApiResponse::<serde_json::Value>::error(e)),
        }
    } else {
        Json(ApiResponse::<serde_json::Value>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Get current GUI lock holder
pub async fn get_gui_lock(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<GuiLockInfo>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let info = bridge_manager.get_gui_lock_info().await;
        Json(ApiResponse::success(info))
    } else {
        Json(ApiResponse::<GuiLockInfo>::error(
            "Bridge manager not initialized",
        ))
    }
}

// ============================================================================
// Headless-Only Mode Endpoints
// ============================================================================

/// Response for headless-only mode status
#[derive(Debug, Serialize)]
pub struct HeadlessOnlyResponse {
    /// Whether headless-only mode is enabled
    enabled: bool,
    /// Description of what this mode does
    description: String,
}

/// Request body for setting headless-only mode
#[derive(Debug, Deserialize)]
pub struct SetHeadlessOnlyRequest {
    /// Whether to enable headless-only mode
    enabled: bool,
}

/// Get headless-only mode status
///
/// When headless-only mode is enabled, GUI bridges cannot be created.
/// This is intended for server deployments without GUI access.
pub async fn get_headless_only(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<HeadlessOnlyResponse>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let enabled = bridge_manager.is_headless_only();
        Json(ApiResponse::success(HeadlessOnlyResponse {
            enabled,
            description: if enabled {
                "Headless-only mode is ENABLED. All bridges must be headless. \
                GUI mode bridges cannot be created. This is intended for server deployments."
                    .to_string()
            } else {
                "Headless-only mode is DISABLED. Both GUI and headless bridges can be created."
                    .to_string()
            },
        }))
    } else {
        Json(ApiResponse::<HeadlessOnlyResponse>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Set headless-only mode
///
/// When enabled, all bridges must be created in headless mode.
/// GUI mode requests will be rejected with an error.
pub async fn set_headless_only(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetHeadlessOnlyRequest>,
) -> Json<ApiResponse<HeadlessOnlyResponse>> {
    info!("Setting headless-only mode to: {}", request.enabled);

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        bridge_manager.set_headless_only(request.enabled);

        Json(ApiResponse::success(HeadlessOnlyResponse {
            enabled: request.enabled,
            description: if request.enabled {
                "Headless-only mode is now ENABLED. All bridges must be headless. \
                GUI mode bridges cannot be created."
                    .to_string()
            } else {
                "Headless-only mode is now DISABLED. Both GUI and headless bridges can be created."
                    .to_string()
            },
        }))
    } else {
        Json(ApiResponse::<HeadlessOnlyResponse>::error(
            "Bridge manager not initialized",
        ))
    }
}
