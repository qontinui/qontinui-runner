//! Design Review HTTP handlers.
//!
//! These are the "control mode" design endpoints that wrap a handful of
//! `design_*` IPC commands (computed styles, state styles, snapshot,
//! responsive snapshots, style-guide load/get/clear, audit). The macro-
//! generated `design/evaluate*` handlers stay in `mod.rs` because their
//! definitions live in a different idiom (`ipc_handler_post!` /
//! `ipc_handler_get!`) and don't fit the manual-handler shape extracted
//! here.
//!
//! The `/ai/design-audit` alias also lives here since it points at the
//! design-audit handler defined below.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

/// Get extended computed styles for a single element.
pub async fn ui_bridge_design_element_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get element styles for {}", id);

    let payload = serde_json::json!({
        "elementId": id
    });

    match ui_bridge_request_sync(&state, "design_get_element_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design element styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get styles across interaction states (hover, focus, active, disabled).
pub async fn ui_bridge_design_state_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get state styles for {}", id);

    let mut payload = serde_json::json!({
        "elementId": id
    });

    if let Some(Json(body)) = body {
        if let (Some(base), Some(extra)) = (payload.as_object_mut(), body.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
    }

    match ui_bridge_request_sync(&state, "design_get_state_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design state styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get design snapshot for all or filtered elements.
pub async fn ui_bridge_design_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get design snapshot");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_get_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design snapshot failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture responsive snapshots at multiple viewport widths.
pub async fn ui_bridge_design_responsive_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get responsive snapshots");

    match ui_bridge_request_sync(&state, "design_get_responsive", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design responsive failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Run a style audit against a loaded or provided style guide.
pub async fn ui_bridge_design_audit_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - run style audit");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_run_audit", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design audit failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Load a style guide for subsequent audits.
pub async fn ui_bridge_design_load_style_guide_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - load style guide");

    match ui_bridge_request_sync(&state, "design_load_style_guide", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design load style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the currently loaded style guide.
pub async fn ui_bridge_design_get_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_get_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design get style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear the currently loaded style guide.
pub async fn ui_bridge_design_clear_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - clear style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_clear_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design clear style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Manual design-review routes (does not include the macro-generated
/// `design/evaluate*` family — those stay in `mod.rs`).
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/design/element/{id}/styles",
            get(ui_bridge_design_element_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/element/{id}/state-styles",
            post(ui_bridge_design_state_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/snapshot",
            post(ui_bridge_design_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/design/responsive",
            post(ui_bridge_design_responsive_handler),
        )
        .route(
            "/ui-bridge/control/design/audit",
            post(ui_bridge_design_audit_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/load",
            post(ui_bridge_design_load_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide",
            get(ui_bridge_design_get_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/clear",
            post(ui_bridge_design_clear_style_guide_handler),
        )
        // /ai/* alias for the design audit handler
        .route(
            "/ui-bridge/ai/design-audit",
            post(ui_bridge_design_audit_handler),
        )
}

/// Static (method, path) tuples corresponding to every route registered
/// by `routes()`. Concatenated into `route_manifest()` in `mod.rs` so the
/// `/ui-bridge/_routes` discovery endpoint stays in sync.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/control/design/element/{id}/styles"),
        (
            "POST",
            "/ui-bridge/control/design/element/{id}/state-styles",
        ),
        ("POST", "/ui-bridge/control/design/snapshot"),
        ("POST", "/ui-bridge/control/design/responsive"),
        ("POST", "/ui-bridge/control/design/audit"),
        ("POST", "/ui-bridge/control/design/style-guide/load"),
        ("GET", "/ui-bridge/control/design/style-guide"),
        ("POST", "/ui-bridge/control/design/style-guide/clear"),
        ("POST", "/ui-bridge/ai/design-audit"),
    ]
}
