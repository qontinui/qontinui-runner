//! Change-tracking and bookmark HTTP handlers.
//!
//! Extracted from `mod.rs` to keep the per-family handler set in one
//! place. Every handler is `pub` so the public re-exports preserved by
//! `mod.rs` (`pub use bookmarks::*;`) keep `crate::mcp::ui_bridge::<name>`
//! resolvable for any external caller — even though no external caller
//! exists today, the prior pass adopted a "re-export everything" rule for
//! the moved handlers and we keep that here.
//!
//! Routes are registered via `routes()` and then merged into the parent
//! router; the parallel `route_entries()` list feeds `route_manifest()`
//! so the `/ui-bridge/_routes` discovery endpoint stays accurate.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

/// Save a bookmark (snapshot) by name.
pub async fn ui_bridge_save_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Save bookmark");
    match ui_bridge_request_sync(&state, "save_bookmark", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Save bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a bookmark by name.
pub async fn ui_bridge_get_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get bookmark '{}'", name);
    match ui_bridge_request_sync(&state, "get_bookmark", serde_json::json!({"name": name})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete a bookmark by name.
pub async fn ui_bridge_delete_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete bookmark '{}'", name);
    match ui_bridge_request_sync(&state, "delete_bookmark", serde_json::json!({"name": name})).await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Delete bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// List all bookmarks.
pub async fn ui_bridge_list_bookmarks_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List bookmarks");
    match ui_bridge_request_sync(&state, "list_bookmarks", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: List bookmarks failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Diff current state from a named bookmark.
pub async fn ui_bridge_diff_from_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff from bookmark '{}'", name);
    match ui_bridge_request_sync(
        &state,
        "diff_from_bookmark",
        serde_json::json!({"name": name}),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Diff from bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action and return the diff.
pub async fn ui_bridge_execute_with_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Execute with diff");
    match ui_bridge_request_sync(&state, "execute_with_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Execute with diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Composite endpoint: execute one or more actions with atomic change-buffer tracking.
///
/// Accepts either a single operation `{operation, elementId, params}` or a batch
/// `{operations: [{operation, elementId, params}, ...]}`. Returns the action
/// result(s) alongside the DOM changes captured during execution.
pub async fn ui_bridge_with_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Detect batch vs single based on presence of "operations" array
    if body.get("operations").is_some() {
        info!("UI Bridge API: Batch execute with diff");
        match ui_bridge_request_sync(&state, "execute_batch_with_diff", body).await {
            Ok(data) => Ok(Json(ApiResponse::success(data))),
            Err(e) => {
                error!("UI Bridge API: Batch execute with diff failed: {}", e);
                Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
            }
        }
    } else {
        // Single operation — wrap into execute_with_diff format
        // ChangeTracker.executeWithDiff expects { elementAction: { elementId, action, params } }
        info!("UI Bridge API: Single execute with diff");
        let element_id = body.get("elementId").cloned().unwrap_or_default();
        let operation = body.get("operation").cloned().unwrap_or_default();
        let params = body
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let payload = serde_json::json!({
            "elementAction": {
                "elementId": element_id,
                "action": operation,
                "params": params,
            },
            // Also pass flat fields for commandHandlers.ts compatibility
            "elementId": element_id,
            "action": operation,
            "params": params,
        });
        match ui_bridge_request_sync(&state, "execute_with_diff", payload).await {
            Ok(data) => Ok(Json(ApiResponse::success(data))),
            Err(e) => {
                error!("UI Bridge API: Single execute with diff failed: {}", e);
                Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
            }
        }
    }
}

/// Wait for a change to occur.
pub async fn ui_bridge_wait_for_change_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for change");
    match ui_bridge_request_sync(&state, "wait_for_change", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for change failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Categorize the last diff.
pub async fn ui_bridge_categorize_last_diff_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Categorize last diff");
    match ui_bridge_request_sync(&state, "categorize_last_diff", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Categorize last diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Compute a scoped diff.
pub async fn ui_bridge_scoped_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Scoped diff");
    match ui_bridge_request_sync(&state, "scoped_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Scoped diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Summarize a diff.
pub async fn ui_bridge_summarize_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Summarize diff");
    match ui_bridge_request_sync(&state, "summarize_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Summarize diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get structured changes.
pub async fn ui_bridge_structured_changes_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Structured changes");
    match ui_bridge_request_sync(&state, "structured_changes", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Structured changes failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Enable the change buffer.
pub async fn ui_bridge_enable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Enable change buffer");
    match ui_bridge_request_sync(&state, "enable_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Enable change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Disable the change buffer.
pub async fn ui_bridge_disable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Disable change buffer");
    match ui_bridge_request_sync(&state, "disable_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Disable change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Drain the change buffer.
pub async fn ui_bridge_drain_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Drain change buffer");
    match ui_bridge_request_sync(&state, "drain_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Drain change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the change buffer size.
pub async fn ui_bridge_get_change_buffer_size_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get change buffer size");
    match ui_bridge_request_sync(&state, "get_change_buffer_size", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get change buffer size failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Bookmark + change-tracking + with-diff routes (control + ai aliases).
///
/// Mirrors the original `routes()` registrations exactly — including the
/// `/control/ai/*` ↔ `/ai/*` aliasing — so the public surface is unchanged.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // Change tracking — /control/ai/* family
        .route(
            "/ui-bridge/control/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/control/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
        // Composite: execute action(s) with atomic change-buffer diffing
        .route(
            "/ui-bridge/control/with-diff",
            post(ui_bridge_with_diff_handler),
        )
        // /ai/* aliases (mirror of /control/ai/*)
        .route(
            "/ui-bridge/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmark/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmark/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into the parent `route_manifest()` so the
/// `/ui-bridge/_routes` discovery endpoint stays in sync.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        // /control/ai/*
        ("GET", "/ui-bridge/control/ai/bookmarks"),
        ("POST", "/ui-bridge/control/ai/bookmarks"),
        ("GET", "/ui-bridge/control/ai/bookmark/{name}"),
        ("DELETE", "/ui-bridge/control/ai/bookmark/{name}"),
        ("GET", "/ui-bridge/control/ai/bookmark/{name}/diff"),
        ("POST", "/ui-bridge/control/ai/execute-with-diff"),
        ("POST", "/ui-bridge/control/ai/wait-for-change"),
        ("GET", "/ui-bridge/control/ai/categorize-last-diff"),
        ("POST", "/ui-bridge/control/ai/scoped-diff"),
        ("POST", "/ui-bridge/control/ai/summarize-diff"),
        ("POST", "/ui-bridge/control/ai/structured-changes"),
        ("POST", "/ui-bridge/control/ai/change-buffer/enable"),
        ("POST", "/ui-bridge/control/ai/change-buffer/disable"),
        ("POST", "/ui-bridge/control/ai/change-buffer/drain"),
        ("GET", "/ui-bridge/control/ai/change-buffer/size"),
        // Composite
        ("POST", "/ui-bridge/control/with-diff"),
        // /ai/* aliases
        ("GET", "/ui-bridge/ai/bookmarks"),
        ("POST", "/ui-bridge/ai/bookmarks"),
        ("GET", "/ui-bridge/ai/bookmark/{name}"),
        ("DELETE", "/ui-bridge/ai/bookmark/{name}"),
        ("GET", "/ui-bridge/ai/bookmark/{name}/diff"),
        ("POST", "/ui-bridge/ai/execute-with-diff"),
        ("POST", "/ui-bridge/ai/wait-for-change"),
        ("GET", "/ui-bridge/ai/categorize-last-diff"),
        ("POST", "/ui-bridge/ai/scoped-diff"),
        ("POST", "/ui-bridge/ai/summarize-diff"),
        ("POST", "/ui-bridge/ai/structured-changes"),
        ("POST", "/ui-bridge/ai/change-buffer/enable"),
        ("POST", "/ui-bridge/ai/change-buffer/disable"),
        ("POST", "/ui-bridge/ai/change-buffer/drain"),
        ("GET", "/ui-bridge/ai/change-buffer/size"),
    ]
}
