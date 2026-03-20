//! Playwright state collection handlers for MCP API
//!
//! Provides HTTP handlers for starting, monitoring, retrieving results from,
//! and stopping Playwright-based UI state collection jobs.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

/// Request to start Playwright state collection
#[derive(Debug, Deserialize)]
pub struct StartPlaywrightCollectionRequest {
    /// Target URL to collect from
    pub url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 50)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Risk level: "safe", "caution", or "dry_run" (default: "safe")
    #[serde(default)]
    pub max_risk_level: Option<String>,
    /// Skip clicking elements (default: false)
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Verify extractions with pattern matching (default: true)
    #[serde(default)]
    pub verify_extractions: Option<bool>,
    /// Verification similarity threshold (default: 0.85)
    #[serde(default)]
    pub verification_threshold: Option<f32>,
    /// Additional keywords to block
    #[serde(default)]
    pub additional_blocked_keywords: Option<Vec<String>>,
    /// Additional keywords to allow
    #[serde(default)]
    pub additional_safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
}

/// Response for Playwright collection status
#[derive(Debug, Serialize)]
pub struct PlaywrightCollectionStatusResponse {
    pub job_id: Option<String>,
    pub status: String,
    pub url: Option<String>,
    pub progress_message: Option<String>,
    pub progress_percent: Option<i32>,
    pub error: Option<String>,
    pub has_results: Option<bool>,
}

// ============================================================================
// Handlers
// ============================================================================

/// Start Playwright state collection
pub async fn start_playwright_collection(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartPlaywrightCollectionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting Playwright collection for URL: {}",
        request.url
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "url": request.url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(50),
        "max_risk_level": request.max_risk_level.clone().unwrap_or_else(|| "safe".to_string()),
        "dry_run": request.dry_run.unwrap_or(false),
        "verify_extractions": request.verify_extractions.unwrap_or(true),
        "verification_threshold": request.verification_threshold.unwrap_or(0.85),
        "additional_blocked_keywords": request.additional_blocked_keywords.clone(),
        "additional_safe_keywords": request.additional_safe_keywords.clone(),
        "blocked_selectors": request.blocked_selectors.clone(),
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_playwright_collection", Some(params), timeout)
        })?
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
                info!("MCP API: Playwright collection started");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start Playwright collection".to_string());
                error!(
                    "MCP API: Playwright collection failed to start: {}",
                    error_msg
                );
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start Playwright collection: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get Playwright collection status
pub async fn get_playwright_collection_status(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let job_id = params.get("job_id").cloned();

    let cmd_params = serde_json::json!({
        "job_id": job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "get_playwright_collection_status",
                Some(cmd_params),
                timeout,
            )
        })?
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
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "status": "idle",
                        "job_id": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());
                error!("MCP API: Playwright collection status error: {}", error_msg);
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "status": "error",
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get Playwright collection status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get Playwright collection results
pub async fn get_playwright_collection_results(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let job_id = params.get("job_id").cloned();

    let cmd_params = serde_json::json!({
        "job_id": job_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            // Use longer timeout for getting results (may include large screenshots)
            let timeout = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait(
                "get_playwright_collection_results",
                Some(cmd_params),
                timeout,
            )
        })?
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
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": false,
                        "error": "No results available"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get results".to_string());
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get Playwright collection results: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop Playwright collection
pub async fn stop_playwright_collection(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping Playwright collection");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_playwright_collection", None)
        })?
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
        Ok(_) => {
            info!("MCP API: Playwright collection stopped");
            Ok(Json(ApiResponse::success(
                "Playwright collection stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop Playwright collection: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/playwright-collection/start",
            post(start_playwright_collection),
        )
        .route(
            "/playwright-collection/status",
            get(get_playwright_collection_status),
        )
        .route(
            "/playwright-collection/results",
            get(get_playwright_collection_results),
        )
        .route(
            "/playwright-collection/stop",
            post(stop_playwright_collection),
        )
}
