//! AWAS (Application Web Automation Specification) handlers for MCP API
//!
//! Provides HTTP handlers for AWAS operations:
//! - Discover AWAS manifest from a URL
//! - Execute AWAS actions
//! - Check AWAS support for a URL
//! - List available AWAS actions
//! - Extract AWAS elements from HTML

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp_api::{api_error, ApiResponse, ApiState};

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request to discover AWAS manifest from a URL
#[derive(Debug, Deserialize)]
pub struct AwasDiscoverRequest {
    /// URL to discover AWAS manifest from
    pub url: String,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Request to execute an AWAS action
#[derive(Debug, Deserialize)]
pub struct AwasExecuteRequest {
    /// URL of the application
    pub url: String,
    /// Action ID to execute
    pub action_id: String,
    /// Optional parameters for the action
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Request to check AWAS support for a URL
#[derive(Debug, Deserialize)]
pub struct AwasCheckSupportRequest {
    /// URL to check for AWAS support
    pub url: String,
    /// Optional timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Request to extract AWAS elements from HTML
#[derive(Debug, Deserialize)]
pub struct AwasExtractElementsRequest {
    /// HTML content to extract elements from
    pub html: String,
    /// Optional base URL for resolving relative URLs
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Response for AWAS discover operation
#[derive(Debug, Serialize)]
pub struct AwasDiscoverResponse {
    /// Whether discovery was successful
    pub success: bool,
    /// The discovered AWAS manifest (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for AWAS execute operation
#[derive(Debug, Serialize)]
pub struct AwasExecuteResponse {
    /// Whether execution was successful
    pub success: bool,
    /// Result data from the action (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for AWAS check support operation
#[derive(Debug, Serialize)]
pub struct AwasCheckSupportResponse {
    /// Whether the URL supports AWAS
    pub supported: bool,
    /// AWAS version (if supported)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Manifest URL (if supported)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    /// Error message (if check failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Single AWAS action info
#[derive(Debug, Serialize)]
pub struct AwasActionInfo {
    /// Action identifier
    pub id: String,
    /// Human-readable action name
    pub name: String,
    /// Description of what the action does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Required parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Response for AWAS list actions operation
#[derive(Debug, Serialize)]
pub struct AwasListActionsResponse {
    /// List of available actions
    pub actions: Vec<AwasActionInfo>,
    /// URL the actions are from
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Response for AWAS extract elements operation
#[derive(Debug, Serialize)]
pub struct AwasExtractElementsResponse {
    /// Whether extraction was successful
    pub success: bool,
    /// Extracted elements
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_timeout() -> u64 {
    30
}

// ============================================================================
// Handlers
// ============================================================================

/// Discover AWAS manifest from a URL
///
/// POST /awas/discover
pub async fn awas_discover(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AwasDiscoverRequest>,
) -> Result<Json<ApiResponse<AwasDiscoverResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: AWAS discover for URL: {}", request.url);

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "url": request.url,
    });
    let timeout_secs = request.timeout_seconds;

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(timeout_secs);
            bridge.send_command_and_wait("awas_discover", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: AWAS discover completed successfully");
                Ok(Json(ApiResponse::success(AwasDiscoverResponse {
                    success: true,
                    manifest: response.data,
                    error: None,
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "AWAS discover failed".to_string());
                error!("MCP API: AWAS discover failed: {}", error_msg);
                Ok(Json(ApiResponse::success(AwasDiscoverResponse {
                    success: false,
                    manifest: None,
                    error: Some(error_msg),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run AWAS discover: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an AWAS action
///
/// POST /awas/execute
pub async fn awas_execute(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AwasExecuteRequest>,
) -> Result<Json<ApiResponse<AwasExecuteResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: AWAS execute action '{}' for URL: {}",
        request.action_id, request.url
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "url": request.url,
        "action_id": request.action_id,
        "params": request.params,
    });
    let timeout_secs = request.timeout_seconds;

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(timeout_secs);
            bridge.send_command_and_wait("awas_execute", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: AWAS execute completed successfully");
                Ok(Json(ApiResponse::success(AwasExecuteResponse {
                    success: true,
                    result: response.data,
                    error: None,
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "AWAS execute failed".to_string());
                error!("MCP API: AWAS execute failed: {}", error_msg);
                Ok(Json(ApiResponse::success(AwasExecuteResponse {
                    success: false,
                    result: None,
                    error: Some(error_msg),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run AWAS execute: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Check if a URL supports AWAS
///
/// POST /awas/check-support
pub async fn awas_check_support(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AwasCheckSupportRequest>,
) -> Result<Json<ApiResponse<AwasCheckSupportResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: AWAS check support for URL: {}", request.url);

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "url": request.url,
    });
    let timeout_secs = request.timeout_seconds;

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(timeout_secs);
            bridge.send_command_and_wait("awas_check_support", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: AWAS check support completed");
                // Parse the response data to extract supported, version, manifest_url
                let data = response.data.unwrap_or(serde_json::json!({}));
                let supported = data.get("supported").and_then(|v| v.as_bool()).unwrap_or(false);
                let version = data.get("version").and_then(|v| v.as_str()).map(String::from);
                let manifest_url = data.get("manifest_url").and_then(|v| v.as_str()).map(String::from);

                Ok(Json(ApiResponse::success(AwasCheckSupportResponse {
                    supported,
                    version,
                    manifest_url,
                    error: None,
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "AWAS check support failed".to_string());
                error!("MCP API: AWAS check support failed: {}", error_msg);
                Ok(Json(ApiResponse::success(AwasCheckSupportResponse {
                    supported: false,
                    version: None,
                    manifest_url: None,
                    error: Some(error_msg),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run AWAS check support: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// List available AWAS actions (requires a loaded manifest)
///
/// GET /awas/actions
pub async fn awas_list_actions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<AwasListActionsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: AWAS list actions");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("awas_list_actions", None, timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: AWAS list actions completed");
                // Parse the response data
                let data = response.data.unwrap_or(serde_json::json!({}));
                let url = data.get("url").and_then(|v| v.as_str()).map(String::from);
                let actions_data = data.get("actions").and_then(|v| v.as_array());

                let actions: Vec<AwasActionInfo> = actions_data
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|a| {
                                let id = a.get("id")?.as_str()?.to_string();
                                let name = a.get("name").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
                                let description = a.get("description").and_then(|v| v.as_str()).map(String::from);
                                let params = a.get("params").cloned();
                                Some(AwasActionInfo {
                                    id,
                                    name,
                                    description,
                                    params,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Ok(Json(ApiResponse::success(AwasListActionsResponse {
                    actions,
                    url,
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "AWAS list actions failed".to_string());
                error!("MCP API: AWAS list actions failed: {}", error_msg);
                // Return empty list on failure
                Ok(Json(ApiResponse::success(AwasListActionsResponse {
                    actions: vec![],
                    url: None,
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run AWAS list actions: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Extract AWAS elements from HTML content
///
/// POST /awas/extract-elements
pub async fn awas_extract_elements(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AwasExtractElementsRequest>,
) -> Result<Json<ApiResponse<AwasExtractElementsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: AWAS extract elements (HTML length: {} bytes)",
        request.html.len()
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "html": request.html,
        "base_url": request.base_url,
    });

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("awas_extract_elements", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: AWAS extract elements completed");
                Ok(Json(ApiResponse::success(AwasExtractElementsResponse {
                    success: true,
                    elements: response.data,
                    error: None,
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "AWAS extract elements failed".to_string());
                error!("MCP API: AWAS extract elements failed: {}", error_msg);
                Ok(Json(ApiResponse::success(AwasExtractElementsResponse {
                    success: false,
                    elements: None,
                    error: Some(error_msg),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run AWAS extract elements: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}
